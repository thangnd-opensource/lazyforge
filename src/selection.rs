//! The account and repository chosen last time, so the next start opens them again.
//! Stored in `$XDG_STATE_HOME/lazyforge/last.toml` (no secrets: only names).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    /// Profile name from the config file.
    pub profile: String,
    /// `group/project` or `owner/repo`; `None` means "all my repositories".
    pub repo: Option<String>,
}

pub fn default_path() -> Option<PathBuf> {
    crate::config::state_dir().map(|dir| dir.join("last.toml"))
}

/// `None` when there is no saved selection or it cannot be read (a fresh start is fine).
pub fn load(path: &Path) -> Option<Selection> {
    let text = std::fs::read_to_string(path).ok()?;
    toml::from_str(&text)
        .map_err(|err| tracing::warn!(error = %err, "ignoring invalid {}", path.display()))
        .ok()
}

pub fn save(path: &Path, selection: &Selection) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, toml::to_string(selection)?)
        .with_context(|| format!("cannot write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_with_and_without_a_repo() {
        let dir = std::env::temp_dir().join(format!("lazyforge-selection-{}", std::process::id()));
        let path = dir.join("last.toml");
        for repo in [Some("acme/api".to_string()), None] {
            let selection = Selection {
                profile: "github.com".into(),
                repo,
            };
            save(&path, &selection).unwrap();
            assert_eq!(load(&path), Some(selection));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_or_broken_files_mean_no_selection() {
        let dir =
            std::env::temp_dir().join(format!("lazyforge-selection-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(load(&dir.join("missing.toml")), None);
        std::fs::write(dir.join("bad.toml"), "profile = [").unwrap();
        assert_eq!(load(&dir.join("bad.toml")), None);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
