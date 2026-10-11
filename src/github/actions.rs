//! GitHub Actions: workflow runs and jobs as pipelines and jobs, and job logs in the format
//! the log viewer expects.

use chrono::{DateTime, Utc};
use serde::Deserialize;

use super::rest::RawLogin;
use crate::forge::{Job, Pipeline, PipelineStatus, PipelineSummary};

/// `GET /repos/{repo}/actions/runs`
#[derive(Debug, Deserialize)]
pub(crate) struct RawRuns {
    pub workflow_runs: Vec<RawRun>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RawRun {
    pub id: u64,
    /// The workflow's name, e.g. "CI".
    pub name: Option<String>,
    pub status: Option<String>,
    pub conclusion: Option<String>,
    pub head_branch: Option<String>,
    pub head_sha: String,
    /// `push`, `pull_request`, `schedule`…
    pub event: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub run_started_at: Option<DateTime<Utc>>,
    pub html_url: String,
    /// Who started this attempt (differs from `actor` on re-runs).
    pub triggering_actor: Option<RawLogin>,
    pub actor: Option<RawLogin>,
}

/// `GET /repos/{repo}/actions/runs/{run_id}/jobs`
#[derive(Debug, Deserialize)]
pub(crate) struct RawJobs {
    pub jobs: Vec<RawJob>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawJob {
    pub id: u64,
    pub name: String,
    pub status: Option<String>,
    pub conclusion: Option<String>,
    pub created_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub html_url: Option<String>,
}

/// GitHub reports a `status` (queued → in_progress → completed) and, once completed, a
/// `conclusion`. Both together map to one pipeline status.
pub(crate) fn status(status: Option<&str>, conclusion: Option<&str>) -> PipelineStatus {
    match (status, conclusion) {
        (Some("completed"), Some("success")) => PipelineStatus::Success,
        (Some("completed"), Some("failure" | "timed_out" | "startup_failure")) => {
            PipelineStatus::Failed
        }
        (Some("completed"), Some("cancelled")) => PipelineStatus::Canceled,
        (Some("completed"), Some("skipped" | "neutral" | "stale")) => PipelineStatus::Skipped,
        (Some("completed"), Some("action_required")) => PipelineStatus::Manual,
        (Some("in_progress"), _) => PipelineStatus::Running,
        (Some("queued" | "requested" | "waiting" | "pending"), _) => PipelineStatus::Pending,
        _ => PipelineStatus::Unknown,
    }
}

impl RawRun {
    fn status(&self) -> PipelineStatus {
        status(self.status.as_deref(), self.conclusion.as_deref())
    }

    pub(crate) fn into_pipeline(self, repo: &str) -> Pipeline {
        Pipeline {
            status: self.status(),
            id: self.id,
            name: self.name,
            project_id: repo.to_string(),
            git_ref: self.head_branch.unwrap_or_default(),
            sha: self.head_sha,
            source: self.event,
            created_at: self.created_at,
            updated_at: self.updated_at,
            web_url: self.html_url,
        }
    }

