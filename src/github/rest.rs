//! Raw REST shapes for a pull request's detail screen, and their conversion into the shared
//! models in `crate::forge`.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::forge::diff::{DiffLineKind, parse_unified_diff};
use crate::forge::{
    Approvals, DiffRefs, Discussion, FileDiff, MrDetail, Note, NotePosition, PipelineStatus, User,
};

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RawLogin {
    pub login: String,
}

/// `GET /repos/{repo}/pulls/{number}`
#[derive(Debug, Deserialize)]
pub(crate) struct RawPullRequest {
    pub number: u64,
    pub title: String,
    pub body: Option<String>,
    /// `open` or `closed`.
    pub state: String,
    #[serde(default)]
    pub merged: bool,
    #[serde(default)]
    pub draft: bool,
    pub user: Option<RawLogin>,
    pub head: RawRef,
    pub base: RawRef,
    pub html_url: String,
    /// `null` while GitHub is still computing it.
    pub mergeable: Option<bool>,
    /// `clean`, `dirty` (conflicts), `blocked`, `behind`, `unstable`, `draft`, `unknown`…
    pub mergeable_state: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawRef {
    #[serde(rename = "ref")]
    pub name: String,
    pub sha: String,
}

/// `GET /repos/{repo}/pulls/{number}/files`
#[derive(Debug, Deserialize)]
pub(crate) struct RawFile {
    pub filename: String,
    pub previous_filename: Option<String>,
    /// `added`, `removed`, `modified`, `renamed`, `copied`, `changed`, `unchanged`.
    pub status: String,
    /// Missing for binary files and for diffs too large to show.
    pub patch: Option<String>,
    #[serde(default)]
    pub additions: usize,
    #[serde(default)]
    pub deletions: usize,
}

/// `GET /repos/{repo}/pulls/{number}/reviews`
#[derive(Debug, Deserialize)]
pub(crate) struct RawReview {
    pub id: u64,
    pub user: Option<RawLogin>,
    /// `APPROVED`, `CHANGES_REQUESTED`, `COMMENTED`, `DISMISSED`, `PENDING`.
    pub state: String,
    #[serde(default)]
    pub body: String,
    pub submitted_at: Option<DateTime<Utc>>,
}

/// `GET /repos/{repo}/issues/{number}/comments`
#[derive(Debug, Deserialize)]
pub(crate) struct RawIssueComment {
    pub id: u64,
    #[serde(default)]
    pub body: String,
    pub user: Option<RawLogin>,
    pub created_at: DateTime<Utc>,
}

/// `GET /repos/{repo}/pulls/{number}/comments`: comments on lines of the diff.
#[derive(Debug, Deserialize)]
pub(crate) struct RawReviewComment {
    pub id: u64,
    #[serde(default)]
    pub body: String,
    pub user: Option<RawLogin>,
    pub created_at: DateTime<Utc>,
    pub path: String,
    /// `null` when the line is no longer in the diff (outdated comment).
    pub line: Option<u32>,
    pub original_line: Option<u32>,
    /// `RIGHT` (the new file: added and unchanged lines) or `LEFT` (the old file).
    pub side: Option<String>,
    /// Replies point at the first comment of their thread.
    pub in_reply_to_id: Option<u64>,
}

/// `GET /repos/{repo}`: what the current user may do, and which merge methods are allowed.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct RawRepository {
    #[serde(default)]
    pub permissions: RawPermissions,
    #[serde(default = "yes")]
    pub allow_merge_commit: bool,
    #[serde(default)]
    pub allow_squash_merge: bool,
    #[serde(default)]
    pub allow_rebase_merge: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct RawPermissions {
    #[serde(default)]
    pub push: bool,
}

impl RawRepository {
    /// The merge method to use: a merge commit when allowed, else squash, else rebase.
    pub(crate) fn merge_method(&self) -> &'static str {
        if self.allow_merge_commit {
            "merge"
        } else if self.allow_squash_merge {
            "squash"
        } else if self.allow_rebase_merge {
            "rebase"
        } else {
            "merge"
        }
    }
}

fn login(user: Option<RawLogin>) -> String {
    user.map_or_else(|| "ghost".to_string(), |u| u.login)
}

fn user(login: String) -> User {
    User {
        name: login.clone(),
        username: login,
    }
}

/// Everything fetched for the detail screen, in one place for `into_detail`.
pub(crate) struct RawDetail {
    pub pull: RawPullRequest,
    pub files: Vec<RawFile>,
    pub reviews: Vec<RawReview>,
    pub issue_comments: Vec<RawIssueComment>,
    pub review_comments: Vec<RawReviewComment>,
    pub repository: RawRepository,
    pub me: String,
    pub checks: Option<PipelineStatus>,
}

