//! Application state and the reducer, in the Elm (TEA) style:
//!
//! - `AppState` is the whole state of the app.
//! - `Action` is everything that can happen (a key press, a network response, a timer).
//! - `update` is the only place that changes the state. It never does I/O: when it needs
//!   something done, it returns a `Command` and the runtime runs it in the background.
//!
//! That keeps this module free of terminal and network code, so it is easy to unit test.

pub mod branches;
pub mod checkout;
pub mod ci;
pub mod detail;
pub mod keymap;
pub mod picker;
pub mod pipelines;

use crate::config::Account;
use crate::forge::Repository;
use crate::forge::{
    ApiError, BranchPage, CiAction, CiSnapshot, Comparison, ForgeKind, MergeRequest, MrAction,
    MrDetail, MrList, MrPage, PipelineList, PipelinePage, ProjectId,
};
use crate::git::{CheckoutRequest, LocalRepo};
use crate::selection::Selection;
use branches::{BranchList, BranchScreen};
use checkout::PendingCheckout;
use ci::{CiActionKind, CiSource, CiState};
use detail::{DetailState, Popup, Section};
use picker::{ConnectedAccount, Picker, PickerStep};
use pipelines::{PipelineScreen, PipelineTab};

/// The top-level views of the dashboard, switched with `1`, `2` and `3`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    MergeRequests,
    Pipelines,
    /// Branches of the chosen repository.
    Branches,
}

/// One tab of the dashboard: a list of merge requests and its loading state.
/// Each tab keeps its data, so switching back to a tab is instant (a light in-memory cache).
#[derive(Debug)]
pub struct MrTab {
    pub list: MrList,
    pub merge_requests: Vec<MergeRequest>,
    /// Position in the *visible* (filtered) list.
    pub selected: usize,
    pub loading: bool,
    /// Has at least one response (success or error) arrived?
    pub loaded_once: bool,
    pub error: Option<String>,
    /// Id of the latest request for this tab; older responses are ignored.
    request_id: u64,
}

impl MrTab {
    fn new(list: MrList) -> Self {
        Self {
            list,
            merge_requests: Vec::new(),
            selected: 0,
            loading: false,
            loaded_once: false,
            error: None,
            request_id: 0,
        }
    }
}

#[derive(Debug)]
pub struct AppState {
    /// GitLab or GitHub: decides labels ("merge request" / "pull request") and features.
    pub forge: ForgeKind,
    pub profile_name: String,
    pub host: String,
    /// Known after the first successful response.
    pub username: Option<String>,
    pub tabs: Vec<MrTab>,
    pub active_tab: usize,
    /// Text typed after `/`. Applies to every tab.
    pub filter: String,
    pub editing_filter: bool,
    /// One-off message (for example "Approved !42"), cleared on the next key.
    pub notice: Option<Notice>,
    pub view: View,
    /// The repository chosen in the picker; `None` means all my repositories.
    pub repo: Option<String>,
    pub pipeline_tabs: Vec<PipelineTab>,
    pub active_pipeline_tab: usize,
    /// The merge request detail screen, when open. The list stays underneath.
    pub detail: Option<DetailState>,
    /// A pipeline opened from the Pipelines view.
    pub pipeline_screen: Option<PipelineScreen>,
    /// Branches of `repo`, loaded when the Branches view is first shown.
    pub branches: BranchList,
    /// A branch compared with the default branch, opened from the Branches view.
    pub branch_screen: Option<BranchScreen>,
    /// Profiles of the config file, for the account picker.
    pub accounts: Vec<Account>,
    /// The git repository lazyforge was started in, for `C` (checkout) and `e` (editor).
    pub local_repo: Option<LocalRepo>,
    /// A checkout waiting for confirmation.
    pub pending_checkout: Option<PendingCheckout>,
    pub checking_out: bool,
    /// The platform → account → repository picker, drawn over everything else.
    pub picker: Option<Picker>,
    /// Set after an action changed something; the list reloads when we go back to it.
    list_stale: bool,
    pub should_quit: bool,
    next_request_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub text: String,
    pub is_error: bool,
}

impl Notice {
    pub fn info(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: false,
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: true,
        }
    }
}

/// Which set of key bindings applies right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyContext {
    List,
    Filter,
    Detail,
    /// The Pipelines section of the detail screen, which adds job and log keys.
    DetailPipelines,
    CommentEditor,
    /// A yes/no popup (merge, cancel a job or pipeline).
    Confirm,
    /// The picker's platform and account steps.
    Picker,
    /// The picker's repository step, where typing filters the list.
    PickerSearch,
    /// A branch compared with the default branch: read-only, files and diffs.
    Branch,
    /// The "compare with…" popup, where typing filters the branches.
    BranchBase,
}

