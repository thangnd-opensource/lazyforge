//! `C`: check out the merge request or branch on screen into the local repository (the one
//! lazyforge was started in), after a confirmation. `e`: open that repository in an editor.

use super::{Action, AppState, Command, Notice, View};
use crate::config;
use crate::forge::ForgeKind;
use crate::git::{CheckoutRequest, Target};

/// A checkout waiting for `y`.
#[derive(Debug)]
pub struct PendingCheckout {
    pub request: CheckoutRequest,
    /// What is checked out, for the popup: "!42 Fix login (fix/login)" or "branch fix/login".
    pub label: String,
}

/// What `C` would check out, and from which project.
fn target(state: &AppState) -> Result<(String, String, Target), String> {
    if let Some(screen) = &state.branch_screen {
        let name = screen.branch.name.clone();
        return Ok((
            screen.project.clone(),
            format!("branch {name}"),
            Target::Branch(name),
        ));
    }
    if let Some(detail) = &state.detail {
        return Ok((
            detail.project_path.clone(),
            format!(
                "{} {} ({})",
                detail.reference, detail.title, detail.source_branch
            ),
            change_request(state.forge, detail.iid, &detail.source_branch),
        ));
    }
    match state.view {
        View::MergeRequests => {
            let mr = state
                .selected_merge_request()
                .ok_or_else(|| "Nothing selected".to_string())?;
            Ok((
                mr.project_path.clone(),
                format!(
                    "{}{} {} ({})",
                    state.forge.reference_prefix(),
                    mr.iid,
                    mr.title,
                    mr.source_branch
                ),
                change_request(state.forge, mr.iid, &mr.source_branch),
            ))
        }
        View::Branches => {
            let project = state.repo.clone().ok_or("Pick a repository first (s)")?;
            let branch = state
                .selected_branch()
                .ok_or_else(|| "Nothing selected".to_string())?;
            let name = branch.name.clone();
            Ok((project, format!("branch {name}"), Target::Branch(name)))
        }
        View::Pipelines => Err(format!(
            "Open a {} or a branch to check it out",
            state
                .forge
                .change_requests()
                .to_lowercase()
                .trim_end_matches('s')
        )),
    }
}

fn change_request(forge: ForgeKind, number: u64, source_branch: &str) -> Target {
    let (head_ref, fallback_branch) = match forge {
        ForgeKind::GitLab => (
            format!("refs/merge-requests/{number}/head"),
            format!("mr/{number}"),
        ),
        ForgeKind::GitHub => (format!("refs/pull/{number}/head"), format!("pr/{number}")),
    };
    Target::ChangeRequest {
        head_ref,
        source_branch: source_branch.to_string(),
        fallback_branch,
    }
}

/// `C`: asks for a confirmation, or says why it cannot check out here.
pub(super) fn start(state: &mut AppState) -> Option<Command> {
    if state.checking_out {
        state.notice = Some(Notice::error("Wait: a checkout is running"));
        return None;
    }
    let (project, label, target) = match target(state) {
        Ok(found) => found,
        Err(message) => {
            state.notice = Some(Notice::error(message));
            return None;
        }
    };
    let Some(repo) = &state.local_repo else {
        state.notice = Some(Notice::error(format!(
            "Start lazyforge inside a clone of {project} to check it out"
        )));
        return None;
    };
    let remote = config::parse_host(&state.host)
        .ok()
        .and_then(|host| repo.remote_for(&host, &project).map(str::to_string));
    let Some(remote) = remote else {
        state.notice = Some(Notice::error(format!(
            "{} is not a clone of {project} on {}",
            repo.root.display(),
            state.host
        )));
        return None;
    };
    state.pending_checkout = Some(PendingCheckout {
        request: CheckoutRequest {
            root: repo.root.clone(),
            remote,
            target,
        },
        label,
    });
    None
}

/// Keys while the confirmation is open: `y` checks out, `n` / `Esc` cancel.
pub(super) fn update_confirm(state: &mut AppState, action: Action) -> Option<Command> {
    match action {
        Action::Confirm => {
            let pending = state.pending_checkout.take()?;
            state.checking_out = true;
            Some(Command::Checkout(pending.request))
        }
        Action::ClosePopup => {
            state.pending_checkout = None;
            None
        }
        _ => None,
    }
}

pub(super) fn on_finished(state: &mut AppState, result: Result<String, String>) {
    state.checking_out = false;
    state.notice = Some(match result {
        Ok(message) => Notice::info(format!("{message} · e opens it in your editor")),
        Err(message) => Notice::error(format!("Checkout failed: {message}")),
    });
}

