//! The event loop: waits for a key press, a finished network request or the refresh timer,
//! feeds it to `app::update`, runs the returned command in the background, and redraws.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyEventKind};
use futures::StreamExt;
use ratatui::DefaultTerminal;
use tokio::sync::mpsc;
use tokio::time::{Instant, Interval, MissedTickBehavior};

use crate::app::ci::CiSource;
use crate::app::picker::ConnectedAccount;
use crate::app::{self, Action, AppState, Command};
use crate::forge::Forge;
use crate::ui;

/// Resolves a profile's token and builds its client.
pub type Connect = Box<dyn Fn(&str) -> Result<(Forge, ConnectedAccount)>>;

/// What the runtime needs besides the client: how to connect to another account, and where
/// to remember the choice.
pub struct Session {
    /// Resolves a profile's token and builds its client. May block briefly (it can read the
    /// OS keychain, which may ask the user for permission).
    pub connect: Connect,
    /// `last.toml`; `None` if there is no state directory.
    pub selection_path: Option<PathBuf>,
}

pub async fn run(
    mut terminal: DefaultTerminal,
    mut state: AppState,
    // `None` until an account is chosen in the picker on a first start.
    mut client: Option<Forge>,
    refresh_interval: Option<Duration>,
    session: Session,
) -> Result<()> {
    // Background tasks send their results back through this channel.
    let (tx, mut rx) = mpsc::unbounded_channel::<Action>();
    let mut events = EventStream::new();
    let mut view = ui::ViewState::default();
    // Drives polling of running pipelines and logs, and keeps running durations fresh.
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut timer = refresh_interval.map(|period| {
        // `interval` would fire immediately; the first refresh should wait a full period.
        let mut timer = tokio::time::interval_at(Instant::now() + period, period);
        // After the laptop sleeps, refresh once instead of catching up on every missed tick.
        timer.set_missed_tick_behavior(MissedTickBehavior::Delay);
        timer
    });

    if let Some(command) = app::update(&mut state, Action::Refresh) {
        execute(command, &mut client, &session, &tx);
    }

    while !state.should_quit {
        terminal.draw(|frame| ui::render(frame, &state, &mut view))?;
        if let Some(max) = view.take_scroll_limit() {
            state.clamp_detail_scroll(max);
        }

        let action = tokio::select! {
            event = events.next() => match event {
                Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                    app::keymap::action_for_key(key, state.key_context())
                }
                Some(Ok(Event::Mouse(mouse))) => ui::mouse_action(&view, mouse, state.key_context()),
                // Resize and other events: nothing to do, the loop redraws.
                Some(Ok(_)) => None,
                Some(Err(err)) => return Err(err.into()),
                None => break,
            },
            Some(action) = rx.recv() => Some(action),
            () = tick(&mut timer) => Some(Action::AutoRefresh),
            _ = ticker.tick() => Some(Action::Tick),
        };

        match action.and_then(|action| app::update(&mut state, action)) {
            // Needs the terminal, so it runs here rather than in `execute`.
            Some(Command::OpenEditor(dir)) => {
                // While the editor runs, nothing else may read the keyboard.
                drop(events);
                let result = open_editor(&mut terminal, &dir);
                events = EventStream::new();
                let _ = tx.send(Action::EditorClosed(result));
            }
            Some(command) => execute(command, &mut client, &session, &tx),
            None => {}
        }
    }
    Ok(())
}

/// Waits for the next tick, or forever when auto-refresh is off. `select!` needs a future
/// for every branch, so "never" is expressed as a future that never completes.
async fn tick(timer: &mut Option<Interval>) {
    match timer {
        Some(timer) => {
            timer.tick().await;
        }
        None => std::future::pending().await,
    }
}

