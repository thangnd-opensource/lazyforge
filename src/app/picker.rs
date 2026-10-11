//! The account switcher: platform → account → repository.
//!
//! Choosing an account connects to it right away (the runtime builds a new client); choosing
//! a repository, or "all my repositories", opens the dashboard for it and remembers the
//! choice for the next start.

use super::{Action, AppState, Command, Notice};
use crate::config::Account;
use crate::forge::{ApiError, ForgeKind, Repository};
use crate::selection::Selection;

/// The account the runtime connected to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectedAccount {
    /// Profile name.
    pub name: String,
    /// For display, e.g. `github.com` or `gitlab.example.com:8443`.
    pub host: String,
    pub kind: ForgeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerStep {
    Platform,
    Account,
    Repository,
}

#[derive(Debug)]
pub struct Picker {
    pub step: PickerStep,
    pub platform: ForgeKind,
    /// Cursor in the list of the current step.
    pub selected: usize,
    /// Set while the runtime connects to the chosen account.
    pub connecting: Option<String>,
    pub repos: Vec<Repository>,
    pub repos_loading: bool,
    pub repos_error: Option<String>,
    /// Typed on the repository step to narrow the list.
    pub filter: String,
    /// Opened from the dashboard: `Esc` on the first step goes back to it. On a first start
    /// there is no dashboard yet, so `Esc` there quits.
    pub can_close: bool,
    repos_request_id: u64,
}

impl Picker {
    pub fn new(platform: ForgeKind, can_close: bool) -> Self {
        Self {
            step: PickerStep::Platform,
            platform,
            selected: platform_index(platform),
            connecting: None,
            repos: Vec::new(),
            repos_loading: false,
            repos_error: None,
            filter: String::new(),
            can_close,
            repos_request_id: 0,
        }
    }

    /// Accounts of the chosen platform.
    pub fn accounts<'a>(&self, all: &'a [Account]) -> Vec<&'a Account> {
        all.iter().filter(|a| a.kind == self.platform).collect()
    }

    /// Repositories matching the filter. The list on screen has "all my repositories"
    /// first, so index `i` there is `visible_repos()[i - 1]`.
    pub fn visible_repos(&self) -> Vec<&Repository> {
        let needle = self.filter.to_lowercase();
        self.repos
            .iter()
            .filter(|r| r.path.to_lowercase().contains(&needle))
            .collect()
    }

    /// Number of rows in the current step's list.
    pub fn len(&self, accounts: &[Account]) -> usize {
        match self.step {
            PickerStep::Platform => PLATFORMS.len(),
            // The last row is "add an account".
            PickerStep::Account => self.accounts(accounts).len() + 1,
            PickerStep::Repository => self.visible_repos().len() + 1,
        }
    }
}

pub const PLATFORMS: [ForgeKind; 2] = [ForgeKind::GitLab, ForgeKind::GitHub];

fn platform_index(kind: ForgeKind) -> usize {
    PLATFORMS.iter().position(|k| *k == kind).unwrap_or(0)
}

/// Keys while the picker is open.
pub(super) fn update(state: &mut AppState, action: Action) -> Option<Command> {
    let accounts = state.accounts.clone();
    let picker = state.picker.as_mut()?;
    if picker.connecting.is_some() {
        // Wait for the connection; only quitting is allowed.
        return None;
    }
    let len = picker.len(&accounts);
    match action {
        Action::SelectNext => picker.selected = (picker.selected + 1).min(len.saturating_sub(1)),
        Action::SelectPrevious => picker.selected = picker.selected.saturating_sub(1),
        Action::PickerInput(c) if picker.step == PickerStep::Repository => {
            picker.filter.push(c);
            picker.selected = 0;
        }
        Action::PickerBackspace if picker.step == PickerStep::Repository => {
            picker.filter.pop();
            picker.selected = 0;
        }
        Action::Back => match picker.step {
            PickerStep::Platform if picker.can_close => state.picker = None,
            // First start: nothing to go back to, and Esc never quits (`q` does).
            PickerStep::Platform => {}
            PickerStep::Account => {
                picker.step = PickerStep::Platform;
                picker.selected = platform_index(picker.platform);
            }
            PickerStep::Repository => {
                picker.step = PickerStep::Account;
                picker.filter.clear();
                picker.selected = 0;
            }
        },
        Action::PickerSelect => match picker.step {
            PickerStep::Platform => {
                picker.platform = PLATFORMS[picker.selected.min(PLATFORMS.len() - 1)];
                picker.step = PickerStep::Account;
                // Start on the account in use, if it is on this platform.
                picker.selected = picker
                    .accounts(&accounts)
                    .iter()
                    .position(|a| a.name == state.profile_name)
                    .unwrap_or(0);
            }
            PickerStep::Account => {
                let choices = picker.accounts(&accounts);
                match choices.get(picker.selected) {
                    Some(account) => {
                        picker.connecting = Some(account.name.clone());
                        return Some(Command::SwitchAccount {
                            profile: account.name.clone(),
                        });
                    }
                    // "Add an account": logging in needs the terminal, outside the TUI.
                    None => {
                        let kind = picker.platform.name().to_lowercase();
                        state.notice = Some(Notice::info(format!(
                            "Quit, run `lazyforge auth login --kind {kind}`, then start lazyforge again"
                        )));
                    }
                }
            }
            PickerStep::Repository => {
                let repo = match picker.selected {
                    0 => None,
                    i => picker.visible_repos().get(i - 1).map(|r| r.path.clone()),
                };
                return Some(open_dashboard(state, repo));
            }
        },
        _ => {}
    }
    None
}

