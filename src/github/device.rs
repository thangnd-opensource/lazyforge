//! Logging in to GitHub through the browser, without a personal access token: the OAuth
//! 2.0 device authorization grant ("device flow"), the way `gh auth login` does it.
//!
//! 1. Ask GitHub for a code (`POST /login/device/code`).
//! 2. The user opens `https://github.com/login/device`, types the code and authorizes.
//! 3. Meanwhile, ask for the token every few seconds (`POST /login/oauth/access_token`)
//!    until GitHub hands it out, the user declines, or the code expires.
//!
//! Only the OAuth App's client id is needed; it is public (there is no client secret in this
//! flow), so it can live in the source code.

use std::time::Duration;

use serde::Deserialize;
use url::Url;

use crate::forge::ApiError;

/// Client id of the lazyforge OAuth App on github.com (Settings → Developer settings → OAuth
/// Apps, with "Enable Device Flow" checked and token expiration off).
const LAZYFORGE_CLIENT_ID: &str = "Ov23likHpjs5g92CB1D1";

/// What lazyforge needs: private repositories (pull requests, Actions), organization
/// repositories, and re-running workflows.
pub const SCOPES: &str = "repo read:org workflow";

/// The client id to use: `LAZYFORGE_GITHUB_CLIENT_ID` if set (for development, or for a GitHub
/// Enterprise server, which needs its own OAuth App), otherwise lazyforge's own, if any.
pub fn client_id(host: &Url) -> Option<String> {
    let from_env = std::env::var("LAZYFORGE_GITHUB_CLIENT_ID")
        .ok()
        .filter(|id| !id.trim().is_empty());
    if from_env.is_some() {
        return from_env;
    }
    // lazyforge's app only exists on github.com.
    (host.host_str() == Some("github.com") && !LAZYFORGE_CLIENT_ID.is_empty())
        .then(|| LAZYFORGE_CLIENT_ID.to_string())
}

/// The code to show the user, from step 1.
#[derive(Debug, Clone, Deserialize)]
pub struct DeviceCode {
    pub device_code: String,
    /// What the user types on the verification page, e.g. `ABCD-1234`.
    pub user_code: String,
    pub verification_uri: String,
    /// Seconds before the code expires.
    pub expires_in: u64,
    /// Seconds to wait between two token requests.
    pub interval: u64,
}

/// How long to wait between token requests, and how much longer after a `slow_down`.
/// GitHub's values in real use; milliseconds in tests.
#[derive(Debug, Clone, Copy)]
pub struct Pacing {
    pub interval: Duration,
    pub slow_down: Duration,
}

impl From<&DeviceCode> for Pacing {
    fn from(code: &DeviceCode) -> Self {
        Self {
            interval: Duration::from_secs(code.interval.max(1)),
            slow_down: Duration::from_secs(5),
        }
    }
}

/// GitHub answers these endpoints with status 200 and either the result or an `error`.
#[derive(Debug, Deserialize)]
struct OAuthError {
    error: Option<String>,
    error_description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

fn http() -> Result<reqwest::Client, ApiError> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!("lazyforge/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(30))
        .build()?)
}

/// Step 1. `host` is the web address, e.g. `https://github.com/`.
pub async fn request_code(host: &Url, client_id: &str) -> Result<DeviceCode, ApiError> {
    let response = http()?
        .post(host.join("login/device/code")?)
        .header(reqwest::header::ACCEPT, "application/json")
        .form(&[("client_id", client_id), ("scope", SCOPES)])
        .send()
        .await?;
    let status = response.status();
    let body = response.text().await?;
    if let Ok(OAuthError {
        error: Some(error),
        error_description,
    }) = serde_json::from_str::<OAuthError>(&body)
    {
        return Err(ApiError::Forbidden(explain(&error, error_description)));
    }
    if !status.is_success() {
        return Err(ApiError::from_response(status, &body));
    }
    serde_json::from_str(&body)
        .map_err(|err| ApiError::UnexpectedResponse(format!("cannot read the device code: {err}")))
}