#[derive(Debug)]
pub enum Action {
    Quit,
    /// `Esc`: clears the filter if there is one, otherwise quits.
    Back,
    SelectNext,
    SelectPrevious,
    SelectFirst,
    SelectLast,
    NextTab,
    PreviousTab,
    Refresh,
    /// From the timer. Unlike `Refresh`, does nothing if a request is already running.
    AutoRefresh,
    OpenInBrowser,
    /// `s`: open the platform → account → repository picker.
    OpenPicker,
    PickerSelect,
    PickerInput(char),
    PickerBackspace,
    /// `1` / `2` / `3`: switch between the merge request, pipeline and branch views.
    ShowMergeRequests,
    ShowPipelines,
    ShowBranches,
    StartFilter,
    FilterInput(char),
    FilterBackspace,
    ApplyFilter,
    ClearFilter,
    OpenDetail,
    CloseDetail,
    NextSection,
    PreviousSection,
    /// `Enter` / `l` / `→` on a file list: move the focus to the diff, so `j`/`k` scroll it.
    FocusContent,
    /// `h` / `←`: back to the file list.
    FocusList,
    /// A click on a file in a file list.
    SelectFile(usize),
    /// `b` on a branch: choose which branch to compare it with.
    PickBase,
    /// `C`: check out the merge request or branch on screen into the local repository.
    Checkout,
    /// `e`: open the local repository in the editor.
    OpenEditor,
    /// Message for the user, or why the checkout stopped.
    CheckoutFinished(Result<String, String>),
    /// The editor exited (or could not start).
    EditorClosed(Result<(), String>),
    ScrollDown(u16),
    ScrollUp(u16),
    ToggleApprove,
    StartComment,
    CommentInput(char),
    CommentNewline,
    CommentBackspace,
    SubmitComment,
    StartMerge,
    /// `y` in a confirmation popup.
    Confirm,
    ClosePopup,
    OpenJobLog,
    ScrollToTop,
    /// Jump to the end of the log and follow it.
    ScrollToBottom,
    ToggleFollow,
    Ci(CiActionKind),
    OlderPipeline,
    NewerPipeline,
    /// Every second, from the runtime. Drives polling of running pipelines and logs.
    Tick,
    MergeRequestsLoaded {
        request_id: u64,
        result: Result<MrPage, ApiError>,
    },
    DetailLoaded {
        request_id: u64,
        /// Boxed: `MrDetail` is large, and every `Action` is as big as its largest variant.
        result: Result<Box<MrDetail>, ApiError>,
    },
    MrActionFinished {
        project_path: String,
        iid: u64,
        action: MrAction,
        result: Result<(), ApiError>,
    },
    PipelinesLoaded {
        request_id: u64,
        result: Result<PipelinePage, ApiError>,
    },
    /// The runtime connected to the account chosen in the picker (or could not).
    AccountConnected(Result<ConnectedAccount, String>),
    RepositoriesLoaded {
        request_id: u64,
        result: Result<Vec<Repository>, ApiError>,
    },
    CiLoaded {
        request_id: u64,
        result: Result<CiSnapshot, ApiError>,
    },
    JobLogLoaded {
        request_id: u64,
        result: Result<String, ApiError>,
    },
    CiActionFinished {
        action: CiAction,
        /// For example "job lint", used in the notice.
        label: String,
        result: Result<(), ApiError>,
    },
    BranchesLoaded {
        request_id: u64,
        result: Result<BranchPage, ApiError>,
    },
    ComparisonLoaded {
        request_id: u64,
        /// Boxed, like `DetailLoaded`: a comparison holds every changed file.
        result: Result<Box<Comparison>, ApiError>,
    },
    BrowserFailed(String),
}

impl Action {
    /// Actions that come from the user (as opposed to the network or the timer).
    fn is_user_input(&self) -> bool {
        !matches!(
            self,
            Self::AutoRefresh
                | Self::Tick
                | Self::MergeRequestsLoaded { .. }
                | Self::PipelinesLoaded { .. }
                | Self::AccountConnected(_)
                | Self::RepositoriesLoaded { .. }
                | Self::DetailLoaded { .. }
                | Self::MrActionFinished { .. }
                | Self::CiLoaded { .. }
                | Self::JobLogLoaded { .. }
                | Self::CiActionFinished { .. }
                | Self::BranchesLoaded { .. }
                | Self::ComparisonLoaded { .. }
                | Self::CheckoutFinished(_)
                | Self::EditorClosed(_)
                | Self::BrowserFailed(_)
        )
    }
}

