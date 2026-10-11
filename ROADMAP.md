# Roadmap

## 0.1 (in progress)

- [x] Config file and environment variables, multiple profiles
- [x] List merge requests assigned to me (`j`/`k`/`g`/`G`/`r`/`q`)
- [x] More MR filters: review requested, authored by me, current project from `git remote`
- [x] Pipeline status and approvals columns
- [x] `/` filter, `o` open in browser, periodic refresh
- [ ] Show required approvals (`1/2`): needs GitLab Premium fields, check availability first
- [x] Choose platform, account and repository in the app (`s`); reopen the last choice
- [x] MR detail: description, changed files, diff, discussions
- [x] MR actions: approve, unapprove, comment, merge (with confirmation)
- [ ] Comment editor: cursor movement, or write the comment in `$EDITOR`
- [ ] Render Markdown in descriptions and comments (currently raw text)
- [ ] Reply to a discussion thread, resolve threads
- [x] Pipelines and jobs: status, duration, job log (follow while running), retry, cancel
- [ ] Pipelines tab on the dashboard (pipelines of my projects, not only of one merge request)
- [ ] Run manual jobs (`POST /jobs/:id/play`)
- [ ] Job logs: fetch only the new part while a job runs (now the whole log is fetched every 2 s), search with `/`, wrap or scroll long lines horizontally
- [ ] Dashboard tabs (MRs / Pipelines) and a context-aware key bar
- [x] Store tokens in the OS keyring (`lazyforge auth login | status | logout`)
- [ ] OAuth device flow login (needs an OAuth application registered on each GitLab server)
- [ ] Pagination of merge request lists (detail endpoints are already paginated)
- [x] Comment on a line of the diff (`c` with the line cursor), on GitLab and GitHub
- [x] Show threads under their diff lines, and reply to a thread from the diff
- [x] Check out a merge request, pull request or branch into the local clone (`C`), and open it in `$EDITOR` (`e`)
- [x] Branches view (`3`): branches of the chosen repository with last commit, CI and open merge request; `Enter` compares a branch with the default branch
- [ ] Browse a branch's files (tree and file contents), not only its changes; more than the 100 most recent branches
- [x] GitHub Actions: CI (fmt, clippy, tests on Linux/macOS/Windows, MSRV) and draft releases with binaries for Linux, macOS, Windows
- [ ] Sign and notarize macOS release binaries with a Developer ID, so the keychain does not ask again after each upgrade

## Later

- Issues
- Creating and editing merge requests
- Notifications
- Side-by-side diff, syntax highlighting (`syntect` behind a feature flag)
- GitHub support. Done: shared `forge/` models, `enum Forge`, GitHub login, pull request lists, platform/account/repository picker, pull request details (diff, comments, review threads) and actions (approve, comment, merge), GitHub Actions (workflow runs, jobs, logs of finished jobs, re-run, cancel run, dashboard). Next: required reviewers from branch protection; live logs if GitHub ever exposes them. Original plan:
  - Add a `forge/` module with shared models (`ChangeRequest`, `CiRun`, `CiJob`, `ReviewState`) that each backend converts into.
  - Pick the backend with an enum (`enum Forge { GitLab(..), GitHub(..) }`) rather than `dyn Trait`: there are only two backends, and async methods on trait objects need extra machinery.
  - Expose per-backend capabilities (for example `supports_live_logs()`) instead of hiding the differences. GitHub's REST API only returns job logs after the job finishes, and it uses reviews (approve / request changes) instead of approvals.
  - Extract the shared models only once the GitHub backend exists, so they follow two real backends instead of guesses.
  - The project would likely need a new name. Done: renamed from lazygl to lazyforge.
