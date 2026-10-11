//! `lazyforge auth login | status | logout`.
//!
//! A token saved by `auth login` goes to one of two places:
//!
//! - the OS credential store (macOS Keychain, Windows Credential Manager, Secret Service on
//!   Linux), the default. On macOS, an unsigned binary is asked for permission again after
//!   every rebuild or upgrade.
//! - `credentials.toml` next to the config file, readable only by you (mode 600), with
//!   `--store file`. Never asks for permission; like `glab`, the token sits on disk in plain
//!   text, protected by file permissions only.
//!
//! The config file itself only gets the profile name and host.

use std::collections::BTreeMap;
use std::io::{self, BufRead, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::config::{self, ConfigFile, TokenSource};
use crate::forge::{Forge, ForgeKind};
use crate::github;

const KEYCHAIN_SERVICE: &str = "lazyforge";
/// The service name before the project was renamed from lazygl. Tokens found there are
/// moved to `KEYCHAIN_SERVICE`.
const LEGACY_KEYCHAIN_SERVICE: &str = "lazygl";

/// Where `auth login` saves the token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Store {
    /// The OS credential store.
    Keychain,
    /// `credentials.toml` next to the config file, readable only by you.
    File,
}

/// One entry per profile and host, so two accounts on the same server can coexist. Used as
/// the keychain account name and as the key in `credentials.toml`.
fn entry_key(profile: &str, host: &Url) -> String {
    format!("{profile}@{}", host.as_str())
}

fn keychain_entry(profile: &str, host: &Url) -> keyring::Result<keyring::Entry> {
    keyring::Entry::new(KEYCHAIN_SERVICE, &entry_key(profile, host))
}

fn legacy_keychain_entry(profile: &str, host: &Url) -> keyring::Result<keyring::Entry> {
    keyring::Entry::new(LEGACY_KEYCHAIN_SERVICE, &entry_key(profile, host))
}

/// A keychain that cannot be read (for example no Secret Service on a headless Linux box)
/// counts as "no token" and is logged.
fn read_keychain(entry: keyring::Result<keyring::Entry>) -> Option<String> {
    match entry.and_then(|entry| entry.get_password()) {
        Ok(token) => Some(token),
        Err(keyring::Error::NoEntry) => None,
        Err(err) => {
            tracing::warn!(error = %err, "cannot read the OS keychain");
            None
        }
    }
}

/// The saved token, looked up under the current name and then under the old one (moving
/// it, so it is not kept twice).
fn keychain_token(profile: &str, host: &Url) -> Option<String> {
    if let Some(token) = read_keychain(keychain_entry(profile, host)) {
        return Some(token);
    }
    let token = read_keychain(legacy_keychain_entry(profile, host))?;
    match keychain_entry(profile, host).and_then(|entry| entry.set_password(&token)) {
        Ok(()) => {
            if let Err(err) =
                legacy_keychain_entry(profile, host).and_then(|e| e.delete_credential())
            {
                tracing::warn!(error = %err, "could not remove the old keychain entry");
            }
            tracing::info!(
                profile,
                "moved the token from the old `lazygl` keychain entry"
            );
        }
        // Still usable this time; the move is tried again on the next start.
        Err(err) => tracing::warn!(error = %err, "could not move the old keychain entry"),
    }
    Some(token)
}

/// Contents of `credentials.toml`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Credentials {
    #[serde(default)]
    tokens: BTreeMap<String, String>,
}

impl Credentials {
    fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text)
                .with_context(|| format!("invalid credentials file {}", path.display())),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(err).with_context(|| format!("cannot read {}", path.display())),
        }
    }

    /// Writes the file so that only the current user can read it.
    fn save(&self, path: &Path) -> Result<()> {
        let text = format!(
            "# Tokens saved by `lazyforge auth login --store file`. Keep this file private.\n{}",
            toml::to_string(self)?
        );
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        write_private(path, text.as_bytes())
            .with_context(|| format!("cannot write {}", path.display()))
    }
}

#[cfg(unix)]
fn write_private(path: &Path, contents: &[u8]) -> io::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        // Applies only when the file is created...
        .mode(0o600)
        .open(path)?;
    // ...so also tighten an existing file.
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(contents)
}

#[cfg(not(unix))]
fn write_private(path: &Path, contents: &[u8]) -> io::Result<()> {
    // On Windows, files under the user profile are private to the user by default.
    std::fs::write(path, contents)
}