/// Side effects requested by `update`, executed by the runtime.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    FetchMergeRequests {
        request_id: u64,
        list: MrList,
    },
    FetchDetail {
        request_id: u64,
        project_path: String,
        iid: u64,
    },
    RunMrAction {
        project_path: String,
        iid: u64,
        action: MrAction,
    },
    FetchPipelines {
        request_id: u64,
        list: PipelineList,
    },
    FetchCi {
        request_id: u64,
        source: CiSource,
        /// For a merge request, which of its pipelines; `None` loads the newest.
        pipeline_id: Option<u64>,
    },
    FetchJobLog {
        request_id: u64,
        project_id: ProjectId,
        job_id: u64,
    },
    RunCiAction {
        project_id: ProjectId,
        action: CiAction,
        label: String,
    },
    /// Build a client for this profile and use it from now on.
    SwitchAccount {
        profile: String,
    },
    FetchRepositories {
        request_id: u64,
    },
    FetchBranches {
        request_id: u64,
        project: String,
    },
    FetchComparison {
        request_id: u64,
        project: String,
        base: String,
        head: String,
    },
    /// Remember the account and repository for the next start.
    SaveSelection(Selection),
    /// Run the git commands of a checkout (in the background: they reach the network).
    Checkout(CheckoutRequest),
    /// Suspend the TUI and open this directory in `$VISUAL` / `$EDITOR`.
    OpenEditor(std::path::PathBuf),
    OpenUrl(String),
    /// Several commands at once, like `Cmd.batch` in Elm.
    Batch(Vec<Command>),
}

impl Command {
    /// `None` for no commands, the command itself for one, `Batch` for more.
    pub fn batch(mut commands: Vec<Command>) -> Option<Command> {
        match commands.len() {
            0 => None,
            1 => commands.pop(),
            _ => Some(Command::Batch(commands)),
        }
    }
}

impl AppState {
    /// `project` adds a "this project" tab, usually detected from `git remote`.
    pub fn new(profile_name: String, host: String, project: Option<String>) -> Self {
        let (lists, pipeline_lists) = dashboard_lists(project.clone());
        Self {
            forge: ForgeKind::GitLab,
            profile_name,
            host,
            username: None,
            tabs: lists.into_iter().map(MrTab::new).collect(),
            active_tab: 0,
            filter: String::new(),
            editing_filter: false,
            notice: None,
            view: View::MergeRequests,
            repo: project,
            pipeline_tabs: pipeline_lists.into_iter().map(PipelineTab::new).collect(),
            active_pipeline_tab: 0,
            detail: None,
            pipeline_screen: None,
            branches: BranchList::default(),
            branch_screen: None,
            accounts: Vec::new(),
            local_repo: None,
            pending_checkout: None,
            checking_out: false,
            picker: None,
            list_stale: false,
            should_quit: false,
            next_request_id: 0,
        }
    }

    /// Rebuilds the dashboard for a repository (its tabs first and active) or for all
    /// repositories, closing any open screen. Used when switching accounts or repositories.
    pub(crate) fn reset_dashboard(&mut self, repo: Option<String>) {
        let (lists, pipeline_lists) = dashboard_lists(repo.clone());
        self.repo = repo;
        self.tabs = lists.into_iter().map(MrTab::new).collect();
        self.active_tab = 0;
        self.pipeline_tabs = pipeline_lists.into_iter().map(PipelineTab::new).collect();
        self.active_pipeline_tab = 0;
        self.view = View::MergeRequests;
        self.filter.clear();
        self.editing_filter = false;
        self.detail = None;
        self.pipeline_screen = None;
        self.branches = BranchList::default();
        self.branch_screen = None;
        self.list_stale = false;
    }

    pub fn key_context(&self) -> KeyContext {
        if let Some(picker) = &self.picker {
            return match picker.step {
                PickerStep::Repository => KeyContext::PickerSearch,
                _ => KeyContext::Picker,
            };
        }
        if self.pending_checkout.is_some() {
            return KeyContext::Confirm;
        }
        if let Some(screen) = &self.branch_screen {
            return match screen.base_picker {
                Some(_) => KeyContext::BranchBase,
                None => KeyContext::Branch,
            };
        }
        if let Some(screen) = &self.pipeline_screen {
            return match screen.popup {
                Some(_) => KeyContext::Confirm,
                None => KeyContext::DetailPipelines,
            };
        }
        match &self.detail {
            Some(detail) => match detail.popup {
                Some(Popup::Comment) => KeyContext::CommentEditor,
                Some(Popup::ConfirmMerge | Popup::ConfirmCi(_)) => KeyContext::Confirm,
                None if detail.section == Section::Pipelines => KeyContext::DetailPipelines,
                None => KeyContext::Detail,
            },
            None if self.editing_filter => KeyContext::Filter,
            None => KeyContext::List,
        }
    }

