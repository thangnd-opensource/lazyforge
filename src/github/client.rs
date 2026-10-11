use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue};
use reqwest::{RequestBuilder, Response};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use url::Url;

// The detail screen's REST shapes are used as `rest::…`: some names (`RawReview`,
// `RawRepository`) also exist below for the GraphQL list and the repository picker.
use super::actions::{self, RawJobs, RawRun, RawRuns};
use super::branches::{BRANCHES_QUERY, BranchesData, RawComparison};
use super::rest::{self, RawDetail};
use crate::forge::{
    ApiError, BranchPage, CiAction, CiSnapshot, Comparison, Job, MergeRequest, MrAction, MrDetail,
    MrList, MrPage, PipelineList, PipelinePage, PipelineStatus, Repository, User, sort_jobs,
};

/// Repositories looked at for the "Mine" and "Recent" run lists.
const DASHBOARD_REPOS: usize = 8;

/// Upper bound for REST pagination (100 items per page).
const MAX_PAGES: usize = 10;

/// Cheap to clone: `reqwest::Client` keeps its connection pool behind an `Arc`.
#[derive(Debug, Clone)]
pub struct GitHubClient {
    http: reqwest::Client,
    api_base: Url,
    graphql_url: Url,
}

impl GitHubClient {
    /// `host` is the web address (`https://github.com/` or a GitHub Enterprise server, ending
    /// with `/`). The API lives at `api.github.com` for github.com and under `/api/` otherwise.
    pub fn new(host: &Url, token: &str) -> Result<Self, ApiError> {
        let mut auth = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| ApiError::InvalidToken)?;
        auth.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, auth);
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(
            "X-GitHub-Api-Version",
            HeaderValue::from_static("2022-11-28"),
        );

        let http = reqwest::Client::builder()
            .default_headers(headers)
            // GitHub rejects requests without a User-Agent.
            .user_agent(concat!("lazyforge/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(30))
            .build()?;

        let (api_base, graphql_url) = if host.host_str() == Some("github.com") {
            let api = Url::parse("https://api.github.com/")?;
            let graphql = api.join("graphql")?;
            (api, graphql)
        } else {
            (host.join("api/v3/")?, host.join("api/graphql")?)
        };
        Ok(Self {
            http,
            api_base,
            graphql_url,
        })
    }

    /// Who the token belongs to.
    pub async fn current_user(&self) -> Result<User, ApiError> {
        let raw: RawUser = self.get_json(self.api_base.join("user")?).await?;
        Ok(raw.into())
    }

    /// Scopes of a classic personal access token, from the `x-oauth-scopes` header.
    /// Fine-grained tokens have no scopes to report: that is an error, so no warning is shown.
    pub async fn token_scopes(&self) -> Result<Vec<String>, ApiError> {
        let response = self
            .send(self.http.get(self.api_base.join("user")?))
            .await?;
        let header = response
            .headers()
            .get("x-oauth-scopes")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| ApiError::Unsupported("this token does not report scopes".into()))?;
        Ok(header
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect())
    }

    /// Open pull requests, most recently updated first. One GraphQL `search` per list.
    pub async fn merge_requests(&self, list: &MrList) -> Result<MrPage, ApiError> {
        let data: SearchData = self
            .graphql(PULL_REQUESTS_QUERY, json!({ "q": search_query(list) }))
            .await?;
        Ok(MrPage {
            current_user: data.viewer.into(),
            merge_requests: data
                .search
                .nodes
                .into_iter()
                // Only pull requests are asked for; skip anything else defensively.
                .filter_map(|node| serde_json::from_value::<RawPullRequest>(node).ok())
                .map(RawPullRequest::into_model)
                .collect(),
        })
    }

    /// Repositories I own, collaborate on or can reach through an organization, most
    /// recently pushed first.
    pub async fn repositories(&self) -> Result<Vec<Repository>, ApiError> {
        let request = self.http.get(self.api_base.join("user/repos")?).query(&[
            ("sort", "pushed"),
            ("per_page", "100"),
            ("affiliation", "owner,collaborator,organization_member"),
        ]);
        let repos: Vec<RawRepository> = self.send(request).await?.json().await?;
        Ok(repos
            .into_iter()
            .filter(|r| !r.archived)
            .map(|r| Repository {
                path: r.full_name,
                description: r.description,
                last_activity: r.pushed_at,
                web_url: r.html_url,
            })
            .collect())
    }

    /// Description, files, reviews, comments, permissions and checks of a pull request.
    /// The requests run concurrently.
    pub async fn merge_request_detail(
        &self,
        repo: &str,
        number: u64,
    ) -> Result<MrDetail, ApiError> {
        let n = number.to_string();
        let (pull, files, reviews, issue_comments, review_comments, repository, me, checks) = tokio::try_join!(
            self.get_json::<rest::RawPullRequest>(self.repo_url(repo, &["pulls", &n])?),
            self.get_all_pages::<rest::RawFile>(self.repo_url(repo, &["pulls", &n, "files"])?),
            self.get_all_pages::<rest::RawReview>(self.repo_url(repo, &["pulls", &n, "reviews"])?),
            self.get_all_pages::<rest::RawIssueComment>(
                self.repo_url(repo, &["issues", &n, "comments"])?
            ),
            self.get_all_pages::<rest::RawReviewComment>(
                self.repo_url(repo, &["pulls", &n, "comments"])?
            ),
            self.get_json::<rest::RawRepository>(self.repo_url(repo, &[])?),
            self.current_user(),
            self.checks(repo, number),
        )?;
        Ok(rest::into_detail(RawDetail {
            pull,
            files,
            reviews,
            issue_comments,
            review_comments,
            repository,
            me: me.username,
            checks,
        }))
    }

    /// Combined state of the checks on the pull request's latest commit.
    async fn checks(&self, repo: &str, number: u64) -> Result<Option<PipelineStatus>, ApiError> {
        let (owner, name) = split_repo(repo)?;
        let data: Value = self
            .graphql(
                "query($owner: String!, $name: String!, $number: Int!) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      commits(last: 1) { nodes { commit { statusCheckRollup { state } } } }
    }
  }
}",
                json!({ "owner": owner, "name": name, "number": number }),
            )
            .await?;
        let state = data
            .pointer("/repository/pullRequest/commits/nodes/0/commit/statusCheckRollup/state")
            .and_then(Value::as_str);
        Ok(state.map(rollup_status))
    }

    /// Approve, comment or merge. GitHub has no API to withdraw an approval.
    pub async fn run_action(
        &self,
        repo: &str,
        number: u64,
        action: &MrAction,
    ) -> Result<(), ApiError> {
        let n = number.to_string();
        let request = match action {
            MrAction::Approve => self
                .http
                .post(self.repo_url(repo, &["pulls", &n, "reviews"])?)
                .json(&json!({ "event": "APPROVE" })),
            MrAction::Unapprove => {
                return Err(ApiError::Unsupported(
                    "GitHub cannot withdraw an approval through its API; dismiss your review on the web (press o)".into(),
                ));
            }
            MrAction::Comment(body) => self
                .http
                .post(self.repo_url(repo, &["issues", &n, "comments"])?)
                .json(&json!({ "body": body })),
            MrAction::Merge { sha } => {
                // Use a merge method the repository allows.
                let repository: rest::RawRepository =
                    self.get_json(self.repo_url(repo, &[])?).await?;
                let mut body = json!({ "merge_method": repository.merge_method() });
                if let Some(sha) = sha {
                    body["sha"] = json!(sha);
                }
                self.http
                    .put(self.repo_url(repo, &["pulls", &n, "merge"])?)
                    .json(&body)
            }
        };
        match self.send(request).await {
            Ok(_) => Ok(()),
            Err(ApiError::Api { status: 409, .. }) if matches!(action, MrAction::Merge { .. }) => {
                Err(ApiError::Forbidden(
                    "the head branch changed since you loaded it; press r and review again".into(),
                ))
            }
            Err(ApiError::Api {
                status: 405 | 422,
                message,
            }) => Err(ApiError::Forbidden(match action {
                MrAction::Merge { .. } => format!("cannot merge: {message}"),
                _ => message,
            })),
            Err(err) => Err(err),
        }
    }

    /// Workflow runs on the pull request's latest commit (one per workflow), and the jobs of
    /// one of them: `run_id` if given and still listed, otherwise the newest.
    pub async fn merge_request_ci(
        &self,
        repo: &str,
        number: u64,
        run_id: Option<u64>,
    ) -> Result<CiSnapshot, ApiError> {
        let pull: rest::RawPullRequest = self
            .get_json(self.repo_url(repo, &["pulls", &number.to_string()])?)
            .await?;
        let request = self
            .http
            .get(self.repo_url(repo, &["actions", "runs"])?)
            .query(&[("head_sha", pull.head.sha.as_str()), ("per_page", "20")]);
        let runs: RawRuns = self.send(request).await?.json().await?;
        let chosen = run_id
            .and_then(|id| runs.workflow_runs.iter().find(|r| r.id == id))
            .or(runs.workflow_runs.first())
            .cloned();
        let jobs = match &chosen {
            Some(run) => self.run_jobs(repo, run).await?,
            None => Vec::new(),
        };
        Ok(CiSnapshot {
            pipelines: runs
                .workflow_runs
                .into_iter()
                .map(|r| r.into_pipeline(repo))
                .collect(),
            pipeline_id: chosen.map(|r| r.id),
            jobs,
        })
    }

    /// One workflow run and its jobs, for a run opened from the dashboard.
    pub async fn pipeline_ci(&self, repo: &str, run_id: u64) -> Result<CiSnapshot, ApiError> {
        let run: RawRun = self
            .get_json(self.repo_url(repo, &["actions", "runs", &run_id.to_string()])?)
            .await?;
        let jobs = self.run_jobs(repo, &run).await?;
        Ok(CiSnapshot {
            pipelines: vec![run.into_pipeline(repo)],
            pipeline_id: Some(run_id),
            jobs,
        })
    }

    async fn run_jobs(&self, repo: &str, run: &RawRun) -> Result<Vec<Job>, ApiError> {
        let url = self.repo_url(repo, &["actions", "runs", &run.id.to_string(), "jobs"])?;
        let request = self.http.get(url).query(&[("per_page", "100")]);
        let raw: RawJobs = self.send(request).await?.json().await?;
        let stage = run.name.as_deref().unwrap_or("jobs");
        let mut jobs: Vec<Job> = raw
            .jobs
            .into_iter()
            .map(|j| j.into_job(stage, run))
            .collect();
        sort_jobs(&mut jobs);
        Ok(jobs)
    }

    /// The log of a finished job, cleaned for the log viewer. GitHub answers with a redirect
    /// to a short-lived download URL; reqwest follows it (and does not send our token to the
    /// other host).
    pub async fn job_trace(&self, repo: &str, job_id: u64) -> Result<String, ApiError> {
        let url = self.repo_url(repo, &["actions", "jobs", &job_id.to_string(), "logs"])?;
        let response = self.send(self.http.get(url)).await?;
        Ok(actions::clean_log(&response.text().await?))
    }

    /// Re-run a job, re-run a run's failed jobs, or cancel a run.
    pub async fn run_ci_action(&self, repo: &str, action: CiAction) -> Result<(), ApiError> {
        let (kind, id, verb) = match action {
            CiAction::RetryJob(id) => ("jobs", id, "rerun"),
            CiAction::RetryPipeline(id) => ("runs", id, "rerun-failed-jobs"),
            CiAction::CancelPipeline(id) => ("runs", id, "cancel"),
            CiAction::CancelJob(_) => {
                return Err(ApiError::Unsupported(
                    "GitHub cancels whole workflow runs, not single jobs; press X to cancel the run"
                        .into(),
                ));
            }
        };
        let url = self.repo_url(repo, &["actions", kind, &id.to_string(), verb])?;
        match self.send(self.http.post(url)).await {
            Ok(_) => Ok(()),
            Err(ApiError::Api {
                status: 403 | 409 | 422,
                message,
            }) => Err(ApiError::Forbidden(format!("GitHub refused: {message}"))),
            Err(err) => Err(err),
        }
    }

    /// Workflow runs for the dashboard, newest first: of one repository, or of my most
    /// recently pushed repositories (one request each, run concurrently).
    pub async fn pipelines(&self, list: &PipelineList) -> Result<PipelinePage, ApiError> {
        let (me, repos) = match list {
            PipelineList::Project(repo) => (self.current_user().await?, vec![repo.clone()]),
            _ => {
                let (me, repos) = tokio::try_join!(self.current_user(), self.repositories())?;
                let paths = repos
                    .into_iter()
                    .take(DASHBOARD_REPOS)
                    .map(|r| r.path)
                    .collect();
                (me, paths)
            }
        };
        let actor = matches!(list, PipelineList::Mine).then_some(me.username.as_str());
        let per_repo = if matches!(list, PipelineList::Project(_)) {
            "30"
        } else {
            "5"
        };
        let results = futures::future::join_all(
            repos
                .iter()
                .map(|repo| self.repo_runs(repo, actor, per_repo)),
        )
        .await;

        let mut pipelines = Vec::new();
        let mut first_error = None;
        for (repo, result) in repos.iter().zip(results) {
            match result {
                Ok(runs) => pipelines.extend(runs.into_iter().map(|r| r.into_summary(repo))),
                // One repository without Actions access should not hide the others.
                Err(err) => {
                    tracing::warn!(%repo, error = %err, "cannot list workflow runs");
                    first_error.get_or_insert(err);
                }
            }
        }
        if let (PipelineList::Project(_), Some(err)) = (list, first_error) {
            return Err(err);
        }
        pipelines.sort_by_key(|p| std::cmp::Reverse(p.created_at));
        Ok(PipelinePage {
            current_user: me,
            pipelines,
        })
    }

    async fn repo_runs(
        &self,
        repo: &str,
        actor: Option<&str>,
        per_page: &str,
    ) -> Result<Vec<RawRun>, ApiError> {
        let mut query = vec![("per_page", per_page)];
        if let Some(actor) = actor {
            query.push(("actor", actor));
        }
        let request = self
            .http
            .get(self.repo_url(repo, &["actions", "runs"])?)
            .query(&query);
        let runs: RawRuns = self.send(request).await?.json().await?;
        Ok(runs.workflow_runs)
    }

    /// The 100 most recently updated branches, with checks and open pull requests.
    pub async fn branches(&self, repo: &str) -> Result<BranchPage, ApiError> {
        let (owner, name) = split_repo(repo)?;
        let data: BranchesData = self
            .graphql(BRANCHES_QUERY, json!({ "owner": owner, "name": name }))
            .await?;
        data.into_page()
            .ok_or_else(|| ApiError::NotFound(format!("repository `{repo}`")))
    }

    /// What `head` changed since it left `base` (GitHub's three-dot compare).
    pub async fn compare(
        &self,
        repo: &str,
        base: &str,
        head: &str,
    ) -> Result<Comparison, ApiError> {
        let mut url = self.repo_url(repo, &["compare"])?;
        // Branch names often contain `/` (`feature/x`), which GitHub expects as is here:
        // pushing each part as its own segment keeps the slashes and encodes the rest.
        url.path_segments_mut()
            .expect("an http(s) URL always has a path")
            .extend(format!("{base}...{head}").split('/'));
        let raw: RawComparison = self.get_json(url).await?;
        Ok(raw.into_model(base, head))
    }

    /// `.../repos/<owner>/<name>/<tail...>`.
    fn repo_url(&self, repo: &str, tail: &[&str]) -> Result<Url, ApiError> {
        let (owner, name) = split_repo(repo)?;
        let mut url = self.api_base.clone();
        // `path_segments_mut` mutably borrows `url`; the block ends that borrow.
        {
            let mut segments = url
                .path_segments_mut()
                .expect("an http(s) URL always has a path");
            segments
                .pop_if_empty()
                .push("repos")
                .push(owner)
                .push(name)
                .extend(tail);
        }
        Ok(url)
    }

    /// Fetches pages of 100 until a short page (or `MAX_PAGES`).
    async fn get_all_pages<T: DeserializeOwned>(&self, url: Url) -> Result<Vec<T>, ApiError> {
        let mut items = Vec::new();
        for page in 1..=MAX_PAGES {
            let request = self
                .http
                .get(url.clone())
                .query(&[("per_page", "100"), ("page", &page.to_string())]);
            let batch: Vec<T> = self.send(request).await?.json().await?;
            let done = batch.len() < 100;
            items.extend(batch);
            if done {
                break;
            }
        }
        Ok(items)
    }

    async fn get_json<T: DeserializeOwned>(&self, url: Url) -> Result<T, ApiError> {
        Ok(self.send(self.http.get(url)).await?.json().await?)
    }

    async fn send(&self, request: RequestBuilder) -> Result<Response, ApiError> {
        let response = request.send().await?;
        let status = response.status();
        tracing::debug!(url = %response.url(), %status, "GitHub REST response");
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
        let request = self
            .http
            .post(self.graphql_url.clone())
            .json(&json!({ "query": query, "variables": variables }));
        let body: GraphQlResponse<T> = self.send(request).await?.json().await?;
        if !body.errors.is_empty() {
            let messages: Vec<String> = body.errors.into_iter().map(|e| e.message).collect();
            return Err(ApiError::GraphQl(messages.join("; ")));
        }
        body.data
            .ok_or_else(|| ApiError::UnexpectedResponse("GraphQL response has no data".into()))
    }
}

