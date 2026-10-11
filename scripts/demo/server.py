"""A fake GitHub Enterprise server for recording the README demo.

Everything here is made up (the "acme" organization, its repositories, people, pull
requests, CI runs and branches), so the demo never shows anyone's real data. Python's
standard library only. See scripts/demo/record.py.
"""

import datetime as dt
import http.server
import json
import re
import sys

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 8080
HOST = f"http://github.acme.localhost:{PORT}"
TOKEN = "demo-token"

now = dt.datetime.now(dt.timezone.utc)


def ago(**delta):
    return (now - dt.timedelta(**delta)).isoformat().replace("+00:00", "Z")


def pr(number, title, author, branch, minutes, checks, approvals=(), draft=False, repo="acme/api"):
    return {
        "number": number, "title": title, "isDraft": draft,
        "url": f"{HOST}/{repo}/pull/{number}", "updatedAt": ago(minutes=minutes),
        "headRefName": branch, "baseRefName": "main", "author": {"login": author},
        "repository": {"nameWithOwner": repo},
        "latestReviews": {"nodes": [{"state": "APPROVED", "author": {"login": a}} for a in approvals]},
        "commits": {"nodes": [{"commit": {"statusCheckRollup": {"state": checks} if checks else None}}]},
    }


PULLS = [
    pr(128, "Add rate limiting to the public API", "linh", "feat/rate-limit", 12, "FAILURE"),
    pr(124, "Cache product search results", "mai", "feat/search-cache", 70, "PENDING", ["hoang"]),
    pr(119, "Bump tokio to 1.53", "dependabot", "deps/tokio", 300, "SUCCESS", ["thang", "linh"]),
    pr(112, "Retry webhooks with exponential backoff", "hoang", "fix/webhook-retry", 1500, "SUCCESS", ["mai"]),
    pr(77, "WIP: new onboarding flow", "thang", "feat/onboarding", 4000, None, (), True, "acme/web"),
]

DIFF_ROUTER = """@@ -10,6 +10,22 @@ use axum::Router;

 pub fn router() -> Router {
     Router::new()
-        .route("/v1/items", get(items))
+        .route("/v1/items", get(items))
+        .layer(RateLimitLayer::new(ANON_LIMIT, Duration::from_secs(60)))
+}
+
+/// Requests per minute for clients without an API key.
+pub const ANON_LIMIT: u32 = 100;
+
+/// Paying keys get their own limit; everyone else the anonymous one.
+fn limit_for(key: Option<&ApiKey>) -> u32 {
+    key.map_or(ANON_LIMIT, |k| k.limit)
+}
+
+#[cfg(test)]
+mod tests {
+    #[test]
+    fn limits_anonymous_clients() { /* ... */ }
 }"""

FILES = [
    {"filename": "src/router.rs", "status": "modified", "additions": 17, "deletions": 1, "patch": DIFF_ROUTER},
    {"filename": "src/limits.rs", "status": "added", "additions": 6, "deletions": 0,
     "patch": "@@ -0,0 +1,6 @@\n+pub struct ApiKey {\n+    pub id: String,\n+    pub limit: u32,\n+}\n+\n+pub const DEFAULT_KEY_LIMIT: u32 = 1_000;"},
    {"filename": "Cargo.toml", "status": "modified", "additions": 1, "deletions": 0,
     "patch": "@@ -8,3 +8,4 @@ axum = \"0.8\"\n tokio = \"1\"\n+tower-governor = \"0.6\"\n serde = \"1\""},
]

PULL_128 = {
    "number": 128, "title": "Add rate limiting to the public API",
    "body": "Adds a per-key rate limit: 100 requests/min for anonymous clients,\n"
            "the key's own limit for paying customers.\n\nCloses #120.",
    "state": "open", "merged": False, "draft": False, "user": {"login": "linh"},
    "head": {"ref": "feat/rate-limit", "sha": "9f8e7d6c5b4a"}, "base": {"ref": "main", "sha": "1a2b3c"},
    "html_url": f"{HOST}/acme/api/pull/128", "mergeable": True, "mergeable_state": "unstable",
}

REVIEWS = [{"id": 10, "user": {"login": "hoang"}, "state": "COMMENTED", "body": "", "submitted_at": ago(minutes=40)}]
COMMENTS = [{"id": 1, "body": "Can we make the anonymous limit configurable?", "user": {"login": "mai"},
             "created_at": ago(minutes=50)}]