/// `e`: the local repository in `$VISUAL` / `$EDITOR`.
pub(super) fn open_editor(state: &mut AppState) -> Option<Command> {
    match &state.local_repo {
        Some(repo) => Some(Command::OpenEditor(repo.root.clone())),
        None => {
            state.notice = Some(Notice::error(
                "Start lazyforge inside a git repository to open it in your editor",
            ));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{KeyContext, update};
    use crate::forge::{MergeRequest, MrPage, User};
    use crate::git::LocalRepo;
    use chrono::Utc;
    use std::path::Path;

    fn mr(iid: u64) -> MergeRequest {
        MergeRequest {
            iid,
            title: "Fix login".into(),
            draft: false,
            author: User {
                username: "linh".into(),
                name: "Linh".into(),
            },
            project_path: "group/api".into(),
            source_branch: "fix/login".into(),
            target_branch: "main".into(),
            web_url: String::new(),
            updated_at: Utc::now(),
            pipeline_status: None,
            approved_by: vec![],
        }
    }

    /// A merge request list, started inside a clone whose `origin` is `remote`.
    fn listed(remote: Option<&str>) -> AppState {
        let mut state = AppState::new("gitlab.com".into(), "gitlab.com".into(), None);
        let Some(Command::FetchMergeRequests { request_id, .. }) =
            update(&mut state, Action::Refresh)
        else {
            panic!("expected a fetch");
        };
        update(
            &mut state,
            Action::MergeRequestsLoaded {
                request_id,
                result: Ok(MrPage {
                    current_user: User {
                        username: "thang".into(),
                        name: "Thang".into(),
                    },
                    merge_requests: vec![mr(42)],
                }),
            },
        );
        state.local_repo = remote.map(|url| LocalRepo::for_tests(Path::new("/work/api"), url));
        state
    }

    #[test]
    fn asks_before_checking_out_a_merge_request() {
        let mut state = listed(Some("git@gitlab.com:group/api.git"));
        assert!(update(&mut state, Action::Checkout).is_none());
        assert_eq!(state.key_context(), KeyContext::Confirm);
        let pending = state.pending_checkout.as_ref().unwrap();
        assert_eq!(pending.label, "!42 Fix login (fix/login)");
        assert_eq!(pending.request.remote, "origin");
        assert_eq!(
            pending.request.target,
            Target::ChangeRequest {
                head_ref: "refs/merge-requests/42/head".into(),
                source_branch: "fix/login".into(),
                fallback_branch: "mr/42".into(),
            }
        );

        let command = update(&mut state, Action::Confirm);
        assert!(matches!(command, Some(Command::Checkout(_))));
        assert!(state.checking_out);
        assert_eq!(state.key_context(), KeyContext::List);

        update(
            &mut state,
            Action::CheckoutFinished(Ok("Checked out fix/login".into())),
        );
        assert!(!state.checking_out);
        assert!(
            state
                .notice
                .as_ref()
                .unwrap()
                .text
                .starts_with("Checked out fix/login")
        );
    }

    #[test]
    fn n_cancels() {
        let mut state = listed(Some("git@gitlab.com:group/api.git"));
        update(&mut state, Action::Checkout);
        assert!(update(&mut state, Action::ClosePopup).is_none());
        assert!(state.pending_checkout.is_none());
    }

    #[test]
    fn explains_when_this_is_not_the_right_clone() {
        let mut state = listed(None);
        update(&mut state, Action::Checkout);
        assert!(state.pending_checkout.is_none());
        assert!(
            state
                .notice
                .as_ref()
                .unwrap()
                .text
                .contains("inside a clone of group/api")
        );

        let mut state = listed(Some("git@gitlab.com:group/web.git"));
        update(&mut state, Action::Checkout);
        assert!(state.pending_checkout.is_none());
        assert!(
            state
                .notice
                .as_ref()
                .unwrap()
                .text
                .contains("is not a clone of group/api")
        );
    }

    #[test]
    fn github_uses_pull_refs() {
        assert_eq!(
            change_request(ForgeKind::GitHub, 7, "feat"),
            Target::ChangeRequest {
                head_ref: "refs/pull/7/head".into(),
                source_branch: "feat".into(),
                fallback_branch: "pr/7".into(),
            }
        );
    }

    #[test]
    fn e_opens_the_repository() {
        let mut state = listed(Some("git@gitlab.com:group/api.git"));
        assert_eq!(
            update(&mut state, Action::OpenEditor),
            Some(Command::OpenEditor("/work/api".into()))
        );
        let mut state = listed(None);
        assert!(update(&mut state, Action::OpenEditor).is_none());
        assert!(state.notice.is_some());
    }
}