pub(crate) fn into_detail(raw: RawDetail) -> MrDetail {
    let pull = raw.pull;
    let author = login(pull.user);
    let state = match (pull.state.as_str(), pull.merged) {
        (_, true) => "merged",
        ("open", _) => "opened",
        _ => "closed",
    }
    .to_string();
    let has_conflicts =
        pull.mergeable == Some(false) || pull.mergeable_state.as_deref() == Some("dirty");
    let merge_status = pull.mergeable_state.map(|s| match s.as_str() {
        "clean" => "mergeable".to_string(),
        "dirty" => "conflict".to_string(),
        "behind" => "behind the base branch".to_string(),
        "blocked" => "blocked by branch protection".to_string(),
        "unstable" => "mergeable, but some checks failed".to_string(),
        "unknown" => "still being checked".to_string(),
        _ => s,
    });

    let approvals = approvals(&raw.reviews, &raw.me, &author, &state);
    MrDetail {
        iid: pull.number,
        title: pull.title,
        description: pull.body.unwrap_or_default(),
        state,
        draft: pull.draft,
        author: user(author),
        source_branch: pull.head.name,
        target_branch: pull.base.name,
        web_url: pull.html_url,
        // GitHub places line comments with the head commit alone.
        diff_refs: Some(DiffRefs {
            head_sha: pull.head.sha.clone(),
            base_sha: Some(pull.base.sha.clone()),
            start_sha: None,
        }),
        sha: Some(pull.head.sha),
        merge_status,
        has_conflicts,
        can_merge: raw.repository.permissions.push,
        pipeline_status: raw.checks,
        approvals,
        files: raw.files.into_iter().map(file_diff).collect(),
        discussions: discussions(raw.reviews, raw.issue_comments, raw.review_comments),
    }
}

/// A reviewer's latest review decides: approving, then requesting changes, means "not
/// approved". Plain comments do not change a decision.
fn approvals(reviews: &[RawReview], me: &str, author: &str, state: &str) -> Approvals {
    let mut latest: Vec<(&str, &str)> = Vec::new();
    for review in reviews {
        let Some(user) = review.user.as_ref() else {
            continue;
        };
        if !matches!(
            review.state.as_str(),
            "APPROVED" | "CHANGES_REQUESTED" | "DISMISSED"
        ) {
            continue;
        }
        match latest.iter_mut().find(|(u, _)| *u == user.login) {
            Some(entry) => entry.1 = &review.state,
            None => latest.push((&user.login, &review.state)),
        }
    }
    let approved_by: Vec<User> = latest
        .iter()
        .filter(|(_, state)| *state == "APPROVED")
        .map(|(login, _)| user(login.to_string()))
        .collect();
    Approvals {
        user_has_approved: approved_by.iter().any(|u| u.username == me),
        // GitHub does not let authors approve their own pull request.
        user_can_approve: me != author && state == "opened",
        approved_by,
        approvals_required: None,
        approvals_left: None,
    }
}

pub(crate) fn file_diff(raw: RawFile) -> FileDiff {
    let lines = raw
        .patch
        .as_deref()
        .map(parse_unified_diff)
        .unwrap_or_default();
    let count = |kind| lines.iter().filter(|l| l.kind == kind).count();
    let (added, removed) = if lines.is_empty() {
        (raw.additions, raw.deletions)
    } else {
        (count(DiffLineKind::Added), count(DiffLineKind::Removed))
    };
    FileDiff {
        old_path: raw
            .previous_filename
            .unwrap_or_else(|| raw.filename.clone()),
        new_path: raw.filename,
        new_file: raw.status == "added",
        deleted_file: raw.status == "removed",
        renamed_file: raw.status == "renamed",
        // No patch for a file with changed lines means GitHub left it out as too large.
        // (Binary files have no patch and no counted lines.)
        too_large: raw.patch.is_none() && raw.additions + raw.deletions > 0,
        lines,
        added,
        removed,
    }
}

fn note(id: u64, body: String, author: String, created_at: DateTime<Utc>) -> Note {
    Note {
        id,
        body,
        author: user(author),
        created_at,
        system: false,
        resolvable: false,
        resolved: false,
        position: None,
    }
}