    pub(crate) fn into_summary(self, repo: &str) -> PipelineSummary {
        let status = self.status();
        let finished = !status.is_active();
        let started = self.run_started_at.unwrap_or(self.created_at);
        PipelineSummary {
            id: self.id,
            name: self.name,
            project_id: repo.to_string(),
            project_path: repo.to_string(),
            status,
            git_ref: self.head_branch.unwrap_or_default(),
            sha: self.head_sha,
            user: self.triggering_actor.or(self.actor).map(|a| a.login),
            created_at: self.created_at,
            finished_at: finished.then_some(self.updated_at),
            // For a finished run, the last update is when it ended.
            duration: finished.then(|| (self.updated_at - started).num_seconds().max(0) as u64),
            web_url: self.html_url,
        }
    }
}

impl RawJob {
    /// GitHub jobs have no stages; they are grouped under their workflow's name.
    pub(crate) fn into_job(self, stage: &str, run: &RawRun) -> Job {
        let duration = match (self.started_at, self.completed_at) {
            (Some(start), Some(end)) => Some((end - start).num_milliseconds() as f64 / 1000.0),
            _ => None,
        };
        Job {
            id: self.id,
            status: status(self.status.as_deref(), self.conclusion.as_deref()),
            name: self.name,
            stage: stage.to_string(),
            created_at: self
                .created_at
                .or(self.started_at)
                .unwrap_or(run.created_at),
            started_at: self.started_at,
            finished_at: self.completed_at,
            duration,
            allow_failure: false,
            web_url: self.html_url.unwrap_or_else(|| run.html_url.clone()),
        }
    }
}

const BOLD_CYAN: &str = "\x1b[36;1m";
const BOLD_RED: &str = "\x1b[31;1m";
const YELLOW: &str = "\x1b[33m";
const GREEN: &str = "\x1b[32m";
const RESET: &str = "\x1b[0m";

/// Turns a GitHub Actions log into what the log viewer shows: without the timestamp at the
/// start of each line, and with workflow commands (`##[group]`, `##[error]`…) as colors.
pub(crate) fn clean_log(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for line in raw.trim_start_matches('\u{feff}').lines() {
        let line = strip_timestamp(line);
        let rendered = if let Some(rest) = command(line, "group") {
            format!("{BOLD_CYAN}▸ {rest}{RESET}")
        } else if command(line, "endgroup").is_some() {
            continue;
        } else if let Some(rest) = command(line, "error") {
            format!("{BOLD_RED}✗ {rest}{RESET}")
        } else if let Some(rest) = command(line, "warning") {
            format!("{YELLOW}! {rest}{RESET}")
        } else if let Some(rest) = command(line, "command") {
            format!("{GREEN}{rest}{RESET}")
        } else if let Some(rest) = command(line, "notice").or_else(|| command(line, "debug")) {
            rest.to_string()
        } else {
            line.to_string()
        };
        out.push_str(&rendered);
        out.push('\n');
    }
    out
}

/// The text after a workflow command such as `##[group]`. Downloaded logs also write some
/// of them without the `##` (`[command]/usr/bin/git …`).
fn command<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let rest = line.strip_prefix("##").unwrap_or(line);
    rest.strip_prefix('[')?
        .strip_prefix(name)?
        .strip_prefix(']')
}

/// `2026-10-10T08:15:30.1234567Z Run cargo test` → `Run cargo test`.
fn strip_timestamp(line: &str) -> &str {
    match line.split_once(' ') {
        Some((stamp, rest))
            if stamp.len() >= 20
                && stamp.ends_with('Z')
                && DateTime::parse_from_rfc3339(stamp).is_ok() =>
        {
            rest
        }
        _ => line,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_status_and_conclusion() {
        assert_eq!(
            status(Some("completed"), Some("success")),
            PipelineStatus::Success
        );
        assert_eq!(
            status(Some("completed"), Some("timed_out")),
            PipelineStatus::Failed
        );
        assert_eq!(
            status(Some("completed"), Some("cancelled")),
            PipelineStatus::Canceled
        );
        assert_eq!(status(Some("in_progress"), None), PipelineStatus::Running);
        assert_eq!(status(Some("queued"), None), PipelineStatus::Pending);
        assert!(status(Some("waiting"), None).is_active());
    }

    fn run(status: &str, conclusion: Option<&str>) -> RawRun {
        serde_json::from_value(json!({
            "id": 77, "name": "CI", "status": status, "conclusion": conclusion,
            "head_branch": "feat/x", "head_sha": "abc", "event": "pull_request",
            "created_at": "2026-10-10T08:00:00Z", "updated_at": "2026-10-10T08:05:30Z",
            "run_started_at": "2026-10-10T08:00:30Z", "html_url": "https://github.com/a/b/actions/runs/77",
            "triggering_actor": {"login": "thang"}, "actor": {"login": "linh"}
        }))
        .unwrap()
    }

    #[test]
    fn runs_become_pipelines_and_dashboard_rows() {
        let pipeline = run("completed", Some("failure")).into_pipeline("acme/api");
        assert_eq!(pipeline.name.as_deref(), Some("CI"));
        assert_eq!(pipeline.project_id, "acme/api");
        assert_eq!(pipeline.status, PipelineStatus::Failed);

        let row = run("completed", Some("success")).into_summary("acme/api");
        assert_eq!(
            row.duration,
            Some(300),
            "from the start of the run to its end"
        );
        assert_eq!(
            row.user.as_deref(),
            Some("thang"),
            "who triggered this attempt"
        );
        let running = run("in_progress", None).into_summary("acme/api");
        assert_eq!((running.duration, running.finished_at), (None, None));
    }

    #[test]
    fn jobs_are_grouped_under_the_workflow() {
        let raw: RawJob = serde_json::from_value(json!({
            "id": 5, "name": "test (ubuntu)", "status": "completed", "conclusion": "success",
            "started_at": "2026-10-10T08:01:00Z", "completed_at": "2026-10-10T08:02:30Z",
            "html_url": null
        }))
        .unwrap();
        let job = raw.into_job("CI", &run("completed", Some("success")));
        assert_eq!(job.stage, "CI");
        assert_eq!(job.duration, Some(90.0));
        assert_eq!(
            job.web_url, "https://github.com/a/b/actions/runs/77",
            "falls back to the run"
        );
    }

    #[test]
    fn cleans_github_log_lines() {
        let raw = "\u{feff}2026-10-10T08:15:30.1234567Z ##[group]Run cargo test\n\
                   2026-10-10T08:15:30.2234567Z \x1b[36;1mcargo test --locked\x1b[0m\n\
                   2026-10-10T08:15:31.0000000Z ##[endgroup]\n\
                   2026-10-10T08:15:39.0000000Z [command]/usr/bin/git version\n\
                   2026-10-10T08:15:40.0000000Z test parse ... ok\n\
                   2026-10-10T08:15:41.0000000Z ##[error]Process completed with exit code 101.\n\
                   no timestamp here\n";
        let cleaned: Vec<String> = clean_log(raw).lines().map(str::to_string).collect();
        assert_eq!(
            cleaned,
            vec![
                format!("{BOLD_CYAN}▸ Run cargo test{RESET}"),
                "\x1b[36;1mcargo test --locked\x1b[0m".to_string(),
                format!("{GREEN}/usr/bin/git version{RESET}"),
                "test parse ... ok".to_string(),
                format!("{BOLD_RED}✗ Process completed with exit code 101.{RESET}"),
                "no timestamp here".to_string(),
            ]
        );
    }

    #[test]
    fn keeps_lines_that_only_look_like_timestamps() {
        assert_eq!(strip_timestamp("2026 is a year"), "2026 is a year");
        assert_eq!(strip_timestamp("Z marks the spot"), "Z marks the spot");
        assert_eq!(command("[groupie] band", "group"), None);
    }
}