/// Runs a command. Account switching and saving the selection happen here (they change the
/// client or touch the disk); everything else goes to the background with `spawn`.
fn execute(
    command: Command,
    client: &mut Option<Forge>,
    session: &Session,
    tx: &mpsc::UnboundedSender<Action>,
) {
    match command {
        Command::SwitchAccount { profile } => {
            let result = match (session.connect)(&profile) {
                Ok((forge, account)) => {
                    // Responses still in flight from the old client carry old request ids
                    // and are ignored by `app::update`.
                    *client = Some(forge);
                    Ok(account)
                }
                Err(err) => Err(format!("{err:#}")),
            };
            let _ = tx.send(Action::AccountConnected(result));
        }
        Command::SaveSelection(selection) => {
            if let Some(path) = &session.selection_path
                && let Err(err) = crate::selection::save(path, &selection)
            {
                tracing::warn!(error = %err, "could not remember the selection");
            }
        }
        Command::Batch(commands) => {
            for command in commands {
                execute(command, client, session, tx);
            }
        }
        command => match client {
            Some(client) => spawn(command, client, tx),
            None => tracing::warn!(?command, "no account connected yet; ignoring"),
        },
    }
}

/// Runs a command in the background so the UI never waits for the network.
fn spawn(command: Command, client: &Forge, tx: &mpsc::UnboundedSender<Action>) {
    // `tokio::spawn` needs a `'static` future, so the task cannot borrow `client` or `tx`
    // from this function. We give it its own clones (both are cheap handles) and `move` them in.
    let tx = tx.clone();
    match command {
        Command::FetchMergeRequests { request_id, list } => {
            let client = client.clone();
            tokio::spawn(async move {
                let result = client.merge_requests(&list).await;
                // Fails only if the app is already shutting down; nothing to do then.
                let _ = tx.send(Action::MergeRequestsLoaded { request_id, result });
            });
        }
        Command::FetchDetail {
            request_id,
            project_path,
            iid,
        } => {
            let client = client.clone();
            tokio::spawn(async move {
                let result = match client.merge_request_detail(&project_path, iid).await {
                    Ok(detail) => Ok(Box::new(highlighted(detail, |d| &mut d.files).await)),
                    Err(err) => Err(err),
                };
                let _ = tx.send(Action::DetailLoaded { request_id, result });
            });
        }
        Command::RunMrAction {
            project_path,
            iid,
            action,
        } => {
            let client = client.clone();
            tokio::spawn(async move {
                let result = client.run_action(&project_path, iid, &action).await;
                let _ = tx.send(Action::MrActionFinished {
                    project_path,
                    iid,
                    action,
                    result,
                });
            });
        }
        Command::FetchPipelines { request_id, list } => {
            let client = client.clone();
            tokio::spawn(async move {
                let result = client.pipelines(&list).await;
                let _ = tx.send(Action::PipelinesLoaded { request_id, result });
            });
        }
        Command::FetchCi {
            request_id,
            source,
            pipeline_id,
        } => {
            let client = client.clone();
            tokio::spawn(async move {
                let result = match source {
                    CiSource::MergeRequest { project_path, iid } => {
                        client
                            .merge_request_ci(&project_path, iid, pipeline_id)
                            .await
                    }
                    CiSource::Pipeline {
                        project_id,
                        pipeline_id,
                    } => client.pipeline_ci(&project_id, pipeline_id).await,
                };
                let _ = tx.send(Action::CiLoaded { request_id, result });
            });
        }
        Command::FetchJobLog {
            request_id,
            project_id,
            job_id,
        } => {
            let client = client.clone();
            tokio::spawn(async move {
                let result = client.job_trace(&project_id, job_id).await;
                let _ = tx.send(Action::JobLogLoaded { request_id, result });
            });
        }
        Command::RunCiAction {
            project_id,
            action,
            label,
        } => {
            let client = client.clone();
            tokio::spawn(async move {
                let result = client.run_ci_action(&project_id, action).await;
                let _ = tx.send(Action::CiActionFinished {
                    action,
                    label,
                    result,
                });
            });
        }
        Command::FetchBranches {
            request_id,
            project,
        } => {
            let client = client.clone();
            tokio::spawn(async move {
                let result = client.branches(&project).await;
                let _ = tx.send(Action::BranchesLoaded { request_id, result });
            });
        }
        Command::FetchComparison {
            request_id,
            project,
            base,
            head,
        } => {
            let client = client.clone();
            tokio::spawn(async move {
                let result = match client.compare(&project, &base, &head).await {
                    Ok(comparison) => Ok(Box::new(highlighted(comparison, |c| &mut c.files).await)),
                    Err(err) => Err(err),
                };
                let _ = tx.send(Action::ComparisonLoaded { request_id, result });
            });
        }
        Command::FetchRepositories { request_id } => {
            let client = client.clone();
            tokio::spawn(async move {
                let result = client.repositories().await;
                let _ = tx.send(Action::RepositoriesLoaded { request_id, result });
            });
        }
        Command::Checkout(request) => {
            // git is blocking and can take a while (it fetches): off the async threads.
            tokio::spawn(async move {
                let result = tokio::task::spawn_blocking(move || crate::git::checkout(&request))
                    .await
                    .unwrap_or_else(|err| Err(err.to_string()));
                let _ = tx.send(Action::CheckoutFinished(result));
            });
        }
        Command::SwitchAccount { .. }
        | Command::SaveSelection(_)
        | Command::Batch(_)
        | Command::OpenEditor(_) => {
            tracing::error!("`execute` handles this command");
        }
        Command::OpenUrl(url) => {
            // Starts the browser without waiting for it to exit.
            if let Err(err) = open::that_detached(&url) {
                tracing::warn!(%url, error = %err, "could not open browser");
                let _ = tx.send(Action::BrowserFailed(err.to_string()));
            }
        }
    }
}

