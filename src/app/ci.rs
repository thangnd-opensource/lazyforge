//! Jobs, logs, retry and cancel of one pipeline. Used by the Pipelines section of the merge
//! request screen and by pipelines opened from the dashboard.

use super::detail::Popup;
use super::{Action, Command, Notice};
use crate::forge::trace::clean_trace;
use crate::forge::{
    ApiError, CiAction, CiSnapshot, ForgeKind, Job, Pipeline, PipelineStatus, ProjectId,
};

/// How often (in seconds, i.e. ticks) to reload an active pipeline and a running job's log.
const PIPELINE_POLL_TICKS: u32 = 5;
const LOG_POLL_TICKS: u32 = 2;

/// Where the pipelines come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CiSource {
    /// All pipelines of a merge request; `P`/`p` move between them.
    MergeRequest { project_path: String, iid: u64 },
    /// A single pipeline, opened from the dashboard.
    Pipeline {
        project_id: ProjectId,
        pipeline_id: u64,
    },
}

#[derive(Debug)]
pub struct CiState {
    pub source: CiSource,
    /// GitLab and GitHub differ in what they allow (live logs, canceling single jobs).
    pub forge: ForgeKind,
    pub snapshot: Option<CiSnapshot>,
    /// Index into the snapshot's jobs.
    pub selected_job: usize,
    pub loading: bool,
    pub error: Option<String>,
    pub(super) request_id: u64,
    pub log: Option<JobLog>,
    /// Set while a retry or cancel runs.
    pub busy: Option<&'static str>,
    /// Seconds the Pipelines section has been visible, to pace polling.
    ticks: u32,
}

#[derive(Debug)]
pub struct JobLog {
    pub job_id: u64,
    pub project_id: ProjectId,
    pub job_name: String,
    /// Status of the job when the log was last requested; when it changes, we fetch again.
    pub status: PipelineStatus,
    /// Cleaned lines, ANSI color codes included.
    pub lines: Vec<String>,
    pub loading: bool,
    pub loaded_once: bool,
    pub error: Option<String>,
    /// Stick to the bottom as new lines arrive (like `tail -f`).
    pub follow: bool,
    pub scroll: usize,
    request_id: u64,
}

impl CiState {
    pub fn new(source: CiSource, forge: ForgeKind) -> Self {
        Self {
            source,
            forge,
            snapshot: None,
            selected_job: 0,
            loading: false,
            error: None,
            request_id: 0,
            log: None,
            busy: None,
            ticks: 0,
        }
    }

    pub fn pipeline(&self) -> Option<&Pipeline> {
        let snapshot = self.snapshot.as_ref()?;
        snapshot
            .pipelines
            .iter()
            .find(|p| Some(p.id) == snapshot.pipeline_id)
    }

    pub fn jobs(&self) -> &[Job] {
        self.snapshot.as_ref().map_or(&[], |s| &s.jobs)
    }

    pub fn selected_job(&self) -> Option<&Job> {
        self.jobs().get(self.selected_job)
    }

    /// `(index, count)` of the shown pipeline among the merge request's pipelines.
    pub fn pipeline_position(&self) -> Option<(usize, usize)> {
        let snapshot = self.snapshot.as_ref()?;
        let index = snapshot
            .pipelines
            .iter()
            .position(|p| Some(p.id) == snapshot.pipeline_id)?;
        Some((index, snapshot.pipelines.len()))
    }

    /// The pipeline currently shown, if loaded.
    fn pipeline_id(&self) -> Option<u64> {
        self.snapshot.as_ref().and_then(|s| s.pipeline_id)
    }

    /// Loads (or reloads) the snapshot. `pipeline_id` picks one of a merge request's
    /// pipelines; `None` means the newest.
    pub(super) fn fetch(&mut self, next_request_id: &mut u64, pipeline_id: Option<u64>) -> Command {
        *next_request_id += 1;
        self.request_id = *next_request_id;
        self.loading = true;
        Command::FetchCi {
            request_id: self.request_id,
            source: self.source.clone(),
            pipeline_id,
        }
    }

