//! The Branches view (one repository's branches) and a branch compared with the default
//! branch. Read-only: nothing here changes anything on the server.

use super::{Action, AppState, Command, Notice};
use crate::forge::{ApiError, Branch, BranchPage, Comparison, FileDiff};

/// The branch list of the repository chosen in the picker.
#[derive(Debug, Default)]
pub struct BranchList {
    pub branches: Vec<Branch>,
    pub default_branch: Option<String>,
    /// Position in the *visible* (filtered) list.
    pub selected: usize,
    pub loading: bool,
    pub loaded_once: bool,
    pub error: Option<String>,
    request_id: u64,
}

/// A branch compared with another one (the default branch unless chosen with `b`): its
/// commits and changed files.
#[derive(Debug)]
pub struct BranchScreen {
    pub project: String,
    pub branch: Branch,
    /// The branch compared with.
    pub base: String,
    /// Every branch name of the repository, default first, for choosing `base`.
    pub branch_names: Vec<String>,
    /// The "compare with…" popup, when open.
    pub base_picker: Option<BasePicker>,
    pub comparison: Option<Comparison>,
    pub loading: bool,
    pub error: Option<String>,
    request_id: u64,
    pub selected_file: usize,
    /// Scroll offset of the diff. Reset when the file changes.
    pub scroll: u16,
    /// `j`/`k` move a line cursor in the diff instead of picking a file.
    pub diff_focused: bool,
    pub diff_cursor: usize,
}

/// Choosing the branch to compare with: typing filters, ↑/↓ move, `Enter` picks.
#[derive(Debug, Default)]
pub struct BasePicker {
    pub filter: String,
    /// Position in the filtered list.
    pub selected: usize,
}

impl BranchScreen {
    /// Branches that can be the base: all but the compared branch, matching the filter.
    pub fn base_candidates(&self) -> Vec<&str> {
        let needle = self
            .base_picker
            .as_ref()
            .map(|p| p.filter.to_lowercase())
            .unwrap_or_default();
        self.branch_names
            .iter()
            .map(String::as_str)
            .filter(|name| *name != self.branch.name && name.to_lowercase().contains(&needle))
            .collect()
    }

    /// Opens the popup on the current base.
    fn open_base_picker(&mut self) {
        // Candidates are read through `self.base_picker`, so open it (empty filter) first.
        self.base_picker = Some(BasePicker::default());
        let selected = self
            .base_candidates()
            .iter()
            .position(|name| *name == self.base)
            .unwrap_or(0);
        if let Some(picker) = &mut self.base_picker {
            picker.selected = selected;
        }
    }

    pub fn selected_file(&self) -> Option<&FileDiff> {
        self.comparison.as_ref()?.files.get(self.selected_file)
    }

    fn file_count(&self) -> usize {
        self.comparison.as_ref().map_or(0, |c| c.files.len())
    }

    fn fetch(&mut self, next_request_id: &mut u64) -> Command {
        *next_request_id += 1;
        self.request_id = *next_request_id;
        self.loading = true;
        Command::FetchComparison {
            request_id: self.request_id,
            project: self.project.clone(),
            base: self.base.clone(),
            head: self.branch.name.clone(),
        }
    }
}

impl AppState {
    /// Branches that match the filter.
    pub fn visible_branches(&self) -> Vec<&Branch> {
        let needle = self.filter.to_lowercase();
        self.branches
            .branches
            .iter()
            .filter(|b| matches_filter(b, &needle))
            .collect()
    }

    pub fn selected_branch(&self) -> Option<&Branch> {
        self.visible_branches().get(self.branches.selected).copied()
    }

    /// `None` on "all repositories": branches belong to one repository.
    pub(super) fn fetch_branches(&mut self) -> Option<Command> {
        let project = self.repo.clone()?;
        self.next_request_id += 1;
        self.branches.request_id = self.next_request_id;
        self.branches.loading = true;
        Some(Command::FetchBranches {
            request_id: self.next_request_id,
            project,
        })
    }

