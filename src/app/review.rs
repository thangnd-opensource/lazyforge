//! Reviews: line comments kept as drafts in lazyforge, then sent together with a verdict
//! (comment, approve, request changes) and a summary, so the author gets one notification.
//!
//! Drafts live in `AppState::review_drafts`, per merge request: leaving one and coming back
//! keeps them, and quitting with drafts left asks first.

use std::collections::HashMap;

use super::detail::{CommentTarget, DetailState, Popup, Section};
use super::{Action, AppState, Command, Notice};
use crate::forge::{DraftComment, MrAction, Verdict};

/// Drafts per merge request: `(project path, iid)`.
pub type Drafts = HashMap<(String, u64), Vec<DraftComment>>;

fn key(detail: &DetailState) -> (String, u64) {
    (detail.project_path.clone(), detail.iid)
}

/// The drafts of the merge request on screen.
pub fn of<'a>(drafts: &'a Drafts, detail: &DetailState) -> &'a [DraftComment] {
    drafts.get(&key(detail)).map_or(&[], Vec::as_slice)
}

/// All drafts, of every merge request.
pub fn total(drafts: &Drafts) -> usize {
    drafts.values().map(Vec::len).sum()
}

/// The draft on the diff line under the cursor, if any.
pub(super) fn on_cursor(detail: &DetailState, drafts: &Drafts) -> Option<usize> {
    if detail.section != Section::Files || !detail.diff_focused {
        return None;
    }
    let file = detail.selected_file()?;
    let line = file.lines.get(detail.diff_cursor)?;
    of(drafts, detail).iter().position(|d| d.is_on(file, line))
}

/// `Ctrl+D` in the comment editor: keep the comment for the review instead of sending it.
pub(super) fn save_draft(
    detail: &mut DetailState,
    drafts: &mut Drafts,
    notice: &mut Option<Notice>,
) {
    let Some(CommentTarget::Line(position)) = detail.comment_target.clone() else {
        *notice = Some(Notice::error(
            "Only new comments on a line can wait for the review; Ctrl+S sends this one now",
        ));
        return;
    };
    let body = detail.draft.trim().to_string();
    let list = drafts.entry(key(detail)).or_default();
    let existing = list.iter().position(|d| d.position == position);
    match (existing, body.is_empty()) {
        // Emptied while editing: the draft goes away.
        (Some(index), true) => {
            list.remove(index);
        }
        (None, true) => {
            *notice = Some(Notice::error("Write something first"));
            return;
        }
        (Some(index), false) => list[index].body = body,
        (None, false) => list.push(DraftComment { position, body }),
    }
    let count = list.len();
    detail.popup = None;
    detail.draft.clear();
    detail.comment_target = None;
    detail.editing_draft = false;
    *notice = Some(Notice::info(format!(
        "{count} comment(s) in your review · R to finish and send it"
    )));
}

/// `d` on a line with a draft: drop it.
pub(super) fn delete_draft(detail: &DetailState, drafts: &mut Drafts, notice: &mut Option<Notice>) {
    let Some(index) = on_cursor(detail, drafts) else {
        *notice = Some(Notice::error("No draft on this line"));
        return;
    };
    let list = drafts.entry(key(detail)).or_default();
    list.remove(index);
    *notice = Some(Notice::info(format!("Draft removed ({} left)", list.len())));
}

/// `R`: the popup that ends the review.
pub(super) fn start(detail: &mut DetailState) {
    detail.popup = Some(Popup::FinishReview);
}

/// Keys of the "finish review" popup: typing writes the summary, `Tab` changes the verdict,
/// `Ctrl+S` sends, `Esc` closes (the summary and drafts are kept).
pub(super) fn update_finish(state: &mut AppState, action: Action) -> Option<Command> {
    let detail = state.detail.as_mut()?;
    match action {
        Action::CommentInput(c) => detail.review_body.push(c),
        Action::CommentNewline => detail.review_body.push('\n'),
        Action::CommentBackspace => {
            detail.review_body.pop();
        }
        Action::CycleVerdict => {
            detail.verdict = match detail.verdict {
                Verdict::Comment => Verdict::Approve,
                Verdict::Approve if detail.forge.can_request_changes() => Verdict::RequestChanges,
                Verdict::Approve | Verdict::RequestChanges => Verdict::Comment,
            };
        }
        Action::ClosePopup => detail.popup = None,
        Action::SubmitComment => {
            if let Some(busy) = detail.busy {
                state.notice = Some(Notice::error(format!("Wait: {busy}")));
                return None;
            }
            let comments = of(&state.review_drafts, detail).to_vec();
            let body = detail.review_body.trim().to_string();
            let problem = match detail.verdict {
                Verdict::RequestChanges if body.is_empty() => {
                    Some("Say what should change: write a summary first")
                }
                Verdict::Comment if body.is_empty() && comments.is_empty() => {
                    Some("Nothing to send: add line comments (Ctrl+D) or a summary")
                }
                _ => None,
            };
            if let Some(problem) = problem {
                state.notice = Some(Notice::error(problem));
                return None;
            }
            detail.popup = None;
            return Some(detail.start_action(MrAction::SubmitReview {
                body,
                verdict: detail.verdict,
                comments,
            }));
        }
        _ => {}
    }
    None
}

/// After the review was sent: its drafts and summary are done. Takes the two fields it needs
/// rather than `&mut AppState`, so the caller can keep its borrow of the open detail.
pub(super) fn on_sent(
    drafts: &mut Drafts,
    detail: Option<&mut DetailState>,
    project: &str,
    iid: u64,
) {
    drafts.remove(&(project.to_string(), iid));
    if let Some(detail) = detail {
        detail.review_body.clear();
        detail.verdict = Verdict::Comment;
    }
}