    /// Reloads the pipeline currently shown.
    pub(super) fn reload(&mut self, next_request_id: &mut u64) -> Command {
        let pipeline_id = self.pipeline_id();
        self.fetch(next_request_id, pipeline_id)
    }

    /// Loads the snapshot the first time it is shown.
    pub(super) fn ensure_loaded(&mut self, next_request_id: &mut u64) -> Option<Command> {
        (self.snapshot.is_none() && !self.loading).then(|| self.fetch(next_request_id, None))
    }

    /// The open log's job, otherwise the pipeline.
    fn browser_url(&self) -> Option<String> {
        let url = match &self.log {
            Some(log) => &self.jobs().iter().find(|j| j.id == log.job_id)?.web_url,
            None => &self.pipeline()?.web_url,
        };
        Some(url.clone())
    }

    /// "job lint" or "pipeline #123", for notices and popups.
    pub fn describe(&self, action: CiAction) -> String {
        match action {
            CiAction::CancelPipeline(id) | CiAction::RetryPipeline(id) => {
                format!("{} #{id}", self.forge.pipeline_noun())
            }
            CiAction::CancelJob(id) | CiAction::RetryJob(id) => {
                let job = self.jobs().iter().find(|j| j.id == id);
                format!("job {}", job.map_or("?", |j| j.name.as_str()))
            }
        }
    }

    pub(super) fn select_next(&mut self) {
        if self.selected_job + 1 < self.jobs().len() {
            self.selected_job += 1;
        }
    }

    pub(super) fn select_previous(&mut self) {
        self.selected_job = self.selected_job.saturating_sub(1);
    }

    pub(super) fn open_log(&mut self, next_request_id: &mut u64) -> Option<Command> {
        let project_id = self.pipeline()?.project_id.clone();
        let job = self.selected_job()?.clone();
        self.log = Some(JobLog {
            job_id: job.id,
            project_id,
            job_name: job.name,
            status: job.status,
            lines: Vec::new(),
            loading: false,
            loaded_once: false,
            error: None,
            // Start at the bottom: that is where a failure is reported.
            follow: true,
            scroll: 0,
            request_id: 0,
        });
        // GitHub only serves the log of a finished job; `on_loaded` fetches it when the job
        // finishes.
        if job.status.is_active() && !self.forge.supports_live_logs() {
            return None;
        }
        self.fetch_log(next_request_id)
    }

    /// Whether the open log can be fetched now.
    fn log_available(&self, status: PipelineStatus) -> bool {
        self.forge.supports_live_logs() || !status.is_active()
    }

    fn fetch_log(&mut self, next_request_id: &mut u64) -> Option<Command> {
        let log = self.log.as_mut()?;
        *next_request_id += 1;
        log.request_id = *next_request_id;
        log.loading = true;
        Some(Command::FetchJobLog {
            request_id: log.request_id,
            project_id: log.project_id.clone(),
            job_id: log.job_id,
        })
    }

    pub(super) fn scroll_log(&mut self, down: bool, lines: u16) {
        if let Some(log) = &mut self.log {
            // Scrolling up means the user wants to read: stop following.
            if !down {
                log.follow = false;
            }
            let lines = usize::from(lines);
            log.scroll = if down {
                log.scroll.saturating_add(lines)
            } else {
                log.scroll.saturating_sub(lines)
            };
        }
    }

    pub(super) fn log_to_top(&mut self) {
        if let Some(log) = &mut self.log {
            log.follow = false;
            log.scroll = 0;
        }
    }

    pub(super) fn set_follow(&mut self, follow: bool) {
        if let Some(log) = &mut self.log {
            log.follow = follow;
        }
    }

    /// The UI reports the largest useful scroll offset after drawing.
    pub(super) fn clamp_log_scroll(&mut self, max: usize) {
        if let Some(log) = &mut self.log {
            log.scroll = if log.follow { max } else { log.scroll.min(max) };
        }
    }