    /// Called by the runtime after drawing, with the largest useful scroll offset for the
    /// detail pane. Only the UI knows it, because it depends on line wrapping and the
    /// terminal size.
    pub fn clamp_detail_scroll(&mut self, max: usize) {
        if let Some(screen) = &mut self.pipeline_screen {
            screen.ci.clamp_log_scroll(max);
        }
        if let Some(screen) = &mut self.branch_screen {
            screen.scroll = screen.scroll.min(u16::try_from(max).unwrap_or(u16::MAX));
        }
        let Some(detail) = &mut self.detail else {
            return;
        };
        if detail.section == Section::Pipelines {
            detail.ci.clamp_log_scroll(max);
        } else {
            let max = u16::try_from(max).unwrap_or(u16::MAX);
            detail.scroll = detail.scroll.min(max);
        }
    }

    pub fn active(&self) -> &MrTab {
        &self.tabs[self.active_tab]
    }

    fn active_mut(&mut self) -> &mut MrTab {
        &mut self.tabs[self.active_tab]
    }

    /// Merge requests of the active tab that match the filter.
    pub fn visible(&self) -> Vec<&MergeRequest> {
        let needle = self.filter.to_lowercase();
        self.active()
            .merge_requests
            .iter()
            .filter(|mr| matches_filter(mr, &needle))
            .collect()
    }

    pub fn selected_merge_request(&self) -> Option<&MergeRequest> {
        self.visible().get(self.active().selected).copied()
    }

    fn fetch_active(&mut self) -> Command {
        self.next_request_id += 1;
        let request_id = self.next_request_id;
        let tab = self.active_mut();
        tab.request_id = request_id;
        tab.loading = true;
        Command::FetchMergeRequests {
            request_id,
            list: tab.list.clone(),
        }
    }

    /// Called after switching tabs: loads the tab the first time it is shown.
    fn on_tab_changed(&mut self) -> Option<Command> {
        self.clamp_selection();
        let tab = self.active();
        (!tab.loaded_once && !tab.loading).then(|| self.fetch_active())
    }

    fn clamp_selection(&mut self) {
        let len = self.visible().len();
        let tab = self.active_mut();
        tab.selected = tab.selected.min(len.saturating_sub(1));
        self.clamp_pipeline_selection();
        self.clamp_branch_selection();
    }

    /// The filter changed: start again from the first match, in both views.
    fn reset_selection(&mut self) {
        self.active_mut().selected = 0;
        self.reset_pipeline_selection();
        self.branches.selected = 0;
    }
}

/// The tabs of the dashboard. With a repository, its own tab comes first.
fn dashboard_lists(repo: Option<String>) -> (Vec<MrList>, Vec<PipelineList>) {
    let mut lists = Vec::new();
    let mut pipeline_lists = Vec::new();
    if let Some(repo) = repo {
        lists.push(MrList::Project(repo.clone()));
        pipeline_lists.push(PipelineList::Project(repo));
    }
    lists.extend([
        MrList::AssignedToMe,
        MrList::ReviewRequested,
        MrList::AuthoredByMe,
    ]);
    pipeline_lists.extend([PipelineList::Mine, PipelineList::Recent]);
    (lists, pipeline_lists)
}

/// The CI state of the screen that is open, if any. A free function over the two fields
/// (rather than a `&mut self` method) so callers can still use other fields of `AppState`,
/// like `next_request_id`, while holding the result.
fn open_ci<'a>(
    detail: &'a mut Option<DetailState>,
    screen: &'a mut Option<PipelineScreen>,
) -> Option<&'a mut CiState> {
    match (detail, screen) {
        (Some(detail), _) => Some(&mut detail.ci),
        (None, Some(screen)) => Some(&mut screen.ci),
        (None, None) => None,
    }
}

/// Case-insensitive match on title, author, project, source or target branch and `!iid`.
/// `needle` must already be lowercase.
fn matches_filter(mr: &MergeRequest, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    [
        mr.title.as_str(),
        mr.author.username.as_str(),
        mr.project_path.as_str(),
        mr.source_branch.as_str(),
        mr.target_branch.as_str(),
    ]
    .iter()
    .any(|field| field.to_lowercase().contains(needle))
        // `!42` (GitLab) or `#42` (GitHub).
        || format!("!{}", mr.iid).contains(needle)
        || format!("#{}", mr.iid).contains(needle)
}

