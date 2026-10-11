//! The Pipelines view of the dashboard, and pipelines opened from it.

use super::ci::{self, CiSource, CiState};
use super::detail::Popup;
use super::{Action, AppState, Command};
use crate::forge::{ApiError, PipelineList, PipelinePage, PipelineSummary};

/// One tab of the Pipelines view. Like `MrTab`, each keeps its data between visits.
#[derive(Debug)]
pub struct PipelineTab {
    pub list: PipelineList,
    pub pipelines: Vec<PipelineSummary>,
    /// Position in the *visible* (filtered) list.
    pub selected: usize,
    pub loading: bool,
    pub loaded_once: bool,
    pub error: Option<String>,
    request_id: u64,
}

impl PipelineTab {
    pub(super) fn new(list: PipelineList) -> Self {
        Self {
            list,
            pipelines: Vec::new(),
            selected: 0,
            loading: false,
            loaded_once: false,
            error: None,
            request_id: 0,
        }
    }
}

/// A pipeline opened from the dashboard: its jobs and logs.
#[derive(Debug)]
pub struct PipelineScreen {
    pub summary: PipelineSummary,
    pub ci: CiState,
    /// Only `Popup::ConfirmCi` is used here.
    pub popup: Option<Popup>,
}

impl AppState {
    pub fn active_pipeline_tab(&self) -> &PipelineTab {
        &self.pipeline_tabs[self.active_pipeline_tab]
    }

    fn active_pipeline_tab_mut(&mut self) -> &mut PipelineTab {
        &mut self.pipeline_tabs[self.active_pipeline_tab]
    }

    /// Pipelines of the active tab that match the filter.
    pub fn visible_pipelines(&self) -> Vec<&PipelineSummary> {
        let needle = self.filter.to_lowercase();
        self.active_pipeline_tab()
            .pipelines
            .iter()
            .filter(|p| matches_filter(p, &needle))
            .collect()
    }

    pub fn selected_pipeline(&self) -> Option<&PipelineSummary> {
        self.visible_pipelines()
            .get(self.active_pipeline_tab().selected)
            .copied()
    }

    pub(super) fn fetch_active_pipelines(&mut self) -> Command {
        self.next_request_id += 1;
        let request_id = self.next_request_id;
        let tab = self.active_pipeline_tab_mut();
        tab.request_id = request_id;
        tab.loading = true;
        Command::FetchPipelines {
            request_id,
            list: tab.list.clone(),
        }
    }

    /// Loads the active tab the first time it is shown.
    pub(super) fn ensure_pipelines_loaded(&mut self) -> Option<Command> {
        self.clamp_pipeline_selection();
        let tab = self.active_pipeline_tab();
        (!tab.loaded_once && !tab.loading).then(|| self.fetch_active_pipelines())
    }

    pub(super) fn clamp_pipeline_selection(&mut self) {
        let len = self.visible_pipelines().len();
        let tab = self.active_pipeline_tab_mut();
        tab.selected = tab.selected.min(len.saturating_sub(1));
    }

    pub(super) fn reset_pipeline_selection(&mut self) {
        self.active_pipeline_tab_mut().selected = 0;
    }
}

/// Case-insensitive match on project, ref, who triggered it, status and `#id`.
/// `needle` must already be lowercase.
fn matches_filter(pipeline: &PipelineSummary, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let status = format!("{:?}", pipeline.status).to_lowercase();
    [
        pipeline.project_path.as_str(),
        pipeline.git_ref.as_str(),
        pipeline.user.as_deref().unwrap_or(""),
        status.as_str(),
    ]
    .iter()
    .any(|field| field.to_lowercase().contains(needle))
        || format!("#{}", pipeline.id).contains(needle)
}

