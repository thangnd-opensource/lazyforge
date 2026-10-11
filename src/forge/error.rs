use reqwest::StatusCode;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("the token contains characters that are not allowed in an HTTP header")]
    InvalidToken,

    #[error("invalid URL: {0}")]
    Url(#[from] url::ParseError),

    #[error(
        "authentication failed (401): check that the token is valid and has the needed scopes (`lazyforge auth status`)"
    )]
    Unauthorized,

    #[error("permission denied (403): {0}")]
    Forbidden(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("GraphQL error: {0}")]
    GraphQl(String),

    #[error("unexpected response from the server: {0}")]
    UnexpectedResponse(String),

    #[error("the server returned {status}: {message}")]
    Api { status: u16, message: String },

    /// A feature this server (or lazyforge's support for it) does not offer.
    #[error("{0}")]
    Unsupported(String),

    /// The first part of an action went through and a later one failed (a review was
    /// published, its approval refused): what was done must not be done again.
    #[error("{0}")]
    PartlyDone(String),

    #[error("network error: {0}")]
    Http(#[from] reqwest::Error),
}

impl ApiError {
    /// Builds an error from a non-2xx response. GitLab and GitHub usually answer with
    /// `{"message": ...}` or `{"error": ...}`, where `message` may be a string or an object.
    pub(crate) fn from_response(status: StatusCode, body: &str) -> Self {
        let message = serde_json::from_str::<serde_json::Value>(body)
            .ok()
            .and_then(|json| json.get("message").or_else(|| json.get("error")).cloned())
            .map(|value| match value {
                serde_json::Value::String(s) => s,
                other => other.to_string(),
            })
            .unwrap_or_else(|| body.chars().take(200).collect());

        match status {
            StatusCode::UNAUTHORIZED => Self::Unauthorized,
            // A `read_api` token can read everything but cannot approve, comment or merge.
            StatusCode::FORBIDDEN if message == "insufficient_scope" => {
                Self::Forbidden("the token needs the `api` scope for this action".to_string())
            }
            StatusCode::FORBIDDEN => Self::Forbidden(message),
            _ => Self::Api {
                status: status.as_u16(),
                message,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_message_field() {
        let err = ApiError::from_response(StatusCode::NOT_FOUND, r#"{"message":"404 Not found"}"#);
        assert_eq!(err.to_string(), "the server returned 404: 404 Not found");
    }

    #[test]
    fn explains_insufficient_scope() {
        let body = r#"{"error":"insufficient_scope","error_description":"..."}"#;
        let err = ApiError::from_response(StatusCode::FORBIDDEN, body);
        assert!(err.to_string().contains("`api` scope"));
    }

    #[test]
    fn reads_error_field_and_non_json_bodies() {
        let err =
            ApiError::from_response(StatusCode::BAD_REQUEST, r#"{"error":"scope is invalid"}"#);
        assert!(err.to_string().contains("scope is invalid"));

        let err = ApiError::from_response(StatusCode::BAD_GATEWAY, "<html>bad gateway</html>");
        assert!(err.to_string().contains("bad gateway"));
    }
}
