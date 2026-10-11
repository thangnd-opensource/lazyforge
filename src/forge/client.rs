//! `Forge`: one type for "a GitLab or GitHub server", so the rest of the app does not care
//! which one it talks to.
//!
//! An enum rather than a `dyn Trait`: there are exactly two backends, a `match` is easy to
//! follow, and async methods on trait objects need extra machinery.

use super::{
    ApiError, BranchPage, CiAction, CiSnapshot, Comparison, MrAction, MrDetail, MrList, MrPage,
    PipelineList, PipelinePage, Repository, User,
};
use crate::github::GitHubClient;
use crate::gitlab::GitLabClient;

/// Which kind of server. Also answers "what can this server do" and "what does it call
/// things", so the UI can adapt instead of pretending both are the same.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize, clap::ValueEnum,
)]
#[serde(rename_all = "lowercase")]
#[value(rename_all = "lowercase")]
pub enum ForgeKind {
    GitLab,
    GitHub,
}

impl ForgeKind {
    /// "GitLab" or "GitHub".
    pub fn name(self) -> &'static str {
        match self {
            Self::GitLab => "GitLab",
            Self::GitHub => "GitHub",
        }
    }

    /// The public server: gitlab.com or github.com.
    pub fn default_host(self) -> &'static str {
        match self {
            Self::GitLab => "gitlab.com",
            Self::GitHub => "github.com",
        }
    }

    /// The kind of a server we have no explicit setting for: github.com is GitHub,
    /// everything else is assumed to be GitLab (self-hosted GitHub needs `kind = "github"`).
    pub fn guess(host: &url::Url) -> Self {
        match host.host_str() {
            Some("github.com" | "www.github.com" | "api.github.com") => Self::GitHub,
            _ => Self::GitLab,
        }
    }

    /// "pipeline" or "run", for sentences about one of them.
    pub fn pipeline_noun(self) -> &'static str {
        match self {
            Self::GitLab => "pipeline",
            Self::GitHub => "run",
        }
    }

    /// Pipelines (GitLab) or workflow runs (GitHub Actions).
    pub fn pipelines(self) -> &'static str {
        match self {
            Self::GitLab => "Pipelines",
            Self::GitHub => "Actions",
        }
    }

    /// "Merge requests" or "Pull requests".
    pub fn change_requests(self) -> &'static str {
        match self {
            Self::GitLab => "Merge requests",
            Self::GitHub => "Pull requests",
        }
    }

    /// The sign before a change request number: `!42` on GitLab, `#42` on GitHub.
    pub fn reference_prefix(self) -> char {
        match self {
            Self::GitLab => '!',
            Self::GitHub => '#',
        }
    }

    /// Can a running job's log be read before it finishes? GitHub's REST API only serves
    /// logs of finished jobs.
    pub fn supports_live_logs(self) -> bool {
        matches!(self, Self::GitLab)
    }

    /// Can a reviewer take their approval back? GitHub has no API for that.
    pub fn can_unapprove(self) -> bool {
        matches!(self, Self::GitLab)
    }

    /// Can a review end with "request changes"? GitLab's free tier has no such state.
    pub fn can_request_changes(self) -> bool {
        matches!(self, Self::GitHub)
    }

    /// Can a single job be canceled? GitHub only cancels whole workflow runs.
    pub fn can_cancel_jobs(self) -> bool {
        matches!(self, Self::GitLab)
    }
}

/// A client for one server.
#[derive(Debug, Clone)]
pub enum Forge {
    GitLab(GitLabClient),
    GitHub(GitHubClient),
}

impl Forge {
    /// A client for `kind` at `host`.
    pub fn connect(kind: ForgeKind, host: &url::Url, token: &str) -> Result<Self, ApiError> {
        Ok(match kind {
            ForgeKind::GitLab => Self::GitLab(GitLabClient::new(host, token)?),
            ForgeKind::GitHub => Self::GitHub(GitHubClient::new(host, token)?),
        })
    }

    pub fn kind(&self) -> ForgeKind {
        match self {
            Self::GitLab(_) => ForgeKind::GitLab,
            Self::GitHub(_) => ForgeKind::GitHub,
        }
    }

