//! Loading the config file and resolving which GitLab server and token to use.
//!
//! Resolution order:
//! 1. If the config file has profiles: `--profile` > `default_profile` > the only profile.
//!    Token: `token_env` (name of an env var) > `token` > the token saved by
//!    `lazyforge auth login` (credentials file, then OS keychain) > `GITLAB_TOKEN`.
//! 2. Without a config file: `GITLAB_HOST` (default gitlab.com) and `GITLAB_TOKEN`.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use url::Url;

use crate::forge::ForgeKind;

pub const DEFAULT_HOST: &str = "https://gitlab.com";
pub const DEFAULT_REFRESH_INTERVAL_SECS: u64 = 120;

/// The raw contents of `config.toml`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    pub default_profile: Option<String>,
    /// Seconds between automatic refreshes of the visible list. `0` turns it off.
    pub refresh_interval: Option<u64>,
    #[serde(default)]
    pub profiles: BTreeMap<String, ProfileConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileConfig {
    pub host: Option<String>,
    /// `gitlab` or `github`. Optional for gitlab.com and github.com.
    pub kind: Option<ForgeKind>,
    pub token: Option<String>,
    pub token_env: Option<String>,
}

/// A profile from the config file, without its token: what the account picker shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub name: String,
    pub host: Url,
    pub kind: ForgeKind,
}

/// The profiles of the config file, in name order. Profiles with an invalid host are skipped.
pub fn accounts(file: Option<&ConfigFile>) -> Vec<Account> {
    let Some(file) = file else {
        return Vec::new();
    };
    file.profiles
        .iter()
        .filter_map(|(name, profile)| {
            let host = parse_host(profile.host.as_deref().unwrap_or(DEFAULT_HOST)).ok()?;
            let kind = profile.kind.unwrap_or_else(|| ForgeKind::guess(&host));
            Some(Account {
                name: name.clone(),
                host,
                kind,
            })
        })
        .collect()
}

/// A fully resolved profile: everything needed to talk to one server.
#[derive(Clone)]
pub struct Profile {
    pub name: String,
    pub host: Url,
    pub kind: ForgeKind,
    pub token: String,
    pub token_source: TokenSource,
}

/// Where the token came from, for `auth status` and error messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenSource {
    EnvVar(String),
    ConfigFile,
    /// `credentials.toml`, written by `auth login --store file`.
    CredentialsFile,
    Keychain,
}

impl fmt::Display for TokenSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EnvVar(var) => write!(f, "env var {var}"),
            Self::ConfigFile => write!(f, "config file (plain text)"),
            Self::CredentialsFile => write!(f, "credentials file"),
            Self::Keychain => write!(f, "OS keychain"),
        }
    }
}

// Written by hand so the token never ends up in logs or panic messages.
impl fmt::Debug for Profile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Profile")
            .field("name", &self.name)
            .field("host", &self.host.as_str())
            .field("kind", &self.kind)
            .field("token", &"<redacted>")
            .field("token_source", &self.token_source)
            .finish()
    }
}

impl ConfigFile {
    /// Returns `Ok(None)` when the file does not exist.
    pub fn load(path: &Path) -> Result<Option<Self>> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text)
                .with_context(|| format!("invalid config file {}", path.display()))
                .map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        Ok(toml::from_str(text)?)
    }
}

/// `None` means automatic refresh is turned off.
pub fn refresh_interval(file: Option<&ConfigFile>) -> Option<Duration> {
    let secs = file
        .and_then(|f| f.refresh_interval)
        .unwrap_or(DEFAULT_REFRESH_INTERVAL_SECS);
    (secs > 0).then(|| Duration::from_secs(secs))
}

/// `$XDG_CONFIG_HOME/lazyforge/config.toml`, or `~/.config/lazyforge/config.toml`.
pub fn default_config_path() -> Option<PathBuf> {
    xdg_dir("XDG_CONFIG_HOME", ".config").map(|d| d.join("config.toml"))
}

/// `credentials.toml`, next to the config file.
pub fn credentials_path(config_path: &Path) -> PathBuf {
    config_path.with_file_name("credentials.toml")
}

