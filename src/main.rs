use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

use lazyforge::app::picker::{ConnectedAccount, Picker};
use lazyforge::app::{AppState, Notice};
use lazyforge::config::{self, ConfigFile};
use lazyforge::forge::{Forge, ForgeKind};
use lazyforge::runtime::Session;
use lazyforge::{auth, runtime, selection};

/// A lazygit-style terminal UI for GitLab and GitHub: merge/pull requests, pipelines and branches.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Profile from the config file to use.
    #[arg(short, long, global = true)]
    profile: Option<String>,

    /// Path to the config file [default: ~/.config/lazyforge/config.toml].
    #[arg(long, value_name = "PATH", global = true)]
    config: Option<PathBuf>,

    /// Open the platform → account → repository picker instead of the last choice.
    #[arg(long, conflicts_with = "project")]
    pick: bool,

    /// Open this repository, e.g. `group/project` or `owner/repo` [default: the account and
    /// repository chosen last time; `s` in the app switches].
    #[arg(long, value_name = "PATH")]
    project: Option<String>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Log in to GitLab or GitHub, check or remove saved tokens.
    Auth {
        #[command(subcommand)]
        action: AuthAction,
    },
}

#[derive(Debug, Subcommand)]
enum AuthAction {
    /// Save a personal access token, to use it from then on.
    Login {
        /// Server, e.g. `gitlab.example.com` or `github.com` [default: asks].
        #[arg(long)]
        host: Option<String>,

        /// `gitlab` or `github` [default: guessed from --host, or asked].
        #[arg(long, value_enum)]
        kind: Option<ForgeKind>,

        /// Where to save the token. `file` never asks for keychain permission (macOS asks
        /// again after every upgrade of an unsigned binary), but keeps the token in plain
        /// text in a file only you can read.
        #[arg(long, value_enum, default_value = "keychain")]
        store: auth::Store,

        /// GitHub: use the token of the GitHub CLI (`gh auth login`, which logs in through
        /// the browser) instead of pasting a personal access token.
        #[arg(long)]
        from_gh: bool,

        /// GitHub: paste a personal access token instead of logging in through the browser.
        #[arg(long, conflicts_with = "from_gh")]
        with_token: bool,
    },
    /// Show each profile and whether its token works.
    Status,
    /// Remove the saved token of a profile.
    Logout,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    // Before anything opens the log or the config: lazyforge used to be called lazygl.
    for (old, new) in config::migrate_legacy_dirs() {
        eprintln!(
            "Moved {} to {} (lazygl is now lazyforge).",
            old.display(),
            new.display()
        );
    }
    // Keep the guard alive until the end of `main`, otherwise buffered log lines are lost.
    let _log_guard = init_logging();

    // Everything that can fail with a readable message happens before the terminal switches
    // to the TUI, so errors print normally.
    let config_path = cli.config.or_else(config::default_config_path);
    let config_file = match &config_path {
        Some(path) => ConfigFile::load(path)?,
        None => None,
    };

    if let Some(Command::Auth { action }) = cli.command {
        return match action {
            AuthAction::Login {
                host,
                kind,
                store,
                from_gh,
                with_token,
            } => {
                let path = config_path.context("cannot find a config directory; pass --config")?;
                auth::login(&path, host, cli.profile, store, kind, from_gh, with_token).await
            }
            AuthAction::Status | AuthAction::Logout => {
                let path = config_path.context("cannot find a config directory; pass --config")?;
                match action {
                    AuthAction::Status => auth::status(&path, config_file.as_ref()).await,
                    _ => auth::logout(&path, config_file.as_ref(), cli.profile.as_deref()),
                }
            }
        };
    }

    let accounts = config::accounts(config_file.as_ref());
    let refresh_interval = config::refresh_interval(config_file.as_ref());
    let selection_path = selection::default_path();