pub fn enable_mouse() {
    if let Err(err) = crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture) {
        tracing::warn!(error = %err, "could not enable the mouse");
    }
}

pub fn disable_mouse() {
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
}

/// Hands the terminal to `$VISUAL` / `$EDITOR` (opened on `dir`) and takes it back when the
/// editor exits. A GUI editor (`code`, `zed`) returns at once.
fn open_editor(terminal: &mut DefaultTerminal, dir: &Path) -> Result<(), String> {
    let editor = ["VISUAL", "EDITOR"]
        .into_iter()
        .filter_map(|var| std::env::var(var).ok())
        .find(|value| !value.trim().is_empty())
        .ok_or("Set $EDITOR (for example `export EDITOR=code`) to open the repository from here")?;

    disable_mouse();
    ratatui::restore();
    let status = editor_command(&editor, dir).status();
    // Back to the TUI, whatever the editor did.
    let _ = crossterm::terminal::enable_raw_mode();
    let _ = crossterm::execute!(std::io::stdout(), crossterm::terminal::EnterAlternateScreen);
    enable_mouse();
    let _ = terminal.clear();

    match status {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!("{editor} exited with {status}")),
        Err(err) => Err(format!("could not start {editor}: {err}")),
    }
}

/// `$EDITOR` may hold arguments (`code --wait`), so it goes through the shell, like git does;
/// the directory is passed as `$1`, never pasted into the command line.
#[cfg(unix)]
fn editor_command(editor: &str, dir: &Path) -> std::process::Command {
    let mut command = std::process::Command::new("sh");
    command
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("sh")
        .arg(dir);
    command
}

#[cfg(not(unix))]
fn editor_command(editor: &str, dir: &Path) -> std::process::Command {
    let mut command = std::process::Command::new("cmd");
    command.arg("/C").arg(editor).arg(dir);
    command
}

/// Adds syntax colors to the files of a loaded diff, on a blocking thread: a large diff takes
/// a moment, and drawing should only read the result.
async fn highlighted<T: Send + 'static>(
    mut loaded: T,
    files: fn(&mut T) -> &mut Vec<crate::forge::FileDiff>,
) -> T {
    tokio::task::spawn_blocking(move || {
        let files = files(&mut loaded);
        // Colors are a nicety: if a grammar ever panics, show the diff without them.
        let colored = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::forge::highlight::highlight_files(files);
        }));
        if colored.is_err() {
            tracing::warn!("syntax highlighting failed; showing the diff without colors");
            for line in files.iter_mut().flat_map(|f| f.lines.iter_mut()) {
                line.tokens.clear();
            }
        }
        loaded
    })
    .await
    .expect("the closure catches panics")
}