LINE_COMMENTS = [
    {"id": 200, "body": "100/min feels low for paying keys.", "user": {"login": "hoang"},
     "created_at": ago(minutes=40), "path": "src/router.rs", "line": 21, "original_line": 21, "in_reply_to_id": None},
    {"id": 201, "body": "Paying keys use k.limit; this is only the anonymous default.", "user": {"login": "linh"},
     "created_at": ago(minutes=35), "path": "src/router.rs", "line": 21, "original_line": 21, "in_reply_to_id": 200},
]


def run(run_id, name, conclusion, minutes, branch="feat/rate-limit", actor="linh", sha="9f8e7d6c5b4a"):
    return {
        "id": run_id, "name": name, "status": "completed", "conclusion": conclusion,
        "head_branch": branch, "head_sha": sha, "event": "pull_request",
        "created_at": ago(minutes=minutes), "updated_at": ago(minutes=minutes - 3),
        "run_started_at": ago(minutes=minutes), "html_url": f"{HOST}/acme/api/actions/runs/{run_id}",
        "triggering_actor": {"login": actor}, "actor": {"login": actor},
    }


RUNS = [
    run(5001, "CI", "failure", 9),
    run(5002, "Lint", "success", 10),
    run(4990, "CI", "success", 75, "feat/search-cache", "mai", "c0ffee"),
    run(4975, "Release", "success", 600, "v1.4.0", "thang", "beef01"),
    run(4960, "CI", "success", 1500, "fix/webhook-retry", "hoang", "f00d42"),
]

JOBS = {
    5001: [
        {"id": 9001, "name": "build", "status": "completed", "conclusion": "success",
         "started_at": ago(minutes=9), "completed_at": ago(minutes=7), "html_url": None},
        {"id": 9002, "name": "test (ubuntu-latest)", "status": "completed", "conclusion": "failure",
         "started_at": ago(minutes=7), "completed_at": ago(minutes=6), "html_url": None},
        {"id": 9003, "name": "test (macos-latest)", "status": "completed", "conclusion": "success",
         "started_at": ago(minutes=7), "completed_at": ago(minutes=5), "html_url": None},
    ],
    5002: [{"id": 9101, "name": "clippy", "status": "completed", "conclusion": "success",
            "started_at": ago(minutes=10), "completed_at": ago(minutes=9), "html_url": None}],
}

ESC = "\x1b"
LOGS = {
    9002: [
        "##[group]Run cargo test --workspace",
        ESC + "[36;1mcargo test --workspace" + ESC + "[0m",
        "##[endgroup]",
        "   Compiling acme-api v1.4.0 (/home/runner/work/api/api)",
        "    Finished `test` profile [unoptimized + debuginfo] target(s) in 41.20s",
        "     Running unittests src/lib.rs",
        "",
        "running 42 tests",
        "test router::tests::limits_anonymous_clients ... ok",
        "test router::tests::limits_api_keys ... FAILED",
        "test search::tests::ranks_exact_matches_first ... ok",
        "",
        "failures:",
        "---- router::tests::limits_api_keys stdout ----",
        "assertion `left == right` failed",
        "  left: 100",
        " right: 1000",
        "",
        "test result: FAILED. 41 passed; 1 failed; 0 ignored",
        "##[error]Process completed with exit code 101.",
    ],
}


def branch(name, title, author, minutes, checks=None, pull=None):
    return {
        "name": name,
        "target": {"messageHeadline": title, "committedDate": ago(minutes=minutes),
                   "author": {"name": author, "user": {"login": author}},
                   "statusCheckRollup": {"state": checks} if checks else None},
        "associatedPullRequests": {"nodes": [{"number": pull}] if pull else []},
    }


BRANCHES = [
    branch("feat/rate-limit", "Limit anonymous clients to 100 req/min", "linh", 12, "FAILURE", 128),
    branch("feat/search-cache", "Cache search results for 30 s", "mai", 70, "PENDING", 124),
    branch("main", "Merge pull request #118 from acme/fix/timeouts", "thang", 200, "SUCCESS"),
    branch("deps/tokio", "Bump tokio from 1.52 to 1.53", "dependabot", 300, "SUCCESS", 119),
    branch("release/1.4", "Release 1.4.0", "thang", 600, "SUCCESS"),
    branch("fix/webhook-retry", "Back off exponentially on 5xx", "hoang", 1500, "SUCCESS", 112),
    branch("spike/graphql", "Try async-graphql for the admin API", "mai", 60 * 24 * 40),
]


