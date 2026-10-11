//! Finds the GitLab project for the repository in the current directory.

use std::process::Command;

use url::Url;

/// Runs `git remote get-url origin` and returns the project path (`group/project`) if the
/// remote points at `host`. Returns `None` outside a git repo, without an `origin` remote,
/// or when the remote is on another server.
pub fn current_project(host: &Url) -> Option<String> {
    let output = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let remote = String::from_utf8(output.stdout).ok()?;
    let project = project_from_remote(remote.trim(), host);
    if project.is_none() {
        tracing::info!(remote = remote.trim(), host = %host, "origin is not on this GitLab server");
    }
    project
}

/// Supports the usual remote formats:
/// - `https://gitlab.com/group/project.git`
/// - `ssh://git@gitlab.com:2222/group/project.git`
/// - `git@gitlab.com:group/project.git` (scp-like syntax, no scheme)
fn project_from_remote(remote: &str, host: &Url) -> Option<String> {
    let host_name = host.host_str()?;
    let (remote_host, path) = if remote.contains("://") {
        let url = Url::parse(remote).ok()?;
        (url.host_str()?.to_string(), url.path().to_string())
    } else {
        // `user@host:path`. The part before `:` is the host, optionally with a user.
        let (user_host, path) = remote.split_once(':')?;
        let host = user_host.rsplit_once('@').map_or(user_host, |(_, h)| h);
        (host.to_string(), path.to_string())
    };
    if !remote_host.eq_ignore_ascii_case(host_name) {
        return None;
    }

    // For sub-path installs (https://example.com/gitlab/) the HTTP remote includes the prefix.
    let path = path.trim_start_matches('/');
    let prefix = host.path().trim_matches('/');
    let path = if prefix.is_empty() {
        path
    } else {
        path.strip_prefix(prefix)
            .map_or(path, |rest| rest.trim_start_matches('/'))
    };
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);

    // A project path always has a namespace: `group/project`.
    path.contains('/').then(|| path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(remote: &str, host: &str) -> Option<String> {
        project_from_remote(remote, &crate::config::parse_host(host).unwrap())
    }

    #[test]
    fn https_remotes() {
        assert_eq!(
            project("https://gitlab.com/group/sub/api.git", "gitlab.com").as_deref(),
            Some("group/sub/api")
        );
        assert_eq!(
            project("https://gitlab.com/group/api", "gitlab.com").as_deref(),
            Some("group/api")
        );
    }

    #[test]
    fn ssh_remotes() {
        assert_eq!(
            project("git@gitlab.com:group/api.git", "gitlab.com").as_deref(),
            Some("group/api")
        );
        assert_eq!(
            project(
                "ssh://git@gitlab.example.com:2222/group/api.git",
                "gitlab.example.com"
            )
            .as_deref(),
            Some("group/api")
        );
    }

    #[test]
    fn sub_path_install() {
        assert_eq!(
            project(
                "https://example.com/gitlab/group/api.git",
                "https://example.com/gitlab"
            )
            .as_deref(),
            Some("group/api")
        );
    }

    #[test]
    fn other_servers_and_bad_paths_are_ignored() {
        assert_eq!(project("git@github.com:me/repo.git", "gitlab.com"), None);
        assert_eq!(
            project("https://gitlab.com/just-a-user", "gitlab.com"),
            None
        );
        assert_eq!(project("/some/local/path", "gitlab.com"), None);
    }

    #[test]
    fn host_comparison_ignores_case() {
        assert_eq!(
            project("git@GitLab.com:group/api.git", "gitlab.com").as_deref(),
            Some("group/api")
        );
    }
}