    /// Loads the branches the first time the view is shown.
    pub(super) fn ensure_branches_loaded(&mut self) -> Option<Command> {
        self.clamp_branch_selection();
        let list = &self.branches;
        if list.loaded_once || list.loading {
            return None;
        }
        self.fetch_branches()
    }

    pub(super) fn clamp_branch_selection(&mut self) {
        let len = self.visible_branches().len();
        self.branches.selected = self.branches.selected.min(len.saturating_sub(1));
    }
}

/// Case-insensitive match on the branch name, the last commit's title and its author.
fn matches_filter(branch: &Branch, needle: &str) -> bool {
    needle.is_empty()
        || [
            branch.name.as_str(),
            branch.commit_title.as_str(),
            branch.commit_author.as_deref().unwrap_or(""),
        ]
        .iter()
        .any(|field| field.to_lowercase().contains(needle))
}

/// List keys of the Branches view. Returns `None` for keys it does not handle (the filter,
/// `Esc`), which the caller handles the same way as on the merge request list.
pub(super) fn update_list(state: &mut AppState, action: &Action) -> Option<Option<Command>> {
    let command = match action {
        Action::OpenDetail => open_comparison(state, false),
        Action::PickBase => open_comparison(state, true),
        Action::SelectNext => {
            let len = state.visible_branches().len();
            let list = &mut state.branches;
            if list.selected + 1 < len {
                list.selected += 1;
            }
            None
        }
        Action::SelectPrevious => {
            state.branches.selected = state.branches.selected.saturating_sub(1);
            None
        }
        Action::SelectFirst => {
            state.branches.selected = 0;
            None
        }
        Action::SelectLast => {
            let len = state.visible_branches().len();
            state.branches.selected = len.saturating_sub(1);
            None
        }
        // One list, no tabs.
        Action::NextTab | Action::PreviousTab => None,
        Action::Refresh => state.fetch_branches(),
        Action::OpenInBrowser => state
            .selected_branch()
            .map(|b| Command::OpenUrl(b.web_url.clone())),
        _ => return None,
    };
    Some(command)
}

/// `Enter` compares the selected branch with the default branch; `b` (`choose_base`) first
/// asks which branch to compare it with.
fn open_comparison(state: &mut AppState, choose_base: bool) -> Option<Command> {
    let project = state.repo.clone()?;
    let branch = state.selected_branch()?.clone();
    let default = state.branches.default_branch.clone();
    if !choose_base {
        match &default {
            None => {
                state.notice = Some(Notice::error(
                    "This repository has no default branch: press b to pick a branch to compare with",
                ));
                return None;
            }
            Some(base) if *base == branch.name => {
                state.notice = Some(Notice::info(format!(
                    "{base} is the default branch: press b to compare it with another branch"
                )));
                return None;
            }
            Some(_) => {}
        }
    }
    let mut screen = BranchScreen {
        project,
        base: default.unwrap_or_default(),
        branch_names: state
            .branches
            .branches
            .iter()
            .map(|b| b.name.clone())
            .collect(),
        base_picker: None,
        branch,
        comparison: None,
        loading: false,
        error: None,
        request_id: 0,
        selected_file: 0,
        scroll: 0,
        diff_focused: false,
        diff_cursor: 0,
    };
    if choose_base {
        // Nothing to load until a branch is picked.
        screen.open_base_picker();
        state.branch_screen = Some(screen);
        return None;
    }
    let command = screen.fetch(&mut state.next_request_id);
    state.branch_screen = Some(screen);
    Some(command)
}