    /// Switches to the next older (`older = true`) or newer pipeline.
    pub(super) fn switch_pipeline(
        &mut self,
        older: bool,
        next_request_id: &mut u64,
    ) -> Option<Command> {
        let (index, count) = self.pipeline_position()?;
        let target = if older {
            (index + 1 < count).then_some(index + 1)?
        } else {
            index.checked_sub(1)?
        };
        let pipeline_id = self.snapshot.as_ref()?.pipelines[target].id;
        self.selected_job = 0;
        self.log = None;
        Some(self.fetch(next_request_id, Some(pipeline_id)))
    }

    /// Called every second while the Pipelines section is visible.
    pub(super) fn tick(&mut self, next_request_id: &mut u64) -> Option<Command> {
        self.ticks = self.ticks.wrapping_add(1);
        let mut commands = Vec::new();

        let pipeline_active = self.pipeline().is_some_and(|p| p.status.is_active());
        if pipeline_active && self.ticks.is_multiple_of(PIPELINE_POLL_TICKS) && !self.loading {
            commands.push(self.reload(next_request_id));
        }

        let log_active = self.forge.supports_live_logs()
            && self
                .log
                .as_ref()
                .is_some_and(|log| log.status.is_active() && !log.loading);
        if log_active && self.ticks.is_multiple_of(LOG_POLL_TICKS) {
            commands.extend(self.fetch_log(next_request_id));
        }
        Command::batch(commands)
    }

    pub(super) fn on_loaded(
        &mut self,
        request_id: u64,
        result: Result<CiSnapshot, ApiError>,
        next_request_id: &mut u64,
    ) -> Option<Command> {
        if request_id != self.request_id {
            return None;
        }
        self.loading = false;
        let snapshot = match result {
            Ok(snapshot) => snapshot,
            Err(err) => {
                tracing::warn!(error = %err, "failed to load pipelines");
                self.error = Some(err.to_string());
                return None;
            }
        };
        self.error = None;

        // Keep the same job selected across reloads (a retried job gets a new id).
        let previous = self
            .selected_job()
            .map(|j| (j.stage.clone(), j.name.clone()));
        self.selected_job = previous
            .and_then(|(stage, name)| {
                snapshot
                    .jobs
                    .iter()
                    .position(|j| j.stage == stage && j.name == name)
            })
            .unwrap_or(0)
            .min(snapshot.jobs.len().saturating_sub(1));
        self.snapshot = Some(snapshot);

        // Keep the open log in sync with its job.
        let log = self.log.as_mut()?;
        let jobs = &self.snapshot.as_ref()?.jobs;
        let job = match jobs.iter().find(|j| j.id == log.job_id) {
            Some(job) => job,
            // The job was retried: follow the new attempt, which has the same name.
            None => {
                let job = jobs.iter().find(|j| j.name == log.job_name)?;
                log.job_id = job.id;
                log.lines.clear();
                log.follow = true;
                job
            }
        };
        let changed = job.status != log.status;
        let missing = log.lines.is_empty() && !log.loading;
        let status = job.status;
        log.status = status;
        if (changed || missing) && self.log_available(status) {
            return self.fetch_log(next_request_id);
        }
        None
    }

    pub(super) fn on_log_loaded(&mut self, request_id: u64, result: Result<String, ApiError>) {
        let Some(log) = self.log.as_mut().filter(|l| l.request_id == request_id) else {
            return;
        };
        log.loading = false;
        log.loaded_once = true;
        match result {
            Ok(raw) => {
                log.lines = clean_trace(&raw);
                log.error = None;
            }
            Err(err) => log.error = Some(err.to_string()),
        }
    }