/// The token saved by `auth login` for this profile: the credentials file first (it never
/// prompts), then the keychain.
pub fn saved_token(
    credentials_path: &Path,
    profile: &str,
    host: &Url,
) -> Option<(String, TokenSource)> {
    let from_file = Credentials::load(credentials_path)
        .map_err(|err| tracing::warn!(error = %err, "cannot read the credentials file"))
        .ok()
        .and_then(|mut c| c.tokens.remove(&entry_key(profile, host)));
    match from_file {
        Some(token) => Some((token, TokenSource::CredentialsFile)),
        None => keychain_token(profile, host).map(|token| (token, TokenSource::Keychain)),
    }
}

pub async fn login(
    config_path: &Path,
    host: Option<String>,
    profile: Option<String>,
    store: Store,
    kind: Option<ForgeKind>,
    from_gh: bool,
    with_token: bool,
) -> Result<()> {
    let credentials_path = config::credentials_path(config_path);
    // The platform decides the default host; a given host decides the platform.
    let kind = match (kind, &host, from_gh) {
        (Some(ForgeKind::GitLab), _, true) => bail!("--from-gh only works with GitHub"),
        (_, _, true) => ForgeKind::GitHub,
        (Some(kind), _, false) => kind,
        (None, Some(host), false) => ForgeKind::guess(&config::parse_host(host)?),
        (None, None, false) => choose_platform()?,
    };
    let host = match host {
        Some(host) => host,
        // With --from-gh there is nothing to ask: gh knows github.com by default.
        None if from_gh => kind.default_host().to_string(),
        None => prompt(&format!("{} host", kind.name()), kind.default_host())?,
    };
    let host = config::parse_host(&host)?;
    // Default profile name: the server name, e.g. `gitlab.com`.
    let profile = profile.unwrap_or_else(|| host.host_str().unwrap_or("default").to_string());
    let existing = saved_token(&credentials_path, &profile, &host);

    // GitHub logs in through the browser when an OAuth App client id is available.
    let wants_browser = kind == ForgeKind::GitHub && !with_token && !from_gh;
    let browser_login = wants_browser
        .then(|| github::device::client_id(&host))
        .flatten();
    if wants_browser && browser_login.is_none() {
        println!(
            "Browser login is not set up for {} (no OAuth App client id); using a personal access token.",
            host.as_str()
        );
    }

    let token = if from_gh {
        println!(
            "Reading the token of the GitHub CLI (gh) for {}…",
            host.as_str()
        );
        gh_token("gh", &host)?
    } else if let Some(client_id) = browser_login {
        match existing {
            Some((saved, _)) if !confirm("A token is already saved. Log in again?", false)? => {
                saved
            }
            _ => browser_login_flow(&host, &client_id).await?,
        }
    } else if existing.is_some() {
        let pasted = rpassword::prompt_password(
            "Paste a new token, or press Enter to keep the saved one (input is hidden): ",
        )
        .context("cannot read the token")?;
        match (pasted.trim(), existing) {
            ("", Some((saved, _))) => saved,
            (pasted, _) => pasted.to_string(),
        }
    } else {
        // Both servers pre-fill the form from these query parameters.
        let (token_page, scopes) = match kind {
            ForgeKind::GitLab => (
                host.join("-/user_settings/personal_access_tokens?name=lazyforge&scopes=api")?,
                "`api`",
            ),
            ForgeKind::GitHub => (
                host.join(
                    "settings/tokens/new?scopes=repo,workflow,read:org&description=lazyforge",
                )?,
                "`repo`, `workflow` and `read:org`",
            ),
        };
        println!("Create a personal access token with the {scopes} scopes here:\n  {token_page}\n");
        if confirm("Open this page in your browser?", true)?
            && let Err(err) = open::that_detached(token_page.as_str())
        {
            println!("Could not open the browser ({err}); open the link above manually.");
        }
        rpassword::prompt_password("Paste the token (input is hidden): ")
            .context("cannot read the token")?
            .trim()
            .to_string()
    };
    if token.is_empty() {
        bail!("no token entered");
    }

    print!("Checking the token with {}… ", host.as_str());
    io::stdout().flush()?;
    let client = Forge::connect(kind, &host, &token)?;
    let user = client
        .current_user()
        .await
        .with_context(|| format!("{} did not accept this token", kind.name()))?;
    println!("ok");
    // Not every token type can report its scopes (OAuth and fine-grained tokens can't); only
    // warn when we know.
    let scopes = client.token_scopes().await.ok();
    let required = client.required_scope();

    let key = entry_key(&profile, &host);
    let mut credentials = Credentials::load(&credentials_path)?;
    let saved_in = match store {
        Store::File => {
            credentials.tokens.insert(key, token);
            credentials.save(&credentials_path)?;
            format!("{} (readable only by you)", credentials_path.display())
        }
        Store::Keychain => {
            keychain_entry(&profile, &host)
                .and_then(|entry| entry.set_password(&token))
                .context(
                    "cannot save the token in the OS keychain; try `lazyforge auth login --store file`",
                )?;
            // A token in the file would take precedence over the keychain: remove it.
            if credentials.tokens.remove(&key).is_some() {
                credentials.save(&credentials_path)?;
            }
            "the OS keychain".to_string()
        }
    };
    save_profile(config_path, &profile, &host, kind)?;

    println!(
        "\n✓ Logged in to {} as @{} ({}).",
        host.as_str(),
        user.username,
        user.name
    );
    println!("  Profile `{profile}` saved in {}", config_path.display());
    println!("  Token saved in {saved_in}.");
    if let Some(scopes) = scopes.filter(|s| !s.iter().any(|scope| scope == required)) {
        println!(
            "\n! This token has the scopes [{}]. You can browse, but acting on {} needs the\n  `{required}` scope.",
            scopes.join(", "),
            kind.change_requests().to_lowercase()
        );
    }
    println!("\nRun `lazyforge` to start.");
    Ok(())
}