/// `$XDG_STATE_HOME/lazyforge`, or `~/.local/state/lazyforge`. Used for the log file.
pub fn state_dir() -> Option<PathBuf> {
    xdg_dir("XDG_STATE_HOME", ".local/state")
}

fn xdg_dir(var: &str, fallback: &str) -> Option<PathBuf> {
    Some(xdg_base(var, fallback)?.join(APP_DIR))
}

fn xdg_base(var: &str, fallback: &str) -> Option<PathBuf> {
    match std::env::var_os(var).filter(|v| !v.is_empty()) {
        Some(dir) => Some(PathBuf::from(dir)),
        None => Some(directories::BaseDirs::new()?.home_dir().join(fallback)),
    }
}

const APP_DIR: &str = "lazyforge";
/// The directory name before the project was renamed from lazygl.
const LEGACY_APP_DIR: &str = "lazygl";

/// Moves `~/.config/lazygl` and `~/.local/state/lazygl` (from before the rename) to their
/// new names, when only the old one exists. Returns what was moved, to tell the user.
pub fn migrate_legacy_dirs() -> Vec<(PathBuf, PathBuf)> {
    let mut moved = Vec::new();
    for (var, fallback) in [
        ("XDG_CONFIG_HOME", ".config"),
        ("XDG_STATE_HOME", ".local/state"),
    ] {
        let Some(base) = xdg_base(var, fallback) else {
            continue;
        };
        if let Some(pair) = migrate_dir(&base.join(LEGACY_APP_DIR), &base.join(APP_DIR)) {
            moved.push(pair);
        }
    }
    moved
}

/// Renames `old` to `new` if `old` is a directory and `new` does not exist yet.
fn migrate_dir(old: &Path, new: &Path) -> Option<(PathBuf, PathBuf)> {
    if !old.is_dir() || new.exists() {
        return None;
    }
    match std::fs::rename(old, new) {
        Ok(()) => Some((old.to_path_buf(), new.to_path_buf())),
        Err(err) => {
            eprintln!(
                "! Could not move {} to {}: {err}",
                old.display(),
                new.display()
            );
            None
        }
    }
}

/// Picks the profile to use. `env` and `saved_token` are injected so tests touch neither real
/// environment variables nor the real keychain. `saved_token(profile, host)` returns the token
/// saved by `lazyforge auth login`, if any, and where it was found.
pub fn resolve(
    file: Option<&ConfigFile>,
    requested: Option<&str>,
    env: impl Fn(&str) -> Option<String>,
    saved_token: impl Fn(&str, &Url) -> Option<(String, TokenSource)>,
) -> Result<Profile> {
    // `export GITLAB_TOKEN=` leaves an empty variable behind; treat it as not set.
    let env = |key: &str| env(key).filter(|value| !value.trim().is_empty());
    let Some(file) = file.filter(|f| !f.profiles.is_empty()) else {
        if let Some(name) = requested {
            bail!("profile `{name}` not found: the config file has no profiles");
        }
        let host = env("GITLAB_HOST").unwrap_or_else(|| DEFAULT_HOST.to_string());
        let token = env("GITLAB_TOKEN")
            .context("not logged in: run `lazyforge auth login` (or set GITLAB_TOKEN)")?;
        let host = parse_host(&host)?;
        return Ok(Profile {
            name: "default".to_string(),
            kind: ForgeKind::guess(&host),
            host,
            token,
            token_source: TokenSource::EnvVar("GITLAB_TOKEN".into()),
        });
    };

    let name = selected_profile(file, requested)?;
    let Some(profile) = file.profiles.get(&name) else {
        bail!(
            "profile `{name}` not found; available: {}",
            profile_names(file)
        );
    };
    let host = profile.host.as_deref().unwrap_or(DEFAULT_HOST);
    let host = parse_host(host).with_context(|| format!("profile `{name}`: invalid host"))?;

    let (token, token_source) = if let Some(var) = &profile.token_env {
        let token =
            env(var).with_context(|| format!("profile `{name}`: env var `{var}` is not set"))?;
        (token, TokenSource::EnvVar(var.clone()))
    } else if let Some(token) = &profile.token {
        (token.clone(), TokenSource::ConfigFile)
    } else if let Some(saved) = saved_token(&name, &host) {
        saved
    } else if let Some(token) = env("GITLAB_TOKEN") {
        (token, TokenSource::EnvVar("GITLAB_TOKEN".into()))
    } else {
        bail!("profile `{name}` is not logged in: run `lazyforge auth login --profile {name}`");
    };
    let kind = profile.kind.unwrap_or_else(|| ForgeKind::guess(&host));
    Ok(Profile {
        name,
        host,
        kind,
        token,
        token_source,
    })
}

