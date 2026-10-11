//! Branches of a repository and comparing a branch with the default branch.
//!
//! The branch list comes from GraphQL: one request gives each branch's last commit, its
//! combined check state and its open pull request, which REST would need one request per
//! branch for.

use chrono::{DateTime, Utc};
use serde::Deserialize;

use super::rest::{self, RawFile};
use crate::forge::{Branch, BranchPage, Comparison, sort_branches};

pub(super) const BRANCHES_QUERY: &str = "
query($owner: String!, $name: String!) {
  repository(owner: $owner, name: $name) {
    url
    defaultBranchRef { name }
    refs(refPrefix: \"refs/heads/\", first: 100,
         orderBy: {field: TAG_COMMIT_DATE, direction: DESC}) {
      nodes {
        name
        target {
          ... on Commit {
            messageHeadline
            committedDate
            author { name user { login } }
            statusCheckRollup { state }
          }
        }
        associatedPullRequests(states: OPEN, first: 1) { nodes { number } }
      }
    }
  }
}";

#[derive(Debug, Deserialize)]
pub(super) struct BranchesData {
    repository: Option<RawRepository>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRepository {
    url: String,
    default_branch_ref: Option<RawName>,
    refs: Nodes<RawRef>,
}

#[derive(Debug, Deserialize)]
struct Nodes<T> {
    nodes: Vec<T>,
}

#[derive(Debug, Deserialize)]
struct RawName {
    name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRef {
    name: String,
    /// Only a `Commit` has these fields; other targets come back as `{}`.
    target: Option<RawTarget>,
    associated_pull_requests: Nodes<RawNumber>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawTarget {
    #[serde(default)]
    message_headline: String,
    committed_date: Option<DateTime<Utc>>,
    author: Option<RawAuthor>,
    status_check_rollup: Option<RawRollup>,
}

#[derive(Debug, Deserialize)]
struct RawAuthor {
    name: Option<String>,
    user: Option<RawLogin>,
}

#[derive(Debug, Deserialize)]
struct RawLogin {
    login: String,
}

#[derive(Debug, Deserialize)]
struct RawRollup {
    state: String,
}

#[derive(Debug, Deserialize)]
struct RawNumber {
    number: u64,
}

impl BranchesData {
    /// `None` when the repository does not exist or the token cannot see it.
    pub(super) fn into_page(self) -> Option<BranchPage> {
        let repository = self.repository?;
        let default_branch = repository.default_branch_ref.map(|r| r.name);
        let mut branches: Vec<Branch> = repository
            .refs
            .nodes
            .into_iter()
            .map(|r| {
                let target = r.target.unwrap_or_default();
                Branch {
                    default: default_branch.as_deref() == Some(r.name.as_str()),
                    commit_title: target.message_headline,
                    // The GitHub login when the author is a known user, else the git name.
                    commit_author: target
                        .author
                        .and_then(|a| a.user.map(|u| u.login).or(a.name)),
                    committed_at: target.committed_date,
                    pipeline_status: target
                        .status_check_rollup
                        .map(|r| super::client::rollup_status(&r.state)),
                    merge_request: r.associated_pull_requests.nodes.first().map(|p| p.number),
                    web_url: format!("{}/tree/{}", repository.url, r.name),
                    name: r.name,
                }
            })
            .collect();
        sort_branches(&mut branches);
        Some(BranchPage {
            default_branch,
            branches,
        })
    }
}

/// `GET /repos/{repo}/compare/{base}...{head}`
#[derive(Debug, Deserialize)]
pub(super) struct RawComparison {
    #[serde(default)]
    total_commits: usize,
    html_url: String,
    /// Missing when nothing changed; GitHub lists at most 300 files.
    #[serde(default)]
    files: Vec<RawFile>,
}

impl RawComparison {
    pub(super) fn into_model(self, base: &str, head: &str) -> Comparison {
        Comparison {
            base: base.to_string(),
            head: head.to_string(),
            commits: self.total_commits,
            files: self.files.into_iter().map(rest::file_diff).collect(),
            web_url: self.html_url,
        }
    }
}
