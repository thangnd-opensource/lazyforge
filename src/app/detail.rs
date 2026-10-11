//! State and key handling for the merge request detail screen.

use super::ci::{self, CiSource, CiState};
use super::review;
use super::{Action, AppState, Command, Notice};
use crate::forge::{
    CiAction, FileDiff, ForgeKind, LinePosition, MergeRequest, MrAction, MrDetail, Verdict,
};

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
    /// Ends the review: summary, verdict, send.
    FinishReview,
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
    /// The selected line of the diff (index into the file's lines) while it has the focus.
    pub diff_cursor: usize,
    pub popup: Option<Popup>,
    /// Comment being written. Kept when the popup is closed, cleared once it is posted.
    pub draft: String,
    /// What the comment being written answers; `None` for a general comment.
    pub comment_target: Option<CommentTarget>,
    /// The comment editor holds an existing review draft (closing it drops the text).
    pub editing_draft: bool,
    /// The review's summary, kept while the "finish review" popup is closed.
    pub review_body: String,
    pub verdict: Verdict,
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
            diff_cursor: 0,
            popup: None,
            draft: String::new(),
            comment_target: None,
            editing_draft: false,
            review_body: String::new(),
            verdict: Verdict::Comment,
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

    pub(super) fn start_action(&mut self, action: MrAction) -> Command {
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
        MrAction::Comment(_) | MrAction::LineComment { .. } | MrAction::Reply { .. } => {
            "posting comment…"
        }
        MrAction::SubmitReview { .. } => "sending review…",
        MrAction::Merge { .. } => "merging…",
    }
}

/// `reference`: `!42` or `#42`.
pub(super) fn success_message(action: &MrAction, reference: &str) -> String {
    match action {
        MrAction::Approve => format!("Approved {reference}"),
        MrAction::Unapprove => format!("Removed your approval from {reference}"),
        MrAction::Comment(_) => format!("Comment posted on {reference}"),
        MrAction::LineComment { position, .. } => {
            format!("Comment posted on {}", position.label())
        }
        MrAction::Reply { label, .. } => format!("Reply posted on {label}"),
        MrAction::SubmitReview {
            verdict, comments, ..
        } => {
            let outcome = match verdict {
                Verdict::Comment => "",
                Verdict::Approve => ", approved",
                Verdict::RequestChanges => ", changes requested",
            };
            format!(
                "Review sent on {reference}: {} comment(s){outcome}",
                comments.len()
            )
        }
        MrAction::Merge { .. } => format!("Merged {reference}"),
    }
}

/// A file list next to the selected file's diff, as on the Files section and a branch
/// comparison. Returns `false` for actions it does not handle.
///
/// The list has the focus at first: `j`/`k` pick a file. `Enter`, `l` or `→` move the focus
/// to the diff, where `j`/`k` scroll; `h`, `←` or `Esc` go back to the list.
/// A comment written from the diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommentTarget {
    /// A new thread on a line.
    Line(LinePosition),
    /// A reply in the thread already on the line.
    Reply {
        discussion: String,
        /// `@hoang on src/a.rs:21`, for the popup.
        label: String,
        /// `src/a.rs:21`, for the success message.
        line: String,
    },
}

impl CommentTarget {
    /// The popup's title: `Comment on src/a.rs:21` or `Reply to @hoang on src/a.rs:21`.
    pub fn title(&self) -> String {
        match self {
            Self::Line(position) => format!("Comment on {}", position.label()),
            Self::Reply { label, .. } => format!("Reply to {label}"),
        }
    }
}

/// The file list and diff state of a screen, for `file_browser_action`.
pub(super) struct FileBrowser<'a> {
    pub selected: &'a mut usize,
    pub scroll: &'a mut u16,
    pub diff_focused: &'a mut bool,
    /// The selected diff line while the diff has the focus.
    pub cursor: &'a mut usize,
    pub file_count: usize,
    /// Lines of the selected file's diff.
    pub line_count: usize,
}

