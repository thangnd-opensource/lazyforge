//! The git repository lazyforge was started in, and checking out a merge request or a branch
//! into it (`C` in the app).
//!
//! Checking out never discards work: it refuses when tracked files have uncommitted changes,
//! never resets or forces, and only fast-forwards a local branch that already exists.

use std::path::{Path, PathBuf};
use std::process::Command;

use url::Url;

/// A git repository on this machine: its top directory and its remotes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalRepo {
    pub root: PathBuf,
    /// `(name, fetch URL)`, in `git remote -v` order.
    remotes: Vec<(String, String)>,
}

impl LocalRepo {
    /// The repository containing the current directory, if any.
    pub fn discover() -> Option<Self> {
        Self::open(&std::env::current_dir().ok()?)
    }

    /// `None` outside a git repository, which is normal: no warning is logged.
    pub fn open(dir: &Path) -> Option<Self> {
        let root = git_output(dir, &["rev-parse", "--show-toplevel"])?;
        let root = PathBuf::from(root.trim());
        let listing = git_output(&root, &["remote", "-v"])?;
        Some(Self {
            remotes: parse_remotes(&listing),
            root,
        })
    }

    #[cfg(test)]
    pub fn for_tests(root: &Path, origin: &str) -> Self {
        Self {
            root: root.to_path_buf(),
            remotes: vec![("origin".into(), origin.into())],
        }
    }

    /// The remote that points at `project` (`group/project` or `owner/repo`) on `host`.
    pub fn remote_for(&self, host: &Url, project: &str) -> Option<&str> {
        self.remotes
            .iter()
            .find(|(_, url)| remote_points_at(url, host, project))
            .map(|(name, _)| name.as_str())
    }
}

/// `origin  git@gitlab.com:group/api.git (fetch)` lines → `(name, url)`.
fn parse_remotes(listing: &str) -> Vec<(String, String)> {
    listing
        .lines()
        .filter(|line| line.ends_with("(fetch)"))
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            Some((parts.next()?.to_string(), parts.next()?.to_string()))
        })
        .collect()
}

/// Whether a remote URL is `project` on `host`. Ports are ignored (SSH and HTTPS use
/// different ones), and a sub-path install (`https://example.com/gitlab/group/api`) matches
/// on the project path at the end.
fn remote_points_at(remote: &str, host: &Url, project: &str) -> bool {
    let Some(host_name) = host.host_str() else {
        return false;
    };
    let Some((remote_host, path)) = split_remote(remote) else {
        return false;
    };
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path).to_lowercase();
    let project = project.trim_matches('/').to_lowercase();
    remote_host.eq_ignore_ascii_case(host_name)
        && (path == project || path.ends_with(&format!("/{project}")))
}

/// The usual remote formats → `(host, path)`:
/// - `https://gitlab.com/group/project.git`
/// - `ssh://git@gitlab.com:2222/group/project.git`
/// - `git@gitlab.com:group/project.git` (scp-like syntax, no scheme)
fn split_remote(remote: &str) -> Option<(String, String)> {
    if remote.contains("://") {
        let url = Url::parse(remote).ok()?;
        Some((url.host_str()?.to_string(), url.path().to_string()))
    } else {
        // `user@host:path`. The part before `:` is the host, optionally with a user.
        let (user_host, path) = remote.split_once(':')?;
        let host = user_host.rsplit_once('@').map_or(user_host, |(_, h)| h);
        // A Windows path such as `C:\repo` is not a remote.
        (!host.is_empty() && !path.starts_with('\\')).then(|| (host.to_string(), path.to_string()))
    }
}