pub fn update(state: &mut AppState, action: Action) -> Option<Command> {
    if action.is_user_input() {
        state.notice = None;
    }
    match action {
        Action::Quit => state.should_quit = true,
        // Results and timers are handled the same way whatever screen is open.
        Action::AutoRefresh
        | Action::Tick
        | Action::MergeRequestsLoaded { .. }
        | Action::PipelinesLoaded { .. }
        | Action::AccountConnected(_)
        | Action::RepositoriesLoaded { .. }
        | Action::DetailLoaded { .. }
        | Action::MrActionFinished { .. }
        | Action::CiLoaded { .. }
        | Action::JobLogLoaded { .. }
        | Action::CiActionFinished { .. }
        | Action::BranchesLoaded { .. }
        | Action::ComparisonLoaded { .. }
        | Action::BrowserFailed(_) => return handle_event(state, action),
        Action::CheckoutFinished(result) => checkout::on_finished(state, result),
        Action::EditorClosed(Err(message)) => state.notice = Some(Notice::error(message)),
        Action::EditorClosed(Ok(())) => {}
        _ if state.picker.is_some() => return picker::update(state, action),
        _ if state.pending_checkout.is_some() => return checkout::update_confirm(state, action),
        Action::Checkout => return checkout::start(state),
        Action::OpenEditor => return checkout::open_editor(state),
        _ if state.detail.is_some() => return detail::update(state, action),
        _ if state.pipeline_screen.is_some() => return pipelines::update_screen(state, action),
        _ if state.branch_screen.is_some() => return branches::update_screen(state, action),
        Action::OpenPicker => state.picker = Some(Picker::new(state.forge, true)),
        Action::ShowMergeRequests => state.view = View::MergeRequests,
        Action::ShowPipelines => {
            state.view = View::Pipelines;
            return state.ensure_pipelines_loaded();
        }
        Action::ShowBranches => {
            state.view = View::Branches;
            return state.ensure_branches_loaded();
        }
        _ if state.view == View::Pipelines => {
            if let Some(command) = pipelines::update_list(state, &action) {
                return command;
            }
            // Filter keys and `Esc` work the same in every view.
            return update_list(state, action);
        }
        _ if state.view == View::Branches => {
            if let Some(command) = branches::update_list(state, &action) {
                return command;
            }
            return update_list(state, action);
        }
        _ => return update_list(state, action),
    }
    None
}

/// Key actions on the merge request list.
fn update_list(state: &mut AppState, action: Action) -> Option<Command> {
    match action {
        Action::OpenDetail => {
            let mut detail = DetailState::new(state.selected_merge_request()?, state.forge);
            let command = detail.fetch(&mut state.next_request_id);
            state.detail = Some(detail);
            return Some(command);
        }
        // Esc never quits (only `q` does), so pressing it once too often is harmless.
        Action::Back => {
            if !state.filter.is_empty() {
                state.filter.clear();
                state.clamp_selection();
            }
        }
        Action::SelectNext => {
            let len = state.visible().len();
            let tab = state.active_mut();
            if tab.selected + 1 < len {
                tab.selected += 1;
            }
        }
        Action::SelectPrevious => {
            let tab = state.active_mut();
            tab.selected = tab.selected.saturating_sub(1);
        }
        Action::SelectFirst => state.active_mut().selected = 0,
        Action::SelectLast => {
            let len = state.visible().len();
            state.active_mut().selected = len.saturating_sub(1);
        }
        Action::NextTab => {
            state.active_tab = (state.active_tab + 1) % state.tabs.len();
            return state.on_tab_changed();
        }
        Action::PreviousTab => {
            state.active_tab = (state.active_tab + state.tabs.len() - 1) % state.tabs.len();
            return state.on_tab_changed();
        }
        Action::Refresh => return Some(state.fetch_active()),
        Action::OpenInBrowser => {
            return state
                .selected_merge_request()
                .map(|mr| Command::OpenUrl(mr.web_url.clone()));
        }
        Action::StartFilter => state.editing_filter = true,
        Action::FilterInput(c) => {
            state.filter.push(c);
            state.reset_selection();
        }
        Action::FilterBackspace => {
            state.filter.pop();
            state.reset_selection();
        }
        Action::ApplyFilter => state.editing_filter = false,
        Action::ClearFilter => {
            state.filter.clear();
            state.editing_filter = false;
            state.clamp_selection();
        }
        _ => {}
    }
    None
}