/// A file list next to the selected file's diff, as on the Files section and a branch
/// comparison. Returns `false` for actions it does not handle.
///
/// The list has the focus at first: `j`/`k` pick a file and `J`/`K` scroll the diff. `Enter`,
/// `l` or `→` move the focus to the diff, where a line cursor appears and `j`/`k` (and
/// `J`/`K`, `Ctrl+d`/`Ctrl+u`) move it; `h`, `←` or `Esc` go back to the list.
pub(super) fn file_browser_action(action: &Action, files: FileBrowser) -> bool {
    let FileBrowser {
        selected,
        scroll,
        diff_focused,
        cursor,
        file_count,
        line_count,
    } = files;
    let last_line = line_count.saturating_sub(1);
    let move_cursor = |cursor: &mut usize, down: bool, lines: usize| {
        *cursor = if down {
            (*cursor + lines).min(last_line)
        } else {
            cursor.saturating_sub(lines)
        };
    };
    let pick = |selected: &mut usize, cursor: &mut usize, scroll: &mut u16, index: usize| {
        if *selected != index {
            *selected = index;
            *cursor = 0;
            *scroll = 0;
        }
    };
    match action {
        Action::FocusContent if file_count > 0 => *diff_focused = true,
        Action::FocusList => *diff_focused = false,
        // `Esc` leaves the diff before it leaves the screen.
        Action::CloseDetail if *diff_focused => *diff_focused = false,
        Action::SelectFile(index) if *index < file_count => {
            pick(selected, cursor, scroll, *index);
            *diff_focused = false;
        }
        Action::SelectNext if *diff_focused => move_cursor(cursor, true, 1),
        Action::SelectPrevious if *diff_focused => move_cursor(cursor, false, 1),
        Action::ScrollDown(lines) if *diff_focused => {
            move_cursor(cursor, true, usize::from(*lines))
        }
        Action::ScrollUp(lines) if *diff_focused => move_cursor(cursor, false, usize::from(*lines)),
        Action::SelectNext if *selected + 1 < file_count => {
            pick(selected, cursor, scroll, *selected + 1);
        }
        Action::SelectPrevious if *selected > 0 => pick(selected, cursor, scroll, *selected - 1),
        Action::SelectNext | Action::SelectPrevious => {}
        _ => return false,
    }
    true
}

/// What `c` on the diff line under the cursor writes: a reply if the line already has a
/// thread (the latest one), otherwise a new thread. `Err` says why there cannot be one.
fn comment_target(detail: &DetailState) -> Result<CommentTarget, String> {
    let loaded = detail.detail.as_ref().ok_or("Still loading…")?;
    let file = detail.selected_file().ok_or("No file selected")?;
    let line = file
        .lines
        .get(detail.diff_cursor)
        .ok_or("Move to a line of the diff first")?;
    let not_a_line = "Pick an added, removed or unchanged line (not a @@ header) to comment on it";
    let thread = loaded
        .discussions
        .iter()
        .rev()
        .find(|d| d.position().is_some_and(|p| p.is_on(file, line)));
    let Some(thread) = thread else {
        let refs = loaded
            .diff_refs
            .as_ref()
            .ok_or("The server has not finished preparing this diff; press r in a moment")?;
        return LinePosition::for_line(file, line, refs)
            .map(CommentTarget::Line)
            .ok_or_else(|| not_a_line.to_string());
    };
    // A reply needs no commits, only the thread; the position is for the labels.
    let refs = crate::forge::DiffRefs {
        base_sha: None,
        start_sha: None,
        head_sha: String::new(),
    };
    let place = LinePosition::for_line(file, line, &refs)
        .ok_or(not_a_line)?
        .label();
    let author = thread
        .notes
        .first()
        .map_or_else(String::new, |n| format!("@{} on ", n.author.username));
    Ok(CommentTarget::Reply {
        discussion: thread.id.clone(),
        label: format!("{author}{place}"),
        line: place,
    })
}

