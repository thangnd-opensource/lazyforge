//! Raw REST v4 response shapes for the merge request detail screen, and their conversion
//! into the domain types in `models`.

use serde::Deserialize;

use crate::forge::diff::{DiffLineKind, parse_unified_diff};
use std::collections::HashMap;

use chrono::{DateTime, Utc};

use crate::forge::{
    Approvals, Branch, BranchPage, Comparison, Discussion, FileDiff, MrDetail, PipelineStatus, User,
};

/// `GET /projects/:id/merge_requests/:iid`
#[derive(Debug, Deserialize)]
pub(crate) struct RawMergeRequest {
    pub iid: u64,
    pub title: String,
    /// `null` when the merge request has no description.
    pub description: Option<String>,
    pub state: String,
    #[serde(default)]
    pub draft: bool,
    pub author: User,
    pub source_branch: String,
    pub target_branch: String,
    pub web_url: String,
    pub sha: Option<String>,
    /// GitLab 15.6+. Older versions only have the coarser `merge_status`.
    pub detailed_merge_status: Option<String>,
    pub merge_status: Option<String>,
    #[serde(default)]
    pub has_conflicts: bool,
    pub head_pipeline: Option<RawPipeline>,
    /// Permissions of the current user on this merge request.
    pub user: Option<RawUserPermissions>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawPipeline {
    pub status: PipelineStatus,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawUserPermissions {
    #[serde(default)]
    pub can_merge: bool,
}

/// `GET /projects/:id/merge_requests/:iid/approvals`
#[derive(Debug, Default, Deserialize)]
pub(crate) struct RawApprovals {
    #[serde(default)]
    pub approved_by: Vec<RawApprover>,
    #[serde(default)]
    pub user_has_approved: bool,
    #[serde(default)]
    pub user_can_approve: bool,
    pub approvals_required: Option<u32>,
    pub approvals_left: Option<u32>,
}

/// Each approver is wrapped: `{"user": {...}}`.
#[derive(Debug, Deserialize)]
pub(crate) struct RawApprover {
    pub user: User,
}

/// One item of `GET /projects/:id/merge_requests/:iid/diffs`.
#[derive(Debug, Deserialize)]
pub(crate) struct RawFileDiff {
    pub old_path: String,
    pub new_path: String,
    #[serde(default)]
    pub diff: String,
    #[serde(default)]
    pub new_file: bool,
    #[serde(default)]
    pub deleted_file: bool,
    #[serde(default)]
    pub renamed_file: bool,
    #[serde(default)]
    pub too_large: bool,
    #[serde(default)]
    pub collapsed: bool,
}

/// `GET /projects/:id/merge_requests/:iid/changes`, the older endpoint used as a fallback
/// on GitLab versions without `/diffs` (before 15.7).
#[derive(Debug, Deserialize)]
pub(crate) struct RawChanges {
    pub changes: Vec<RawFileDiff>,
}

/// `GET /projects/:id/repository/branches`
#[derive(Debug, Deserialize)]
pub(crate) struct RawBranch {
    pub name: String,
    #[serde(default)]
    pub default: bool,
    pub web_url: Option<String>,
    pub commit: Option<RawCommit>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawCommit {
    #[serde(default)]
    pub title: String,
    pub author_name: Option<String>,
    pub committed_date: Option<DateTime<Utc>>,
}

/// Open merge requests of a project, to know which branches have one.
#[derive(Debug, Deserialize)]
pub(crate) struct RawOpenMergeRequest {
    pub iid: u64,
    pub source_branch: String,
    pub source_project_id: Option<u64>,
    pub project_id: Option<u64>,
}

/// Recent pipelines of a project, newest first, for the CI column.
#[derive(Debug, Deserialize)]
pub(crate) struct RawRefPipeline {
    #[serde(rename = "ref")]
    pub git_ref: String,
    pub status: PipelineStatus,
}

/// `GET /projects/:id/repository/compare`
#[derive(Debug, Deserialize)]
pub(crate) struct RawCompare {
    #[serde(default)]
    pub commits: Vec<serde_json::Value>,
    #[serde(default)]
    pub diffs: Vec<RawFileDiff>,
    /// GitLab 15.x+.
    pub web_url: Option<String>,
}

/// Puts the branch list together with the open merge requests and recent pipelines.
/// `web_base` builds branch links for servers that do not send `web_url`.
pub(crate) fn into_branch_page(
    raw: Vec<RawBranch>,
    merge_requests: Vec<RawOpenMergeRequest>,
    pipelines: Vec<RawRefPipeline>,
    project_web_url: &str,
) -> BranchPage {
    // Only merge requests from this project: a fork can have a branch of the same name.
    let merge_requests: HashMap<String, u64> = merge_requests
        .into_iter()
        .filter(|mr| match (mr.source_project_id, mr.project_id) {
            (Some(source), Some(target)) => source == target,
            _ => true,
        })
        .map(|mr| (mr.source_branch, mr.iid))
        .collect();
    // Pipelines come newest first: keep the first one seen for each branch.
    let mut statuses: HashMap<String, PipelineStatus> = HashMap::new();
    for pipeline in pipelines {
        statuses.entry(pipeline.git_ref).or_insert(pipeline.status);
    }

    let default_branch = raw.iter().find(|b| b.default).map(|b| b.name.clone());
    let mut branches: Vec<Branch> = raw
        .into_iter()
        .map(|b| {
            let commit = b.commit;
            Branch {
                pipeline_status: statuses.get(&b.name).copied(),
                merge_request: merge_requests.get(&b.name).copied(),
                web_url: b
                    .web_url
                    .unwrap_or_else(|| format!("{project_web_url}/-/tree/{}", b.name)),
                default: b.default,
                commit_title: commit.as_ref().map(|c| c.title.clone()).unwrap_or_default(),
                commit_author: commit.as_ref().and_then(|c| c.author_name.clone()),
                committed_at: commit.and_then(|c| c.committed_date),
                name: b.name,
            }
        })
        .collect();
    crate::forge::sort_branches(&mut branches);
    BranchPage {
        default_branch,
        branches,
    }
}

pub(crate) fn into_comparison(
    raw: RawCompare,
    base: &str,
    head: &str,
    web_url: String,
) -> Comparison {
    Comparison {
        base: base.to_string(),
        head: head.to_string(),
        commits: raw.commits.len(),
        files: raw.diffs.into_iter().map(FileDiff::from).collect(),
        web_url: raw.web_url.unwrap_or(web_url),
    }
}

impl From<RawApprovals> for Approvals {
    fn from(raw: RawApprovals) -> Self {
        Approvals {
            approved_by: raw.approved_by.into_iter().map(|a| a.user).collect(),
            user_has_approved: raw.user_has_approved,
            user_can_approve: raw.user_can_approve,
            approvals_required: raw.approvals_required,
            approvals_left: raw.approvals_left,
        }
    }
}

impl From<RawFileDiff> for FileDiff {
    fn from(raw: RawFileDiff) -> Self {
        let lines = parse_unified_diff(&raw.diff);
        let count = |kind| lines.iter().filter(|l| l.kind == kind).count();
        let (added, removed) = (count(DiffLineKind::Added), count(DiffLineKind::Removed));
        FileDiff {
            old_path: raw.old_path,
            new_path: raw.new_path,
            new_file: raw.new_file,
            deleted_file: raw.deleted_file,
            renamed_file: raw.renamed_file,
            // A collapsed or too-large file comes with an empty diff.
            too_large: raw.too_large || (raw.collapsed && raw.diff.is_empty()),
            lines,
            added,
            removed,
        }
    }
}

pub(crate) fn into_detail(
    mr: RawMergeRequest,
    approvals: RawApprovals,
    files: Vec<RawFileDiff>,
    discussions: Vec<Discussion>,
) -> MrDetail {
    MrDetail {
        iid: mr.iid,
        title: mr.title,
        description: mr.description.unwrap_or_default(),
        state: mr.state,
        draft: mr.draft,
        author: mr.author,
        source_branch: mr.source_branch,
        target_branch: mr.target_branch,
        web_url: mr.web_url,
        sha: mr.sha,
        merge_status: mr.detailed_merge_status.or(mr.merge_status),
        has_conflicts: mr.has_conflicts,
        can_merge: mr.user.is_some_and(|u| u.can_merge),
        pipeline_status: mr.head_pipeline.map(|p| p.status),
        approvals: approvals.into(),
        files: files.into_iter().map(FileDiff::from).collect(),
        discussions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn converts_file_diffs_and_counts_changes() {
        let raw: RawFileDiff = serde_json::from_value(json!({
            "old_path": "src/a.rs", "new_path": "src/a.rs",
            "diff": "@@ -1,2 +1,3 @@\n keep\n-old\n+new\n+more\n",
            "new_file": false, "deleted_file": false, "renamed_file": false
        }))
        .unwrap();
        let file = FileDiff::from(raw);
        assert_eq!((file.added, file.removed), (2, 1));
        assert!(!file.too_large);
    }

    #[test]
    fn empty_collapsed_diff_is_too_large() {
        let raw: RawFileDiff = serde_json::from_value(json!({
            "old_path": "big.json", "new_path": "big.json", "diff": "", "collapsed": true
        }))
        .unwrap();
        assert!(FileDiff::from(raw).too_large);
    }

    #[test]
    fn approvals_unwrap_users() {
        let raw: RawApprovals = serde_json::from_value(json!({
            "approved_by": [{"user": {"id": 1, "username": "linh", "name": "Linh"}}],
            "user_has_approved": true, "user_can_approve": false
        }))
        .unwrap();
        let approvals = Approvals::from(raw);
        assert_eq!(approvals.approved_by[0].username, "linh");
        assert!(approvals.user_has_approved);
        assert_eq!(approvals.approvals_required, None);
    }

    #[test]
    fn branches_get_their_merge_request_and_latest_pipeline() {
        let raw: Vec<RawBranch> = serde_json::from_value(json!([
            {"name": "old", "default": false, "web_url": "https://gl/g/p/-/tree/old",
             "commit": {"title": "Old work", "author_name": "Nam", "committed_date": "2026-01-01T00:00:00Z"}},
            {"name": "feat/x", "default": false,
             "commit": {"title": "Add x", "author_name": "Linh", "committed_date": "2026-10-09T00:00:00Z"}},
            {"name": "main", "default": true, "web_url": "https://gl/g/p/-/tree/main",
             "commit": {"title": "Merge", "author_name": "Thang", "committed_date": "2026-10-01T00:00:00Z"}}
        ]))
        .unwrap();
        let mrs: Vec<RawOpenMergeRequest> = serde_json::from_value(json!([
            {"iid": 7, "source_branch": "feat/x", "source_project_id": 1, "project_id": 1},
            // From a fork that happens to use the same branch name.
            {"iid": 8, "source_branch": "old", "source_project_id": 99, "project_id": 1}
        ]))
        .unwrap();
        let pipelines: Vec<RawRefPipeline> = serde_json::from_value(json!([
            {"ref": "feat/x", "status": "failed"},
            {"ref": "feat/x", "status": "success"},
            {"ref": "main", "status": "success"}
        ]))
        .unwrap();

        let page = into_branch_page(raw, mrs, pipelines, "https://gl/g/p");
        assert_eq!(page.default_branch.as_deref(), Some("main"));
        let names: Vec<&str> = page.branches.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(
            names,
            ["main", "feat/x", "old"],
            "default first, then newest"
        );
        let feat = &page.branches[1];
        assert_eq!(feat.merge_request, Some(7));
        assert_eq!(
            feat.pipeline_status,
            Some(PipelineStatus::Failed),
            "the newest pipeline"
        );
        assert_eq!(feat.web_url, "https://gl/g/p/-/tree/feat/x");
        assert_eq!(
            page.branches[2].merge_request, None,
            "fork merge requests are not ours"
        );
    }

    #[test]
    fn falls_back_to_merge_status_and_handles_null_description() {
        let mr: RawMergeRequest = serde_json::from_value(json!({
            "iid": 5, "title": "t", "description": null, "state": "opened",
            "author": {"id": 1, "username": "a", "name": "A"},
            "source_branch": "f", "target_branch": "main", "web_url": "u",
            "sha": "abc", "merge_status": "can_be_merged",
            "head_pipeline": {"id": 9, "status": "running"},
            "user": {"can_merge": true}
        }))
        .unwrap();
        let detail = into_detail(mr, RawApprovals::default(), vec![], vec![]);
        assert_eq!(detail.description, "");
        assert_eq!(detail.merge_status.as_deref(), Some("can_be_merged"));
        assert_eq!(detail.pipeline_status, Some(PipelineStatus::Running));
        assert!(detail.can_merge);
    }
}