/// Closes the picker and loads the dashboard for `repo` (or all repositories).
fn open_dashboard(state: &mut AppState, repo: Option<String>) -> Command {
    let selection = Selection {
        profile: state.profile_name.clone(),
        repo: repo.clone(),
    };
    state.reset_dashboard(repo);
    state.picker = None;
    Command::Batch(vec![
        state.fetch_active(),
        Command::SaveSelection(selection),
    ])
}

/// The runtime finished connecting to an account.
pub(super) fn on_connected(
    state: &mut AppState,
    result: Result<ConnectedAccount, String>,
) -> Option<Command> {
    let picker = state.picker.as_mut()?;
    picker.connecting = None;
    let account = match result {
        Ok(account) => account,
        Err(err) => {
            state.notice = Some(Notice::error(err));
            return None;
        }
    };
    // The dashboard now belongs to the new account (all repositories, until one is picked),
    // so closing the picker never shows the previous account's data with the new client.
    state.profile_name = account.name;
    state.host = account.host;
    state.forge = account.kind;
    state.username = None;
    state.reset_dashboard(None);

    let picker = state.picker.as_mut()?;
    picker.step = PickerStep::Repository;
    picker.selected = 0;
    picker.filter.clear();
    picker.repos.clear();
    picker.repos_error = None;
    picker.repos_loading = true;
    state.next_request_id += 1;
    picker.repos_request_id = state.next_request_id;
    Some(Command::FetchRepositories {
        request_id: picker.repos_request_id,
    })
}