/// Handles user actions while the detail screen is open.
pub(super) fn update(state: &mut AppState, action: Action) -> Option<Command> {
    if matches!(state.detail.as_ref()?.popup, Some(Popup::FinishReview)) {
        return review::update_finish(state, action);
    }
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
        let line_count = detail.selected_file().map_or(0, |f| f.lines.len());
        let files = FileBrowser {
            file_count: detail.file_count(),
            line_count,
            selected: &mut detail.selected_file,
            scroll: &mut detail.scroll,
            diff_focused: &mut detail.diff_focused,
            cursor: &mut detail.diff_cursor,
        };
        if file_browser_action(&action, files) {
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
        Action::StartComment => {
            // In the diff, the comment is about the line under the cursor; a draft already
            // there opens for editing.
            detail.comment_target = None;
            if let Some(index) = review::on_cursor(detail, &state.review_drafts) {
                let draft = review::of(&state.review_drafts, detail)[index].clone();
                detail.comment_target = Some(CommentTarget::Line(draft.position));
                detail.draft = draft.body;
                detail.editing_draft = true;
                detail.popup = Some(Popup::Comment);
                return None;
            }
            if detail.section == Section::Files && detail.diff_focused {
                match comment_target(detail) {
                    Ok(target) => detail.comment_target = Some(target),
                    Err(message) => {
                        state.notice = Some(Notice::error(message));
                        return None;
                    }
                }
            }
            detail.popup = Some(Popup::Comment);
        }
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
            let body = body.to_string();
            let action = match detail.comment_target.clone() {
                Some(CommentTarget::Line(position)) => MrAction::LineComment { body, position },
                Some(CommentTarget::Reply {
                    discussion, line, ..
                }) => MrAction::Reply {
                    body,
                    discussion,
                    label: line,
                },
                None => MrAction::Comment(body),
            };
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
        Action::ClosePopup => {
            detail.popup = None;
            // An edited draft is kept as it was; the editor's copy goes.
            if std::mem::take(&mut detail.editing_draft) {
                detail.draft.clear();
            }
        }
        Action::SaveDraft => {
            review::save_draft(detail, &mut state.review_drafts, &mut state.notice)
        }
        Action::DeleteDraft => {
            review::delete_draft(detail, &mut state.review_drafts, &mut state.notice)
        }
        Action::StartReview => review::start(detail),
        _ => {}
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::update;
    use crate::forge::{ApiError, Approvals, DiffRefs, MrPage, User};
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
            // Header, unchanged line 1, removed old line 2, added new lines 2 and 3.
            lines: crate::forge::diff::parse_unified_diff(
                "@@ -1,2 +1,3 @@\n keep\n-old\n+new\n+more\n",
            ),
            added: 2,
            removed: 1,
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
            diff_refs: Some(DiffRefs {
                base_sha: Some("base1".into()),
                start_sha: Some("start1".into()),
                head_sha: "abc123".into(),
            }),
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
    fn enter_moves_the_focus_to_the_diff_so_j_and_k_move_a_line_cursor() {
        let mut state = open(loaded_detail(can_approve(), true));
        update(&mut state, Action::NextSection);
        update(&mut state, Action::FocusContent);
        assert!(detail(&state).diff_focused);
        update(&mut state, Action::SelectNext);
        update(&mut state, Action::SelectNext);
        assert_eq!(detail(&state).selected_file, 0, "still the first file");
        assert_eq!(detail(&state).diff_cursor, 2, "the cursor moved instead");
        // It stops at the last line.
        update(&mut state, Action::ScrollDown(10));
        assert_eq!(detail(&state).diff_cursor, 4);

        // Esc leaves the diff first, then the screen.
        update(&mut state, Action::CloseDetail);
        assert!(state.detail.is_some());
        assert!(!detail(&state).diff_focused);
        update(&mut state, Action::SelectNext);
        assert_eq!(detail(&state).selected_file, 1);
        assert_eq!(
            detail(&state).diff_cursor,
            0,
            "a new file starts at the top"
        );

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
    fn c_in_the_diff_comments_on_the_line_under_the_cursor() {
        let mut state = open(loaded_detail(can_approve(), true));
        update(&mut state, Action::NextSection);
        update(&mut state, Action::FocusContent);

        // The `@@` header cannot take a comment.
        update(&mut state, Action::StartComment);
        assert!(detail(&state).popup.is_none());
        assert!(
            state
                .notice
                .as_ref()
                .unwrap()
                .text
                .contains("not a @@ header")
        );

        // An unchanged line: both line numbers.
        update(&mut state, Action::SelectNext);
        update(&mut state, Action::StartComment);
        let Some(CommentTarget::Line(target)) = detail(&state).comment_target.clone() else {
            panic!("expected a new thread on the line");
        };
        assert_eq!((target.old_line, target.new_line), (Some(1), Some(1)));
        update(&mut state, Action::ClosePopup);

        // An added line: the new line only, and the commits of the diff.
        update(&mut state, Action::ScrollDown(2));
        update(&mut state, Action::StartComment);
        assert_eq!(state.key_context(), crate::app::KeyContext::CommentEditor);
        for c in "Typo".chars() {
            update(&mut state, Action::CommentInput(c));
        }
        let command = update(&mut state, Action::SubmitComment);
        let Some(Command::RunMrAction {
            action: MrAction::LineComment { body, position },
            ..
        }) = command
        else {
            panic!("expected a line comment, got {command:?}");
        };
        assert_eq!(body, "Typo");
        assert_eq!((position.old_line, position.new_line), (None, Some(2)));
        assert_eq!(position.new_path, "a.rs");
        assert_eq!(position.refs.head_sha, "abc123");
        assert_eq!(position.refs.base_sha.as_deref(), Some("base1"));
        assert_eq!(position.label(), "a.rs:2");

        let action = MrAction::LineComment { body, position };
        finish(&mut state, action, Ok(()));
        assert!(detail(&state).draft.is_empty());
        assert!(detail(&state).comment_target.is_none());
        assert!(state.notice.as_ref().unwrap().text.contains("a.rs:2"));
    }

    #[test]
    fn c_on_a_line_with_a_thread_replies_to_it() {
        let mut loaded = loaded_detail(can_approve(), true);
        let mut note: crate::forge::Note = serde_json::from_value(serde_json::json!({
            "id": 9, "body": "Why?", "author": {"username": "hoang", "name": "Hoang"},
            "created_at": "2026-10-10T08:00:00Z",
            "position": {"new_path": "a.rs", "new_line": 2, "old_line": null}
        }))
        .unwrap();
        note.system = false;
        loaded.discussions = vec![crate::forge::Discussion {
            id: "d42".into(),
            notes: vec![note],
        }];
        let mut state = open(loaded);
        update(&mut state, Action::NextSection);
        update(&mut state, Action::FocusContent);
        update(&mut state, Action::ScrollDown(3)); // the added line 2
        update(&mut state, Action::StartComment);
        let target = detail(&state).comment_target.clone().unwrap();
        assert_eq!(target.title(), "Reply to @hoang on a.rs:2");
        update(&mut state, Action::CommentInput('k'));
        let command = update(&mut state, Action::SubmitComment);
        assert!(
            matches!(
                &command,
                Some(Command::RunMrAction {
                    action: MrAction::Reply { discussion, label, body },
                    ..
                }) if discussion == "d42" && label == "a.rs:2" && body == "k"
            ),
            "{command:?}"
        );
    }

    /// The Files section with the diff focused and the cursor on the added line 2.
    fn on_added_line(state: &mut AppState) {
        update(state, Action::NextSection);
        update(state, Action::FocusContent);
        update(state, Action::ScrollDown(3));
    }

    fn type_text(state: &mut AppState, text: &str) {
        for c in text.chars() {
            update(state, Action::CommentInput(c));
        }
    }

    #[test]
    fn line_comments_can_wait_for_the_review() {
        let mut state = open(loaded_detail(can_approve(), true));
        on_added_line(&mut state);
        update(&mut state, Action::StartComment);
        type_text(&mut state, "Typo");
        assert!(
            update(&mut state, Action::SaveDraft).is_none(),
            "nothing sent"
        );
        assert!(detail(&state).popup.is_none());
        let drafts = &state.review_drafts[&("group/api".to_string(), 42)];
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].body, "Typo");
        assert_eq!(drafts[0].position.new_line, Some(2));

        // `c` on the same line edits the draft; emptying it removes it.
        update(&mut state, Action::StartComment);
        assert_eq!(detail(&state).draft, "Typo");
        type_text(&mut state, "!");
        update(&mut state, Action::SaveDraft);
        assert_eq!(
            state.review_drafts[&("group/api".to_string(), 42)][0].body,
            "Typo!"
        );
        // Closing the editor of a draft keeps the draft as it was.
        update(&mut state, Action::StartComment);
        update(&mut state, Action::CommentBackspace);
        update(&mut state, Action::ClosePopup);
        assert!(detail(&state).draft.is_empty());
        assert_eq!(
            state.review_drafts[&("group/api".to_string(), 42)][0].body,
            "Typo!"
        );

        // `d` drops it.
        update(&mut state, Action::DeleteDraft);
        assert!(state.review_drafts[&("group/api".to_string(), 42)].is_empty());
    }

    #[test]
    fn a_review_is_sent_once_with_its_drafts() {
        let mut state = open(loaded_detail(can_approve(), true));
        on_added_line(&mut state);
        update(&mut state, Action::StartComment);
        type_text(&mut state, "Typo");
        update(&mut state, Action::SaveDraft);

        update(&mut state, Action::StartReview);
        assert_eq!(state.key_context(), crate::app::KeyContext::ReviewEditor);
        // GitLab: Comment → Approve → Comment (no "request changes").
        update(&mut state, Action::CycleVerdict);
        assert_eq!(detail(&state).verdict, Verdict::Approve);
        update(&mut state, Action::CycleVerdict);
        assert_eq!(detail(&state).verdict, Verdict::Comment);
        update(&mut state, Action::CycleVerdict);
        type_text(&mut state, "LGTM");
        let command = update(&mut state, Action::SubmitComment);
        let Some(Command::RunMrAction {
            action:
                MrAction::SubmitReview {
                    body,
                    verdict,
                    comments,
                },
            ..
        }) = command
        else {
            panic!("expected a review, got {command:?}");
        };
        assert_eq!(
            (body.as_str(), verdict, comments.len()),
            ("LGTM", Verdict::Approve, 1)
        );
        // Drafts stay until the server says yes...
        assert_eq!(state.review_drafts[&("group/api".to_string(), 42)].len(), 1);
        let action = MrAction::SubmitReview {
            body,
            verdict,
            comments,
        };
        finish(&mut state, action, Ok(()));
        // ...then they are gone.
        assert!(
            !state
                .review_drafts
                .contains_key(&("group/api".to_string(), 42))
        );
        assert!(detail(&state).review_body.is_empty());
        assert!(
            state
                .notice
                .as_ref()
                .unwrap()
                .text
                .contains("1 comment(s), approved")
        );
    }

    #[test]
    fn a_half_sent_review_drops_its_drafts() {
        let mut state = open(loaded_detail(can_approve(), true));
        on_added_line(&mut state);
        update(&mut state, Action::StartComment);
        type_text(&mut state, "Typo");
        update(&mut state, Action::SaveDraft);
        let comments = state.review_drafts[&("group/api".to_string(), 42)].clone();
        let action = MrAction::SubmitReview {
            body: String::new(),
            verdict: Verdict::Approve,
            comments,
        };
        // Published, then the approval was refused: the drafts are gone, the error shown.
        finish(
            &mut state,
            action,
            Err(ApiError::PartlyDone(
                "comments sent, but no approval".into(),
            )),
        );
        assert!(
            !state
                .review_drafts
                .contains_key(&("group/api".to_string(), 42))
        );
        assert!(state.notice.as_ref().unwrap().is_error);

        // Any other failure keeps them for another try.
        update(&mut state, Action::StartComment);
        type_text(&mut state, "Again");
        update(&mut state, Action::SaveDraft);
        let comments = state.review_drafts[&("group/api".to_string(), 42)].clone();
        let action = MrAction::SubmitReview {
            body: String::new(),
            verdict: Verdict::Comment,
            comments,
        };
        finish(&mut state, action, Err(ApiError::Forbidden("nope".into())));
        assert_eq!(state.review_drafts[&("group/api".to_string(), 42)].len(), 1);
    }

    #[test]
    fn an_empty_review_is_not_sent() {
        let mut state = open(loaded_detail(can_approve(), true));
        update(&mut state, Action::StartReview);
        assert!(update(&mut state, Action::SubmitComment).is_none());
        assert!(
            state
                .notice
                .as_ref()
                .unwrap()
                .text
                .contains("Nothing to send")
        );
    }

    #[test]
    fn drafts_survive_leaving_the_merge_request_and_quitting_asks_first() {
        let mut state = open(loaded_detail(can_approve(), true));
        on_added_line(&mut state);
        update(&mut state, Action::StartComment);
        type_text(&mut state, "Typo");
        update(&mut state, Action::SaveDraft);
        update(&mut state, Action::FocusList);
        update(&mut state, Action::CloseDetail);
        assert!(state.detail.is_none());
        assert_eq!(state.review_drafts[&("group/api".to_string(), 42)].len(), 1);

        update(&mut state, Action::Quit);
        assert!(!state.should_quit);
        assert!(
            state
                .notice
                .as_ref()
                .unwrap()
                .text
                .contains("1 review comment(s) not sent")
        );
        update(&mut state, Action::Quit);
        assert!(state.should_quit, "the second q quits");
    }

    #[test]
    fn c_outside_the_diff_is_a_general_comment() {
        let mut state = open(loaded_detail(can_approve(), true));
        update(&mut state, Action::NextSection);
        update(&mut state, Action::StartComment);
        assert!(detail(&state).comment_target.is_none());
        update(&mut state, Action::CommentInput('x'));
        assert!(matches!(
            update(&mut state, Action::SubmitComment),
            Some(Command::RunMrAction {
                action: MrAction::Comment(_),
                ..
            })
        ));
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
