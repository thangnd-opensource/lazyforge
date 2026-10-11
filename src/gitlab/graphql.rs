//! GraphQL queries and the raw response shapes. We use GraphQL for lists because one
//! request returns everything a row needs (pipeline status, approvals); with REST that
//! would be one or two extra requests per merge request.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::forge::{
    ApiError, MergeRequest, MrList, MrPage, PipelineList, PipelinePage, PipelineStatus,
    PipelineSummary, User,
};

const MR_FIELDS: &str = "
fragment MrFields on MergeRequest {
  iid
  title
  draft
  webUrl
  updatedAt
  sourceBranch
  targetBranch
  author { username name }
  project { fullPath }
  headPipeline { status }
  approvedBy { nodes { username name } }
}";

/// Builds the query and its variables. The three "my merge requests" lists are fields on
/// `currentUser`; we alias each of them to `mergeRequests` so they share one response type.
pub(crate) fn merge_requests_query(list: &MrList, first: u32) -> (String, Value) {
    let args = "state: opened, sort: UPDATED_DESC, first: $first";
    let user_field = match list {
        MrList::AssignedToMe => "assignedMergeRequests",
        MrList::ReviewRequested => "reviewRequestedMergeRequests",
        MrList::AuthoredByMe => "authoredMergeRequests",
        MrList::Project(path) => {
            let query = format!(
                "query($first: Int!, $project: ID!) {{
  currentUser {{ username name }}
  project(fullPath: $project) {{
    mergeRequests({args}) {{ nodes {{ ...MrFields }} }}
  }}
}}{MR_FIELDS}"
            );
            return (query, json!({ "first": first, "project": path }));
        }
    };
    let query = format!(
        "query($first: Int!) {{
  currentUser {{
    username
    name
    mergeRequests: {user_field}({args}) {{ nodes {{ ...MrFields }} }}
  }}
}}{MR_FIELDS}"
    );
    (query, json!({ "first": first }))
}

const PIPELINE_FIELDS: &str = "
fragment PipelineFields on Pipeline {
  id status ref sha createdAt finishedAt duration path
  user { username }
}";

/// Pipelines per project for the "my projects" lists, and for a single project. Kept small:
/// nested lists multiply the cost of a query.
const PIPELINES_PER_PROJECT: u32 = 5;
const PIPELINES_PER_SINGLE_PROJECT: u32 = 30;