class Handler(http.server.BaseHTTPRequestHandler):
    def reply(self, code, body, content_type="application/json", headers=()):
        data = body if isinstance(body, bytes) else json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        for key, value in headers:
            self.send_header(key, value)
        self.end_headers()
        self.wfile.write(data)

    def authorized(self):
        return self.headers.get("Authorization") == f"Bearer {TOKEN}"

    def do_GET(self):
        path = self.path.split("?")[0]
        if path.startswith("/blob/"):
            # Like GitHub's signed log URLs: no token needed.
            job = int(re.findall(r"\d+", path)[0])
            text = "".join(f"2026-10-10T08:01:{i:02d}.0000000Z {line}\n" for i, line in enumerate(LOGS.get(job, [])))
            return self.reply(200, text.encode(), "text/plain")
        if not self.authorized():
            return self.reply(401, {"message": "Bad credentials"})
        # Only the first page has items (`[?&]`: not to be confused with `per_page=20`).
        if re.search(r"[?&]page=([2-9]|\d\d)", self.path):
            return self.reply(200, [])
        api = "/api/v3/repos/acme/api"
        routes = {
            "/api/v3/user": {"login": "thang", "name": "Thang"},
            "/api/v3/user/repos": [
                {"full_name": "acme/api", "description": "Public REST API", "pushed_at": ago(minutes=12),
                 "html_url": f"{HOST}/acme/api", "archived": False},
                {"full_name": "acme/web", "description": "Web frontend", "pushed_at": ago(hours=3),
                 "html_url": f"{HOST}/acme/web", "archived": False},
            ],
            api: {"permissions": {"push": True}, "allow_merge_commit": False, "allow_squash_merge": True},
            api + "/pulls/128": PULL_128,
            api + "/pulls/128/files": FILES,
            api + "/pulls/128/reviews": REVIEWS,
            api + "/pulls/128/comments": LINE_COMMENTS,
            api + "/issues/128/comments": COMMENTS,
            api + "/actions/runs": {"total_count": len(RUNS), "workflow_runs": RUNS},
            "/api/v3/repos/acme/web/actions/runs": {"workflow_runs": []},
        }
        if path == api + "/actions/runs" and (sha := re.search(r"head_sha=(\w+)", self.path)):
            runs = [r for r in RUNS if r["head_sha"] == sha[1]]
            return self.reply(200, {"total_count": len(runs), "workflow_runs": runs})
        if path in routes:
            headers = [("x-oauth-scopes", "repo, workflow, read:org")] if path == "/api/v3/user" else []
            return self.reply(200, routes[path], headers=headers)
        if m := re.fullmatch(api + r"/actions/runs/(\d+)", path):
            return self.reply(200, next(r for r in RUNS if r["id"] == int(m[1])))
        if m := re.fullmatch(api + r"/actions/runs/(\d+)/jobs", path):
            return self.reply(200, {"jobs": JOBS.get(int(m[1]), [])})
        if m := re.fullmatch(api + r"/actions/jobs/(\d+)/logs", path):
            return self.reply(302, b"", headers=[("Location", f"{HOST}/blob/{m[1]}.txt")])
        if m := re.fullmatch(api + r"/compare/(.+)\.\.\.(.+)", path):
            commits = 3 if m[2] == "feat/rate-limit" else 1
            return self.reply(200, {"total_commits": commits, "html_url": f"{HOST}/acme/api/compare/{m[1]}...{m[2]}",
                                    "files": FILES})
        self.reply(404, {"message": "Not Found"})

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get("Content-Length") or 0)) or b"{}")
        if not self.authorized():
            return self.reply(401, {"message": "Bad credentials"})
        query = body.get("query", "")
        if "refs/heads/" in query:
            return self.reply(200, {"data": {"repository": {
                "url": f"{HOST}/acme/api", "defaultBranchRef": {"name": "main"}, "refs": {"nodes": BRANCHES}}}})
        if "pullRequest(number" in query:
            return self.reply(200, {"data": {"repository": {"pullRequest": {"commits": {"nodes": [
                {"commit": {"statusCheckRollup": {"state": "FAILURE"}}}]}}}}})
        search = body.get("variables", {}).get("q", "")
        if "repo:" in search:
            repo = search.split("repo:")[1].split()[0]
            nodes = [p for p in PULLS if p["repository"]["nameWithOwner"] == repo]
        elif "review-requested:@me" in search:
            nodes = [PULLS[0], PULLS[1]]
        elif "author:@me" in search:
            nodes = [PULLS[4]]
        else:
            nodes = [PULLS[3]]
        self.reply(200, {"data": {"viewer": {"login": "thang", "name": "Thang"}, "search": {"nodes": nodes}}})

    def log_message(self, *args):
        pass


if __name__ == "__main__":
    print(f"Fake GitHub Enterprise for the demo on {HOST}", flush=True)
    http.server.HTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