/// `owner/repo` → `("owner", "repo")`.
fn split_repo(repo: &str) -> Result<(&str, &str), ApiError> {
    repo.split_once('/')
        .ok_or_else(|| ApiError::NotFound(format!("repository `{repo}` (expected owner/name)")))
}

/// GitHub search syntax for each list. `archived:false` hides pull requests of archived
/// repositories, which can no longer be acted on.
fn search_query(list: &MrList) -> String {
    let filter = match list {
        MrList::AssignedToMe => "assignee:@me".to_string(),
        MrList::ReviewRequested => "review-requested:@me".to_string(),
        MrList::AuthoredByMe => "author:@me".to_string(),
        MrList::Project(repo) => format!("repo:{repo}"),
    };
    format!("is:pr is:open archived:false sort:updated-desc {filter}")
}

const PULL_REQUESTS_QUERY: &str = "
query($q: String!) {
  viewer { login name }
  search(query: $q, type: ISSUE, first: 50) {
    nodes {
      ... on PullRequest {
        number title isDraft url updatedAt headRefName baseRefName
        author { login }
        repository { nameWithOwner }
        latestReviews(first: 20) { nodes { state author { login } } }
        commits(last: 1) { nodes { commit { statusCheckRollup { state } } } }
      }
    }
  }
}";