pub async fn status(config_path: &Path, file: Option<&ConfigFile>) -> Result<()> {
    let credentials_path = config::credentials_path(config_path);
    let names: Vec<Option<String>> = match file.filter(|f| !f.profiles.is_empty()) {
        Some(file) => file.profiles.keys().cloned().map(Some).collect(),
        // No profiles: report the environment-variable setup.
        None => vec![None],
    };
    for name in names {
        let label = name.clone().unwrap_or_else(|| "default".into());
        let profile = match config::resolve(
            file,
            name.as_deref(),
            |key| std::env::var(key).ok(),
            |profile, host| saved_token(&credentials_path, profile, host),
        ) {
            Ok(profile) => profile,
            Err(err) => {
                println!("✗ {label}: {err:#}");
                continue;
            }
        };
        let client = Forge::connect(profile.kind, &profile.host, &profile.token)?;
        match client.current_user().await {
            Ok(user) => println!(
                "✓ {label}: {} as @{} (token from {})",
                profile.host.as_str(),
                user.username,
                profile.token_source
            ),
            Err(err) => println!(
                "✗ {label}: {} rejected the token from {}: {err}",
                profile.host.as_str(),
                profile.token_source
            ),
        }
    }
    Ok(())
}

/// Removes the saved token from both places. The profile stays in the config file.
pub fn logout(
    config_path: &Path,
    file: Option<&ConfigFile>,
    requested: Option<&str>,
) -> Result<()> {
    let Some(file) = file.filter(|f| !f.profiles.is_empty()) else {
        bail!("no profiles in the config file; nothing to log out of");
    };
    let name = config::selected_profile(file, requested)?;
    let profile = file
        .profiles
        .get(&name)
        .with_context(|| format!("profile `{name}` not found"))?;
    let host = config::parse_host(profile.host.as_deref().unwrap_or(config::DEFAULT_HOST))?;

    let credentials_path = config::credentials_path(config_path);
    let mut credentials = Credentials::load(&credentials_path)?;
    let mut removed = credentials
        .tokens
        .remove(&entry_key(&name, &host))
        .is_some();
    if removed {
        credentials.save(&credentials_path)?;
    }
    // Also under the old name, in case it was never moved.
    for entry in [
        keychain_entry(&name, &host),
        legacy_keychain_entry(&name, &host),
    ] {
        match entry.and_then(|entry| entry.delete_credential()) {
            Ok(()) => removed = true,
            Err(keyring::Error::NoEntry) => {}
            Err(err) => println!("! Could not update the OS keychain: {err}"),
        }
    }
    if removed {
        println!("✓ Removed the saved token of profile `{name}`.");
    } else {
        println!("Profile `{name}` has no saved token.");
    }
    Ok(())
}

fn save_profile(path: &Path, name: &str, host: &Url, kind: ForgeKind) -> Result<()> {
    let current = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err).with_context(|| format!("cannot read {}", path.display())),
    };
    let updated = config::upsert_profile(&current, name, host, kind)
        .with_context(|| format!("cannot update {}", path.display()))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, updated).with_context(|| format!("cannot write {}", path.display()))
}

/// GitHub's device flow: show a code, let the user authorize in the browser, get a token.
async fn browser_login_flow(host: &Url, client_id: &str) -> Result<String> {
    let code = github::device::request_code(host, client_id)
        .await
        .context("cannot start the browser login")?;
    println!(
        "\nOpen {} and enter this code:\n\n    {}\n",
        code.verification_uri, code.user_code
    );
    if confirm("Open the page in your browser?", true)?
        && let Err(err) = open::that_detached(&code.verification_uri)
    {
        println!("Could not open the browser ({err}); open the link above manually.");
    }
    println!("Waiting for you to authorize lazyforge on GitHub… (Ctrl+C to cancel)");
    let token =
        github::device::wait_for_token(host, client_id, &code, github::device::Pacing::from(&code))
            .await
            .context("the browser login did not finish")?;
    Ok(token)
}

