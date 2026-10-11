use std::time::Duration;

use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use reqwest::{RequestBuilder, Response};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use url::Url;

use super::graphql::{self, MrListData, PipelineListData};
use super::rest::{
    self, RawApprovals, RawBranch, RawChanges, RawCompare, RawFileDiff, RawMergeRequest,
    RawOpenMergeRequest, RawRefPipeline,
};
use crate::forge::{
    ApiError, BranchPage, CiAction, CiSnapshot, Comparison, Discussion, Job, MrAction, MrDetail,
    MrList, MrPage, Pipeline, PipelineList, PipelinePage, Repository, User, sort_jobs,
};

const PAGE_SIZE: u32 = 50;
/// Upper bound for REST pagination (100 items per page), so a huge merge request cannot
/// make us loop forever.
const MAX_PAGES: usize = 20;

/// Cheap to clone: `reqwest::Client` keeps its connection pool behind an `Arc`.
#[derive(Debug, Clone)]
pub struct GitLabClient {
    http: reqwest::Client,
    /// The server itself, e.g. `https://gitlab.com/`, for building web links.
    web_base: Url,
    api_base: Url,
    graphql_url: Url,
}

impl GitLabClient {
    /// `host` must end with `/` (see `config::parse_host`).
    pub fn new(host: &Url, token: &str) -> Result<Self, ApiError> {
        // `Authorization: Bearer` works for personal access tokens on both REST and GraphQL.
        let mut auth = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| ApiError::InvalidToken)?;
        auth.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, auth);

        let http = reqwest::Client::builder()
            .default_headers(headers)
            .user_agent(concat!("lazyforge/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(30))
            .build()?;

        Ok(Self {
            http,
            web_base: host.clone(),
            api_base: host.join("api/v4/")?,
            graphql_url: host.join("api/graphql")?,
        })
    }

    /// Open merge requests, most recently updated first.
    /// TODO: pagination (only the first 50 for now).
    pub async fn merge_requests(&self, list: &MrList) -> Result<MrPage, ApiError> {
        let (query, variables) = graphql::merge_requests_query(list, PAGE_SIZE);
        let data: MrListData = self.graphql(&query, variables).await?;
        data.into_page(list)
    }

    /// Pipelines for the dashboard, newest first.
    ///
    /// For `Mine` and `Recent`, the projects are picked with REST first: GraphQL's
    /// `projects(membership: true)` can only be sorted by id cheaply (any other `sort`
    /// times out on gitlab.com), while REST can order by last activity.
    pub async fn pipelines(&self, list: &PipelineList) -> Result<PipelinePage, ApiError> {
        if let PipelineList::Project(_) = list {
            let (query, variables) = graphql::pipelines_query(list, None, &[]);
            let data: PipelineListData = self.graphql(&query, variables).await?;
            return data.into_page(list, &self.web_base);
        }

        // "Mine" filters by username, which we need to know; both requests run at once.
        let (user, project_ids) = tokio::try_join!(self.current_user(), self.active_projects())?;
        if project_ids.is_empty() {
            // `projects(ids: [])` would mean "no filter": do not ask.
            return Ok(PipelinePage {
                current_user: user,
                pipelines: Vec::new(),
            });
        }
        let username = matches!(list, PipelineList::Mine).then_some(user.username.as_str());
        let (query, variables) = graphql::pipelines_query(list, username, &project_ids);
        let data: PipelineListData = self.graphql(&query, variables).await?;
        data.into_page(list, &self.web_base)
    }

    /// Projects I am a member of, most recently active first, for the repository picker.
    pub async fn repositories(&self) -> Result<Vec<Repository>, ApiError> {
        #[derive(serde::Deserialize)]
        struct RawProject {
            path_with_namespace: String,
            description: Option<String>,
            last_activity_at: Option<chrono::DateTime<chrono::Utc>>,
            web_url: String,
        }
        let url = self.api_base.join("projects")?;
        let request = self.http.get(url).query(&[
            ("membership", "true"),
            ("archived", "false"),
            ("order_by", "last_activity_at"),
            ("sort", "desc"),
            ("simple", "true"),
            ("per_page", "100"),
        ]);
        let projects: Vec<RawProject> = self.send(request).await?.json().await?;
        Ok(projects
            .into_iter()
            .map(|p| Repository {
                path: p.path_with_namespace,
                description: p.description.filter(|d| !d.is_empty()),
                last_activity: p.last_activity_at,
                web_url: p.web_url,
            })
            .collect())
    }

    /// The 100 most recently updated branches, with their open merge request and latest
    /// pipeline. Only the branch list is required: if the other two are refused (for
    /// example CI is disabled, or the token cannot read it), those columns stay empty.
    pub async fn branches(&self, project: &str) -> Result<BranchPage, ApiError> {
        let branches = self
            .http
            .get(self.project_url(project, &["repository", "branches"]))
            .query(&[("per_page", "100"), ("sort", "updated_desc")]);
        let merge_requests = self
            .http
            .get(self.project_url(project, &["merge_requests"]))
            .query(&[("state", "opened"), ("per_page", "100")]);
        let pipelines = self
            .http
            .get(self.project_url(project, &["pipelines"]))
            .query(&[("per_page", "100")]);
        let (branches, merge_requests, pipelines) = tokio::join!(
            self.fetch_json::<Vec<RawBranch>>(branches),
            self.fetch_json::<Vec<RawOpenMergeRequest>>(merge_requests),
            self.fetch_json::<Vec<RawRefPipeline>>(pipelines),
        );
        Ok(rest::into_branch_page(
            branches?,
            optional(merge_requests, "merge requests"),
            optional(pipelines, "pipelines"),
            &self.project_web_url(project),
        ))
    }

    /// What `head` changed since it left `base` (`straight=false`: from the merge base).
    pub async fn compare(
        &self,
        project: &str,
        base: &str,
        head: &str,
    ) -> Result<Comparison, ApiError> {
        let request = self
            .http
            .get(self.project_url(project, &["repository", "compare"]))
            .query(&[("from", base), ("to", head), ("straight", "false")]);
        let raw: RawCompare = self.fetch_json(request).await?;
        let web_url = format!(
            "{}/-/compare/{base}...{head}",
            self.project_web_url(project)
        );
        Ok(rest::into_comparison(raw, base, head, web_url))
    }

    /// `https://gitlab.com/group/project`, for links the API does not send.
    fn project_web_url(&self, project: &str) -> String {
        format!("{}{project}", self.web_base)
    }

    /// Ids of the projects I am a member of, most recently active first.
    async fn active_projects(&self) -> Result<Vec<u64>, ApiError> {
        #[derive(serde::Deserialize)]
        struct ProjectId {
            id: u64,
        }
        let url = self.api_base.join("projects")?;
        let request = self.http.get(url).query(&[
            ("membership", "true"),
            ("archived", "false"),
            ("order_by", "last_activity_at"),
            ("sort", "desc"),
            ("simple", "true"),
            ("per_page", "15"),
        ]);
        let projects: Vec<ProjectId> = self.send(request).await?.json().await?;
        Ok(projects.into_iter().map(|p| p.id).collect())
    }

    /// One pipeline and its jobs, for a pipeline opened from the dashboard.
    pub async fn pipeline_ci(
        &self,
        project_id: &str,
        pipeline_id: u64,
    ) -> Result<CiSnapshot, ApiError> {
        let project = project_id;
        let pipeline_path = pipeline_id.to_string();
        let pipeline: Pipeline = self
            .get_json(self.project_url(project, &["pipelines", &pipeline_path]))
            .await?;
        let url = self.project_url(project, &["pipelines", &pipeline_path, "jobs"]);
        let mut jobs: Vec<Job> = self.get_all_pages(url).await?;
        sort_jobs(&mut jobs);
        Ok(CiSnapshot {
            pipelines: vec![pipeline],
            pipeline_id: Some(pipeline_id),
            jobs,
        })
    }

    /// Who the token belongs to. Used to check a token at login.
    pub async fn current_user(&self) -> Result<User, ApiError> {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Data {
            current_user: Option<User>,
        }
        let data: Data = self
            .graphql("query { currentUser { username name } }", json!({}))
            .await?;
        // `currentUser` is null when the token is not accepted.
        data.current_user.ok_or(ApiError::Unauthorized)
    }

    /// Scopes of the personal access token in use, for example `["api"]`.
    pub async fn token_scopes(&self) -> Result<Vec<String>, ApiError> {
        #[derive(serde::Deserialize)]
        struct TokenInfo {
            scopes: Vec<String>,
        }
        let url = self.api_base.join("personal_access_tokens/self")?;
        Ok(self.get_json::<TokenInfo>(url).await?.scopes)
    }

    /// Description, approvals, diffs and discussions. The four requests run concurrently.
    pub async fn merge_request_detail(
        &self,
        project: &str,
        iid: u64,
    ) -> Result<MrDetail, ApiError> {
        let (mr, approvals, files, discussions) = tokio::try_join!(
            self.get_json::<RawMergeRequest>(self.mr_url(project, iid, &[])),
            self.get_json::<RawApprovals>(self.mr_url(project, iid, &["approvals"])),
            self.file_diffs(project, iid),
            self.get_all_pages::<Discussion>(self.mr_url(project, iid, &["discussions"])),
        )?;
        Ok(rest::into_detail(mr, approvals, files, discussions))
    }

    async fn file_diffs(&self, project: &str, iid: u64) -> Result<Vec<RawFileDiff>, ApiError> {
        match self
            .get_all_pages(self.mr_url(project, iid, &["diffs"]))
            .await
        {
            // `/diffs` exists since GitLab 15.7; older servers only have `/changes`.
            Err(ApiError::Api { status: 404, .. }) => {
                let changes: RawChanges = self
                    .get_json(self.mr_url(project, iid, &["changes"]))
                    .await?;
                Ok(changes.changes)
            }
            other => other,
        }
    }

    /// Approve, unapprove, comment or merge. Needs a token with the `api` scope.
    pub async fn run_action(
        &self,
        project: &str,
        iid: u64,
        action: &MrAction,
    ) -> Result<(), ApiError> {
        let request = match action {
            MrAction::Approve => self.http.post(self.mr_url(project, iid, &["approve"])),
            MrAction::Unapprove => self.http.post(self.mr_url(project, iid, &["unapprove"])),
            MrAction::Comment(body) => self
                .http
                .post(self.mr_url(project, iid, &["notes"]))
                .json(&json!({ "body": body })),
            MrAction::Merge { sha } => {
                // Without `sha`, GitLab merges whatever the branch points to right now.
                let body = match sha {
                    Some(sha) => json!({ "sha": sha }),
                    None => json!({}),
                };
                self.http
                    .put(self.mr_url(project, iid, &["merge"]))
                    .json(&body)
            }
        };
        match self.send(request).await {
            Ok(_) => Ok(()),
            // GitLab answers 401 when approving twice or without permission. That is not a
            // token problem, so don't report it as one.
            Err(ApiError::Unauthorized)
                if matches!(action, MrAction::Approve | MrAction::Unapprove) =>
            {
                Err(ApiError::Forbidden(
                    "GitLab refused: already done, or you are not allowed to approve".into(),
                ))
            }
            Err(ApiError::Api { status: 409, .. }) if matches!(action, MrAction::Merge { .. }) => {
                Err(ApiError::Forbidden(
                    "the source branch changed since you loaded it; press r and review again"
                        .into(),
                ))
            }
            Err(ApiError::Api {
                status: 405 | 406 | 422,
                message,
            }) if matches!(action, MrAction::Merge { .. }) => {
                Err(ApiError::Forbidden(format!("cannot merge: {message}")))
            }
            Err(err) => Err(err),
        }
    }

    /// Pipelines of a merge request (newest first) and the jobs of one of them: `pipeline_id`
    /// if given and still listed, otherwise the newest.
    pub async fn merge_request_ci(
        &self,
        project: &str,
        iid: u64,
        pipeline_id: Option<u64>,
    ) -> Result<CiSnapshot, ApiError> {
        // The 20 most recent pipelines are plenty to browse.
        let url = self.mr_url(project, iid, &["pipelines"]);
        let request = self.http.get(url).query(&[("per_page", "20")]);
        let pipelines: Vec<Pipeline> = self.send(request).await?.json().await?;

        let pipeline = pipeline_id
            .and_then(|id| pipelines.iter().find(|p| p.id == id))
            .or(pipelines.first());
        let (pipeline_id, jobs) = match pipeline {
            Some(pipeline) => {
                let project_id = &pipeline.project_id;
                let url =
                    self.project_url(project_id, &["pipelines", &pipeline.id.to_string(), "jobs"]);
                let mut jobs: Vec<Job> = self.get_all_pages(url).await?;
                sort_jobs(&mut jobs);
                (Some(pipeline.id), jobs)
            }
            None => (None, Vec::new()),
        };
        Ok(CiSnapshot {
            pipelines,
            pipeline_id,
            jobs,
        })
    }

    /// The raw log of a job (ANSI codes and section markers included, see `trace`).
    pub async fn job_trace(&self, project_id: &str, job_id: u64) -> Result<String, ApiError> {
        let url = self.project_url(project_id, &["jobs", &job_id.to_string(), "trace"]);
        Ok(self.send(self.http.get(url)).await?.text().await?)
    }

    /// Retry or cancel a job or pipeline. Needs a token with the `api` scope.
    pub async fn run_ci_action(&self, project_id: &str, action: CiAction) -> Result<(), ApiError> {
        let project = project_id;
        let (kind, id, verb) = match action {
            CiAction::RetryJob(id) => ("jobs", id, "retry"),
            CiAction::CancelJob(id) => ("jobs", id, "cancel"),
            CiAction::RetryPipeline(id) => ("pipelines", id, "retry"),
            CiAction::CancelPipeline(id) => ("pipelines", id, "cancel"),
        };
        let url = self.project_url(project, &[kind, &id.to_string(), verb]);
        match self.send(self.http.post(url)).await {
            Ok(_) => Ok(()),
            // GitLab answers 403 when the job cannot be retried or canceled in its state.
            Err(ApiError::Forbidden(message)) if message.contains("403 Forbidden") => Err(
                ApiError::Forbidden(format!("GitLab refused to {verb} this ({message})")),
            ),
            Err(err) => Err(err),
        }
    }

    /// `.../projects/<group%2Fproject>/merge_requests/<iid>/<tail...>`.
    fn mr_url(&self, project: &str, iid: u64, tail: &[&str]) -> Url {
        let iid = iid.to_string();
        let mut path = vec!["merge_requests", iid.as_str()];
        path.extend(tail);
        self.project_url(project, &path)
    }

    /// `.../projects/<project>/<tail...>`, where `project` is a numeric id or a path. A path
    /// is one URL segment, so its `/` must be encoded as `%2F`; `push` does that for us.
    fn project_url(&self, project: &str, tail: &[&str]) -> Url {
        let mut url = self.api_base.clone();
        // `path_segments_mut` mutably borrows `url`. The inner block ends that borrow
        // before we return `url`.
        {
            let mut segments = url
                .path_segments_mut()
                .expect("an http(s) URL always has a path");
            segments
                .pop_if_empty()
                .push("projects")
                .push(project)
                .extend(tail);
        }
        url
    }

    async fn get_json<T: DeserializeOwned>(&self, url: Url) -> Result<T, ApiError> {
        self.fetch_json(self.http.get(url)).await
    }

    async fn fetch_json<T: DeserializeOwned>(
        &self,
        request: RequestBuilder,
    ) -> Result<T, ApiError> {
        Ok(self.send(request).await?.json().await?)
    }

    /// Follows GitLab's `x-next-page` header until the last page (or `MAX_PAGES`).
    async fn get_all_pages<T: DeserializeOwned>(&self, url: Url) -> Result<Vec<T>, ApiError> {
        let mut items = Vec::new();
        let mut next_page = Some("1".to_string());
        for _ in 0..MAX_PAGES {
            let Some(page) = next_page.take() else {
                break;
            };
            let request = self
                .http
                .get(url.clone())
                .query(&[("per_page", "100"), ("page", page.as_str())]);
            let response = self.send(request).await?;
            next_page = response
                .headers()
                .get("x-next-page")
                .and_then(|value| value.to_str().ok())
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            items.extend(response.json::<Vec<T>>().await?);
        }
        Ok(items)
    }

    /// Sends a REST request and turns non-2xx responses into errors.
    async fn send(&self, request: RequestBuilder) -> Result<Response, ApiError> {
        let response = request.send().await?;
        let status = response.status();
        tracing::debug!(url = %response.url(), %status, "REST response");
        if status.is_success() {
            Ok(response)
        } else {
            let body = response.text().await.unwrap_or_default();
            Err(ApiError::from_response(status, &body))
        }
    }

    async fn graphql<T: DeserializeOwned>(
        &self,
        query: &str,
        variables: Value,
    ) -> Result<T, ApiError> {
        let response = self
            .http
            .post(self.graphql_url.clone())
            .json(&json!({ "query": query, "variables": variables }))
            .send()
            .await?;
        let status = response.status();
        tracing::debug!(%status, "GraphQL response");
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ApiError::from_response(status, &body));
        }

        // GraphQL reports most errors with status 200 and an `errors` array.
        let body: graphql::Response<T> = response.json().await?;
        if !body.errors.is_empty() {
            let messages: Vec<String> = body.errors.into_iter().map(|e| e.message).collect();
            return Err(ApiError::GraphQl(messages.join("; ")));
        }
        body.data
            .ok_or_else(|| ApiError::UnexpectedResponse("GraphQL response has no data".into()))
    }
}