/// `--profile` > `default_profile` > the only profile.
pub fn selected_profile(file: &ConfigFile, requested: Option<&str>) -> Result<String> {
    match requested.or(file.default_profile.as_deref()) {
        Some(name) => Ok(name.to_string()),
        None if file.profiles.len() == 1 => Ok(file.profiles.keys().next().unwrap().clone()),
        None => bail!(
            "several profiles configured ({}); pick one with --profile or set `default_profile`",
            profile_names(file)
        ),
    }
}

/// Adds or updates `[profiles.<name>]` with `host`, keeping the rest of the file (comments
/// included) as it is. Removes `token` / `token_env` from that profile, because they would
/// take precedence over the keychain. Also sets `default_profile` if there is none.
pub fn upsert_profile(text: &str, name: &str, host: &Url, kind: ForgeKind) -> Result<String> {
    use toml_edit::{DocumentMut, Item, Table, value};

    let mut doc: DocumentMut = text.parse().context("the config file is not valid TOML")?;
    if !doc.contains_key("default_profile") {
        doc["default_profile"] = value(name);
    }
    let profiles = doc
        .entry("profiles")
        .or_insert_with(|| {
            let mut table = Table::new();
            // Implicit: print only `[profiles.<name>]`, not an empty `[profiles]` header.
            table.set_implicit(true);
            Item::Table(table)
        })
        .as_table_mut()
        .context("`profiles` in the config file must be a table")?;
    let profile = profiles
        .entry(name)
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut()
        .with_context(|| format!("`profiles.{name}` must be a table"))?;
    profile["host"] = value(host.as_str());
    // Only written when it cannot be guessed from the host (self-hosted GitHub).
    if kind == ForgeKind::guess(host) {
        profile.remove("kind");
    } else {
        profile["kind"] = value(match kind {
            ForgeKind::GitLab => "gitlab",
            ForgeKind::GitHub => "github",
        });
    }
    profile.remove("token");
    profile.remove("token_env");
    Ok(doc.to_string())
}

fn profile_names(file: &ConfigFile) -> String {
    file.profiles.keys().cloned().collect::<Vec<_>>().join(", ")
}