/// Keys of the "compare with…" popup. Every action stops here while it is open.
fn update_base_picker(state: &mut AppState, action: Action) -> Option<Command> {
    let screen = state.branch_screen.as_mut()?;
    let count = screen.base_candidates().len();
    let picker = screen.base_picker.as_mut()?;
    match action {
        Action::PickerInput(c) => {
            picker.filter.push(c);
            picker.selected = 0;
        }
        Action::PickerBackspace => {
            picker.filter.pop();
            picker.selected = 0;
        }
        Action::SelectNext if picker.selected + 1 < count => picker.selected += 1,
        Action::SelectPrevious => picker.selected = picker.selected.saturating_sub(1),
        Action::ClosePopup => {
            screen.base_picker = None;
            // Opened from the list with `b` and nothing compared yet: back to the list.
            if screen.comparison.is_none() && !screen.loading {
                state.branch_screen = None;
            }
        }
        Action::PickerSelect => {
            let selected = picker.selected;
            let base = screen.base_candidates().get(selected)?.to_string();
            screen.base_picker = None;
            screen.base = base;
            screen.comparison = None;
            screen.error = None;
            screen.selected_file = 0;
            screen.scroll = 0;
            screen.diff_focused = false;
            screen.diff_cursor = 0;
            return Some(screen.fetch(&mut state.next_request_id));
        }
        _ => {}
    }
    None
}

/// Keys of the comparison screen: pick a file, scroll its diff, open it in the browser.
pub(super) fn update_screen(state: &mut AppState, action: Action) -> Option<Command> {
    let screen = state.branch_screen.as_mut()?;
    if screen.base_picker.is_some() {
        return update_base_picker(state, action);
    }
    if matches!(action, Action::PickBase) {
        screen.open_base_picker();
        return None;
    }
    let line_count = screen.selected_file().map_or(0, |f| f.lines.len());
    let files = super::detail::FileBrowser {
        file_count: screen.file_count(),
        line_count,
        selected: &mut screen.selected_file,
        scroll: &mut screen.scroll,
        diff_focused: &mut screen.diff_focused,
        cursor: &mut screen.diff_cursor,
    };
    if super::detail::file_browser_action(&action, files) {
        return None;
    }
    match action {
        Action::CloseDetail => state.branch_screen = None,
        Action::Refresh => return Some(screen.fetch(&mut state.next_request_id)),
        Action::OpenInBrowser => {
            let url = match &screen.comparison {
                Some(comparison) => comparison.web_url.clone(),
                None => screen.branch.web_url.clone(),
            };
            return Some(Command::OpenUrl(url));
        }
        Action::ScrollDown(lines) => screen.scroll = screen.scroll.saturating_add(lines),
        Action::ScrollUp(lines) => screen.scroll = screen.scroll.saturating_sub(lines),
        _ => {}
    }
    None
}

pub(super) fn on_branches_loaded(
    state: &mut AppState,
    request_id: u64,
    result: Result<BranchPage, ApiError>,
) {
    let list = &mut state.branches;
    if list.request_id != request_id {
        tracing::debug!(request_id, "ignoring stale branches response");
        return;
    }
    list.loading = false;
    list.loaded_once = true;
    match result {
        Ok(page) => {
            list.branches = page.branches;
            list.default_branch = page.default_branch;
            list.error = None;
        }
        Err(err) => {
            tracing::warn!(error = %err, "failed to load branches");
            list.error = Some(err.to_string());
        }
    }
    state.clamp_branch_selection();
}