/// List keys of the Pipelines view. Returns `None` for keys it does not handle (the filter,
/// `Esc`), which the caller handles the same way as on the merge request list.
pub(super) fn update_list(state: &mut AppState, action: &Action) -> Option<Option<Command>> {
    let command = match action {
        Action::OpenDetail => {
            let summary = state.selected_pipeline()?.clone();
            let mut ci = CiState::new(
                CiSource::Pipeline {
                    project_id: summary.project_id.clone(),
                    pipeline_id: summary.id,
                },
                state.forge,
            );
            let command = ci.fetch(&mut state.next_request_id, Some(summary.id));
            state.pipeline_screen = Some(PipelineScreen {
                summary,
                ci,
                popup: None,
            });
            Some(command)
        }
        Action::SelectNext => {
            let len = state.visible_pipelines().len();
            let tab = state.active_pipeline_tab_mut();
            if tab.selected + 1 < len {
                tab.selected += 1;
            }
            None
        }
        Action::SelectPrevious => {
            let tab = state.active_pipeline_tab_mut();
            tab.selected = tab.selected.saturating_sub(1);
            None
        }
        Action::SelectFirst => {
            state.reset_pipeline_selection();
            None
        }
        Action::SelectLast => {
            let len = state.visible_pipelines().len();
            state.active_pipeline_tab_mut().selected = len.saturating_sub(1);
            None
        }
        Action::NextTab | Action::PreviousTab => {
            let count = state.pipeline_tabs.len();
            state.active_pipeline_tab = if matches!(action, Action::NextTab) {
                (state.active_pipeline_tab + 1) % count
            } else {
                (state.active_pipeline_tab + count - 1) % count
            };
            state.ensure_pipelines_loaded()
        }
        Action::Refresh => Some(state.fetch_active_pipelines()),
        Action::OpenInBrowser => state
            .selected_pipeline()
            .map(|p| Command::OpenUrl(p.web_url.clone())),
        _ => return None,
    };
    Some(command)
}

/// Keys of a pipeline opened from the dashboard.
pub(super) fn update_screen(state: &mut AppState, action: Action) -> Option<Command> {
    let screen = state.pipeline_screen.as_mut()?;
    if let Some(command) = ci::handle_action(
        &mut screen.ci,
        &mut screen.popup,
        &action,
        &mut state.next_request_id,
        &mut state.notice,
    ) {
        return command;
    }
    match action {
        Action::CloseDetail => {
            state.pipeline_screen = None;
            // A retry or cancel changed the pipeline: reload the list.
            if std::mem::take(&mut state.list_stale) {
                return Some(state.fetch_active_pipelines());
            }
        }
        Action::ClosePopup => screen.popup = None,
        _ => {}
    }
    None
}