/// Network results, the refresh timer and other events that are not key presses.
fn handle_event(state: &mut AppState, action: Action) -> Option<Command> {
    match action {
        Action::AutoRefresh => match state.view {
            View::MergeRequests => {
                let tab = state.active();
                if tab.loaded_once && !tab.loading {
                    return Some(state.fetch_active());
                }
            }
            View::Pipelines => {
                let tab = state.active_pipeline_tab();
                if tab.loaded_once && !tab.loading {
                    return Some(state.fetch_active_pipelines());
                }
            }
            View::Branches => {
                if state.branches.loaded_once && !state.branches.loading {
                    return state.fetch_branches();
                }
            }
        },
        Action::BranchesLoaded { request_id, result } => {
            branches::on_branches_loaded(state, request_id, result);
        }
        Action::ComparisonLoaded { request_id, result } => {
            branches::on_comparison_loaded(state, request_id, result);
        }
        Action::PipelinesLoaded { request_id, result } => {
            pipelines::on_loaded(state, request_id, result);
        }
        Action::AccountConnected(result) => return picker::on_connected(state, result),
        Action::RepositoriesLoaded { request_id, result } => {
            picker::on_repositories(state, request_id, result);
        }
        Action::MergeRequestsLoaded { request_id, result } => {
            // Find the tab waiting for this response. None means a newer request replaced it.
            let Some(tab) = state.tabs.iter_mut().find(|t| t.request_id == request_id) else {
                tracing::debug!(request_id, "ignoring stale response");
                return None;
            };
            tab.loading = false;
            tab.loaded_once = true;
            match result {
                Ok(page) => {
                    tab.merge_requests = page.merge_requests;
                    tab.error = None;
                    // `tab` borrows `state.tabs`, and this touches `state.username`: two
                    // different fields, so the borrow checker allows both at once.
                    state.username = Some(page.current_user.username);
                }
                // Keep the old list on screen; only show the error.
                Err(err) => {
                    tracing::warn!(error = %err, list = ?tab.list, "failed to load merge requests");
                    tab.error = Some(err.to_string());
                }
            }
            state.clamp_selection();
        }
        Action::DetailLoaded { request_id, result } => {
            let Some(detail) = state.detail.as_mut().filter(|d| d.request_id == request_id) else {
                tracing::debug!(request_id, "ignoring stale detail response");
                return None;
            };
            detail.loading = false;
            match result {
                Ok(loaded) => {
                    detail.title = loaded.title.clone();
                    detail.selected_file = detail
                        .selected_file
                        .min(loaded.files.len().saturating_sub(1));
                    detail.detail = Some(*loaded);
                    detail.error = None;
                }
                Err(err) => {
                    tracing::warn!(error = %err, "failed to load merge request");
                    detail.error = Some(err.to_string());
                }
            }
        }
        Action::MrActionFinished {
            project_path,
            iid,
            action,
            result,
        } => {
            // The user may have left (or opened another merge request) in the meantime.
            let detail = state
                .detail
                .as_mut()
                .filter(|d| d.project_path == project_path && d.iid == iid);
            match result {
                Ok(()) => {
                    state.list_stale = true;
                    let reference = format!("{}{iid}", state.forge.reference_prefix());
                    state.notice = Some(Notice::info(detail::success_message(&action, &reference)));
                    if let Some(detail) = detail {
                        detail.busy = None;
                        if matches!(action, MrAction::Comment(_)) {
                            detail.draft.clear();
                        }
                        return Some(detail.fetch(&mut state.next_request_id));
                    }
                }
                Err(err) => {
                    tracing::warn!(error = %err, ?action, "merge request action failed");
                    if let Some(detail) = detail {
                        detail.busy = None;
                    }
                    state.notice = Some(Notice::error(err.to_string()));
                }
            }
        }
        Action::Tick => {
            // Only poll what is on screen.
            let visible = state.pipeline_screen.is_some()
                || state
                    .detail
                    .as_ref()
                    .is_some_and(|d| d.section == Section::Pipelines);
            if visible {
                let ci = open_ci(&mut state.detail, &mut state.pipeline_screen)?;
                return ci.tick(&mut state.next_request_id);
            }
        }
        Action::CiLoaded { request_id, result } => {
            let ci = open_ci(&mut state.detail, &mut state.pipeline_screen)?;
            return ci.on_loaded(request_id, result, &mut state.next_request_id);
        }
        Action::JobLogLoaded { request_id, result } => {
            open_ci(&mut state.detail, &mut state.pipeline_screen)?
                .on_log_loaded(request_id, result);
        }
        Action::CiActionFinished {
            action,
            label,
            result,
        } => {
            match result {
                Ok(()) => state.notice = Some(Notice::info(ci::success_message(action, &label))),
                Err(err) => {
                    tracing::warn!(error = %err, ?action, "CI action failed");
                    state.notice = Some(Notice::error(err.to_string()));
                }
            }
            state.list_stale = true;
            let ci = open_ci(&mut state.detail, &mut state.pipeline_screen)?;
            ci.busy = None;
            // Reload to show the new job or status, whether it worked or not.
            return Some(ci.reload(&mut state.next_request_id));
        }
        Action::BrowserFailed(message) => {
            state.notice = Some(Notice::error(format!(
                "could not open the browser: {message}"
            )));
        }
        _ => {}
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::User;
    use chrono::Utc;

    fn mr(iid: u64, title: &str) -> MergeRequest {
        MergeRequest {
            iid,
            title: title.into(),
            draft: false,
            author: User {
                username: "hoang".into(),
                name: "Hoang".into(),
            },
            project_path: "group/api".into(),
            source_branch: "feature".into(),
            target_branch: "main".into(),
            web_url: format!("https://gitlab.com/group/api/-/merge_requests/{iid}"),
            updated_at: Utc::now(),
            pipeline_status: None,
            approved_by: Vec::new(),
        }
    }

    fn page(mrs: Vec<MergeRequest>) -> Result<MrPage, ApiError> {
        Ok(MrPage {
            current_user: User {
                username: "thang".into(),
                name: "Thang".into(),
            },
            merge_requests: mrs,
        })
    }

    fn request_id(cmd: Option<Command>) -> u64 {
        match cmd {
            Some(Command::FetchMergeRequests { request_id, .. }) => request_id,
            other => panic!("expected a fetch, got {other:?}"),
        }
    }

    fn new_state() -> AppState {
        AppState::new("default".into(), "gitlab.com".into(), None)
    }

    /// A state whose first tab has `titles` loaded.
    fn loaded(titles: &[&str]) -> AppState {
        let mut state = new_state();
        let request_id = request_id(update(&mut state, Action::Refresh));
        let mrs = titles
            .iter()
            .enumerate()
            .map(|(i, t)| mr(i as u64 + 1, t))
            .collect();
        update(
            &mut state,
            Action::MergeRequestsLoaded {
                request_id,
                result: page(mrs),
            },
        );
        state
    }

    #[test]
    fn project_tab_comes_first_when_known() {
        assert_eq!(new_state().tabs.len(), 3);
        let state = AppState::new("d".into(), "h".into(), Some("group/api".into()));
        assert_eq!(state.tabs.len(), 4);
        assert_eq!(state.active().list, MrList::Project("group/api".into()));
        assert_eq!(
            state.active_pipeline_tab().list,
            PipelineList::Project("group/api".into())
        );
    }

    #[test]
    fn selection_stays_in_bounds() {
        let mut state = loaded(&["a", "b", "c"]);
        update(&mut state, Action::SelectPrevious);
        assert_eq!(state.active().selected, 0);
        for _ in 0..10 {
            update(&mut state, Action::SelectNext);
        }
        assert_eq!(state.active().selected, 2);
        update(&mut state, Action::SelectFirst);
        update(&mut state, Action::SelectLast);
        assert_eq!(state.selected_merge_request().unwrap().title, "c");
    }

    #[test]
    fn selection_on_empty_list_does_not_panic() {
        let mut state = loaded(&[]);
        update(&mut state, Action::SelectNext);
        update(&mut state, Action::SelectLast);
        assert!(state.selected_merge_request().is_none());
    }

    #[test]
    fn loaded_response_sets_username() {
        let state = loaded(&["a"]);
        assert_eq!(state.username.as_deref(), Some("thang"));
    }

    #[test]
    fn switching_tabs_loads_each_tab_once() {
        let mut state = loaded(&["a"]);
        let cmd = update(&mut state, Action::NextTab);
        assert_eq!(state.active_tab, 1);
        let id = request_id(cmd);
        assert!(matches!(state.active().list, MrList::ReviewRequested));

        update(
            &mut state,
            Action::MergeRequestsLoaded {
                request_id: id,
                result: page(vec![]),
            },
        );
        // Back to the first tab: already loaded, no request.
        assert!(update(&mut state, Action::PreviousTab).is_none());
        // Wraps around from the first tab to the last.
        update(&mut state, Action::PreviousTab);
        assert_eq!(state.active_tab, 2);
    }

    #[test]
    fn responses_go_to_the_tab_that_asked() {
        let mut state = new_state();
        let first = request_id(update(&mut state, Action::Refresh));
        let second = request_id(update(&mut state, Action::NextTab));

        update(
            &mut state,
            Action::MergeRequestsLoaded {
                request_id: first,
                result: page(vec![mr(1, "assigned")]),
            },
        );
        update(
            &mut state,
            Action::MergeRequestsLoaded {
                request_id: second,
                result: page(vec![mr(2, "review")]),
            },
        );

        assert_eq!(state.tabs[0].merge_requests[0].title, "assigned");
        assert_eq!(state.tabs[1].merge_requests[0].title, "review");
    }

    #[test]
    fn stale_responses_are_ignored() {
        let mut state = new_state();
        let old = request_id(update(&mut state, Action::Refresh));
        let new = request_id(update(&mut state, Action::Refresh));

        update(
            &mut state,
            Action::MergeRequestsLoaded {
                request_id: new,
                result: page(vec![mr(2, "new")]),
            },
        );
        update(
            &mut state,
            Action::MergeRequestsLoaded {
                request_id: old,
                result: page(vec![mr(1, "old")]),
            },
        );

        assert_eq!(state.active().merge_requests[0].title, "new");
        assert!(!state.active().loading);
    }

    #[test]
    fn errors_keep_the_previous_list() {
        let mut state = loaded(&["a", "b"]);
        let id = request_id(update(&mut state, Action::Refresh));
        let result = Err(ApiError::Unauthorized);
        update(
            &mut state,
            Action::MergeRequestsLoaded {
                request_id: id,
                result,
            },
        );
        assert_eq!(state.active().merge_requests.len(), 2);
        assert!(state.active().error.as_deref().unwrap().contains("401"));
    }

    #[test]
    fn auto_refresh_skips_tabs_that_are_loading_or_never_loaded() {
        let mut state = new_state();
        assert!(update(&mut state, Action::AutoRefresh).is_none());
        let mut state = loaded(&["a"]);
        assert!(update(&mut state, Action::AutoRefresh).is_some());
        assert!(update(&mut state, Action::AutoRefresh).is_none());
    }

    #[test]
    fn filter_matches_title_author_and_iid_case_insensitively() {
        let mut state = loaded(&["Fix Token race", "Add retry", "Bump deps"]);
        for c in "TOKEN".chars() {
            update(&mut state, Action::FilterInput(c));
        }
        assert_eq!(state.visible().len(), 1);

        update(&mut state, Action::ClearFilter);
        for c in "!3".chars() {
            update(&mut state, Action::FilterInput(c));
        }
        assert_eq!(state.selected_merge_request().unwrap().title, "Bump deps");

        update(&mut state, Action::ClearFilter);
        for c in "hoang".chars() {
            update(&mut state, Action::FilterInput(c));
        }
        assert_eq!(state.visible().len(), 3);
    }

    #[test]
    fn filter_matches_the_target_branch() {
        let mut state = loaded(&["a", "b"]);
        state.tabs[0].merge_requests[1].target_branch = "release/2.4".into();
        for c in "release".chars() {
            update(&mut state, Action::FilterInput(c));
        }
        assert_eq!(state.selected_merge_request().unwrap().title, "b");
        assert_eq!(state.visible().len(), 1);
    }

    #[test]
    fn selection_follows_the_filtered_list() {
        // `y` appears only in the last two titles (not in the shared author/branch/project).
        let mut state = loaded(&["alpha", "beta y", "gamma y"]);
        update(&mut state, Action::SelectLast);
        update(&mut state, Action::FilterInput('y'));
        assert_eq!(state.visible().len(), 2);
        // Selection resets to the first match.
        assert_eq!(state.selected_merge_request().unwrap().title, "beta y");
        update(&mut state, Action::SelectNext);
        update(&mut state, Action::SelectNext);
        assert_eq!(state.selected_merge_request().unwrap().title, "gamma y");
    }

    #[test]
    fn esc_clears_the_filter_but_never_quits() {
        let mut state = loaded(&["a"]);
        update(&mut state, Action::FilterInput('x'));
        update(&mut state, Action::ApplyFilter);
        update(&mut state, Action::Back);
        assert!(state.filter.is_empty());
        assert!(!state.should_quit);
        update(&mut state, Action::Back);
        assert!(!state.should_quit);
    }

    #[test]
    fn open_in_browser_uses_the_selected_merge_request() {
        let mut state = loaded(&["a", "b"]);
        update(&mut state, Action::SelectNext);
        assert_eq!(
            update(&mut state, Action::OpenInBrowser),
            Some(Command::OpenUrl(
                "https://gitlab.com/group/api/-/merge_requests/2".into()
            ))
        );
        assert!(update(&mut new_state(), Action::OpenInBrowser).is_none());
    }

    #[test]
    fn notices_are_cleared_by_the_next_key() {
        let mut state = loaded(&["a"]);
        update(&mut state, Action::BrowserFailed("no browser".into()));
        assert!(state.notice.is_some());
        update(&mut state, Action::SelectNext);
        assert!(state.notice.is_none());
    }
}