#[derive(Debug, Deserialize)]
struct GraphQlResponse<T> {
    data: Option<T>,
    #[serde(default)]
    errors: Vec<GraphQlError>,
}

#[derive(Debug, Deserialize)]
struct GraphQlError {
    message: String,
}

#[derive(Debug, Deserialize)]
struct RawUser {
    login: String,
    /// `null` when the user has not set a display name.
    name: Option<String>,
}

impl From<RawUser> for User {
    fn from(raw: RawUser) -> Self {
        User {
            name: raw.name.unwrap_or_else(|| raw.login.clone()),
            username: raw.login,
        }
    }
}

#[derive(Debug, Deserialize)]
struct SearchData {
    viewer: RawUser,
    search: SearchResult,
}

#[derive(Debug, Deserialize)]
struct SearchResult {
    nodes: Vec<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPullRequest {
    number: u64,
    title: String,
    is_draft: bool,
    url: String,
    updated_at: DateTime<Utc>,
    head_ref_name: String,
    base_ref_name: String,
    /// `null` for deleted accounts.
    author: Option<RawLogin>,
    repository: RawRepositoryRef,
    latest_reviews: Nodes<RawReview>,
    commits: Nodes<RawCommitNode>,
}

#[derive(Debug, Deserialize)]
struct Nodes<T> {
    nodes: Vec<T>,
}

#[derive(Debug, Deserialize)]
struct RawLogin {
    login: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRepositoryRef {
    name_with_owner: String,
}

#[derive(Debug, Deserialize)]
struct RawReview {
    state: String,
    author: Option<RawLogin>,
}

#[derive(Debug, Deserialize)]
struct RawCommitNode {
    commit: RawCommit,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCommit {
    /// `null` when no checks or statuses ran on the commit.
    status_check_rollup: Option<RawRollup>,
}

#[derive(Debug, Deserialize)]
struct RawRollup {
    state: String,
}

/// GitHub's combined check state (`StatusState`) as a pipeline status.
pub(super) fn rollup_status(state: &str) -> PipelineStatus {
    match state {
        "SUCCESS" => PipelineStatus::Success,
        "FAILURE" | "ERROR" => PipelineStatus::Failed,
        "PENDING" => PipelineStatus::Running,
        "EXPECTED" => PipelineStatus::Pending,
        _ => PipelineStatus::Unknown,
    }
}

impl RawPullRequest {
    fn into_model(self) -> MergeRequest {
        let author = self.author.map_or_else(|| "ghost".to_string(), |a| a.login);
        MergeRequest {
            iid: self.number,
            title: self.title,
            draft: self.is_draft,
            author: User {
                username: author.clone(),
                name: author,
            },
            project_path: self.repository.name_with_owner,
            source_branch: self.head_ref_name,
            target_branch: self.base_ref_name,
            web_url: self.url,
            updated_at: self.updated_at,
            pipeline_status: self
                .commits
                .nodes
                .into_iter()
                .next()
                .and_then(|c| c.commit.status_check_rollup)
                .map(|r| rollup_status(&r.state)),
            approved_by: self
                .latest_reviews
                .nodes
                .into_iter()
                .filter(|r| r.state == "APPROVED")
                .filter_map(|r| r.author.map(|a| a.login))
                .collect(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct RawRepository {
    full_name: String,
    description: Option<String>,
    pushed_at: Option<DateTime<Utc>>,
    html_url: String,
    #[serde(default)]
    archived: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// A client pointed at the mock server as if it were GitHub Enterprise (`/api/v3/`).
    fn client_for(server: &MockServer) -> GitHubClient {
        let host = crate::config::parse_host(&server.uri()).unwrap();
        GitHubClient::new(&host, "ghp_test").unwrap()
    }

    #[test]
    fn github_com_uses_the_api_subdomain() {
        let client = GitHubClient::new(&Url::parse("https://github.com/").unwrap(), "t").unwrap();
        assert_eq!(client.api_base.as_str(), "https://api.github.com/");
        assert_eq!(
            client.graphql_url.as_str(),
            "https://api.github.com/graphql"
        );
        let ghe = Url::parse("https://github.example.com/").unwrap();
        let client = GitHubClient::new(&ghe, "t").unwrap();
        assert_eq!(
            client.api_base.as_str(),
            "https://github.example.com/api/v3/"
        );
        assert_eq!(
            client.graphql_url.as_str(),
            "https://github.example.com/api/graphql"
        );
    }

    #[test]
    fn search_queries_per_list() {
        assert!(search_query(&MrList::ReviewRequested).ends_with("review-requested:@me"));
        assert!(
            search_query(&MrList::Project("rust-lang/rust".into()))
                .ends_with("repo:rust-lang/rust")
        );
        assert!(search_query(&MrList::AssignedToMe).starts_with("is:pr is:open"));
    }

    #[tokio::test]
    async fn reads_the_user_and_token_scopes() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/user"))
            .and(header("Authorization", "Bearer ghp_test"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("x-oauth-scopes", "repo, workflow")
                    .set_body_json(json!({"login": "octocat", "name": null, "id": 1})),
            )
            .mount(&server)
            .await;

        let client = client_for(&server);
        let user = client.current_user().await.unwrap();
        assert_eq!(
            (user.username.as_str(), user.name.as_str()),
            ("octocat", "octocat")
        );
        assert_eq!(
            client.token_scopes().await.unwrap(),
            vec!["repo", "workflow"]
        );
    }

    #[tokio::test]
    async fn bad_credentials_are_unauthorized() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(401).set_body_json(
                json!({"message": "Bad credentials", "documentation_url": "https://docs.github.com/rest"}),
            ))
            .mount(&server)
            .await;
        let err = client_for(&server).current_user().await.unwrap_err();
        assert!(matches!(err, ApiError::Unauthorized));
    }

    #[tokio::test]
    async fn lists_pull_requests_with_checks_and_approvals() {
        let server = MockServer::start().await;
        let pr = json!({
            "number": 42, "title": "Add retries", "isDraft": false,
            "url": "https://github.com/acme/api/pull/42", "updatedAt": "2026-10-10T08:00:00Z",
            "headRefName": "feat/retry", "baseRefName": "main",
            "author": {"login": "linh"}, "repository": {"nameWithOwner": "acme/api"},
            "latestReviews": {"nodes": [
                {"state": "APPROVED", "author": {"login": "hoang"}},
                {"state": "CHANGES_REQUESTED", "author": {"login": "mai"}}
            ]},
            "commits": {"nodes": [{"commit": {"statusCheckRollup": {"state": "FAILURE"}}}]}
        });
        let no_checks = json!({
            "number": 7, "title": "Docs", "isDraft": true,
            "url": "https://github.com/acme/web/pull/7", "updatedAt": "2026-10-09T08:00:00Z",
            "headRefName": "docs", "baseRefName": "main",
            "author": null, "repository": {"nameWithOwner": "acme/web"},
            "latestReviews": {"nodes": []},
            "commits": {"nodes": [{"commit": {"statusCheckRollup": null}}]}
        });
        Mock::given(method("POST"))
            .and(path("/api/graphql"))
            .and(body_string_contains("review-requested:@me"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": {
                "viewer": {"login": "thang", "name": "Thang"},
                // An issue (empty object) would be skipped.
                "search": {"nodes": [pr, {}, no_checks]}
            }})))
            .expect(1)
            .mount(&server)
            .await;