/// `username`: only pipelines triggered by this user (for `PipelineList::Mine`).
/// `project_ids`: for `Mine` and `Recent`, the projects to look at (see
/// `GitLabClient::pipelines` for why they are picked with REST first).
pub(crate) fn pipelines_query(
    list: &PipelineList,
    username: Option<&str>,
    project_ids: &[u64],
) -> (String, Value) {
    match list {
        PipelineList::Project(path) => {
            let query = format!(
                "query($project: ID!, $perProject: Int!, $username: String) {{
  currentUser {{ username name }}
  project(fullPath: $project) {{
    id fullPath
    pipelines(first: $perProject, username: $username) {{ nodes {{ ...PipelineFields }} }}
  }}
}}{PIPELINE_FIELDS}"
            );
            let vars = json!({
                "project": path,
                "perProject": PIPELINES_PER_SINGLE_PROJECT,
                "username": username,
            });
            (query, vars)
        }
        PipelineList::Mine | PipelineList::Recent => {
            let query = format!(
                "query($ids: [ID!], $count: Int!, $perProject: Int!, $username: String) {{
  currentUser {{ username name }}
  projects(ids: $ids, first: $count) {{
    nodes {{
      id fullPath
      pipelines(first: $perProject, username: $username) {{ nodes {{ ...PipelineFields }} }}
    }}
  }}
}}{PIPELINE_FIELDS}"
            );
            let ids: Vec<String> = project_ids
                .iter()
                .map(|id| format!("gid://gitlab/Project/{id}"))
                .collect();
            let vars = json!({
                "ids": ids,
                "count": project_ids.len(),
                "perProject": PIPELINES_PER_PROJECT,
                "username": username,
            });
            (query, vars)
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PipelineListData {
    current_user: Option<RawAuthor>,
    projects: Option<Connection<RawPipelineProject>>,
    project: Option<RawPipelineProject>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPipelineProject {
    /// `gid://gitlab/Project/123`
    id: String,
    full_path: String,
    /// `null` when CI is disabled for the project.
    pipelines: Option<Connection<RawPipelineNode>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPipelineNode {
    /// `gid://gitlab/Ci::Pipeline/123`
    id: String,
    status: PipelineStatus,
    #[serde(rename = "ref")]
    git_ref: Option<String>,
    sha: Option<String>,
    created_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
    duration: Option<u64>,
    /// `/group/project/-/pipelines/123`
    path: Option<String>,
    user: Option<RawUsername>,
}

#[derive(Debug, Deserialize)]
struct RawUsername {
    username: String,
}

/// The number at the end of a global id like `gid://gitlab/Project/123`.
fn gid_number(gid: &str) -> Result<u64, ApiError> {
    gid.rsplit('/')
        .next()
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| ApiError::UnexpectedResponse(format!("invalid id `{gid}`")))
}

impl PipelineListData {
    /// `web_base` is the server URL (ending with `/`), used to build links from `path`.
    pub(crate) fn into_page(
        self,
        list: &PipelineList,
        web_base: &url::Url,
    ) -> Result<PipelinePage, ApiError> {
        let user = self.current_user.ok_or(ApiError::Unauthorized)?;
        let projects = match list {
            PipelineList::Project(path) => vec![
                self.project
                    .ok_or_else(|| ApiError::NotFound(format!("project `{path}`")))?,
            ],
            _ => self
                .projects
                .map(|c| c.nodes.into_iter().flatten().collect())
                .unwrap_or_default(),
        };

        let mut pipelines = Vec::new();
        for project in projects {
            let project_id = gid_number(&project.id)?.to_string();
            let nodes = project.pipelines.map(|c| c.nodes).unwrap_or_default();
            for node in nodes.into_iter().flatten() {
                let web_url = node
                    .path
                    .as_deref()
                    .and_then(|path| web_base.join(path.trim_start_matches('/')).ok())
                    .map(String::from)
                    .unwrap_or_default();
                pipelines.push(PipelineSummary {
                    id: gid_number(&node.id)?,
                    name: None,
                    project_id: project_id.clone(),
                    project_path: project.full_path.clone(),
                    status: node.status,
                    git_ref: node.git_ref.unwrap_or_default(),
                    sha: node.sha.unwrap_or_default(),
                    user: node.user.map(|u| u.username),
                    created_at: node.created_at,
                    finished_at: node.finished_at,
                    duration: node.duration,
                    web_url,
                });
            }
        }
        // Several projects are merged into one list: newest first.
        pipelines.sort_by_key(|p| std::cmp::Reverse(p.created_at));
        Ok(PipelinePage {
            current_user: user.into(),
            pipelines,
        })
    }
}

/// The standard GraphQL envelope: `{"data": ..., "errors": [...]}`.
#[derive(Debug, Deserialize)]
pub(crate) struct Response<T> {
    pub data: Option<T>,
    #[serde(default)]
    pub errors: Vec<ErrorMessage>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ErrorMessage {
    pub message: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MrListData {
    current_user: Option<RawUser>,
    project: Option<RawProject>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawUser {
    username: String,
    name: String,
    merge_requests: Option<Connection<RawMergeRequest>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawProject {
    merge_requests: Connection<RawMergeRequest>,
}

/// GraphQL lists are `{"nodes": [...]}` and items are allowed to be `null`.
#[derive(Debug, Deserialize)]
struct Connection<T> {
    nodes: Vec<Option<T>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawMergeRequest {
    /// A string in GraphQL, a number in REST.
    iid: String,
    title: String,
    draft: bool,
    web_url: String,
    updated_at: DateTime<Utc>,
    source_branch: String,
    target_branch: String,
    /// `null` when the author's account was deleted.
    author: Option<RawAuthor>,
    project: RawProjectRef,
    head_pipeline: Option<RawPipeline>,
    approved_by: Option<Connection<RawAuthor>>,
}

#[derive(Debug, Deserialize)]
struct RawAuthor {
    username: String,
    name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawProjectRef {
    full_path: String,
}

#[derive(Debug, Deserialize)]
struct RawPipeline {
    status: PipelineStatus,
}

impl From<RawAuthor> for User {
    fn from(raw: RawAuthor) -> Self {
        User {
            username: raw.username,
            name: raw.name,
        }
    }
}

impl MrListData {
    pub(crate) fn into_page(self, list: &MrList) -> Result<MrPage, ApiError> {
        // `currentUser` is null when the request was not authenticated.
        let user = self.current_user.ok_or(ApiError::Unauthorized)?;
        let connection = match list {
            MrList::Project(path) => {
                self.project
                    .ok_or_else(|| ApiError::NotFound(format!("project `{path}`")))?
                    .merge_requests
            }
            _ => user
                .merge_requests
                .ok_or_else(|| ApiError::UnexpectedResponse("merge request list missing".into()))?,
        };
        let merge_requests = connection
            .nodes
            .into_iter()
            .flatten()
            .map(RawMergeRequest::into_model)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(MrPage {
            current_user: User {
                username: user.username,
                name: user.name,
            },
            merge_requests,
        })
    }
}

impl RawMergeRequest {
    fn into_model(self) -> Result<MergeRequest, ApiError> {
        let iid = self
            .iid
            .parse()
            .map_err(|_| ApiError::UnexpectedResponse(format!("invalid iid `{}`", self.iid)))?;
        let author = self.author.map(User::from).unwrap_or_else(|| User {
            username: "ghost".into(),
            name: "Deleted user".into(),
        });
        let approved_by = self
            .approved_by
            .map(|c| c.nodes.into_iter().flatten().map(|u| u.username).collect())
            .unwrap_or_default();
        Ok(MergeRequest {
            iid,
            title: self.title,
            draft: self.draft,
            author,
            project_path: self.project.full_path,
            source_branch: self.source_branch,
            target_branch: self.target_branch,
            web_url: self.web_url,
            updated_at: self.updated_at,
            pipeline_status: self.head_pipeline.map(|p| p.status),
            approved_by,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mr_json(iid: &str, pipeline: Value) -> Value {
        json!({
            "iid": iid, "title": "Fix token refresh race", "draft": false,
            "webUrl": "https://gitlab.com/group/api/-/merge_requests/42",
            "updatedAt": "2026-10-10T08:15:30Z",
            "sourceBranch": "fix/token", "targetBranch": "main",
            "author": {"username": "hoang", "name": "Hoang"},
            "project": {"fullPath": "group/api"},
            "headPipeline": pipeline,
            "approvedBy": {"nodes": [{"username": "linh", "name": "Linh"}]}
        })
    }

    fn parse(data: Value, list: &MrList) -> Result<MrPage, ApiError> {
        serde_json::from_value::<MrListData>(data)
            .unwrap()
            .into_page(list)
    }

    #[test]
    fn parses_user_list() {
        let data = json!({"currentUser": {
            "username": "thang", "name": "Thang",
            "mergeRequests": {"nodes": [mr_json("42", json!({"status": "FAILED"})), null]}
        }});
        let page = parse(data, &MrList::AssignedToMe).unwrap();
        assert_eq!(page.current_user.username, "thang");
        assert_eq!(page.merge_requests.len(), 1);
        let mr = &page.merge_requests[0];
        assert_eq!(mr.iid, 42);
        assert_eq!(mr.project_path, "group/api");
        assert_eq!(mr.pipeline_status, Some(PipelineStatus::Failed));
        assert_eq!(mr.approved_by, vec!["linh"]);
    }

    #[test]
    fn parses_project_list_and_unknown_pipeline_status() {
        let data = json!({
            "currentUser": {"username": "thang", "name": "Thang"},
            "project": {"mergeRequests": {"nodes": [mr_json("7", json!({"status": "SOMETHING_NEW"}))]}}
        });
        let page = parse(data, &MrList::Project("group/api".into())).unwrap();
        assert_eq!(
            page.merge_requests[0].pipeline_status,
            Some(PipelineStatus::Unknown)
        );
    }

    #[test]
    fn missing_pipeline_and_deleted_author() {
        let mut mr = mr_json("1", Value::Null);
        mr["author"] = Value::Null;
        let data = json!({"currentUser": {"username": "t", "name": "T", "mergeRequests": {"nodes": [mr]}}});
        let page = parse(data, &MrList::AuthoredByMe).unwrap();
        assert_eq!(page.merge_requests[0].pipeline_status, None);
        assert_eq!(page.merge_requests[0].author.username, "ghost");
    }

    #[test]
    fn null_current_user_means_unauthorized() {
        let err = parse(json!({"currentUser": null}), &MrList::AssignedToMe).unwrap_err();
        assert!(matches!(err, ApiError::Unauthorized));
    }

    #[test]
    fn missing_project_is_not_found() {
        let data = json!({"currentUser": {"username": "t", "name": "T"}, "project": null});
        let err = parse(data, &MrList::Project("nope/nope".into())).unwrap_err();
        assert!(err.to_string().contains("nope/nope"));
    }

    #[test]
    fn parses_a_real_project_pipeline_list() {
        // Trimmed from gitlab.com: `project(fullPath: "gitlab-org/cli") { pipelines }`.
        let data = json!({
            "currentUser": {"username": "thang", "name": "Thang"},
            "project": {"id": "gid://gitlab/Project/34675721", "fullPath": "gitlab-org/cli",
                "pipelines": {"nodes": [
                    {"id": "gid://gitlab/Ci::Pipeline/2932792983", "status": "SUCCESS",
                     "ref": "refs/workloads/f4716a2bf68", "sha": "d667566",
                     "createdAt": "2026-10-10T04:10:02Z", "finishedAt": "2026-10-10T04:11:25Z",
                     "duration": 80, "path": "/gitlab-org/cli/-/pipelines/2932792983",
                     "user": {"username": "duo-code-review-gitlab-org"}},
                    {"id": "gid://gitlab/Ci::Pipeline/2933081394", "status": "RUNNING",
                     "ref": "main", "sha": "9a7963a", "createdAt": "2026-10-10T07:46:01Z",
                     "finishedAt": null, "duration": null,
                     "path": "/gitlab-org/cli/-/pipelines/2933081394", "user": null}
                ]}}
        });
        let host = url::Url::parse("https://gitlab.com/").unwrap();
        let page = serde_json::from_value::<PipelineListData>(data)
            .unwrap()
            .into_page(&PipelineList::Project("gitlab-org/cli".into()), &host)
            .unwrap();
        let p = &page.pipelines[0];
        assert_eq!(p.id, 2933081394, "newest first");
        assert_eq!(p.project_id, "34675721");
        assert_eq!(p.status, PipelineStatus::Running);
        assert_eq!(
            p.web_url,
            "https://gitlab.com/gitlab-org/cli/-/pipelines/2933081394"
        );
        assert_eq!(page.pipelines[1].duration, Some(80));
        assert_eq!(
            page.pipelines[1].user.as_deref(),
            Some("duo-code-review-gitlab-org")
        );
    }

    #[test]
    fn merges_pipelines_of_several_projects_and_skips_projects_without_ci() {
        let node = |id: u64, at: &str| {
            json!({"id": format!("gid://gitlab/Ci::Pipeline/{id}"),
            "status": "FAILED", "ref": "main", "sha": "x", "createdAt": at, "finishedAt": null,
            "duration": null, "path": format!("/p/-/pipelines/{id}"), "user": {"username": "thang"}})
        };
        let data = json!({
            "currentUser": {"username": "thang", "name": "Thang"},
            "projects": {"nodes": [
                {"id": "gid://gitlab/Project/1", "fullPath": "a/one",
                 "pipelines": {"nodes": [node(10, "2026-10-10T01:00:00Z")]}},
                {"id": "gid://gitlab/Project/2", "fullPath": "a/no-ci", "pipelines": null},
                {"id": "gid://gitlab/Project/3", "fullPath": "a/three",
                 "pipelines": {"nodes": [node(30, "2026-10-10T03:00:00Z")]}}
            ]}
        });
        let host = url::Url::parse("https://gitlab.example.com/gitlab/").unwrap();
        let page = serde_json::from_value::<PipelineListData>(data)
            .unwrap()
            .into_page(&PipelineList::Recent, &host)
            .unwrap();
        let ids: Vec<_> = page
            .pipelines
            .iter()
            .map(|p| (p.project_path.as_str(), p.id))
            .collect();
        assert_eq!(ids, vec![("a/three", 30), ("a/one", 10)]);
        // Sub-path installs keep their prefix in links.
        assert_eq!(
            page.pipelines[0].web_url,
            "https://gitlab.example.com/gitlab/p/-/pipelines/30"
        );
    }

    #[test]
    fn pipeline_queries_pass_the_username_filter() {
        let (query, vars) = pipelines_query(&PipelineList::Mine, Some("thang"), &[7, 9]);
        assert!(query.contains("projects(ids: $ids"));
        assert_eq!(vars["username"], "thang");
        assert_eq!(
            vars["ids"],
            json!(["gid://gitlab/Project/7", "gid://gitlab/Project/9"])
        );
        // No `sort` argument: on gitlab.com, sorting `projects` makes the query time out.
        assert!(!query.contains("sort"));
        let (query, vars) = pipelines_query(&PipelineList::Project("a/b".into()), None, &[]);
        assert!(query.contains("project(fullPath: $project)"));
        assert!(vars["username"].is_null());
    }

    #[test]
    fn queries_alias_user_lists_to_merge_requests() {
        let (query, vars) = merge_requests_query(&MrList::ReviewRequested, 50);
        assert!(query.contains("mergeRequests: reviewRequestedMergeRequests("));
        assert_eq!(vars, json!({"first": 50}));

        let (query, vars) = merge_requests_query(&MrList::Project("a/b".into()), 50);
        assert!(query.contains("project(fullPath: $project)"));
        assert_eq!(vars["project"], "a/b");
    }
}