pub(super) fn on_loaded(
    state: &mut AppState,
    request_id: u64,
    result: Result<PipelinePage, ApiError>,
) {
    let Some(tab) = state
        .pipeline_tabs
        .iter_mut()
        .find(|t| t.request_id == request_id)
    else {
        tracing::debug!(request_id, "ignoring stale pipelines response");
        return;
    };
    tab.loading = false;
    tab.loaded_once = true;
    match result {
        Ok(page) => {
            tab.pipelines = page.pipelines;
            tab.error = None;
            state.username = Some(page.current_user.username);
        }
        Err(err) => {
            tracing::warn!(error = %err, list = ?tab.list, "failed to load pipelines");
            tab.error = Some(err.to_string());
        }
    }
    state.clamp_pipeline_selection();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{KeyContext, View, update};
    use crate::forge::{CiAction, CiSnapshot, Pipeline, PipelineStatus, User};
    use chrono::Utc;

    fn summary(id: u64, project: &str, git_ref: &str, status: PipelineStatus) -> PipelineSummary {
        PipelineSummary {
            id,
            name: None,
            project_id: "7".into(),
            project_path: project.into(),
            status,
            git_ref: git_ref.into(),
            sha: "abc".into(),
            user: Some("thang".into()),
            created_at: Utc::now(),
            finished_at: None,
            duration: None,
            web_url: format!("https://gitlab.com/{project}/-/pipelines/{id}"),
        }
    }

    fn page(pipelines: Vec<PipelineSummary>) -> Result<PipelinePage, ApiError> {
        Ok(PipelinePage {
            current_user: User {
                username: "thang".into(),
                name: "Thang".into(),
            },
            pipelines,
        })
    }

    fn request_id(command: Option<Command>) -> u64 {
        match command {
            Some(Command::FetchPipelines { request_id, .. })
            | Some(Command::FetchCi { request_id, .. }) => request_id,
            other => panic!("expected a fetch, got {other:?}"),
        }
    }

    /// Pipelines view, first tab ("Mine") loaded with `pipelines`.
    fn pipelines_view(pipelines: Vec<PipelineSummary>) -> AppState {
        let mut state = AppState::new("d".into(), "gitlab.com".into(), None);
        let command = update(&mut state, Action::ShowPipelines);
        assert!(matches!(
            command,
            Some(Command::FetchPipelines {
                list: PipelineList::Mine,
                ..
            })
        ));
        let id = request_id(command);
        update(
            &mut state,
            Action::PipelinesLoaded {
                request_id: id,
                result: page(pipelines),
            },
        );
        state
    }

    fn two() -> Vec<PipelineSummary> {
        vec![
            summary(2, "group/api", "main", PipelineStatus::Running),
            summary(1, "group/web", "fix/login", PipelineStatus::Failed),
        ]
    }

    #[test]
    fn switching_views_loads_pipelines_once() {
        let mut state = pipelines_view(two());
        assert_eq!(state.view, View::Pipelines);
        assert_eq!(state.pipeline_tabs.len(), 2, "Mine and Recent");
        update(&mut state, Action::ShowMergeRequests);
        assert_eq!(state.view, View::MergeRequests);
        // Already loaded: no new request.
        assert!(update(&mut state, Action::ShowPipelines).is_none());
    }

    #[test]
    fn list_keys_act_on_the_pipelines_view() {
        let mut state = pipelines_view(two());
        update(&mut state, Action::SelectNext);
        assert_eq!(state.selected_pipeline().unwrap().id, 1);
        assert!(matches!(
            update(&mut state, Action::NextTab),
            Some(Command::FetchPipelines {
                list: PipelineList::Recent,
                ..
            })
        ));
        // The merge request list was not touched.
        assert_eq!(state.active().selected, 0);
    }

    #[test]
    fn filter_applies_to_pipelines() {
        let mut state = pipelines_view(two());
        for c in "web".chars() {
            update(&mut state, Action::FilterInput(c));
        }
        assert_eq!(state.visible_pipelines().len(), 1);
        update(&mut state, Action::ClearFilter);
        for c in "fail".chars() {
            update(&mut state, Action::FilterInput(c));
        }
        assert_eq!(
            state.selected_pipeline().unwrap().id,
            1,
            "status matches too"
        );
    }

    #[test]
    fn opening_a_pipeline_reuses_the_ci_screen() {
        let mut state = pipelines_view(two());
        let command = update(&mut state, Action::OpenDetail);
        assert!(matches!(
            &command,
            Some(Command::FetchCi { source: CiSource::Pipeline { project_id, pipeline_id: 2 }, .. }) if project_id == "7"
        ));
        assert_eq!(state.key_context(), KeyContext::DetailPipelines);

        let snapshot = CiSnapshot {
            pipelines: vec![Pipeline {
                id: 2,
                name: None,
                project_id: "7".into(),
                status: PipelineStatus::Running,
                git_ref: "main".into(),
                sha: "abc".into(),
                source: None,
                created_at: Utc::now(),
                updated_at: Utc::now(),
                web_url: String::new(),
            }],
            pipeline_id: Some(2),
            jobs: vec![],
        };
        let id = request_id(command);
        update(
            &mut state,
            Action::CiLoaded {
                request_id: id,
                result: Ok(snapshot),
            },
        );
        // The pipeline is running: it is polled while the screen is open...
        let polls = (0..5)
            .filter_map(|_| update(&mut state, Action::Tick))
            .count();
        assert_eq!(polls, 1);

        // ...cancel asks first...
        update(
            &mut state,
            Action::Ci(crate::app::ci::CiActionKind::CancelPipeline),
        );
        assert_eq!(state.key_context(), KeyContext::Confirm);
        assert!(matches!(
            update(&mut state, Action::Confirm),
            Some(Command::RunCiAction { project_id, action: CiAction::CancelPipeline(2), .. }) if project_id == "7"
        ));
        let finished = Action::CiActionFinished {
            action: CiAction::CancelPipeline(2),
            label: "pipeline #2".into(),
            result: Ok(()),
        };
        update(&mut state, finished);

        // ...and going back reloads the list, which changed.
        assert!(matches!(
            update(&mut state, Action::CloseDetail),
            Some(Command::FetchPipelines { .. })
        ));
        assert!(state.pipeline_screen.is_none());
    }

    #[test]
    fn esc_on_the_list_clears_the_filter_but_never_quits() {
        let mut state = pipelines_view(two());
        update(&mut state, Action::FilterInput('x'));
        update(&mut state, Action::ApplyFilter);
        update(&mut state, Action::Back);
        assert!(!state.should_quit);
        update(&mut state, Action::Back);
        assert!(!state.should_quit);
    }
}