/// Step 3: waits for the user to authorize, then returns the access token.
pub async fn wait_for_token(
    host: &Url,
    client_id: &str,
    code: &DeviceCode,
    mut pacing: Pacing,
) -> Result<String, ApiError> {
    let http = http()?;
    let url = host.join("login/oauth/access_token")?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(code.expires_in);
    loop {
        tokio::time::sleep(pacing.interval).await;
        if tokio::time::Instant::now() > deadline {
            return Err(ApiError::Forbidden(explain("expired_token", None)));
        }
        let response: TokenResponse = http
            .post(url.clone())
            .header(reqwest::header::ACCEPT, "application/json")
            .form(&[
                ("client_id", client_id),
                ("device_code", code.device_code.as_str()),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .send()
            .await?
            .json()
            .await?;
        match (response.access_token, response.error.as_deref()) {
            (Some(token), _) if !token.is_empty() => return Ok(token),
            (_, Some("authorization_pending")) => {}
            // Asking too often: wait longer from now on, as the spec requires.
            (_, Some("slow_down")) => pacing.interval += pacing.slow_down,
            (_, Some(error)) => {
                return Err(ApiError::Forbidden(explain(
                    error,
                    response.error_description,
                )));
            }
            _ => {
                return Err(ApiError::UnexpectedResponse(
                    "GitHub sent neither a token nor an error".into(),
                ));
            }
        }
    }
}

/// Plain-language versions of GitHub's OAuth errors.
fn explain(error: &str, description: Option<String>) -> String {
    match error {
        "expired_token" => "the code expired before it was entered; run the login again".into(),
        "access_denied" => "the authorization was declined on GitHub".into(),
        "device_flow_disabled" => {
            "device flow is not enabled for this OAuth App (enable it in the app's settings)".into()
        }
        "incorrect_client_credentials" => "GitHub does not know this OAuth App client id".into(),
        "unsupported_grant_type" | "incorrect_device_code" => {
            format!("GitHub rejected the login ({error})")
        }
        _ => description.unwrap_or_else(|| error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn host(server: &MockServer) -> Url {
        crate::config::parse_host(&server.uri()).unwrap()
    }

    fn code() -> DeviceCode {
        DeviceCode {
            device_code: "dev-123".into(),
            user_code: "ABCD-1234".into(),
            verification_uri: "https://github.com/login/device".into(),
            expires_in: 900,
            interval: 5,
        }
    }

    const FAST: Pacing = Pacing {
        interval: Duration::from_millis(5),
        slow_down: Duration::from_millis(5),
    };

    #[tokio::test]
    async fn requests_a_code_with_the_scopes() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/login/device/code"))
            .and(header("accept", "application/json"))
            .and(body_string_contains("client_id=Iv1.test"))
            .and(body_string_contains("scope=repo+read%3Aorg+workflow"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "device_code": "dev-123", "user_code": "ABCD-1234",
                "verification_uri": "https://github.com/login/device",
                "expires_in": 899, "interval": 5
            })))
            .expect(1)
            .mount(&server)
            .await;

        let code = request_code(&host(&server), "Iv1.test").await.unwrap();
        assert_eq!(code.user_code, "ABCD-1234");
        assert_eq!(Pacing::from(&code).interval, Duration::from_secs(5));
    }

    #[tokio::test]
    async fn explains_a_disabled_device_flow() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "error": "device_flow_disabled", "error_description": "Device Flow must be enabled"
            })))
            .mount(&server)
            .await;
        let err = request_code(&host(&server), "Iv1.test").await.unwrap_err();
        assert!(
            err.to_string().contains("enable it in the app's settings"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn waits_while_pending_and_slows_down_when_asked() {
        let server = MockServer::start().await;
        // First pending, then slow down, then the token.
        Mock::given(method("POST"))
            .and(path("/login/oauth/access_token"))
            .and(body_string_contains("device_code=dev-123"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"error": "authorization_pending"})),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/login/oauth/access_token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"error": "slow_down", "interval": 10})),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/login/oauth/access_token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"access_token": "gho_secret", "token_type": "bearer", "scope": "repo,read:org,workflow"}),
            ))
            .mount(&server)
            .await;

        let token = wait_for_token(&host(&server), "Iv1.test", &code(), FAST)
            .await
            .unwrap();
        assert_eq!(token, "gho_secret");
        assert_eq!(server.received_requests().await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn stops_when_declined() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"error": "access_denied"})),
            )
            .mount(&server)
            .await;
        let err = wait_for_token(&host(&server), "Iv1.test", &code(), FAST)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("declined"), "{err}");
    }

    #[tokio::test]
    async fn gives_up_when_the_code_expires() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"error": "authorization_pending"})),
            )
            .mount(&server)
            .await;
        let mut short = code();
        short.expires_in = 0;
        let err = wait_for_token(&host(&server), "Iv1.test", &short, FAST)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("expired"), "{err}");
    }

    #[test]
    fn lazyforge_client_id_is_for_github_com_only() {
        // Without the environment variable: no id for an Enterprise server.
        if std::env::var_os("LAZYFORGE_GITHUB_CLIENT_ID").is_none() {
            let ghe = Url::parse("https://github.example.com/").unwrap();
            assert_eq!(client_id(&ghe), None);
        }
    }
}