    /// Checks that `action` makes sense now; returns the notice to show if not.
    pub(super) fn plan_action(&self, kind: CiActionKind) -> Result<(CiAction, String), Notice> {
        if let Some(busy) = self.busy {
            return Err(Notice::error(format!("Wait: {busy}")));
        }
        let pipeline = self
            .pipeline()
            .ok_or_else(|| Notice::info("No pipeline to act on"))?;
        let job = self.selected_job();
        match kind {
            CiActionKind::RetryJob => {
                let job = job.ok_or_else(|| Notice::info("No job selected"))?;
                if job.status.is_active() {
                    return Err(Notice::error(format!("{} is still running", job.name)));
                }
                Ok((CiAction::RetryJob(job.id), format!("job {}", job.name)))
            }
            CiActionKind::CancelJob => {
                if !self.forge.can_cancel_jobs() {
                    return Err(Notice::error(format!(
                        "{} cancels whole runs, not single jobs: press X to cancel the run",
                        self.forge.name()
                    )));
                }
                let job = job.ok_or_else(|| Notice::info("No job selected"))?;
                if !job.status.is_active() {
                    return Err(Notice::error(format!("{} is not running", job.name)));
                }
                Ok((CiAction::CancelJob(job.id), format!("job {}", job.name)))
            }
            CiActionKind::RetryPipeline => Ok((
                CiAction::RetryPipeline(pipeline.id),
                self.describe(CiAction::RetryPipeline(pipeline.id)),
            )),
            CiActionKind::CancelPipeline => {
                let what = self.describe(CiAction::CancelPipeline(pipeline.id));
                if !pipeline.status.is_active() {
                    return Err(Notice::error(format!("The {what} is not running")));
                }
                Ok((CiAction::CancelPipeline(pipeline.id), what))
            }
        }
    }

    pub(super) fn start_action(&mut self, action: CiAction, label: String) -> Option<Command> {
        let project_id = self.pipeline()?.project_id.clone();
        self.busy = Some(match action {
            CiAction::RetryJob(_) | CiAction::RetryPipeline(_) => "retrying…",
            CiAction::CancelJob(_) | CiAction::CancelPipeline(_) => "canceling…",
        });
        Some(Command::RunCiAction {
            project_id,
            action,
            label,
        })
    }
}