/// The token `gh auth login` saved for `host`, from `gh auth token`. It never appears on
/// screen. `program` is `gh`, or a stand-in in tests.
fn gh_token(program: &str, host: &Url) -> Result<String> {
    let hostname = host.host_str().unwrap_or("github.com");
    let output = std::process::Command::new(program)
        .args(["auth", "token", "--hostname", hostname])
        .output()
        .map_err(|err| match err.kind() {
            io::ErrorKind::NotFound => anyhow::anyhow!(
                "the GitHub CLI (gh) is not installed; install it, or log in with a token instead"
            ),
            _ => anyhow::Error::new(err).context("cannot run gh"),
        })?;
    if !output.status.success() {
        let reason = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!("gh has no token for {hostname} ({reason}); run `gh auth login` first");
    }
    let token = String::from_utf8(output.stdout)
        .context("gh printed something that is not text")?
        .trim()
        .to_string();
    if token.is_empty() {
        bail!("gh has no token for {hostname}; run `gh auth login` first");
    }
    Ok(token)
}

fn choose_platform() -> Result<ForgeKind> {
    let answer = prompt("Platform: gitlab or github", "gitlab")?;
    match answer.to_lowercase().as_str() {
        "gitlab" | "gl" | "1" => Ok(ForgeKind::GitLab),
        "github" | "gh" | "2" => Ok(ForgeKind::GitHub),
        other => bail!("unknown platform `{other}`: answer gitlab or github"),
    }
}

fn prompt(question: &str, default: &str) -> Result<String> {
    print!("{question} [{default}]: ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().lock().read_line(&mut answer)?;
    let answer = answer.trim();
    Ok(if answer.is_empty() { default } else { answer }.to_string())
}

fn confirm(question: &str, default: bool) -> Result<bool> {
    let hint = if default { "Y/n" } else { "y/N" };
    let answer = prompt(question, hint)?;
    Ok(match answer.to_lowercase().as_str() {
        "y" | "yes" => true,
        "n" | "no" => false,
        _ => default,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lazyforge-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn credentials_round_trip_and_are_private() {
        let dir = temp_dir("credentials");
        let path = dir.join("credentials.toml");
        let host = config::parse_host("gitlab.com").unwrap();

        let mut credentials = Credentials::default();
        credentials
            .tokens
            .insert(entry_key("work", &host), "glpat-secret".into());
        credentials.save(&path).unwrap();

        // `saved_token` finds it without touching the keychain.
        let (token, source) = saved_token(&path, "work", &host).unwrap();
        assert_eq!(token, "glpat-secret");
        assert_eq!(source, TokenSource::CredentialsFile);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "only the owner may read the tokens");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn saving_tightens_an_existing_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("tighten");
        let path = dir.join("credentials.toml");
        std::fs::write(&path, "").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        Credentials::default().save(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_file_means_no_tokens() {
        let path = temp_dir("missing").join("credentials.toml");
        assert!(Credentials::load(&path).unwrap().tokens.is_empty());
    }

    /// Writes an executable shell script standing in for `gh`.
    #[cfg(unix)]
    fn fake_gh(name: &str, body: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = temp_dir(name).join("gh");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn reads_the_token_from_gh() {
        let host = config::parse_host("github.com").unwrap();
        // The fake gh checks it is asked for the right host.
        let gh = fake_gh(
            "gh-ok",
            r#"[ "$1 $2 $3 $4" = "auth token --hostname github.com" ] && echo "gho_secret" || exit 2"#,
        );
        assert_eq!(gh_token(gh.to_str().unwrap(), &host).unwrap(), "gho_secret");
    }

    #[cfg(unix)]
    #[test]
    fn explains_when_gh_is_not_logged_in_or_missing() {
        let host = config::parse_host("github.com").unwrap();
        let gh = fake_gh(
            "gh-out",
            "echo 'no oauth token found for github.com' >&2; exit 1",
        );
        let err = gh_token(gh.to_str().unwrap(), &host)
            .unwrap_err()
            .to_string();
        assert!(err.contains("gh auth login"), "{err}");
        assert!(err.contains("no oauth token"), "{err}");

        let err = gh_token("/nonexistent/gh", &host).unwrap_err().to_string();
        assert!(err.contains("not installed"), "{err}");
    }

    #[test]
    fn credentials_path_sits_next_to_the_config() {
        let path = config::credentials_path(Path::new("/home/me/.config/lazyforge/config.toml"));
        assert_eq!(
            path,
            Path::new("/home/me/.config/lazyforge/credentials.toml")
        );
    }
}