pub(super) fn on_comparison_loaded(
    state: &mut AppState,
    request_id: u64,
    result: Result<Box<Comparison>, ApiError>,
) {
    let Some(screen) = state
        .branch_screen
        .as_mut()
        .filter(|s| s.request_id == request_id)
    else {
        tracing::debug!(request_id, "ignoring stale comparison response");
        return;
    };
    screen.loading = false;
    match result {
        Ok(comparison) => {
            screen.selected_file = screen
                .selected_file
                .min(comparison.files.len().saturating_sub(1));
            screen.comparison = Some(*comparison);
            screen.error = None;
        }
        Err(err) => {
            tracing::warn!(error = %err, "failed to compare branches");
            screen.error = Some(err.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{KeyContext, View, update};
    use crate::forge::diff::parse_unified_diff;

    fn branch(name: &str, default: bool) -> Branch {
        Branch {
            name: name.into(),
            default,
            commit_title: format!("work on {name}"),
            commit_author: Some("linh".into()),
            committed_at: None,
            pipeline_status: None,
            merge_request: None,
            web_url: format!("https://gitlab.com/g/api/-/tree/{name}"),
        }
    }

    fn page() -> Result<BranchPage, ApiError> {
        Ok(BranchPage {
            default_branch: Some("develop".into()),
            branches: vec![
                branch("develop", true),
                branch("feature/export", false),
                branch("fix/login", false),
            ],
        })
    }

    fn request_id(command: Option<Command>) -> u64 {
        match command {
            Some(Command::FetchBranches { request_id, .. })
            | Some(Command::FetchComparison { request_id, .. }) => request_id,
            other => panic!("expected a fetch, got {other:?}"),
        }
    }

    /// Branches view of `g/api`, loaded.
    fn branches_view() -> AppState {
        let mut state = AppState::new("d".into(), "gitlab.com".into(), Some("g/api".into()));
        let command = update(&mut state, Action::ShowBranches);
        assert!(matches!(
            &command,
            Some(Command::FetchBranches { project, .. }) if project == "g/api"
        ));
        let request_id = request_id(command);
        update(
            &mut state,
            Action::BranchesLoaded {
                request_id,
                result: page(),
            },
        );
        state
    }

    fn comparison() -> Comparison {
        let file = |path: &str| FileDiff {
            old_path: path.into(),
            new_path: path.into(),
            new_file: false,
            deleted_file: false,
            renamed_file: false,
            too_large: false,
            lines: parse_unified_diff("@@ -1 +1 @@\n-a\n+b\n"),
            added: 1,
            removed: 1,
        };
        Comparison {
            base: "develop".into(),
            head: "fix/login".into(),
            commits: 2,
            files: vec![file("a.rs"), file("b.rs")],
            web_url: "https://gitlab.com/g/api/-/compare/develop...fix/login".into(),
        }
    }

    #[test]
    fn needs_a_repository() {
        let mut state = AppState::new("d".into(), "gitlab.com".into(), None);
        assert!(update(&mut state, Action::ShowBranches).is_none());
        assert_eq!(state.view, View::Branches);
        assert!(!state.branches.loading);
    }

    #[test]
    fn loads_once_and_filters() {
        let mut state = branches_view();
        assert_eq!(state.visible_branches().len(), 3);
        // Coming back does not load again.
        update(&mut state, Action::ShowMergeRequests);
        assert!(update(&mut state, Action::ShowBranches).is_none());

        for c in "login".chars() {
            update(&mut state, Action::FilterInput(c));
        }
        assert_eq!(state.selected_branch().unwrap().name, "fix/login");
        update(&mut state, Action::Back);
        assert_eq!(state.visible_branches().len(), 3);
    }

    #[test]
    fn opens_a_branch_compared_with_the_default_branch() {
        let mut state = branches_view();
        update(&mut state, Action::SelectLast);
        let command = update(&mut state, Action::OpenDetail);
        assert!(matches!(
            &command,
            Some(Command::FetchComparison { project, base, head, .. })
                if project == "g/api" && base == "develop" && head == "fix/login"
        ));
        assert_eq!(state.key_context(), KeyContext::Branch);

        let request_id = request_id(command);
        update(
            &mut state,
            Action::ComparisonLoaded {
                request_id,
                result: Ok(Box::new(comparison())),
            },
        );
        update(&mut state, Action::SelectNext);
        let screen = state.branch_screen.as_ref().unwrap();
        assert_eq!(screen.selected_file().unwrap().new_path, "b.rs");

        // Reading the diff: j scrolls, Esc goes back to the files before leaving.
        update(&mut state, Action::FocusContent);
        update(&mut state, Action::SelectNext);
        let screen = state.branch_screen.as_ref().unwrap();
        assert_eq!((screen.selected_file, screen.diff_cursor), (1, 1));
        update(&mut state, Action::CloseDetail);
        assert!(!state.branch_screen.as_ref().unwrap().diff_focused);

        // Approve, comment and merge keys do nothing here.
        assert!(update(&mut state, Action::StartMerge).is_none());
        update(&mut state, Action::CloseDetail);
        assert!(state.branch_screen.is_none());
        assert_eq!(state.key_context(), KeyContext::List);
    }

    #[test]
    fn the_default_branch_has_nothing_to_compare() {
        let mut state = branches_view();
        assert!(update(&mut state, Action::OpenDetail).is_none());
        assert!(state.branch_screen.is_none());
        assert!(
            state
                .notice
                .as_ref()
                .unwrap()
                .text
                .contains("default branch")
        );
    }

    #[test]
    fn b_compares_with_a_branch_of_your_choice() {
        let mut state = branches_view();
        update(&mut state, Action::SelectLast); // fix/login
        assert!(
            update(&mut state, Action::PickBase).is_none(),
            "nothing to load yet"
        );
        assert_eq!(state.key_context(), KeyContext::BranchBase);
        let screen = state.branch_screen.as_ref().unwrap();
        assert_eq!(screen.base_candidates(), ["develop", "feature/export"]);
        assert_eq!(
            screen.base_picker.as_ref().unwrap().selected,
            0,
            "on the default branch"
        );

        for c in "exp".chars() {
            update(&mut state, Action::PickerInput(c));
        }
        let command = update(&mut state, Action::PickerSelect);
        assert!(matches!(
            &command,
            Some(Command::FetchComparison { base, head, .. })
                if base == "feature/export" && head == "fix/login"
        ));
        assert_eq!(state.key_context(), KeyContext::Branch);
    }

    #[test]
    fn b_changes_the_base_of_an_open_comparison() {
        let mut state = branches_view();
        update(&mut state, Action::SelectLast);
        let request_id = request_id(update(&mut state, Action::OpenDetail));
        update(
            &mut state,
            Action::ComparisonLoaded {
                request_id,
                result: Ok(Box::new(comparison())),
            },
        );
        update(&mut state, Action::PickBase);
        // Esc closes the popup but keeps the comparison on screen.
        update(&mut state, Action::ClosePopup);
        let screen = state.branch_screen.as_ref().unwrap();
        assert!(screen.base_picker.is_none());
        assert_eq!(screen.base, "develop");

        update(&mut state, Action::PickBase);
        update(&mut state, Action::SelectNext);
        let command = update(&mut state, Action::PickerSelect);
        assert!(matches!(
            &command,
            Some(Command::FetchComparison { base, .. }) if base == "feature/export"
        ));
        let screen = state.branch_screen.as_ref().unwrap();
        assert!(
            screen.comparison.is_none(),
            "the old files are not shown as the new ones"
        );
    }

    #[test]
    fn cancelling_the_first_choice_goes_back_to_the_list() {
        let mut state = branches_view();
        update(&mut state, Action::PickBase);
        update(&mut state, Action::ClosePopup);
        assert!(state.branch_screen.is_none());
        assert_eq!(state.key_context(), KeyContext::List);
    }

    #[test]
    fn the_default_branch_can_be_compared_with_another_one() {
        let mut state = branches_view();
        update(&mut state, Action::OpenDetail);
        assert!(state.notice.as_ref().unwrap().text.contains("press b"));
        update(&mut state, Action::PickBase);
        let screen = state.branch_screen.as_ref().unwrap();
        assert_eq!(screen.base_candidates(), ["feature/export", "fix/login"]);
    }

    #[test]
    fn ignores_stale_responses() {
        let mut state = branches_view();
        let first = request_id(update(&mut state, Action::Refresh));
        let second = request_id(update(&mut state, Action::Refresh));
        update(
            &mut state,
            Action::BranchesLoaded {
                request_id: first,
                result: Err(ApiError::Forbidden("old".into())),
            },
        );
        assert!(state.branches.error.is_none());
        assert!(state.branches.loading);
        update(
            &mut state,
            Action::BranchesLoaded {
                request_id: second,
                result: page(),
            },
        );
        assert!(!state.branches.loading);
    }

    #[test]
    fn switching_repository_forgets_the_branches() {
        let mut state = branches_view();
        state.reset_dashboard(Some("g/web".into()));
        assert!(state.branches.branches.is_empty());
        assert!(!state.branches.loaded_once);
    }
}
