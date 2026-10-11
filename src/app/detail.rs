//! State and key handling for the merge request detail screen.

use super::ci::{self, CiSource, CiState};
use super::{Action, AppState, Command, Notice};
use crate::forge::{CiAction, FileDiff, ForgeKind, MergeRequest, MrAction, MrDetail};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Overview,
    Files,
    Discussion,
    Pipelines,
}

impl Section {
    pub const ALL: [Section; 4] = [
        Section::Overview,
        Section::Files,
        Section::Discussion,
        Section::Pipelines,
    ];

    fn next(self) -> Self {
        match self {
            Self::Overview => Self::Files,
            Self::Files => Self::Discussion,
            Self::Discussion => Self::Pipelines,
            Self::Pipelines => Self::Overview,
        }
    }

    fn previous(self) -> Self {
        match self {
            Self::Overview => Self::Pipelines,
            Self::Files => Self::Overview,
            Self::Discussion => Self::Files,
            Self::Pipelines => Self::Discussion,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Popup {
    Comment,
    ConfirmMerge,
    /// Asks before canceling a job or pipeline.
    ConfirmCi(CiAction),
}

#[derive(Debug)]
pub struct DetailState {
    pub project_path: String,
    pub iid: u64,
    /// How the server writes this change request's number: `!42` (GitLab) or `#42` (GitHub).
    pub reference: String,
    pub forge: ForgeKind,
    /// Copied from the list so the header has something to show while loading.
    pub title: String,
    /// For checking it out (`C`) before the details have loaded.
    pub source_branch: String,
    pub web_url: String,
    pub detail: Option<MrDetail>,
    pub loading: bool,
    pub error: Option<String>,
    pub(super) request_id: u64,
    pub section: Section,
    pub selected_file: usize,
    /// Scroll offset of the main pane (description, diff or discussion). Reset when the
    /// section or file changes.
    pub scroll: u16,
    /// Files section: `j`/`k` scroll the diff instead of picking a file.
    pub diff_focused: bool,
    pub popup: Option<Popup>,
    /// Comment being written. Kept when the popup is closed, cleared once it is posted.
    pub draft: String,
    /// Set while an action runs ("approving…"); other actions wait for it.
    pub busy: Option<&'static str>,
    /// The Pipelines section. Loaded the first time the section is opened.
    pub ci: CiState,
}

impl DetailState {
    pub(super) fn new(mr: &MergeRequest, forge: ForgeKind) -> Self {
        Self {
            project_path: mr.project_path.clone(),
            iid: mr.iid,
            reference: format!("{}{}", forge.reference_prefix(), mr.iid),
            forge,
            title: mr.title.clone(),
            source_branch: mr.source_branch.clone(),
            web_url: mr.web_url.clone(),
            detail: None,
            loading: false,
            error: None,
            request_id: 0,
            section: Section::Overview,
            selected_file: 0,
            scroll: 0,
            diff_focused: false,
            popup: None,
            draft: String::new(),
            busy: None,
            ci: CiState::new(
                CiSource::MergeRequest {
                    project_path: mr.project_path.clone(),
                    iid: mr.iid,
                },
                forge,
            ),
        }
    }

    pub fn selected_file(&self) -> Option<&FileDiff> {
        self.detail.as_ref()?.files.get(self.selected_file)
    }

    fn file_count(&self) -> usize {
        self.detail.as_ref().map_or(0, |d| d.files.len())
    }

    /// Takes the request counter as a separate `&mut u64` instead of `&mut AppState`:
    /// callers already hold `&mut DetailState`, which borrows part of `AppState`.
    pub(super) fn fetch(&mut self, next_request_id: &mut u64) -> Command {
        *next_request_id += 1;
        self.request_id = *next_request_id;
        self.loading = true;
        Command::FetchDetail {
            request_id: self.request_id,
            project_path: self.project_path.clone(),
            iid: self.iid,
        }
    }

    fn start_action(&mut self, action: MrAction) -> Command {
        self.busy = Some(busy_label(&action));
        Command::RunMrAction {
            project_path: self.project_path.clone(),
            iid: self.iid,
            action,
        }
    }
}

fn busy_label(action: &MrAction) -> &'static str {
    match action {
        MrAction::Approve => "approving…",
        MrAction::Unapprove => "removing approval…",
        MrAction::Comment(_) => "posting comment…",
        MrAction::Merge { .. } => "merging…",
    }
}

/// `reference`: `!42` or `#42`.
pub(super) fn success_message(action: &MrAction, reference: &str) -> String {
    match action {
        MrAction::Approve => format!("Approved {reference}"),
        MrAction::Unapprove => format!("Removed your approval from {reference}"),
        MrAction::Comment(_) => format!("Comment posted on {reference}"),
        MrAction::Merge { .. } => format!("Merged {reference}"),
    }
}

/// A file list next to the selected file's diff, as on the Files section and a branch
/// comparison. Returns `false` for actions it does not handle.
///
/// The list has the focus at first: `j`/`k` pick a file. `Enter`, `l` or `→` move the focus
/// to the diff, where `j`/`k` scroll; `h`, `←` or `Esc` go back to the list.
pub(super) fn file_browser_action(
    action: &Action,
    selected: &mut usize,
    scroll: &mut u16,
    diff_focused: &mut bool,
    file_count: usize,
) -> bool {
    match action {
        Action::FocusContent if file_count > 0 => *diff_focused = true,
        Action::FocusList => *diff_focused = false,
        // `Esc` leaves the diff before it leaves the screen.
        Action::CloseDetail if *diff_focused => *diff_focused = false,
        Action::SelectFile(index) if *index < file_count => {
            if *selected != *index {
                *selected = *index;
                *scroll = 0;
            }
            *diff_focused = false;
        }
        Action::SelectNext if *diff_focused => *scroll = scroll.saturating_add(1),
        Action::SelectPrevious if *diff_focused => *scroll = scroll.saturating_sub(1),
        Action::SelectNext => {
            if *selected + 1 < file_count {
                *selected += 1;
                *scroll = 0;
            }
        }
        Action::SelectPrevious => {
            if *selected > 0 {
                *selected -= 1;
                *scroll = 0;
            }
        }
        _ => return false,
    }
    true
}

/// Handles user actions while the detail screen is open.
pub(super) fn update(state: &mut AppState, action: Action) -> Option<Command> {
    // `detail` mutably borrows `state.detail`. While it is alive we can still use other
    // *fields* of `state` (`state.notice`, `state.next_request_id`), because the borrow
    // checker tracks fields separately, but not methods that take `&mut self` on all of
    // `state`, since those could touch `state.detail` too.
    let detail = state.detail.as_mut()?;

    if let Some(busy) = detail.busy
        && matches!(
            action,
            Action::ToggleApprove | Action::SubmitComment | Action::StartMerge
        )
    {
        state.notice = Some(Notice::error(format!("Wait: {busy}")));
        return None;
    }

    // In the Pipelines section (or its cancel confirmation), CI keys come first.
    if (detail.section == Section::Pipelines || matches!(detail.popup, Some(Popup::ConfirmCi(_))))
        && let Some(command) = ci::handle_action(
            &mut detail.ci,
            &mut detail.popup,
            &action,
            &mut state.next_request_id,
            &mut state.notice,
        )
    {
        return command;
    }

    if detail.section == Section::Files {
        let count = detail.file_count();
        if file_browser_action(
            &action,
            &mut detail.selected_file,
            &mut detail.scroll,
            &mut detail.diff_focused,
            count,
        ) {
            return None;
        }
    }

    match action {
        Action::CloseDetail => {
            state.detail = None;
            // Something changed while we were here (approved, merged…): reload the list.
            if std::mem::take(&mut state.list_stale) {
                return Some(state.fetch_active());
            }
        }
        Action::Refresh => return Some(detail.fetch(&mut state.next_request_id)),
        Action::OpenInBrowser => return Some(Command::OpenUrl(detail.web_url.clone())),
        Action::NextSection | Action::PreviousSection => {
            detail.section = if matches!(action, Action::NextSection) {
                detail.section.next()
            } else {
                detail.section.previous()
            };
            detail.scroll = 0;
            detail.diff_focused = false;
            if detail.section == Section::Pipelines {
                return detail.ci.ensure_loaded(&mut state.next_request_id);
            }
        }
        // The Files section is handled above; elsewhere j/k scroll.
        Action::SelectNext => detail.scroll = detail.scroll.saturating_add(1),
        Action::SelectPrevious => detail.scroll = detail.scroll.saturating_sub(1),
        Action::ScrollDown(lines) => detail.scroll = detail.scroll.saturating_add(lines),
        Action::ScrollUp(lines) => detail.scroll = detail.scroll.saturating_sub(lines),
        Action::ToggleApprove => {
            let Some(loaded) = &detail.detail else {
                state.notice = Some(Notice::info("Still loading…"));
                return None;
            };
            let action = if loaded.approvals.user_has_approved {
                if !detail.forge.can_unapprove() {
                    state.notice = Some(Notice::error(format!(
                        "{} can't withdraw an approval from here; dismiss your review on the web (press o)",
                        detail.forge.name()
                    )));
                    return None;
                }
                MrAction::Unapprove
            } else if loaded.approvals.user_can_approve {
                MrAction::Approve
            } else {
                state.notice = Some(Notice::error(format!(
                    "You can't approve this (authors can't approve their own on {}, or you're not an eligible approver)",
                    detail.forge.name()
                )));
                return None;
            };
            return Some(detail.start_action(action));
        }
        Action::StartComment => detail.popup = Some(Popup::Comment),
        Action::CommentInput(c) => detail.draft.push(c),
        Action::CommentNewline => detail.draft.push('\n'),
        Action::CommentBackspace => {
            detail.draft.pop();
        }
        Action::SubmitComment => {
            let body = detail.draft.trim();
            if body.is_empty() {
                state.notice = Some(Notice::error("Write something first"));
                return None;
            }
            let action = MrAction::Comment(body.to_string());
            detail.popup = None;
            return Some(detail.start_action(action));
        }
        Action::StartMerge => {
            let Some(loaded) = &detail.detail else {
                state.notice = Some(Notice::info("Still loading…"));
                return None;
            };
            if loaded.state != "opened" {
                state.notice = Some(Notice::error(format!(
                    "This merge request is {}",
                    loaded.state
                )));
            } else if !loaded.can_merge {
                state.notice = Some(Notice::error("You don't have permission to merge"));
            } else {
                detail.popup = Some(Popup::ConfirmMerge);
            }
        }
        Action::Confirm => match detail.popup.take() {
            Some(Popup::ConfirmMerge) => {
                let sha = detail.detail.as_ref().and_then(|d| d.sha.clone());
                return Some(detail.start_action(MrAction::Merge { sha }));
            }
            other => detail.popup = other,
        },
        Action::ClosePopup => detail.popup = None,
        _ => {}
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::update;
    use crate::forge::{ApiError, Approvals, MrPage, User};
    use chrono::Utc;

    fn user(name: &str) -> User {
        User {
            username: name.into(),
            name: name.into(),
        }
    }

    fn list_item() -> MergeRequest {
        MergeRequest {
            iid: 42,
            title: "Fix bug".into(),
            draft: false,
            author: user("hoang"),
            project_path: "group/api".into(),
            source_branch: "fix".into(),
            target_branch: "main".into(),
            web_url: "https://gitlab.com/group/api/-/merge_requests/42".into(),
            updated_at: Utc::now(),
            pipeline_status: None,
            approved_by: vec![],
        }
    }

    fn file(path: &str) -> FileDiff {
        FileDiff {
            old_path: path.into(),
            new_path: path.into(),
            new_file: false,
            deleted_file: false,
            renamed_file: false,
            too_large: false,
            lines: vec![],
            added: 0,
            removed: 0,
        }
    }

    fn loaded_detail(approvals: Approvals, can_merge: bool) -> MrDetail {
        MrDetail {
            iid: 42,
            title: "Fix bug (updated)".into(),
            description: String::new(),
            state: "opened".into(),
            draft: false,
            author: user("hoang"),
            source_branch: "fix".into(),
            target_branch: "main".into(),
            web_url: "https://gitlab.com/group/api/-/merge_requests/42".into(),
            sha: Some("abc123".into()),
            merge_status: Some("mergeable".into()),
            has_conflicts: false,
            can_merge,
            pipeline_status: None,
            approvals,
            files: vec![file("a.rs"), file("b.rs")],
            discussions: vec![],
        }
    }

    fn can_approve() -> Approvals {
        Approvals {
            user_can_approve: true,
            ..Approvals::default()
        }
    }

    /// Fetch request id from a command, panicking on anything else.
    fn fetch_id(command: Option<Command>) -> u64 {
        match command {
            Some(Command::FetchMergeRequests { request_id, .. })
            | Some(Command::FetchDetail { request_id, .. }) => request_id,
            other => panic!("expected a fetch, got {other:?}"),
        }
    }

    /// App with one merge request in the list and its detail screen open and loaded.
    fn open(detail: MrDetail) -> AppState {
        let mut state = AppState::new("default".into(), "gitlab.com".into(), None);
        let id = fetch_id(update(&mut state, Action::Refresh));
        let page = MrPage {
            current_user: user("thang"),
            merge_requests: vec![list_item()],
        };
        update(
            &mut state,
            Action::MergeRequestsLoaded {
                request_id: id,
                result: Ok(page),
            },
        );
        let id = fetch_id(update(&mut state, Action::OpenDetail));
        update(
            &mut state,
            Action::DetailLoaded {
                request_id: id,
                result: Ok(Box::new(detail)),
            },
        );
        state
    }

    fn detail(state: &AppState) -> &DetailState {
        state.detail.as_ref().expect("detail screen is open")
    }

    fn finish(
        state: &mut AppState,
        action: MrAction,
        result: Result<(), ApiError>,
    ) -> Option<Command> {
        let finished = Action::MrActionFinished {
            project_path: "group/api".into(),
            iid: 42,
            action,
            result,
        };
        update(state, finished)
    }

    #[test]
    fn opening_loads_the_detail_and_esc_goes_back() {
        let mut state = open(loaded_detail(can_approve(), true));
        assert_eq!(detail(&state).title, "Fix bug (updated)");
        // Nothing changed, so going back does not reload the list.
        assert!(update(&mut state, Action::CloseDetail).is_none());
        assert!(state.detail.is_none());
    }

    #[test]
    fn stale_detail_responses_are_ignored() {
        let mut state = open(loaded_detail(can_approve(), true));
        let old = detail(&state).request_id;
        fetch_id(update(&mut state, Action::Refresh));
        let mut other = loaded_detail(can_approve(), true);
        other.title = "old".into();
        update(
            &mut state,
            Action::DetailLoaded {
                request_id: old,
                result: Ok(Box::new(other)),
            },
        );
        assert_eq!(detail(&state).title, "Fix bug (updated)");
    }

    #[test]
    fn j_and_k_pick_files_or_scroll_depending_on_the_section() {
        let mut state = open(loaded_detail(can_approve(), true));
        update(&mut state, Action::SelectNext);
        assert_eq!(detail(&state).scroll, 1);

        update(&mut state, Action::NextSection);
        assert_eq!(detail(&state).section, Section::Files);
        assert_eq!(detail(&state).scroll, 0);
        update(&mut state, Action::SelectNext);
        update(&mut state, Action::SelectNext);
        assert_eq!(detail(&state).selected_file().unwrap().new_path, "b.rs");

        update(&mut state, Action::PreviousSection);
        update(&mut state, Action::PreviousSection);
        // Four sections, wrapping around: Files → Overview → Pipelines.
        assert_eq!(detail(&state).section, Section::Pipelines);
    }

    #[test]
    fn enter_moves_the_focus_to_the_diff_so_j_and_k_scroll_it() {
        let mut state = open(loaded_detail(can_approve(), true));
        update(&mut state, Action::NextSection);
        update(&mut state, Action::FocusContent);
        assert!(detail(&state).diff_focused);
        update(&mut state, Action::SelectNext);
        update(&mut state, Action::SelectNext);
        assert_eq!(detail(&state).selected_file, 0, "still the first file");
        assert_eq!(detail(&state).scroll, 2, "the diff scrolled instead");

        // Esc leaves the diff first, then the screen.
        update(&mut state, Action::CloseDetail);
        assert!(state.detail.is_some());
        assert!(!detail(&state).diff_focused);
        update(&mut state, Action::SelectNext);
        assert_eq!(detail(&state).selected_file, 1);
        assert_eq!(detail(&state).scroll, 0, "a new file starts at the top");

        // A click on a file focuses the list again.
        update(&mut state, Action::FocusContent);
        update(&mut state, Action::SelectFile(0));
        assert_eq!(detail(&state).selected_file, 0);
        assert!(!detail(&state).diff_focused);
        update(&mut state, Action::SelectFile(9));
        assert_eq!(detail(&state).selected_file, 0, "out of range is ignored");

        // Changing section forgets the focus.
        update(&mut state, Action::FocusContent);
        update(&mut state, Action::NextSection);
        assert!(!detail(&state).diff_focused);
        update(&mut state, Action::CloseDetail);
        assert!(state.detail.is_none());
    }

    #[test]
    fn approve_toggles_based_on_current_approval() {
        let mut state = open(loaded_detail(can_approve(), true));
        let command = update(&mut state, Action::ToggleApprove);
        assert!(matches!(
            command,
            Some(Command::RunMrAction {
                action: MrAction::Approve,
                ..
            })
        ));

        let approved = Approvals {
            user_has_approved: true,
            ..can_approve()
        };
        let mut state = open(loaded_detail(approved, true));
        let command = update(&mut state, Action::ToggleApprove);
        assert!(matches!(
            command,
            Some(Command::RunMrAction {
                action: MrAction::Unapprove,
                ..
            })
        ));
    }

    #[test]
    fn github_approvals_cannot_be_withdrawn_from_here() {
        let approved = Approvals {
            user_has_approved: true,
            ..can_approve()
        };
        let mut state = open(loaded_detail(approved, true));
        state.detail.as_mut().unwrap().forge = ForgeKind::GitHub;
        assert!(update(&mut state, Action::ToggleApprove).is_none());
        assert!(state.notice.as_ref().unwrap().text.contains("press o"));
    }

    #[test]
    fn approve_is_refused_when_not_allowed() {
        let mut state = open(loaded_detail(Approvals::default(), true));
        assert!(update(&mut state, Action::ToggleApprove).is_none());
        assert!(state.notice.as_ref().unwrap().is_error);
    }

    #[test]
    fn actions_wait_for_the_running_one() {
        let mut state = open(loaded_detail(can_approve(), true));
        update(&mut state, Action::ToggleApprove);
        assert_eq!(detail(&state).busy, Some("approving…"));
        assert!(update(&mut state, Action::StartMerge).is_none());
        assert!(state.notice.as_ref().unwrap().text.contains("approving"));
    }

    #[test]
    fn successful_action_refreshes_detail_and_later_the_list() {
        let mut state = open(loaded_detail(can_approve(), true));
        update(&mut state, Action::ToggleApprove);
        let command = finish(&mut state, MrAction::Approve, Ok(()));
        fetch_id(command);
        assert_eq!(detail(&state).busy, None);
        assert_eq!(state.notice.as_ref().unwrap().text, "Approved !42");
        // The list may be out of date now: going back reloads it.
        assert!(matches!(
            update(&mut state, Action::CloseDetail),
            Some(Command::FetchMergeRequests { .. })
        ));
    }

    #[test]
    fn comment_flow_keeps_the_draft_until_it_is_posted() {
        let mut state = open(loaded_detail(can_approve(), true));
        update(&mut state, Action::StartComment);
        assert!(
            update(&mut state, Action::SubmitComment).is_none(),
            "empty comment"
        );

        for c in "  LGTM".chars() {
            update(&mut state, Action::CommentInput(c));
        }
        update(&mut state, Action::ClosePopup);
        update(&mut state, Action::StartComment);
        assert_eq!(
            detail(&state).draft,
            "  LGTM",
            "draft survives closing the popup"
        );

        let command = update(&mut state, Action::SubmitComment);
        assert!(matches!(
            &command,
            Some(Command::RunMrAction { action: MrAction::Comment(body), .. }) if body == "LGTM"
        ));
        assert_eq!(detail(&state).popup, None);

        // Failure: the draft stays so nothing is lost.
        finish(
            &mut state,
            MrAction::Comment("LGTM".into()),
            Err(ApiError::Forbidden("no".into())),
        );
        assert_eq!(detail(&state).draft, "  LGTM");
        // Success: the draft is cleared.
        finish(&mut state, MrAction::Comment("LGTM".into()), Ok(()));
        assert!(detail(&state).draft.is_empty());
    }

    #[test]
    fn merge_asks_first_and_sends_the_reviewed_sha() {
        let mut state = open(loaded_detail(can_approve(), true));
        assert!(update(&mut state, Action::StartMerge).is_none());
        assert_eq!(detail(&state).popup, Some(Popup::ConfirmMerge));
        let command = update(&mut state, Action::Confirm);
        assert_eq!(
            command,
            Some(Command::RunMrAction {
                project_path: "group/api".into(),
                iid: 42,
                action: MrAction::Merge {
                    sha: Some("abc123".into())
                },
            })
        );
    }

    #[test]
    fn merge_is_refused_without_permission() {
        let mut state = open(loaded_detail(can_approve(), false));
        update(&mut state, Action::StartMerge);
        assert_eq!(detail(&state).popup, None);
        assert!(state.notice.as_ref().unwrap().text.contains("permission"));
    }

    #[test]
    fn results_for_another_merge_request_only_show_a_notice() {
        let mut state = open(loaded_detail(can_approve(), true));
        update(&mut state, Action::ToggleApprove);
        let other = Action::MrActionFinished {
            project_path: "group/other".into(),
            iid: 7,
            action: MrAction::Approve,
            result: Ok(()),
        };
        assert!(update(&mut state, other).is_none());
        assert_eq!(detail(&state).busy, Some("approving…"));
    }

    #[test]
    fn scroll_is_clamped_by_the_ui_limit() {
        let mut state = open(loaded_detail(can_approve(), true));
        update(&mut state, Action::ScrollDown(50));
        state.clamp_detail_scroll(7);
        assert_eq!(detail(&state).scroll, 7);
    }
}