/// What to check out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A branch of the repository.
    Branch(String),
    /// A merge request (GitLab) or pull request (GitHub).
    ChangeRequest {
        /// The server's ref for its latest commit: `refs/merge-requests/42/head` or
        /// `refs/pull/42/head`. It exists for merge requests from forks too.
        head_ref: String,
        /// Its branch, in the repository it comes from.
        source_branch: String,
        /// The local branch to use when the source branch is not in this remote (a fork):
        /// `mr/42` or `pr/42`.
        fallback_branch: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutRequest {
    pub root: PathBuf,
    pub remote: String,
    pub target: Target,
}

/// Fetches and switches to the target. `Ok` is a message for the user, `Err` says why
/// nothing (or not everything) was done.
pub fn checkout(request: &CheckoutRequest) -> Result<String, String> {
    let root = request.root.as_path();
    let remote = request.remote.as_str();
    let changes = git(root, &["status", "--porcelain", "--untracked-files=no"])?;
    if !changes.trim().is_empty() {
        return Err("You have uncommitted changes: commit or stash them first".to_string());
    }
    match &request.target {
        Target::Branch(branch) => {
            fetch_branch(root, remote, branch)?;
            switch_to(root, branch, &format!("{remote}/{branch}"), true)
        }
        Target::ChangeRequest {
            head_ref,
            source_branch,
            fallback_branch,
        } => {
            // One round trip in the usual case: the merge request's ref and its branch
            // together. If the branch is not in this remote (a fork), only the ref.
            let branch_refspec =
                format!("refs/heads/{source_branch}:refs/remotes/{remote}/{source_branch}");
            let both = git(
                root,
                &["fetch", "--quiet", remote, head_ref, &branch_refspec],
            );
            if both.is_err() {
                git(root, &["fetch", "--quiet", remote, head_ref])?;
            }
            // FETCH_HEAD's first line is the first ref asked for: the merge request's.
            let head = git(root, &["rev-parse", "FETCH_HEAD"])?.trim().to_string();
            // Use the real branch only if it is exactly the merge request's code; a branch of
            // the same name may exist here while the merge request comes from a fork.
            let branch_tip = both.and_then(|_| {
                git(
                    root,
                    &[
                        "rev-parse",
                        &format!("refs/remotes/{remote}/{source_branch}"),
                    ],
                )
            });
            if branch_tip.is_ok_and(|tip| tip.trim() == head) {
                switch_to(
                    root,
                    source_branch,
                    &format!("{remote}/{source_branch}"),
                    true,
                )
            } else {
                switch_to(root, fallback_branch, &head, false)
            }
        }
    }
}

/// Updates `refs/remotes/<remote>/<branch>` (a plain `git fetch <remote> <branch>` would only
/// set FETCH_HEAD).
fn fetch_branch(root: &Path, remote: &str, branch: &str) -> Result<(), String> {
    let refspec = format!("refs/heads/{branch}:refs/remotes/{remote}/{branch}");
    git(root, &["fetch", "--quiet", remote, &refspec]).map(drop)
}

/// Switches to `local`, creating it at `start` (tracking it if `track`) or fast-forwarding it
/// to `start` if it exists. A local branch with commits of its own is left as it is.
fn switch_to(root: &Path, local: &str, start: &str, track: bool) -> Result<String, String> {
    let exists = git_succeeds(
        root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{local}"),
        ],
    );
    if !exists {
        let mut args = vec!["switch", "--quiet", "-c", local];
        if track {
            args.push("--track");
        }
        args.push(start);
        git(root, &args)?;
        return Ok(format!("Checked out {local}"));
    }
    git(root, &["switch", "--quiet", local])?;
    match git(root, &["merge", "--ff-only", "--quiet", start]) {
        Ok(_) => Ok(format!("Checked out {local} (up to date)")),
        Err(_) => Ok(format!(
            "Checked out {local}; it has commits of its own, so it was not updated"
        )),
    }
}

/// For checks whose "no" is a normal answer (does this branch exist?): no warning logged.
fn git_succeeds(dir: &Path, args: &[&str]) -> bool {
    command(dir, args)
        .output()
        .is_ok_and(|output| output.status.success())
}