/// Keys shared by the merge request's Pipelines section and the pipeline screen.
/// Returns `None` when `action` is not a CI action, so the caller can handle it;
/// `Some(command)` when it was handled here.
pub(super) fn handle_action(
    ci: &mut CiState,
    popup: &mut Option<Popup>,
    action: &Action,
    next_request_id: &mut u64,
    notice: &mut Option<Notice>,
) -> Option<Option<Command>> {
    let command = match action {
        Action::Refresh => Some(ci.reload(next_request_id)),
        Action::OpenInBrowser => ci.browser_url().map(Command::OpenUrl),
        Action::SelectNext => {
            ci.select_next();
            None
        }
        Action::SelectPrevious => {
            ci.select_previous();
            None
        }
        Action::ScrollDown(lines) => {
            ci.scroll_log(true, *lines);
            None
        }
        Action::ScrollUp(lines) => {
            ci.scroll_log(false, *lines);
            None
        }
        Action::OpenJobLog => ci.open_log(next_request_id),
        Action::ScrollToTop => {
            ci.log_to_top();
            None
        }
        Action::ScrollToBottom => {
            ci.set_follow(true);
            None
        }
        Action::ToggleFollow => {
            let follow = ci.log.as_ref().is_some_and(|l| !l.follow);
            ci.set_follow(follow);
            None
        }
        Action::OlderPipeline | Action::NewerPipeline => {
            ci.switch_pipeline(matches!(action, Action::OlderPipeline), next_request_id)
        }
        Action::Ci(kind) => match ci.plan_action(*kind) {
            Err(message) => {
                *notice = Some(message);
                None
            }
            // Canceling stops work in progress: ask first. Retrying is harmless.
            Ok((action @ (CiAction::CancelJob(_) | CiAction::CancelPipeline(_)), _)) => {
                *popup = Some(Popup::ConfirmCi(action));
                None
            }
            Ok((action, label)) => ci.start_action(action, label),
        },
        Action::Confirm => {
            let Some(Popup::ConfirmCi(action)) = *popup else {
                return None;
            };
            *popup = None;
            let label = ci.describe(action);
            ci.start_action(action, label)
        }
        _ => return None,
    };
    Some(command)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CiActionKind {
    RetryJob,
    CancelJob,
    RetryPipeline,
    CancelPipeline,
}

pub(super) fn success_message(action: CiAction, label: &str) -> String {
    match action {
        CiAction::RetryJob(_) | CiAction::RetryPipeline(_) => format!("Retried {label}"),
        CiAction::CancelJob(_) | CiAction::CancelPipeline(_) => format!("Canceled {label}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::detail::{Popup, Section};
    use crate::app::{Action, AppState, update};
    use crate::forge::{MergeRequest, MrPage, User};
    use chrono::Utc;

    fn user() -> User {
        User {
            username: "thang".into(),
            name: "Thang".into(),
        }
    }

    fn pipeline(id: u64, status: PipelineStatus) -> Pipeline {
        Pipeline {
            id,
            name: None,
            project_id: "99".into(),
            status,
            git_ref: "fix".into(),
            sha: "abc".into(),
            source: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            web_url: format!("https://gitlab.com/p/{id}"),
        }
    }

    fn job(id: u64, name: &str, status: PipelineStatus) -> Job {
        Job {
            id,
            name: name.into(),
            stage: "test".into(),
            status,
            created_at: Utc::now(),
            started_at: None,
            finished_at: None,
            duration: None,
            allow_failure: false,
            web_url: format!("https://gitlab.com/j/{id}"),
        }
    }

    fn snapshot(pipelines: Vec<Pipeline>, shown: u64, jobs: Vec<Job>) -> CiSnapshot {
        CiSnapshot {
            pipelines,
            pipeline_id: Some(shown),
            jobs,
        }
    }

    fn request_id(command: Option<Command>) -> u64 {
        match command {
            Some(Command::FetchMergeRequests { request_id, .. })
            | Some(Command::FetchDetail { request_id, .. })
            | Some(Command::FetchCi { request_id, .. })
            | Some(Command::FetchJobLog { request_id, .. }) => request_id,
            other => panic!("expected a fetch, got {other:?}"),
        }
    }

    /// Detail screen open on the Pipelines section, with `ci` loaded.
    fn open(ci: CiSnapshot) -> AppState {
        let mut state = AppState::new("default".into(), "gitlab.com".into(), None);
        let id = request_id(update(&mut state, Action::Refresh));
        let mr = MergeRequest {
            iid: 42,
            title: "Fix".into(),
            draft: false,
            author: user(),
            project_path: "group/api".into(),
            source_branch: "fix".into(),
            target_branch: "main".into(),
            web_url: String::new(),
            updated_at: Utc::now(),
            pipeline_status: None,
            approved_by: vec![],
        };
        let page = MrPage {
            current_user: user(),
            merge_requests: vec![mr],
        };
        update(
            &mut state,
            Action::MergeRequestsLoaded {
                request_id: id,
                result: Ok(page),
            },
        );
        update(&mut state, Action::OpenDetail);
        for _ in 0..3 {
            update(&mut state, Action::NextSection);
        }
        assert_eq!(state.detail.as_ref().unwrap().section, Section::Pipelines);
        let id = ci_state(&state).request_id;
        assert!(ci_state(&state).loading, "opening the section loads it");
        update(
            &mut state,
            Action::CiLoaded {
                request_id: id,
                result: Ok(ci),
            },
        );
        state
    }

    fn ci_state(state: &AppState) -> &CiState {
        &state.detail.as_ref().unwrap().ci
    }

    fn tick(state: &mut AppState, times: u32) -> Vec<Command> {
        let mut commands = Vec::new();
        for _ in 0..times {
            match update(state, Action::Tick) {
                Some(Command::Batch(batch)) => commands.extend(batch),
                Some(command) => commands.push(command),
                None => {}
            }
        }
        commands
    }

    fn running() -> CiSnapshot {
        snapshot(
            vec![pipeline(10, PipelineStatus::Running)],
            10,
            vec![
                job(1, "lint", PipelineStatus::Success),
                job(2, "unit", PipelineStatus::Running),
            ],
        )
    }

    #[test]
    fn opening_a_log_fetches_it_from_the_pipeline_project() {
        let mut state = open(running());
        update(&mut state, Action::SelectNext);
        let command = update(&mut state, Action::OpenJobLog);
        assert!(matches!(
            &command,
            Some(Command::FetchJobLog { project_id, job_id: 2, .. }) if project_id == "99"
        ));
        let id = request_id(command);
        let raw = "section_start:1:step\r\x1b[0K$ cargo test\nok\n".to_string();
        update(
            &mut state,
            Action::JobLogLoaded {
                request_id: id,
                result: Ok(raw),
            },
        );
        let log = ci_state(&state).log.as_ref().unwrap();
        assert_eq!(log.lines, vec!["$ cargo test", "ok"]);
        assert!(log.follow, "logs open at the bottom");
    }

    #[test]
    fn polls_running_pipelines_and_logs() {
        let mut state = open(running());
        update(&mut state, Action::SelectNext);
        let id = request_id(update(&mut state, Action::OpenJobLog));
        update(
            &mut state,
            Action::JobLogLoaded {
                request_id: id,
                result: Ok("a\n".into()),
            },
        );

        let commands = tick(&mut state, 5);
        let ci_polls = commands
            .iter()
            .filter(|c| matches!(c, Command::FetchCi { .. }))
            .count();
        let log_polls = commands
            .iter()
            .filter(|c| matches!(c, Command::FetchJobLog { .. }))
            .count();
        assert_eq!(ci_polls, 1, "pipeline every 5 s");
        // Ticks 2 and 4 poll the log; the request from tick 2 is still "loading" at tick 4.
        assert_eq!(log_polls, 1);
    }

    #[test]
    fn finished_pipelines_are_not_polled() {
        let done = snapshot(
            vec![pipeline(10, PipelineStatus::Success)],
            10,
            vec![job(1, "lint", PipelineStatus::Success)],
        );
        let mut state = open(done);
        assert!(tick(&mut state, 20).is_empty());
    }

    #[test]
    fn polling_stops_when_leaving_the_section() {
        let mut state = open(running());
        update(&mut state, Action::NextSection);
        assert!(tick(&mut state, 10).is_empty());
    }

    #[test]
    fn a_retried_job_keeps_its_log_open() {
        let failed = snapshot(
            vec![pipeline(10, PipelineStatus::Failed)],
            10,
            vec![
                job(1, "lint", PipelineStatus::Success),
                job(2, "unit", PipelineStatus::Failed),
            ],
        );
        let mut state = open(failed);
        update(&mut state, Action::SelectNext);
        let id = request_id(update(&mut state, Action::OpenJobLog));
        update(
            &mut state,
            Action::JobLogLoaded {
                request_id: id,
                result: Ok("boom\n".into()),
            },
        );

        let command = update(&mut state, Action::Ci(CiActionKind::RetryJob));
        assert!(matches!(
            command,
            Some(Command::RunCiAction { project_id, action: CiAction::RetryJob(2), .. }) if project_id == "99"
        ));
        let finished = Action::CiActionFinished {
            action: CiAction::RetryJob(2),
            label: "job unit".into(),
            result: Ok(()),
        };
        let id = request_id(update(&mut state, finished));
        assert_eq!(state.notice.as_ref().unwrap().text, "Retried job unit");

        // GitLab replaced job 2 with job 3.
        let retried = snapshot(
            vec![pipeline(10, PipelineStatus::Running)],
            10,
            vec![
                job(1, "lint", PipelineStatus::Success),
                job(3, "unit", PipelineStatus::Pending),
            ],
        );
        let command = update(
            &mut state,
            Action::CiLoaded {
                request_id: id,
                result: Ok(retried),
            },
        );
        assert!(matches!(
            command,
            Some(Command::FetchJobLog { job_id: 3, .. })
        ));
        assert_eq!(ci_state(&state).selected_job().unwrap().id, 3);
    }

    #[test]
    fn cancel_asks_first_and_checks_the_state() {
        let mut state = open(running());
        // `lint` has finished: nothing to cancel.
        assert!(update(&mut state, Action::Ci(CiActionKind::CancelJob)).is_none());
        assert!(state.notice.as_ref().unwrap().is_error);

        update(&mut state, Action::SelectNext);
        // `unit` is running: cannot be retried yet, but can be canceled after confirming.
        assert!(update(&mut state, Action::Ci(CiActionKind::RetryJob)).is_none());
        assert!(update(&mut state, Action::Ci(CiActionKind::CancelJob)).is_none());
        let popup = state.detail.as_ref().unwrap().popup;
        assert_eq!(popup, Some(Popup::ConfirmCi(CiAction::CancelJob(2))));
        assert!(matches!(
            update(&mut state, Action::Confirm),
            Some(Command::RunCiAction {
                action: CiAction::CancelJob(2),
                ..
            })
        ));
        // A second action waits for the first.
        assert!(update(&mut state, Action::Ci(CiActionKind::RetryPipeline)).is_none());
    }

    #[test]
    fn scrolling_up_stops_following() {
        let mut state = open(running());
        let id = request_id(update(&mut state, Action::OpenJobLog));
        let long: String = (0..100).map(|i| format!("line {i}\n")).collect();
        update(
            &mut state,
            Action::JobLogLoaded {
                request_id: id,
                result: Ok(long),
            },
        );

        // Following: the UI reports 80 as the bottom, and the scroll sticks to it.
        state.clamp_detail_scroll(80);
        update(&mut state, Action::ScrollUp(5));
        let log = ci_state(&state).log.as_ref().unwrap();
        assert!(!log.follow);
        assert_eq!(log.scroll, 75);

        update(&mut state, Action::ScrollToBottom);
        assert!(ci_state(&state).log.as_ref().unwrap().follow);
    }

    #[test]
    fn github_logs_wait_for_the_job_to_finish() {
        let mut state = open(running());
        state.detail.as_mut().unwrap().ci.forge = ForgeKind::GitHub;
        update(&mut state, Action::SelectNext);
        // `unit` is running: no request, and no polling either.
        assert!(update(&mut state, Action::OpenJobLog).is_none());
        assert!(
            tick(&mut state, 4)
                .iter()
                .all(|c| !matches!(c, Command::FetchJobLog { .. }))
        );

        // The next reload shows the job finished: now the log is fetched.
        let id = ci_state(&state).request_id;
        let mut finished = running();
        finished.jobs[1].status = PipelineStatus::Failed;
        let command = update(
            &mut state,
            Action::CiLoaded {
                request_id: id,
                result: Ok(finished),
            },
        );
        assert!(matches!(
            command,
            Some(Command::FetchJobLog { job_id: 2, .. })
        ));
    }

    #[test]
    fn github_cannot_cancel_single_jobs() {
        let mut state = open(running());
        state.detail.as_mut().unwrap().ci.forge = ForgeKind::GitHub;
        update(&mut state, Action::SelectNext);
        assert!(update(&mut state, Action::Ci(CiActionKind::CancelJob)).is_none());
        assert!(state.notice.as_ref().unwrap().text.contains("press X"));
        assert_eq!(state.detail.as_ref().unwrap().popup, None);
    }

    #[test]
    fn switches_between_pipelines() {
        let ci = snapshot(
            vec![
                pipeline(30, PipelineStatus::Running),
                pipeline(20, PipelineStatus::Failed),
            ],
            30,
            vec![job(1, "lint", PipelineStatus::Running)],
        );
        let mut state = open(ci);
        assert!(
            update(&mut state, Action::NewerPipeline).is_none(),
            "already the newest"
        );
        assert!(matches!(
            update(&mut state, Action::OlderPipeline),
            Some(Command::FetchCi {
                pipeline_id: Some(20),
                ..
            })
        ));
    }
}