/// Extra data that a screen can do without: an error becomes "nothing", with a warning.
/// A generic function rather than a closure, since a closure's parameter types are fixed by
/// its first call and this is used with several item types.
fn optional<T>(result: Result<Vec<T>, ApiError>, what: &str) -> Vec<T> {
    result.unwrap_or_else(|err| {
        tracing::warn!(error = %err, "branches: could not load {what}");
        Vec::new()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client_for(server: &MockServer, token: &str) -> GitLabClient {
        let host = crate::config::parse_host(&server.uri()).unwrap();
        GitLabClient::new(&host, token).unwrap()
    }

    fn user_list_body() -> Value {
        json!({"data": {"currentUser": {
            "username": "thang", "name": "Thang",
            "mergeRequests": {"nodes": [{
                "iid": "42", "title": "Fix bug", "draft": false,
                "webUrl": "https://example.com/mr/42", "updatedAt": "2026-10-10T08:00:00Z",
                "sourceBranch": "fix", "targetBranch": "main",
                "author": {"username": "hoang", "name": "Hoang"},
                "project": {"fullPath": "group/api"},
                "headPipeline": {"status": "SUCCESS"},
                "approvedBy": {"nodes": []}
            }]}
        }}})
    }

    #[tokio::test]
    async fn sends_graphql_query_with_bearer_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/graphql"))
            .and(header("Authorization", "Bearer glpat-test"))
            .and(body_string_contains("reviewRequestedMergeRequests"))
            .respond_with(ResponseTemplate::new(200).set_body_json(user_list_body()))
            .expect(1)
            .mount(&server)
            .await;

        let page = client_for(&server, "glpat-test")
            .merge_requests(&MrList::ReviewRequested)
            .await
            .unwrap();

        assert_eq!(page.current_user.username, "thang");
        assert_eq!(page.merge_requests[0].title, "Fix bug");
    }

    #[tokio::test]
    async fn maps_401_to_unauthorized() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(401).set_body_json(json!({"message": "401 Unauthorized"})),
            )
            .mount(&server)
            .await;

        let err = client_for(&server, "bad")
            .merge_requests(&MrList::AssignedToMe)
            .await
            .unwrap_err();

        assert!(matches!(err, ApiError::Unauthorized), "got {err:?}");
    }

    #[tokio::test]
    async fn surfaces_graphql_errors() {
        let server = MockServer::start().await;
        let body = json!({"errors": [{"message": "Field 'foo' doesn't exist"}]});
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let err = client_for(&server, "t")
            .merge_requests(&MrList::AssignedToMe)
            .await
            .unwrap_err();

        assert!(err.to_string().contains("Field 'foo' doesn't exist"));
    }

    #[tokio::test]
    async fn works_with_sub_path_installs() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/gitlab/api/graphql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(user_list_body()))
            .expect(1)
            .mount(&server)
            .await;

        let host = crate::config::parse_host(&format!("{}/gitlab", server.uri())).unwrap();
        GitLabClient::new(&host, "t")
            .unwrap()
            .merge_requests(&MrList::AssignedToMe)
            .await
            .unwrap();
    }

    const MR_PATH: &str = "/api/v4/projects/group%2Fapi/merge_requests/42";

    async fn mount_get(server: &MockServer, path_suffix: &str, body: Value) {
        Mock::given(method("GET"))
            .and(path(format!("{MR_PATH}{path_suffix}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    fn mr_body() -> Value {
        json!({
            "iid": 42, "title": "Fix bug", "description": "Details", "state": "opened",
            "author": {"id": 1, "username": "hoang", "name": "Hoang"},
            "source_branch": "fix", "target_branch": "main",
            "web_url": "https://example.com/mr/42", "sha": "abc123",
            "detailed_merge_status": "mergeable", "has_conflicts": false,
            "head_pipeline": {"status": "success"}, "user": {"can_merge": true}
        })
    }

    fn note(id: u64) -> Value {
        json!({"id": id, "body": format!("note {id}"), "system": false,
               "author": {"id": 1, "username": "linh", "name": "Linh"},
               "created_at": "2026-10-10T08:00:00Z"})
    }

    #[tokio::test]
    async fn loads_detail_and_follows_pagination() {
        let server = MockServer::start().await;
        mount_get(&server, "", mr_body()).await;
        mount_get(
            &server,
            "/approvals",
            json!({"approved_by": [], "user_has_approved": false, "user_can_approve": true}),
        )
        .await;
        mount_get(
            &server,
            "/diffs",
            json!([{"old_path": "a", "new_path": "a", "diff": "@@ -1 +1 @@\n-x\n+y\n"}]),
        )
        .await;
        // Two pages of discussions.
        Mock::given(method("GET"))
            .and(path(format!("{MR_PATH}/discussions")))
            .and(query_param("page", "1"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("x-next-page", "2")
                    .set_body_json(json!([{"id": "d1", "notes": [note(1)]}])),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("{MR_PATH}/discussions")))
            .and(query_param("page", "2"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("x-next-page", "")
                    .set_body_json(json!([{"id": "d2", "notes": [note(2)]}])),
            )
            .mount(&server)
            .await;

        let detail = client_for(&server, "t")
            .merge_request_detail("group/api", 42)
            .await
            .unwrap();

        assert_eq!(detail.description, "Details");
        assert_eq!(detail.sha.as_deref(), Some("abc123"));
        assert_eq!(detail.files[0].added, 1);
        assert_eq!(detail.discussions.len(), 2);
        assert!(detail.approvals.user_can_approve);
    }

    #[tokio::test]
    async fn falls_back_to_changes_on_old_gitlab() {
        let server = MockServer::start().await;
        mount_get(&server, "", mr_body()).await;
        mount_get(&server, "/approvals", json!({})).await;
        mount_get(&server, "/discussions", json!([])).await;
        Mock::given(method("GET"))
            .and(path(format!("{MR_PATH}/diffs")))
            .respond_with(
                ResponseTemplate::new(404).set_body_json(json!({"message": "404 Not found"})),
            )
            .mount(&server)
            .await;
        mount_get(&server, "/changes", json!({"changes": [{"old_path": "a", "new_path": "b", "diff": "", "renamed_file": true}]})).await;

        let detail = client_for(&server, "t")
            .merge_request_detail("group/api", 42)
            .await
            .unwrap();
        assert!(detail.files[0].renamed_file);
    }

    #[tokio::test]
    async fn merge_sends_the_reviewed_sha() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path(format!("{MR_PATH}/merge")))
            .and(body_string_contains("\"sha\":\"abc123\""))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let action = MrAction::Merge {
            sha: Some("abc123".into()),
        };
        client_for(&server, "t")
            .run_action("group/api", 42, &action)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn merge_conflict_on_sha_has_a_clear_message() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .respond_with(
                ResponseTemplate::new(409)
                    .set_body_json(json!({"message": "SHA does not match HEAD of source branch"})),
            )
            .mount(&server)
            .await;

        let action = MrAction::Merge {
            sha: Some("old".into()),
        };
        let err = client_for(&server, "t")
            .run_action("group/api", 42, &action)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("source branch changed"), "{err}");
    }

    #[tokio::test]
    async fn comment_and_approve_hit_the_right_endpoints() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(format!("{MR_PATH}/notes")))
            .and(body_string_contains("LGTM"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("{MR_PATH}/approve")))
            .respond_with(
                ResponseTemplate::new(401).set_body_json(json!({"message": "401 Unauthorized"})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = client_for(&server, "t");
        client
            .run_action("group/api", 42, &MrAction::Comment("LGTM".into()))
            .await
            .unwrap();
        let err = client
            .run_action("group/api", 42, &MrAction::Approve)
            .await
            .unwrap_err();
        // A 401 on approve is a refusal, not a bad token.
        assert!(!matches!(err, ApiError::Unauthorized));
    }

    #[tokio::test]
    async fn checks_a_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/graphql"))
            .and(body_string_contains("currentUser"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"data": {"currentUser": {"username": "thang", "name": "Thang"}}}),
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v4/personal_access_tokens/self"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"id": 1, "name": "lazyforge", "scopes": ["read_api"], "active": true}),
            ))
            .mount(&server)
            .await;

        let client = client_for(&server, "t");
        assert_eq!(client.current_user().await.unwrap().username, "thang");
        assert_eq!(client.token_scopes().await.unwrap(), vec!["read_api"]);
    }

    fn pipeline(id: u64) -> Value {
        json!({"id": id, "iid": 1, "project_id": 99, "status": "running", "ref": "fix",
               "sha": "abc", "source": "merge_request_event",
               "created_at": "2026-10-10T08:00:00Z", "updated_at": "2026-10-10T08:00:00Z",
               "web_url": "https://example.com/p"})
    }

    fn job(id: u64, stage: &str) -> Value {
        json!({"id": id, "name": format!("job{id}"), "stage": stage, "status": "success",
               "created_at": "2026-10-10T08:00:00Z", "started_at": null, "finished_at": null,
               "duration": 1.5, "allow_failure": false, "web_url": "https://example.com/j"})
    }

    #[tokio::test]
    async fn loads_pipelines_and_jobs_from_the_pipeline_project() {
        let server = MockServer::start().await;
        mount_get(&server, "/pipelines", json!([pipeline(20), pipeline(10)])).await;
        // Jobs come from the pipeline's own project (99), e.g. a fork.
        Mock::given(method("GET"))
            .and(path("/api/v4/projects/99/pipelines/10/jobs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                job(3, "test"),
                job(2, "build"),
                job(1, "build")
            ])))
            .expect(1)
            .mount(&server)
            .await;

        let ci = client_for(&server, "t")
            .merge_request_ci("group/api", 42, Some(10))
            .await
            .unwrap();

        assert_eq!(ci.pipelines.len(), 2);
        assert_eq!(ci.pipeline_id, Some(10));
        let ids: Vec<_> = ci.jobs.iter().map(|j| j.id).collect();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn no_pipelines_means_no_jobs() {
        let server = MockServer::start().await;
        mount_get(&server, "/pipelines", json!([])).await;
        let ci = client_for(&server, "t")
            .merge_request_ci("group/api", 42, None)
            .await
            .unwrap();
        assert_eq!(ci.pipeline_id, None);
        assert!(ci.jobs.is_empty());
    }

    #[tokio::test]
    async fn fetches_traces_and_runs_ci_actions() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v4/projects/99/jobs/7/trace"))
            .respond_with(ResponseTemplate::new(200).set_body_string("line 1\nline 2\n"))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v4/projects/99/pipelines/10/retry"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v4/projects/99/jobs/7/cancel"))
            .respond_with(
                ResponseTemplate::new(403).set_body_json(json!({"message": "403 Forbidden"})),
            )
            .mount(&server)
            .await;

        let client = client_for(&server, "t");
        assert_eq!(client.job_trace("99", 7).await.unwrap(), "line 1\nline 2\n");
        client
            .run_ci_action("99", CiAction::RetryPipeline(10))
            .await
            .unwrap();
        let err = client
            .run_ci_action("99", CiAction::CancelJob(7))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("refused to cancel"), "{err}");
    }

    async fn mount_current_user(server: &MockServer) {
        Mock::given(method("POST"))
            .and(body_string_contains("query { currentUser"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"data": {"currentUser": {"username": "thang", "name": "Thang"}}}),
            ))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn mine_looks_at_recently_active_projects_and_filters_by_user() {
        let server = MockServer::start().await;
        mount_current_user(&server).await;
        Mock::given(method("GET"))
            .and(path("/api/v4/projects"))
            .and(query_param("membership", "true"))
            .and(query_param("order_by", "last_activity_at"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"id": 7}, {"id": 9}])))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_string_contains("projects(ids: $ids"))
            .and(body_string_contains("\"username\":\"thang\""))
            .and(body_string_contains("gid://gitlab/Project/9"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": {
                "currentUser": {"username": "thang", "name": "Thang"},
                "projects": {"nodes": []}
            }})))
            .expect(1)
            .mount(&server)
            .await;

        let page = client_for(&server, "t")
            .pipelines(&PipelineList::Mine)
            .await
            .unwrap();
        assert!(page.pipelines.is_empty());
    }

    #[tokio::test]
    async fn no_projects_means_no_pipeline_query() {
        let server = MockServer::start().await;
        mount_current_user(&server).await;
        Mock::given(method("GET"))
            .and(path("/api/v4/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_string_contains("projects(ids"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;

        let page = client_for(&server, "t")
            .pipelines(&PipelineList::Recent)
            .await
            .unwrap();
        assert_eq!(page.current_user.username, "thang");
        assert!(page.pipelines.is_empty());
    }

    #[tokio::test]
    async fn opens_a_single_pipeline() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v4/projects/99/pipelines/10"))
            .respond_with(ResponseTemplate::new(200).set_body_json(pipeline(10)))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v4/projects/99/pipelines/10/jobs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([job(1, "build")])))
            .mount(&server)
            .await;

        let ci = client_for(&server, "t")
            .pipeline_ci("99", 10)
            .await
            .unwrap();
        assert_eq!(ci.pipeline_id, Some(10));
        assert_eq!(ci.jobs.len(), 1);
    }

    #[test]
    fn rejects_tokens_with_newlines() {
        let host = Url::parse("https://gitlab.com/").unwrap();
        let err = GitLabClient::new(&host, "abc\n").unwrap_err();
        assert!(matches!(err, ApiError::InvalidToken));
    }

    #[tokio::test]
    async fn branches_survive_a_refused_pipeline_list() {
        let server = MockServer::start().await;
        let project = "/api/v4/projects/group%2Fapi";
        Mock::given(method("GET"))
            .and(path(format!("{project}/repository/branches")))
            .and(query_param("sort", "updated_desc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"name": "main", "default": true,
                 "commit": {"title": "Merge", "author_name": "Thang",
                            "committed_date": "2026-10-01T00:00:00Z"}},
                {"name": "feat/x", "default": false,
                 "commit": {"title": "Add x", "author_name": "Linh",
                            "committed_date": "2026-10-09T00:00:00Z"}}
            ])))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("{project}/merge_requests")))
            .and(query_param("state", "opened"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"iid": 3, "source_branch": "feat/x", "source_project_id": 1, "project_id": 1}
            ])))
            .mount(&server)
            .await;
        // A token or role without access to CI.
        Mock::given(method("GET"))
            .and(path(format!("{project}/pipelines")))
            .respond_with(
                ResponseTemplate::new(403).set_body_json(json!({"message": "403 Forbidden"})),
            )
            .mount(&server)
            .await;

        let page = client_for(&server, "t")
            .branches("group/api")
            .await
            .unwrap();
        assert_eq!(page.default_branch.as_deref(), Some("main"));
        let feat = &page.branches[1];
        assert_eq!(feat.merge_request, Some(3));
        assert_eq!(feat.pipeline_status, None);
        assert!(
            feat.web_url.ends_with("/group/api/-/tree/feat/x"),
            "{}",
            feat.web_url
        );
    }

    #[tokio::test]
    async fn compares_from_the_merge_base() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v4/projects/group%2Fapi/repository/compare"))
            .and(query_param("from", "main"))
            .and(query_param("to", "feat/x"))
            .and(query_param("straight", "false"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "commits": [{"id": "a"}, {"id": "b"}],
                "diffs": [{"old_path": "a.rs", "new_path": "a.rs",
                           "diff": "@@ -1 +1,2 @@\n keep\n+new\n"}],
                "web_url": "https://gitlab.example/group/api/-/compare/main...feat%2Fx"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let comparison = client_for(&server, "t")
            .compare("group/api", "main", "feat/x")
            .await
            .unwrap();
        assert_eq!(comparison.commits, 2);
        assert_eq!(comparison.files[0].added, 1);
        assert!(comparison.web_url.contains("/-/compare/"));
    }
}