/// Like `git`, for questions whose "no" is a normal answer: `None`, and no warning logged.
fn git_output(dir: &Path, args: &[&str]) -> Option<String> {
    let output = command(dir, args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

fn command(dir: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(dir)
        .args(args)
        // Never wait for a password or a host key prompt behind the TUI.
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null());
    command
}

/// Runs `git` in `dir`. `Err` holds what git printed, for the user.
fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = command(dir, args)
        .output()
        .map_err(|err| format!("cannot run git: {err}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let message = stderr.trim().lines().last().unwrap_or("").to_string();
        tracing::warn!(?args, %stderr, "git failed");
        Err(if message.is_empty() {
            format!("git {} failed", args.first().unwrap_or(&""))
        } else {
            message
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(text: &str) -> Url {
        crate::config::parse_host(text).unwrap()
    }

    #[test]
    fn matches_remote_urls() {
        let gitlab = host("gitlab.com");
        for remote in [
            "https://gitlab.com/group/api.git",
            "https://gitlab.com/group/api",
            "git@gitlab.com:group/api.git",
            "ssh://git@gitlab.com:2222/group/api.git",
            "git@GitLab.com:Group/API.git",
        ] {
            assert!(remote_points_at(remote, &gitlab, "group/api"), "{remote}");
        }
        assert!(remote_points_at(
            "https://example.com/gitlab/group/api.git",
            &host("https://example.com/gitlab"),
            "group/api"
        ));
        assert!(!remote_points_at(
            "git@github.com:group/api.git",
            &gitlab,
            "group/api"
        ));
        assert!(!remote_points_at(
            "git@gitlab.com:group/api-v2.git",
            &gitlab,
            "group/api"
        ));
        assert!(!remote_points_at(
            "git@gitlab.com:other/group/api-old.git",
            &gitlab,
            "group/api"
        ));
        assert!(!remote_points_at("/some/local/path", &gitlab, "group/api"));
    }

    #[test]
    fn parses_remote_listing() {
        let listing = "origin\tgit@gitlab.com:group/api.git (fetch)\n\
                       origin\tgit@gitlab.com:group/api.git (push)\n\
                       fork\thttps://gitlab.com/me/api.git (fetch)\n";
        assert_eq!(
            parse_remotes(listing),
            vec![
                ("origin".into(), "git@gitlab.com:group/api.git".into()),
                ("fork".into(), "https://gitlab.com/me/api.git".into()),
            ]
        );
    }

    /// A bare "server" repository and a clone of it, in a fresh temporary directory.
    struct Fixture {
        dir: PathBuf,
        clone: PathBuf,
    }

    /// Commits in the test repositories, independent of the user's git configuration
    /// (signing, hooks, default branch).
    fn run(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn commit(dir: &Path, file: &str, text: &str) -> String {
        std::fs::write(dir.join(file), text).unwrap();
        run(dir, &["add", file]);
        run(dir, &["commit", "--quiet", "-m", text]);
        run(dir, &["rev-parse", "HEAD"])
    }

    impl Fixture {
        /// The server has `main` and `feat/x`, and merge request 7 from a fork (its commit
        /// is only reachable through `refs/merge-requests/7/head`).
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("lazyforge-git-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let server = dir.join("server.git");
            let work = dir.join("work");
            run(
                &dir,
                &[
                    "init",
                    "--quiet",
                    "--bare",
                    "-b",
                    "main",
                    server.to_str().unwrap(),
                ],
            );
            run(
                &dir,
                &["init", "--quiet", "-b", "main", work.to_str().unwrap()],
            );
            commit(&work, "a.txt", "one");
            run(
                &work,
                &["remote", "add", "origin", server.to_str().unwrap()],
            );
            run(&work, &["push", "--quiet", "origin", "main"]);
            run(&work, &["switch", "--quiet", "-c", "feat/x"]);
            commit(&work, "b.txt", "feature");
            run(&work, &["push", "--quiet", "origin", "feat/x"]);
            run(&work, &["switch", "--quiet", "-c", "fork-work", "main"]);
            commit(&work, "c.txt", "from a fork");
            run(
                &work,
                &[
                    "push",
                    "--quiet",
                    "origin",
                    "HEAD:refs/merge-requests/7/head",
                ],
            );

            let clone = dir.join("clone");
            run(
                &dir,
                &[
                    "clone",
                    "--quiet",
                    server.to_str().unwrap(),
                    clone.to_str().unwrap(),
                ],
            );
            Self { dir, clone }
        }

        fn request(&self, target: Target) -> CheckoutRequest {
            CheckoutRequest {
                root: self.clone.clone(),
                remote: "origin".into(),
                target,
            }
        }

        fn current_branch(&self) -> String {
            run(&self.clone, &["branch", "--show-current"])
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn merge_request(number: u64, source_branch: &str) -> Target {
        Target::ChangeRequest {
            head_ref: format!("refs/merge-requests/{number}/head"),
            source_branch: source_branch.into(),
            fallback_branch: format!("mr/{number}"),
        }
    }

    #[test]
    fn checks_out_a_branch_tracking_the_remote() {
        let fx = Fixture::new("branch");
        let message = checkout(&fx.request(Target::Branch("feat/x".into()))).unwrap();
        assert_eq!(message, "Checked out feat/x");
        assert_eq!(fx.current_branch(), "feat/x");
        assert_eq!(
            run(&fx.clone, &["rev-parse", "--abbrev-ref", "@{upstream}"]),
            "origin/feat/x"
        );
        assert!(fx.clone.join("b.txt").exists());
    }

    #[test]
    fn a_merge_request_from_this_repository_uses_its_branch() {
        let fx = Fixture::new("same-repo");
        // The server keeps the merge request ref in step with the branch.
        let work = fx.dir.join("work");
        run(
            &work,
            &[
                "push",
                "--quiet",
                "origin",
                "feat/x:refs/merge-requests/8/head",
            ],
        );
        checkout(&fx.request(merge_request(8, "feat/x"))).unwrap();
        assert_eq!(fx.current_branch(), "feat/x");
    }

    #[test]
    fn a_merge_request_from_a_fork_gets_its_own_branch() {
        let fx = Fixture::new("fork");
        // The fork's branch name does not exist here.
        checkout(&fx.request(merge_request(7, "patch-1"))).unwrap();
        assert_eq!(fx.current_branch(), "mr/7");
        assert!(fx.clone.join("c.txt").exists());
    }

    #[test]
    fn a_same_named_branch_with_other_code_is_not_used() {
        let fx = Fixture::new("same-name");
        // Merge request 7 comes from a fork's `feat/x`, which is not this repository's.
        checkout(&fx.request(merge_request(7, "feat/x"))).unwrap();
        assert_eq!(fx.current_branch(), "mr/7");
        assert!(
            !fx.clone.join("b.txt").exists(),
            "not this repository's feat/x"
        );
    }

    #[test]
    fn refuses_with_uncommitted_changes() {
        let fx = Fixture::new("dirty");
        std::fs::write(fx.clone.join("a.txt"), "edited").unwrap();
        let err = checkout(&fx.request(Target::Branch("feat/x".into()))).unwrap_err();
        assert!(err.contains("uncommitted changes"), "{err}");
        assert_eq!(fx.current_branch(), "main", "nothing was switched");
        assert_eq!(
            std::fs::read_to_string(fx.clone.join("a.txt")).unwrap(),
            "edited"
        );
    }

    #[test]
    fn untracked_files_do_not_block() {
        let fx = Fixture::new("untracked");
        std::fs::write(fx.clone.join("notes.txt"), "mine").unwrap();
        checkout(&fx.request(Target::Branch("feat/x".into()))).unwrap();
        assert!(fx.clone.join("notes.txt").exists());
    }

    #[test]
    fn an_existing_branch_is_fast_forwarded_never_reset() {
        let fx = Fixture::new("existing");
        checkout(&fx.request(Target::Branch("feat/x".into()))).unwrap();
        // Someone pushes to feat/x: the next checkout fast-forwards.
        let work = fx.dir.join("work");
        run(&work, &["switch", "--quiet", "feat/x"]);
        let pushed = commit(&work, "d.txt", "more");
        run(&work, &["push", "--quiet", "origin", "feat/x"]);
        let message = checkout(&fx.request(Target::Branch("feat/x".into()))).unwrap();
        assert!(message.contains("up to date"), "{message}");
        assert_eq!(run(&fx.clone, &["rev-parse", "HEAD"]), pushed);

        // A local commit: the branch is left alone, the commit kept.
        let mine = commit(&fx.clone, "e.txt", "local work");
        commit(&work, "f.txt", "even more");
        run(&work, &["push", "--quiet", "origin", "feat/x"]);
        let message = checkout(&fx.request(Target::Branch("feat/x".into()))).unwrap();
        assert!(message.contains("not updated"), "{message}");
        assert_eq!(run(&fx.clone, &["rev-parse", "HEAD"]), mine);
    }

    #[test]
    fn reports_a_missing_branch() {
        let fx = Fixture::new("missing");
        let err = checkout(&fx.request(Target::Branch("nope".into()))).unwrap_err();
        assert!(!err.is_empty());
        assert_eq!(fx.current_branch(), "main");
    }

    #[test]
    fn outside_a_repository_there_is_none() {
        let dir = std::env::temp_dir().join(format!("lazyforge-no-repo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Unless the temporary directory itself is inside a repository.
        if !git_succeeds(&dir, &["rev-parse", "--git-dir"]) {
            assert!(LocalRepo::open(&dir).is_none());
        }
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn finds_the_repository_and_its_remote() {
        let fx = Fixture::new("discover");
        let repo = LocalRepo::open(&fx.clone.join(".git").join("..")).unwrap();
        assert_eq!(repo.remotes.len(), 1);
        assert_eq!(repo.remotes[0].0, "origin");
    }
}