/// General comments, review summaries and threads on diff lines, oldest first.
fn discussions(
    reviews: Vec<RawReview>,
    issue_comments: Vec<RawIssueComment>,
    review_comments: Vec<RawReviewComment>,
) -> Vec<Discussion> {
    let mut threads: Vec<Discussion> = Vec::new();

    for comment in issue_comments {
        threads.push(Discussion {
            id: format!("comment-{}", comment.id),
            notes: vec![note(
                comment.id,
                comment.body,
                login(comment.user),
                comment.created_at,
            )],
        });
    }

    // A review is worth showing when it decided something or said something.
    for review in reviews {
        let Some(submitted_at) = review.submitted_at else {
            continue;
        };
        let verdict = match review.state.as_str() {
            "APPROVED" => "approved these changes",
            "CHANGES_REQUESTED" => "requested changes",
            _ => "",
        };
        let body = match (verdict, review.body.trim()) {
            ("", "") => continue,
            (verdict, "") => verdict.to_string(),
            ("", body) => body.to_string(),
            (verdict, body) => format!("{verdict}: {body}"),
        };
        threads.push(Discussion {
            id: format!("review-{}", review.id),
            notes: vec![note(review.id, body, login(review.user), submitted_at)],
        });
    }

    // Line comments: replies join the thread of the comment they answer.
    let mut thread_of: HashMap<u64, usize> = HashMap::new();
    for comment in review_comments {
        let thread = comment
            .in_reply_to_id
            .and_then(|parent| thread_of.get(&parent).copied());
        let mut item = note(
            comment.id,
            comment.body,
            login(comment.user),
            comment.created_at,
        );
        let left = comment.side.as_deref() == Some("LEFT");
        item.position = Some(match comment.line {
            Some(line) => NotePosition {
                new_path: Some(comment.path),
                old_path: None,
                new_line: (!left).then_some(line),
                old_line: left.then_some(line),
                outdated: false,
            },
            // No longer in the diff: keep the line it was written on, for the label only.
            None => NotePosition {
                new_path: Some(comment.path),
                old_path: None,
                new_line: comment.original_line,
                old_line: None,
                outdated: true,
            },
        });
        let index = match thread {
            Some(index) => {
                threads[index].notes.push(item);
                index
            }
            None => {
                threads.push(Discussion {
                    id: format!("thread-{}", comment.id),
                    notes: vec![item],
                });
                threads.len() - 1
            }
        };
        thread_of.insert(comment.id, index);
    }

    threads.sort_by_key(|d| d.notes.first().map(|n| n.created_at));
    threads
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn review(id: u64, who: &str, state: &str, body: &str, minute: u32) -> RawReview {
        serde_json::from_value(json!({
            "id": id, "user": {"login": who}, "state": state, "body": body,
            "submitted_at": format!("2026-10-10T08:{minute:02}:00Z")
        }))
        .unwrap()
    }

    #[test]
    fn latest_review_per_person_decides_approval() {
        let reviews = vec![
            review(1, "hoang", "APPROVED", "", 1),
            review(2, "mai", "APPROVED", "", 2),
            review(3, "mai", "CHANGES_REQUESTED", "wait", 3),
            review(4, "hoang", "COMMENTED", "nit", 4),
        ];
        let a = approvals(&reviews, "thang", "linh", "opened");
        let names: Vec<_> = a.approved_by.iter().map(|u| u.username.as_str()).collect();
        assert_eq!(names, vec!["hoang"], "a later comment keeps the approval");
        assert!(!a.user_has_approved);
        assert!(a.user_can_approve);
        // Authors cannot approve their own pull request.
        assert!(!approvals(&reviews, "linh", "linh", "opened").user_can_approve);
    }

    #[test]
    fn line_comments_keep_their_side() {
        let comment = |id: u64, line: Option<u32>, side: &str| -> RawReviewComment {
            serde_json::from_value(json!({
                "id": id, "body": "b", "user": {"login": "hoang"},
                "created_at": "2026-10-10T09:00:00Z", "path": "src/lib.rs",
                "line": line, "original_line": 30, "side": side, "in_reply_to_id": null
            }))
            .unwrap()
        };
        let threads = discussions(
            vec![],
            vec![],
            vec![
                comment(1, Some(12), "RIGHT"),
                comment(2, Some(7), "LEFT"),
                comment(3, None, "RIGHT"),
            ],
        );
        let position = |i: usize| threads[i].position().unwrap().clone();
        assert_eq!(
            (position(0).new_line, position(0).old_line),
            (Some(12), None)
        );
        assert_eq!(
            (position(1).new_line, position(1).old_line),
            (None, Some(7))
        );
        // No longer in the diff: listed with the line it was written on, but not placed.
        assert!(position(2).outdated);
        assert_eq!(position(2).new_line, Some(30));
    }

    #[test]
    fn replies_join_their_thread_and_everything_is_in_time_order() {
        let comment = |id: u64, reply_to: Option<u64>, minute: u32| -> RawReviewComment {
            serde_json::from_value(json!({
                "id": id, "body": format!("c{id}"), "user": {"login": "hoang"},
                "created_at": format!("2026-10-10T09:{minute:02}:00Z"), "path": "src/lib.rs",
                "line": 12, "original_line": 10, "in_reply_to_id": reply_to
            }))
            .unwrap()
        };
        let issue: RawIssueComment = serde_json::from_value(json!({
            "id": 50, "body": "Looks good overall", "user": null,
            "created_at": "2026-10-10T07:00:00Z"
        }))
        .unwrap();
        let reviews = vec![
            review(9, "mai", "APPROVED", "", 30),
            review(8, "mai", "COMMENTED", "", 20), // nothing to show
        ];
        let threads = discussions(
            reviews,
            vec![issue],
            vec![
                comment(100, None, 1),
                comment(101, Some(100), 2),
                comment(102, Some(101), 3),
            ],
        );

        assert_eq!(threads.len(), 3);
        assert_eq!(threads[0].notes[0].body, "Looks good overall");
        assert_eq!(threads[0].notes[0].author.username, "ghost");
        // In time order: the comment (07:00), the approval (08:30), the thread (09:01).
        assert_eq!(threads[1].notes[0].body, "approved these changes");
        assert_eq!(
            threads[2].notes.len(),
            3,
            "a reply to a reply stays in the thread"
        );
        assert_eq!(
            threads[2].notes[0]
                .position
                .as_ref()
                .unwrap()
                .new_path
                .as_deref(),
            Some("src/lib.rs")
        );
    }

    #[test]
    fn files_use_the_patch_or_mark_large_diffs() {
        let file = |patch: Option<&str>, additions: usize| -> RawFile {
            serde_json::from_value(json!({
                "filename": "a.rs", "status": "modified", "patch": patch,
                "additions": additions, "deletions": 0
            }))
            .unwrap()
        };
        let with_patch = file_diff(file(Some("@@ -1 +1,2 @@\n-a\n+b\n+c"), 2));
        assert_eq!((with_patch.added, with_patch.removed), (2, 1));
        assert!(!with_patch.too_large);
        let large = file_diff(file(None, 5000));
        assert!(large.too_large);
        assert_eq!(large.added, 5000);
        let binary = file_diff(file(None, 0));
        assert!(!binary.too_large);
    }

    #[test]
    fn maps_states_and_merge_status() {
        let pull = |state: &str, merged: bool, mergeable_state: &str| -> RawPullRequest {
            serde_json::from_value(json!({
                "number": 7, "title": "t", "body": null, "state": state, "merged": merged,
                "draft": false, "user": {"login": "linh"},
                "head": {"ref": "feat", "sha": "abc"}, "base": {"ref": "main", "sha": "def"},
                "html_url": "u", "mergeable": mergeable_state != "dirty",
                "mergeable_state": mergeable_state
            }))
            .unwrap()
        };
        let detail = |p| {
            into_detail(RawDetail {
                pull: p,
                files: vec![],
                reviews: vec![],
                issue_comments: vec![],
                review_comments: vec![],
                repository: RawRepository::default(),
                me: "thang".into(),
                checks: Some(PipelineStatus::Success),
            })
        };
        let open = detail(pull("open", false, "clean"));
        assert_eq!(open.state, "opened");
        assert_eq!(open.merge_status.as_deref(), Some("mergeable"));
        assert_eq!(open.sha.as_deref(), Some("abc"));
        assert_eq!(open.description, "");
        let conflict = detail(pull("open", false, "dirty"));
        assert!(conflict.has_conflicts);
        assert_eq!(detail(pull("closed", true, "unknown")).state, "merged");
        assert_eq!(detail(pull("closed", false, "unknown")).state, "closed");
    }

    #[test]
    fn merge_method_follows_repository_settings() {
        let repo = |merge: bool, squash: bool| RawRepository {
            permissions: RawPermissions::default(),
            allow_merge_commit: merge,
            allow_squash_merge: squash,
            allow_rebase_merge: true,
        };
        assert_eq!(repo(true, true).merge_method(), "merge");
        assert_eq!(repo(false, true).merge_method(), "squash");
        assert_eq!(repo(false, false).merge_method(), "rebase");
    }
}