        let page = client_for(&server)
            .merge_requests(&MrList::ReviewRequested)
            .await
            .unwrap();

        assert_eq!(page.current_user.username, "thang");
        assert_eq!(page.merge_requests.len(), 2);
        let first = &page.merge_requests[0];
        assert_eq!(first.iid, 42);
        assert_eq!(first.project_path, "acme/api");
        assert_eq!(first.pipeline_status, Some(PipelineStatus::Failed));
        assert_eq!(first.approved_by, vec!["hoang"]);
        let second = &page.merge_requests[1];
        assert!(second.draft);
        assert_eq!(second.author.username, "ghost");
        assert_eq!(second.pipeline_status, None);
    }

    #[tokio::test]
    async fn graphql_errors_are_reported() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"data": null, "errors": [{"message": "Field 'foo' doesn't exist"}]}),
            ))
            .mount(&server)
            .await;
        let err = client_for(&server)
            .merge_requests(&MrList::AssignedToMe)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("foo"));
    }

    const PR: &str = "/api/v3/repos/acme/api/pulls/42";

    async fn mount(server: &MockServer, verb: &str, route: &str, body: Value) {
        Mock::given(method(verb))
            .and(path(route))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    async fn mount_detail(server: &MockServer) {
        mount(server, "GET", PR, json!({
            "number": 42, "title": "Add retries", "body": "Retries 5xx.", "state": "open",
            "merged": false, "draft": false, "user": {"login": "linh"},
            "head": {"ref": "feat/retry", "sha": "abc123"}, "base": {"ref": "main", "sha": "def"},
            "html_url": "https://github.com/acme/api/pull/42",
            "mergeable": true, "mergeable_state": "clean"
        })).await;
        mount(
            server,
            "GET",
            &format!("{PR}/files"),
            json!([
                {"filename": "src/retry.rs", "status": "added", "additions": 2, "deletions": 0,
                 "patch": "@@ -0,0 +1,2 @@\n+fn retry() {}\n+"}
            ]),
        )
        .await;
        mount(
            server,
            "GET",
            &format!("{PR}/reviews"),
            json!([
                {"id": 1, "user": {"login": "hoang"}, "state": "APPROVED", "body": "",
                 "submitted_at": "2026-10-10T08:00:00Z"}
            ]),
        )
        .await;
        mount(server, "GET", "/api/v3/repos/acme/api/issues/42/comments", json!([
            {"id": 5, "body": "Thanks!", "user": {"login": "mai"}, "created_at": "2026-10-10T07:00:00Z"}
        ])).await;
        mount(server, "GET", &format!("{PR}/comments"), json!([])).await;
        mount(server, "GET", "/api/v3/repos/acme/api", json!({
            "permissions": {"push": true}, "allow_merge_commit": false, "allow_squash_merge": true
        })).await;
        mount(
            server,
            "GET",
            "/api/v3/user",
            json!({"login": "thang", "name": "Thang"}),
        )
        .await;
        mount(
            server,
            "POST",
            "/api/graphql",
            json!({"data": {"repository": {"pullRequest": {
                "commits": {"nodes": [{"commit": {"statusCheckRollup": {"state": "SUCCESS"}}}]}
            }}}}),
        )
        .await;
    }

    #[tokio::test]
    async fn loads_pull_request_details() {
        let server = MockServer::start().await;
        mount_detail(&server).await;

        let detail = client_for(&server)
            .merge_request_detail("acme/api", 42)
            .await
            .unwrap();

        assert_eq!(detail.state, "opened");
        assert_eq!(detail.sha.as_deref(), Some("abc123"));
        assert!(detail.can_merge);
        assert_eq!(detail.pipeline_status, Some(PipelineStatus::Success));
        assert_eq!(detail.files[0].added, 2);
        assert!(detail.files[0].new_file);
        assert_eq!(detail.approvals.approved_by[0].username, "hoang");
        assert!(detail.approvals.user_can_approve);
        // The comment and the approval, oldest first.
        assert_eq!(detail.discussions.len(), 2);
        assert_eq!(detail.discussions[0].notes[0].body, "Thanks!");
    }

    #[tokio::test]
    async fn merge_uses_an_allowed_method_and_the_reviewed_sha() {
        let server = MockServer::start().await;
        mount_detail(&server).await;
        Mock::given(method("PUT"))
            .and(path(format!("{PR}/merge")))
            .and(body_string_contains("\"merge_method\":\"squash\""))
            .and(body_string_contains("\"sha\":\"abc123\""))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"merged": true})))
            .expect(1)
            .mount(&server)
            .await;

        let merge = MrAction::Merge {
            sha: Some("abc123".into()),
        };
        client_for(&server)
            .run_action("acme/api", 42, &merge)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn explains_refused_actions() {
        let server = MockServer::start().await;
        mount_detail(&server).await;
        Mock::given(method("PUT"))
            .and(path(format!("{PR}/merge")))
            .respond_with(
                ResponseTemplate::new(409)
                    .set_body_json(json!({"message": "Head branch was modified."})),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("{PR}/reviews")))
            .respond_with(ResponseTemplate::new(422).set_body_json(
                json!({"message": "Unprocessable Entity: Can not approve your own pull request"}),
            ))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v3/repos/acme/api/issues/42/comments"))
            .and(body_string_contains("LGTM"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": 9})))
            .expect(1)
            .mount(&server)
            .await;

        let client = client_for(&server);
        let merge = MrAction::Merge {
            sha: Some("old".into()),
        };
        let err = client.run_action("acme/api", 42, &merge).await.unwrap_err();
        assert!(err.to_string().contains("head branch changed"), "{err}");
        let err = client
            .run_action("acme/api", 42, &MrAction::Approve)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("own pull request"), "{err}");
        let err = client
            .run_action("acme/api", 42, &MrAction::Unapprove)
            .await
            .unwrap_err();
        assert!(matches!(err, ApiError::Unsupported(_)));
        client
            .run_action("acme/api", 42, &MrAction::Comment("LGTM".into()))
            .await
            .unwrap();
    }

    fn run_json(id: u64, status: &str, conclusion: Option<&str>) -> Value {
        json!({"id": id, "name": "CI", "status": status, "conclusion": conclusion,
               "head_branch": "feat/retry", "head_sha": "abc123", "event": "pull_request",
               "created_at": format!("2026-10-10T08:0{}:00Z", id % 10),
               "updated_at": "2026-10-10T08:09:00Z", "run_started_at": null,
               "html_url": format!("https://github.com/acme/api/actions/runs/{id}"),
               "triggering_actor": {"login": "thang"}, "actor": {"login": "thang"}})
    }

    #[tokio::test]
    async fn loads_the_runs_of_a_pull_requests_head_commit() {
        let server = MockServer::start().await;
        mount_detail(&server).await;
        Mock::given(method("GET"))
            .and(path("/api/v3/repos/acme/api/actions/runs"))
            .and(query_param("head_sha", "abc123"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "total_count": 2,
                "workflow_runs": [run_json(2, "in_progress", None), run_json(1, "completed", Some("failure"))]
            })))
            .mount(&server)
            .await;
        mount(&server, "GET", "/api/v3/repos/acme/api/actions/runs/1/jobs", json!({"jobs": [
            {"id": 11, "name": "test", "status": "completed", "conclusion": "failure",
             "started_at": "2026-10-10T08:01:00Z", "completed_at": "2026-10-10T08:03:00Z", "html_url": null},
            {"id": 10, "name": "lint", "status": "completed", "conclusion": "success",
             "started_at": null, "completed_at": null, "html_url": null}
        ]})).await;

        let ci = client_for(&server)
            .merge_request_ci("acme/api", 42, Some(1))
            .await
            .unwrap();

        assert_eq!(ci.pipelines.len(), 2);
        assert_eq!(ci.pipeline_id, Some(1));
        assert_eq!(ci.pipelines[1].status, PipelineStatus::Failed);
        let jobs: Vec<_> = ci
            .jobs
            .iter()
            .map(|j| (j.stage.as_str(), j.name.as_str()))
            .collect();
        assert_eq!(jobs, vec![("CI", "lint"), ("CI", "test")]);
    }

    #[tokio::test]
    async fn follows_the_log_redirect_and_cleans_the_log() {
        let server = MockServer::start().await;
        // GitHub answers with a redirect to a download URL on another host. `localhost` and
        // `127.0.0.1` count as different hosts, like api.github.com and the log storage.
        let other_host = server.uri().replace("127.0.0.1", "localhost");
        Mock::given(method("GET"))
            .and(path("/api/v3/repos/acme/api/actions/jobs/11/logs"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", format!("{other_host}/download/log.txt")),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/download/log.txt"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "2026-10-10T08:01:00.0000000Z ##[group]Run cargo test\n2026-10-10T08:01:05.0000000Z ok\n",
            ))
            .mount(&server)
            .await;

        let log = client_for(&server).job_trace("acme/api", 11).await.unwrap();
        assert!(log.contains("▸ Run cargo test"));
        assert!(log.ends_with("ok\n"));

        // The token must not follow the redirect to the other host.
        let requests = server.received_requests().await.unwrap();
        let download = requests
            .iter()
            .find(|r| r.url.path() == "/download/log.txt")
            .expect("the download was requested");
        assert!(
            download.headers.get("authorization").is_none(),
            "token leaked"
        );
    }

    #[tokio::test]
    async fn reruns_and_cancels_runs() {
        let server = MockServer::start().await;
        for route in [
            "/api/v3/repos/acme/api/actions/jobs/11/rerun",
            "/api/v3/repos/acme/api/actions/runs/1/rerun-failed-jobs",
            "/api/v3/repos/acme/api/actions/runs/1/cancel",
        ] {
            Mock::given(method("POST"))
                .and(path(route))
                .respond_with(ResponseTemplate::new(201).set_body_json(json!({})))
                .expect(1)
                .mount(&server)
                .await;
        }
        let client = client_for(&server);
        client
            .run_ci_action("acme/api", CiAction::RetryJob(11))
            .await
            .unwrap();
        client
            .run_ci_action("acme/api", CiAction::RetryPipeline(1))
            .await
            .unwrap();
        client
            .run_ci_action("acme/api", CiAction::CancelPipeline(1))
            .await
            .unwrap();
        let err = client
            .run_ci_action("acme/api", CiAction::CancelJob(11))
            .await
            .unwrap_err();
        assert!(matches!(err, ApiError::Unsupported(_)));
    }

    #[tokio::test]
    async fn dashboard_runs_of_my_repositories_filtered_by_actor() {
        let server = MockServer::start().await;
        mount(
            &server,
            "GET",
            "/api/v3/user",
            json!({"login": "thang", "name": "Thang"}),
        )
        .await;
        mount(&server, "GET", "/api/v3/user/repos", json!([
            {"full_name": "acme/api", "description": null, "pushed_at": null, "html_url": "x"},
            {"full_name": "acme/broken", "description": null, "pushed_at": null, "html_url": "x"}
        ])).await;
        Mock::given(method("GET"))
            .and(path("/api/v3/repos/acme/api/actions/runs"))
            .and(query_param("actor", "thang"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "workflow_runs": [run_json(1, "completed", Some("success")), run_json(3, "queued", None)]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v3/repos/acme/broken/actions/runs"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({"message": "Not Found"})))
            .mount(&server)
            .await;

        let page = client_for(&server)
            .pipelines(&PipelineList::Mine)
            .await
            .unwrap();
        // The repository that failed does not hide the other one; newest first.
        let ids: Vec<_> = page.pipelines.iter().map(|p| p.id).collect();
        assert_eq!(ids, vec![3, 1]);
        assert_eq!(page.pipelines[0].name.as_deref(), Some("CI"));
        assert_eq!(page.pipelines[1].project_path, "acme/api");
    }

    #[test]
    fn repository_paths_are_two_segments() {
        let client = GitHubClient::new(&Url::parse("https://github.com/").unwrap(), "t").unwrap();
        let url = client.repo_url("acme/api", &["pulls", "7"]).unwrap();
        assert_eq!(
            url.as_str(),
            "https://api.github.com/repos/acme/api/pulls/7"
        );
        assert!(client.repo_url("no-slash", &[]).is_err());
    }

    #[tokio::test]
    async fn lists_repositories_skipping_archived_ones() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/user/repos"))
            .and(query_param("sort", "pushed"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"full_name": "acme/api", "description": "API", "pushed_at": "2026-10-10T08:00:00Z",
                 "html_url": "https://github.com/acme/api", "archived": false},
                {"full_name": "acme/old", "description": null, "pushed_at": null,
                 "html_url": "https://github.com/acme/old", "archived": true}
            ])))
            .mount(&server)
            .await;
        let repos = client_for(&server).repositories().await.unwrap();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].path, "acme/api");
    }

    #[tokio::test]
    async fn branches_come_from_one_graphql_query() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/graphql"))
            .and(body_string_contains("refs/heads/"))
            .and(body_string_contains("\"owner\":\"acme\""))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": {"repository": {
                "url": "https://github.com/acme/api",
                "defaultBranchRef": {"name": "main"},
                "refs": {"nodes": [
                    {"name": "feat/x",
                     "target": {"messageHeadline": "Add x", "committedDate": "2026-10-09T00:00:00Z",
                                "author": {"name": "Linh N", "user": {"login": "linh"}},
                                "statusCheckRollup": {"state": "FAILURE"}},
                     "associatedPullRequests": {"nodes": [{"number": 12}]}},
                    {"name": "main",
                     "target": {"messageHeadline": "Merge", "committedDate": "2026-10-01T00:00:00Z",
                                "author": {"name": "Thang", "user": null},
                                "statusCheckRollup": null},
                     "associatedPullRequests": {"nodes": []}}
                ]}
            }}})))
            .expect(1)
            .mount(&server)
            .await;

        let page = client_for(&server).branches("acme/api").await.unwrap();
        let names: Vec<&str> = page.branches.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, ["main", "feat/x"], "default branch first");
        let feat = &page.branches[1];
        assert_eq!(feat.commit_author.as_deref(), Some("linh"));
        assert_eq!(feat.pipeline_status, Some(PipelineStatus::Failed));
        assert_eq!(feat.merge_request, Some(12));
        assert_eq!(feat.web_url, "https://github.com/acme/api/tree/feat/x");
        assert_eq!(page.branches[0].commit_author.as_deref(), Some("Thang"));
    }

    #[tokio::test]
    async fn missing_repository_is_not_found() {
        let server = MockServer::start().await;
        mount(
            &server,
            "POST",
            "/api/graphql",
            json!({"data": {"repository": null}}),
        )
        .await;
        let err = client_for(&server).branches("acme/gone").await.unwrap_err();
        assert!(matches!(err, ApiError::NotFound(_)), "{err:?}");
    }

    #[tokio::test]
    async fn compare_keeps_the_slashes_of_branch_names() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/repos/acme/api/compare/main...feature/x"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "total_commits": 3,
                "html_url": "https://github.com/acme/api/compare/main...feature/x",
                "files": [{"filename": "src/x.rs", "status": "added", "additions": 1,
                           "deletions": 0, "patch": "@@ -0,0 +1 @@\n+fn x() {}"}]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let comparison = client_for(&server)
            .compare("acme/api", "main", "feature/x")
            .await
            .unwrap();
        assert_eq!(comparison.commits, 3);
        assert!(comparison.files[0].new_file);
    }
}