    /// Scopes of the token, when the server can tell.
    pub async fn token_scopes(&self) -> Result<Vec<String>, ApiError> {
        match self {
            Self::GitLab(client) => client.token_scopes().await,
            Self::GitHub(client) => client.token_scopes().await,
        }
    }

    /// The scope needed for everything lazyforge does, to warn when a token lacks it.
    pub fn required_scope(&self) -> &'static str {
        match self {
            Self::GitLab(_) => "api",
            Self::GitHub(_) => "repo",
        }
    }

    pub async fn repositories(&self) -> Result<Vec<Repository>, ApiError> {
        match self {
            Self::GitLab(client) => client.repositories().await,
            Self::GitHub(client) => client.repositories().await,
        }
    }

    pub async fn current_user(&self) -> Result<User, ApiError> {
        match self {
            Self::GitLab(client) => client.current_user().await,
            Self::GitHub(client) => client.current_user().await,
        }
    }

    pub async fn merge_requests(&self, list: &MrList) -> Result<MrPage, ApiError> {
        match self {
            Self::GitLab(client) => client.merge_requests(list).await,
            Self::GitHub(client) => client.merge_requests(list).await,
        }
    }

    pub async fn merge_request_detail(
        &self,
        project: &str,
        iid: u64,
    ) -> Result<MrDetail, ApiError> {
        match self {
            Self::GitLab(client) => client.merge_request_detail(project, iid).await,
            Self::GitHub(client) => client.merge_request_detail(project, iid).await,
        }
    }

    pub async fn run_action(
        &self,
        project: &str,
        iid: u64,
        action: &MrAction,
    ) -> Result<(), ApiError> {
        match self {
            Self::GitLab(client) => client.run_action(project, iid, action).await,
            Self::GitHub(client) => client.run_action(project, iid, action).await,
        }
    }

    pub async fn merge_request_ci(
        &self,
        project: &str,
        iid: u64,
        pipeline_id: Option<u64>,
    ) -> Result<CiSnapshot, ApiError> {
        match self {
            Self::GitLab(client) => client.merge_request_ci(project, iid, pipeline_id).await,
            Self::GitHub(client) => client.merge_request_ci(project, iid, pipeline_id).await,
        }
    }

    pub async fn pipeline_ci(
        &self,
        project_id: &str,
        pipeline_id: u64,
    ) -> Result<CiSnapshot, ApiError> {
        match self {
            Self::GitLab(client) => client.pipeline_ci(project_id, pipeline_id).await,
            Self::GitHub(client) => client.pipeline_ci(project_id, pipeline_id).await,
        }
    }

    pub async fn job_trace(&self, project_id: &str, job_id: u64) -> Result<String, ApiError> {
        match self {
            Self::GitLab(client) => client.job_trace(project_id, job_id).await,
            Self::GitHub(client) => client.job_trace(project_id, job_id).await,
        }
    }

    pub async fn run_ci_action(&self, project_id: &str, action: CiAction) -> Result<(), ApiError> {
        match self {
            Self::GitLab(client) => client.run_ci_action(project_id, action).await,
            Self::GitHub(client) => client.run_ci_action(project_id, action).await,
        }
    }

    pub async fn pipelines(&self, list: &PipelineList) -> Result<PipelinePage, ApiError> {
        match self {
            Self::GitLab(client) => client.pipelines(list).await,
            Self::GitHub(client) => client.pipelines(list).await,
        }
    }

    /// Branches of `project`, most recently updated first.
    pub async fn branches(&self, project: &str) -> Result<BranchPage, ApiError> {
        match self {
            Self::GitLab(client) => client.branches(project).await,
            Self::GitHub(client) => client.branches(project).await,
        }
    }

    /// What `head` changed since it left `base`.
    pub async fn compare(
        &self,
        project: &str,
        base: &str,
        head: &str,
    ) -> Result<Comparison, ApiError> {
        match self {
            Self::GitLab(client) => client.compare(project, base, head).await,
            Self::GitHub(client) => client.compare(project, base, head).await,
        }
    }
}