/// Accepts `gitlab.example.com`, `https://gitlab.example.com` or a sub-path install like
/// `https://example.com/gitlab`. The path always ends with `/` so `Url::join` appends to it
/// instead of replacing the last segment.
pub fn parse_host(host: &str) -> Result<Url> {
    let host = host.trim();
    let with_scheme = if host.contains("://") {
        host.to_string()
    } else {
        format!("https://{host}")
    };
    let mut url =
        Url::parse(&with_scheme).with_context(|| format!("`{host}` is not a valid URL"))?;
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    #[test]
    fn moves_the_old_directory_only_when_the_new_one_is_missing() {
        let base = std::env::temp_dir().join(format!("lazyforge-migrate-{}", std::process::id()));
        let (old, new) = (base.join("lazygl"), base.join("lazyforge"));
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("config.toml"), "x = 1").unwrap();

        assert!(super::migrate_dir(&old, &new).is_some());
        assert_eq!(
            std::fs::read_to_string(new.join("config.toml")).unwrap(),
            "x = 1"
        );
        assert!(!old.exists());

        // Both exist (for example the old one came back): leave them alone.
        std::fs::create_dir_all(&old).unwrap();
        assert!(super::migrate_dir(&old, &new).is_none());
        assert!(old.exists() && new.exists());
        // Nothing to move.
        assert!(super::migrate_dir(&base.join("missing"), &base.join("other")).is_none());
        std::fs::remove_dir_all(base).unwrap();
    }

    use super::*;
    use std::collections::HashMap;

    fn env_from(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |key| map.get(key).cloned()
    }

    fn no_keychain(_: &str, _: &Url) -> Option<(String, TokenSource)> {
        None
    }

    const SAMPLE: &str = r#"
        default_profile = "work"

        [profiles.work]
        host = "gitlab.example.com"
        token_env = "WORK_TOKEN"

        [profiles.public]
        token = "glpat-public"
    "#;

    #[test]
    fn no_config_uses_env_vars() {
        let p = resolve(None, None, env_from(&[("GITLAB_TOKEN", "t")]), no_keychain).unwrap();
        assert_eq!(p.name, "default");
        assert_eq!(p.host.as_str(), "https://gitlab.com/");
        assert_eq!(p.token, "t");
    }

    #[test]
    fn no_config_and_no_token_is_an_error() {
        let err = resolve(None, None, env_from(&[]), no_keychain).unwrap_err();
        assert!(err.to_string().contains("GITLAB_TOKEN"));
    }

    #[test]
    fn default_profile_reads_token_from_named_env_var() {
        let file = ConfigFile::parse(SAMPLE).unwrap();
        let p = resolve(
            Some(&file),
            None,
            env_from(&[("WORK_TOKEN", "w")]),
            no_keychain,
        )
        .unwrap();
        assert_eq!(p.name, "work");
        assert_eq!(p.host.as_str(), "https://gitlab.example.com/");
        assert_eq!(p.token, "w");
    }

    #[test]
    fn requested_profile_wins_over_default() {
        let file = ConfigFile::parse(SAMPLE).unwrap();
        let p = resolve(Some(&file), Some("public"), env_from(&[]), no_keychain).unwrap();
        assert_eq!(p.name, "public");
        assert_eq!(p.host.as_str(), "https://gitlab.com/");
        assert_eq!(p.token, "glpat-public");
    }

    #[test]
    fn missing_token_env_var_is_an_error() {
        let file = ConfigFile::parse(SAMPLE).unwrap();
        let err = resolve(Some(&file), None, env_from(&[]), no_keychain).unwrap_err();
        assert!(err.to_string().contains("WORK_TOKEN"));
    }

    #[test]
    fn unknown_profile_lists_available_ones() {
        let file = ConfigFile::parse(SAMPLE).unwrap();
        let err = resolve(Some(&file), Some("nope"), env_from(&[]), no_keychain).unwrap_err();
        assert!(err.to_string().contains("public, work"));
    }

    #[test]
    fn keychain_is_used_after_config_and_before_gitlab_token() {
        let file = ConfigFile::parse("[profiles.work]\nhost = \"gitlab.example.com\"").unwrap();
        let keychain = |profile: &str, host: &Url| {
            (profile == "work" && host.as_str() == "https://gitlab.example.com/")
                .then(|| ("from-keychain".to_string(), TokenSource::Keychain))
        };
        let env = env_from(&[("GITLAB_TOKEN", "from-env")]);
        let p = resolve(Some(&file), None, env, keychain).unwrap();
        assert_eq!(p.token, "from-keychain");
        assert_eq!(p.token_source, TokenSource::Keychain);

        let p = resolve(
            Some(&file),
            None,
            env_from(&[("GITLAB_TOKEN", "e")]),
            no_keychain,
        )
        .unwrap();
        assert_eq!(p.token_source, TokenSource::EnvVar("GITLAB_TOKEN".into()));
    }

    #[test]
    fn empty_env_vars_count_as_unset() {
        let err = resolve(None, None, env_from(&[("GITLAB_TOKEN", "")]), no_keychain).unwrap_err();
        assert!(err.to_string().contains("not logged in"));
    }

    #[test]
    fn not_logged_in_suggests_auth_login() {
        let file = ConfigFile::parse("[profiles.work]\nhost = \"gitlab.example.com\"").unwrap();
        let err = resolve(Some(&file), None, env_from(&[]), no_keychain).unwrap_err();
        assert!(
            err.to_string()
                .contains("lazyforge auth login --profile work")
        );
    }

    #[test]
    fn upsert_creates_a_profile_in_an_empty_file() {
        let host = parse_host("gitlab.example.com").unwrap();
        let text = upsert_profile("", "work", &host, ForgeKind::GitLab).unwrap();
        assert_eq!(
            text,
            "default_profile = \"work\"\n\n[profiles.work]\nhost = \"https://gitlab.example.com/\"\n"
        );
        // The result is a valid config that resolves to the same profile.
        let file = ConfigFile::parse(&text).unwrap();
        assert_eq!(selected_profile(&file, None).unwrap(), "work");
    }

    #[test]
    fn upsert_keeps_comments_and_other_profiles() {
        let before = "# my settings\nrefresh_interval = 30\ndefault_profile = \"public\"\n\n[profiles.public]\ntoken = \"plain\"\n";
        let host = parse_host("gitlab.example.com").unwrap();
        let after = upsert_profile(before, "work", &host, ForgeKind::GitLab).unwrap();
        assert!(after.starts_with("# my settings\n"));
        assert!(
            after.contains("default_profile = \"public\""),
            "existing default kept"
        );
        assert!(after.contains("[profiles.public]\ntoken = \"plain\""));
        assert!(after.contains("[profiles.work]\nhost ="));

        // Logging in again to `public` drops its plain-text token.
        let host = parse_host("gitlab.com").unwrap();
        let again = upsert_profile(&after, "public", &host, ForgeKind::GitLab).unwrap();
        assert!(!again.contains("plain"));
    }

    #[test]
    fn kind_is_guessed_from_the_host_or_configured() {
        let file = ConfigFile::parse(
            "[profiles.hub]\nhost = \"github.com\"\ntoken = \"t\"\n\n\
             [profiles.ghe]\nhost = \"github.example.com\"\nkind = \"github\"\ntoken = \"t\"\n\n\
             [profiles.lab]\nhost = \"gitlab.example.com\"\ntoken = \"t\"",
        )
        .unwrap();
        let kind = |name| {
            resolve(Some(&file), Some(name), env_from(&[]), no_keychain)
                .unwrap()
                .kind
        };
        assert_eq!(kind("hub"), ForgeKind::GitHub);
        assert_eq!(kind("ghe"), ForgeKind::GitHub);
        assert_eq!(kind("lab"), ForgeKind::GitLab);

        let names: Vec<_> = accounts(Some(&file))
            .into_iter()
            .map(|a| (a.name, a.kind))
            .collect();
        assert_eq!(
            names,
            vec![
                ("ghe".to_string(), ForgeKind::GitHub),
                ("hub".to_string(), ForgeKind::GitHub),
                ("lab".to_string(), ForgeKind::GitLab),
            ]
        );
    }

    #[test]
    fn upsert_writes_kind_only_when_it_cannot_be_guessed() {
        let ghe = parse_host("github.example.com").unwrap();
        let text = upsert_profile("", "work", &ghe, ForgeKind::GitHub).unwrap();
        assert!(text.contains("kind = \"github\""));
        let hub = parse_host("github.com").unwrap();
        let text = upsert_profile("", "hub", &hub, ForgeKind::GitHub).unwrap();
        assert!(!text.contains("kind"));
    }

    #[test]
    fn unknown_config_keys_are_rejected() {
        assert!(ConfigFile::parse("defualt_profile = \"x\"").is_err());
    }

    #[test]
    fn refresh_interval_defaults_and_can_be_disabled() {
        assert_eq!(refresh_interval(None), Some(Duration::from_secs(120)));
        let file = ConfigFile::parse("refresh_interval = 30").unwrap();
        assert_eq!(refresh_interval(Some(&file)), Some(Duration::from_secs(30)));
        let file = ConfigFile::parse("refresh_interval = 0").unwrap();
        assert_eq!(refresh_interval(Some(&file)), None);
    }

    #[test]
    fn host_is_normalized() {
        assert_eq!(
            parse_host("gitlab.com").unwrap().as_str(),
            "https://gitlab.com/"
        );
        assert_eq!(
            parse_host("http://localhost:8080").unwrap().as_str(),
            "http://localhost:8080/"
        );
        assert_eq!(
            parse_host("https://example.com/gitlab").unwrap().as_str(),
            "https://example.com/gitlab/"
        );
    }

    #[test]
    fn debug_output_hides_token() {
        let p = resolve(
            None,
            None,
            env_from(&[("GITLAB_TOKEN", "secret")]),
            no_keychain,
        )
        .unwrap();
        assert!(!format!("{p:?}").contains("secret"));
    }
}