pub(super) fn on_repositories(
    state: &mut AppState,
    request_id: u64,
    result: Result<Vec<Repository>, ApiError>,
) {
    let Some(picker) = state
        .picker
        .as_mut()
        .filter(|p| p.repos_request_id == request_id)
    else {
        return;
    };
    picker.repos_loading = false;
    match result {
        Ok(repos) => picker.repos = repos,
        Err(err) => picker.repos_error = Some(err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::update;

    fn accounts() -> Vec<Account> {
        let account = |name: &str, host: &str, kind| Account {
            name: name.into(),
            host: crate::config::parse_host(host).unwrap(),
            kind,
        };
        vec![
            account("gitlab.com", "gitlab.com", ForgeKind::GitLab),
            account("github.com", "github.com", ForgeKind::GitHub),
            account("work", "github.example.com", ForgeKind::GitHub),
        ]
    }

    fn repo(path: &str) -> Repository {
        Repository {
            path: path.into(),
            description: None,
            last_activity: None,
            web_url: String::new(),
        }
    }

    /// A first start: no dashboard yet, the picker is open.
    fn first_start() -> AppState {
        let mut state = AppState::new(String::new(), String::new(), None);
        state.accounts = accounts();
        state.picker = Some(Picker::new(ForgeKind::GitLab, false));
        state
    }

    fn picker(state: &AppState) -> &Picker {
        state.picker.as_ref().expect("picker is open")
    }

    #[test]
    fn walks_platform_account_repository() {
        let mut state = first_start();
        update(&mut state, Action::SelectNext);
        update(&mut state, Action::PickerSelect);
        assert_eq!(picker(&state).step, PickerStep::Account);
        assert_eq!(picker(&state).platform, ForgeKind::GitHub);
        // Two GitHub accounts plus "add an account".
        assert_eq!(picker(&state).len(&state.accounts), 3);

        update(&mut state, Action::SelectNext);
        let command = update(&mut state, Action::PickerSelect);
        assert_eq!(
            command,
            Some(Command::SwitchAccount {
                profile: "work".into()
            })
        );
        // Keys wait while connecting.
        assert!(update(&mut state, Action::Back).is_none());
        assert_eq!(picker(&state).step, PickerStep::Account);

        let connected = ConnectedAccount {
            name: "work".into(),
            host: "github.example.com".into(),
            kind: ForgeKind::GitHub,
        };
        let command = update(&mut state, Action::AccountConnected(Ok(connected)));
        let Some(Command::FetchRepositories { request_id }) = command else {
            panic!("expected a repository fetch, got {command:?}");
        };
        assert_eq!(state.forge, ForgeKind::GitHub);
        assert_eq!(state.profile_name, "work");

        let repos = Ok(vec![repo("acme/api"), repo("acme/web")]);
        update(
            &mut state,
            Action::RepositoriesLoaded {
                request_id,
                result: repos,
            },
        );
        for c in "web".chars() {
            update(&mut state, Action::PickerInput(c));
        }
        update(&mut state, Action::SelectNext);
        let command = update(&mut state, Action::PickerSelect);

        assert!(state.picker.is_none());
        assert_eq!(
            state.repo.as_deref(),
            Some("acme/web"),
            "shown in the header"
        );
        let Some(Command::Batch(commands)) = command else {
            panic!("expected a batch");
        };
        assert!(commands.contains(&Command::SaveSelection(Selection {
            profile: "work".into(),
            repo: Some("acme/web".into()),
        })));
        // The repository's tab comes first and is the active one.
        assert_eq!(
            state.active().list,
            crate::forge::MrList::Project("acme/web".into())
        );
    }

    #[test]
    fn first_row_of_repositories_means_all_of_them() {
        let mut state = first_start();
        update(&mut state, Action::PickerSelect); // GitLab
        update(&mut state, Action::PickerSelect); // gitlab.com
        let connected = ConnectedAccount {
            name: "gitlab.com".into(),
            host: "gitlab.com".into(),
            kind: ForgeKind::GitLab,
        };
        update(&mut state, Action::AccountConnected(Ok(connected)));
        let Some(Command::Batch(commands)) = update(&mut state, Action::PickerSelect) else {
            panic!("expected a batch");
        };
        assert!(commands.contains(&Command::SaveSelection(Selection {
            profile: "gitlab.com".into(),
            repo: None,
        })));
        assert_eq!(state.active().list, crate::forge::MrList::AssignedToMe);
    }

    #[test]
    fn connection_errors_keep_the_picker_on_the_account_step() {
        let mut state = first_start();
        update(&mut state, Action::PickerSelect);
        update(&mut state, Action::PickerSelect);
        update(
            &mut state,
            Action::AccountConnected(Err("profile `gitlab.com` is not logged in".into())),
        );
        assert_eq!(picker(&state).step, PickerStep::Account);
        assert!(picker(&state).connecting.is_none());
        assert!(state.notice.as_ref().unwrap().is_error);
    }

    #[test]
    fn add_account_explains_how() {
        let mut state = first_start();
        update(&mut state, Action::PickerSelect); // GitLab: one account + "add"
        update(&mut state, Action::SelectNext);
        assert!(update(&mut state, Action::PickerSelect).is_none());
        assert!(
            state
                .notice
                .as_ref()
                .unwrap()
                .text
                .contains("auth login --kind gitlab")
        );
    }

    #[test]
    fn esc_goes_back_a_step_but_never_quits_on_first_start() {
        let mut state = first_start();
        update(&mut state, Action::PickerSelect);
        update(&mut state, Action::Back);
        assert_eq!(picker(&state).step, PickerStep::Platform);
        update(&mut state, Action::Back);
        assert!(!state.should_quit);
        assert!(state.picker.is_some());
    }

    #[test]
    fn esc_closes_a_picker_opened_from_the_dashboard() {
        let mut state = AppState::new("gitlab.com".into(), "gitlab.com".into(), None);
        state.accounts = accounts();
        update(&mut state, Action::OpenPicker);
        assert!(state.picker.is_some());
        update(&mut state, Action::Back);
        assert!(state.picker.is_none());
        assert!(!state.should_quit);
    }
}