    // Connecting to a profile: used now, and later by the account picker.
    let credentials_path = config_path.as_deref().map(config::credentials_path);
    let connect = move |name: Option<&str>| -> Result<(Forge, ConnectedAccount)> {
        let profile = config::resolve(
            config_file.as_ref(),
            name,
            |key| std::env::var(key).ok(),
            |profile, host| auth::saved_token(credentials_path.as_deref()?, profile, host),
        )?;
        tracing::info!(?profile, "connecting");
        let forge = Forge::connect(profile.kind, &profile.host, &profile.token)
            .context("cannot create the API client")?;
        let account = ConnectedAccount {
            name: profile.name,
            host: display_host(&profile.host),
            kind: profile.kind,
        };
        Ok((forge, account))
    };

    // Where to start: `--profile`/`--project` if given, else the account and repository
    // chosen last time, else the picker (or, with no profiles at all, `GITLAB_TOKEN`).
    let explicit = cli.profile.is_some() || cli.project.is_some();
    let last = selection_path
        .as_deref()
        .and_then(selection::load)
        .filter(|last| accounts.iter().any(|a| a.name == last.profile));
    let start = if cli.pick && !accounts.is_empty() {
        None
    } else if explicit {
        Some((cli.profile, cli.project))
    } else if let Some(last) = last {
        Some((Some(last.profile), last.repo))
    } else if accounts.is_empty() {
        Some((None, None))
    } else {
        None
    };

    let first_kind = accounts.first().map_or(ForgeKind::GitLab, |a| a.kind);
    let open_picker = |notice: Option<String>| {
        let mut state = AppState::new(String::new(), String::new(), None);
        state.picker = Some(Picker::new(first_kind, false));
        state.notice = notice.map(Notice::error);
        state
    };
    let (client, mut state) = match start {
        Some((profile, repo)) => match connect(profile.as_deref()) {
            Ok((forge, account)) => {
                let mut state = AppState::new(account.name, account.host, repo);
                state.forge = account.kind;
                (Some(forge), state)
            }
            // The account saved last time no longer works (logged out?): let the user pick.
            Err(err) if !explicit && !accounts.is_empty() => {
                (None, open_picker(Some(format!("{err:#}"))))
            }
            Err(err) => return Err(err),
        },
        None => (None, open_picker(None)),
    };
    state.accounts = accounts;
    let session = Session {
        connect: Box::new(move |name| connect(Some(name))),
        selection_path,
    };

    // `ratatui::init` enters raw mode + the alternate screen and installs a panic hook that
    // restores the terminal, so a panic does not leave the shell broken.
    let terminal = ratatui::init();
    enable_mouse();
    let result = runtime::run(terminal, state, client, refresh_interval, session).await;
    disable_mouse();
    ratatui::restore();
    result
}

/// Mouse reporting, for the wheel and clicks. The terminal then stops selecting text on a
/// plain drag: hold Option (macOS) or Shift (most Linux terminals) to select.
fn enable_mouse() {
    // Turn it off again before ratatui's own hook restores the terminal after a panic,
    // or the shell would keep receiving mouse codes.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        disable_mouse();
        previous(info);
    }));
    if let Err(err) = crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture) {
        tracing::warn!(error = %err, "could not enable the mouse");
    }
}

fn disable_mouse() {
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
}

/// `host` or `host:port`, for the header.
fn display_host(url: &url::Url) -> String {
    match (url.host_str(), url.port()) {
        (Some(host), Some(port)) => format!("{host}:{port}"),
        (Some(host), None) => host.to_string(),
        (None, _) => url.to_string(),
    }
}

/// Logs go to a file because the TUI owns stdout. Level is set with `LAZYFORGE_LOG`
/// (for example `LAZYFORGE_LOG=debug`). Returns `None` if no log directory is available.
fn init_logging() -> Option<WorkerGuard> {
    let dir = config::state_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    let (writer, guard) =
        tracing_appender::non_blocking(tracing_appender::rolling::never(dir, "lazyforge.log"));
    let filter = EnvFilter::try_from_env("LAZYFORGE_LOG")
        .unwrap_or_else(|_| EnvFilter::new("lazyforge=info"));
    tracing_subscriber::fmt()
        .with_writer(writer)
        .with_ansi(false)
        .with_env_filter(filter)
        .init();
    Some(guard)
}
