//! Drawing. Reads `AppState`, never changes it.

mod branches;
mod ci;
mod detail;
mod format;
mod picker;
mod pipelines;

use chrono::Utc;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, ListState, Paragraph, Row, Table, TableState};

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Position;

use crate::app::{Action, AppState, KeyContext, MrTab, View};
use crate::forge::{ForgeKind, MergeRequest, MrList, PipelineStatus};

/// State that only matters for drawing, like each table's scroll offset. It lives in the
/// runtime instead of `AppState` so the app logic does not depend on ratatui types.
#[derive(Debug, Default)]
pub struct ViewState {
    tables: Vec<TableState>,
    pipeline_tables: Vec<TableState>,
    branch_table: TableState,
    file_list: ListState,
    job_list: ListState,
    picker_list: ListState,
    base_list: ListState,
    /// Largest useful scroll offset of the detail pane drawn last, for `clamp_detail_scroll`.
    scroll_limit: Option<usize>,
    /// Scroll offset (in rows) of a diff with a line cursor, and which file it is for.
    diff_offset: usize,
    diff_offset_for: Option<(usize, String)>,
    /// Where a list and its content pane were drawn last (files and diff, jobs and log),
    /// so the mouse wheel scrolls the pane under the pointer.
    panes: Option<Panes>,
}

#[derive(Debug, Clone, Copy)]
struct Panes {
    list: Rect,
    content: Rect,
    /// A click on row `n` of the list picks item `n` (file lists; not the job list, whose
    /// stage headers are rows too).
    clickable: bool,
}

impl ViewState {
    pub fn take_scroll_limit(&mut self) -> Option<usize> {
        self.scroll_limit.take()
    }
}

/// Background of the selected row in lists and tables. A dark gray from the 256-color
/// palette rather than `DarkGray` (color 8), which many themes also use for dimmed text:
/// dimmed text on a `DarkGray` row (`#128`, `→`) would vanish.
pub(crate) const SELECTED_BG: Color = Color::Indexed(237);

/// Lines scrolled per mouse wheel step.
const WHEEL_LINES: u16 = 3;

/// What a mouse event means on the screen drawn last. The wheel scrolls the pane under the
/// pointer (or moves through a list); a click picks a file or focuses the diff.
pub fn mouse_action(view: &ViewState, mouse: MouseEvent, context: KeyContext) -> Option<Action> {
    // While typing a comment or answering a yes/no question, the mouse does nothing.
    if matches!(context, KeyContext::CommentEditor | KeyContext::Confirm) {
        return None;
    }
    // In the "compare with…" popup the wheel moves through the branches; clicks do nothing.
    if context == KeyContext::BranchBase {
        return match mouse.kind {
            MouseEventKind::ScrollDown => Some(Action::SelectNext),
            MouseEventKind::ScrollUp => Some(Action::SelectPrevious),
            _ => None,
        };
    }
    let at = Position::new(mouse.column, mouse.row);
    let over = |rect: Rect| rect.contains(at);
    let panes = view.panes;
    match mouse.kind {
        MouseEventKind::ScrollDown => Some(match panes {
            Some(p) if over(p.content) => Action::ScrollDown(WHEEL_LINES),
            // Over a file list: the next file, even while the diff has the focus.
            Some(p) if p.clickable && over(p.list) => {
                Action::SelectFile(view.file_list.selected().unwrap_or(0) + 1)
            }
            _ => Action::SelectNext,
        }),
        MouseEventKind::ScrollUp => Some(match panes {
            Some(p) if over(p.content) => Action::ScrollUp(WHEEL_LINES),
            Some(p) if p.clickable && over(p.list) => {
                Action::SelectFile(view.file_list.selected().unwrap_or(0).saturating_sub(1))
            }
            _ => Action::SelectPrevious,
        }),
        MouseEventKind::Down(MouseButton::Left) => {
            let p = panes?;
            if over(p.content) {
                Some(Action::FocusContent)
            } else if p.clickable && over(p.list) {
                // Row 0 of the pane is its border.
                let row = mouse.row.checked_sub(p.list.y + 1)?;
                Some(Action::SelectFile(
                    view.file_list.offset() + usize::from(row),
                ))
            } else {
                None
            }
        }
        _ => None,
    }
}

pub fn render(frame: &mut Frame, state: &AppState, view: &mut ViewState) {
    // Set again below by the screens that have a list and a content pane.
    view.panes = None;
    let on_list =
        state.detail.is_none() && state.pipeline_screen.is_none() && state.branch_screen.is_none();
    let show_filter = on_list && (state.editing_filter || !state.filter.is_empty());
    let message = message_line(state);
    // Optional lines only take space when they have something to show.
    let [header, main, filter, message_area, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(u16::from(show_filter)),
        Constraint::Length(u16::from(message.is_some())),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    render_header(frame, header, state);
    if let Some(open) = &state.picker {
        picker::render(frame, main, state, open, view);
    } else if let Some(detail) = &state.detail {
        let drafts = crate::app::review::of(&state.review_drafts, detail);
        detail::render(frame, main, detail, drafts, view);
    } else if let Some(screen) = &state.pipeline_screen {
        pipelines::render_screen(frame, main, screen, view);
    } else if let Some(screen) = &state.branch_screen {
        branches::render_screen(frame, main, screen, view);
    } else if state.view == View::Pipelines {
        pipelines::render_list(frame, main, state, view);
    } else if state.view == View::Branches {
        branches::render_list(frame, main, state, view);
    } else {
        let [tabs, body] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(main);
        render_tabs(frame, tabs, state);
        render_merge_requests(frame, body, state, view);
    }
    if show_filter {
        render_filter(frame, filter, state);
    }
    if let Some(message) = message {
        frame.render_widget(Paragraph::new(message), message_area);
    }
    render_footer(frame, footer, state);

    // Popups last, so they are drawn over everything else.
    if let Some(detail) = &state.detail {
        let drafts = crate::app::review::of(&state.review_drafts, detail);
        detail::render_popup(frame, detail, drafts);
    }
    if let Some(screen) = &state.pipeline_screen
        && let Some(crate::app::detail::Popup::ConfirmCi(action)) = screen.popup
    {
        detail::render_ci_popup(frame, &screen.ci, action);
    }
    if let Some(screen) = &state.branch_screen
        && screen.base_picker.is_some()
    {
        branches::render_base_picker(frame, screen, view);
    }
    if let Some(pending) = &state.pending_checkout {
        render_checkout_popup(frame, pending);
    }
}

/// The notice if there is one, otherwise the load error of what is on screen.
fn message_line(state: &AppState) -> Option<Line<'static>> {
    if let Some(notice) = &state.notice {
        let (symbol, color) = if notice.is_error {
            ("✗", Color::Red)
        } else {
            ("✓", Color::Green)
        };
        return Some(Line::from(format!(" {symbol} {}", notice.text)).fg(color));
    }
    let error = if let Some(detail) = &state.detail {
        detail.error.as_ref().or(detail.ci.error.as_ref())
    } else if let Some(screen) = &state.pipeline_screen {
        screen.ci.error.as_ref()
    } else if let Some(screen) = &state.branch_screen {
        screen.error.as_ref()
    } else if state.view == View::Pipelines {
        state.active_pipeline_tab().error.as_ref()
    } else if state.view == View::Branches {
        state.branches.error.as_ref()
    } else {
        state.active().error.as_ref()
    };
    error.map(|e| Line::from(format!(" ✗ {e}")).fg(Color::Red))
}

fn render_header(frame: &mut Frame, area: Rect, state: &AppState) {
    // View switcher on the right, like lazygit's panels: `1`, `2` and `3`.
    let mut views = Vec::new();
    for (key, label, view) in [
        ("1", state.forge.change_requests(), View::MergeRequests),
        ("2", state.forge.pipelines(), View::Pipelines),
        ("3", "Branches", View::Branches),
    ] {
        let text = format!(" {key} {label} ");
        views.push(if state.view == view {
            Span::raw(text).bold().fg(Color::Black).bg(Color::Cyan)
        } else {
            Span::raw(text).dark_gray()
        });
        views.push(Span::raw(" "));
    }
    let views = Line::from(views);
    // Two columns, so a long repository name is cut instead of being drawn over.
    let views_width = u16::try_from(views.width()).unwrap_or(u16::MAX);
    let [left, right] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(views_width)]).areas(area);
    frame.render_widget(Paragraph::new(views), right);

    let mut spans = vec![
        Span::raw(" lazyforge ")
            .bold()
            .fg(Color::Black)
            .bg(Color::Blue),
    ];
    if state.profile_name.is_empty() {
        // First start: no account chosen yet.
        spans.push(Span::raw(" no account selected").dark_gray());
    } else {
        spans.push(Span::raw(format!(" {}", state.host)).fg(Color::Blue));
        // Profiles are often named after their host; say it only when it differs.
        if state.profile_name != state.host {
            spans.push(Span::raw(format!(" · profile: {}", state.profile_name)).dark_gray());
        }
    }
    if let Some(username) = &state.username {
        spans.push(Span::raw(format!(" · @{username}")).dark_gray());
    }
    if !state.profile_name.is_empty() {
        let repo = state.repo.as_deref().unwrap_or("all repositories");
        spans.push(Span::raw(" · ").dark_gray());
        // The footer shows `s` to switch, so the repository gets the room.
        spans.push(Span::raw(repo.to_string()).fg(Color::Cyan).bold());
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), left);
}

/// A row of tabs; the active one is highlighted.
pub(crate) fn render_tab_bar(
    frame: &mut Frame,
    area: Rect,
    labels: impl IntoIterator<Item = String>,
    active: usize,
) {
    let mut spans = vec![Span::raw(" ")];
    for (i, label) in labels.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" │ ").dark_gray());
        }
        let label = format!(" {label} ");
        spans.push(if i == active {
            Span::raw(label).bold().fg(Color::Black).bg(Color::Blue)
        } else {
            Span::raw(label).dark_gray()
        });
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn tab_label(list: &MrList) -> &str {
    match list {
        MrList::AssignedToMe => "Assigned",
        MrList::ReviewRequested => "Review requested",
        MrList::AuthoredByMe => "Authored",
        MrList::Project(path) => path,
    }
}

fn render_tabs(frame: &mut Frame, area: Rect, state: &AppState) {
    let labels = state.tabs.iter().map(|tab| {
        if tab.loaded_once && tab.error.is_none() {
            format!("{} ({})", tab_label(&tab.list), tab.merge_requests.len())
        } else {
            tab_label(&tab.list).to_string()
        }
    });
    render_tab_bar(frame, area, labels, state.active_tab);
}

/// Spacing between table columns, and the width of the `▶ ` selection marker.
pub(super) const COLUMN_SPACING: u16 = 2;
const HIGHLIGHT_WIDTH: u16 = 2;

/// The width the table will give column `index`, worked out the same way the table does.
pub(super) fn column_width(inner: Rect, widths: &[Constraint], index: usize) -> usize {
    let area = Rect {
        width: inner.width.saturating_sub(HIGHLIGHT_WIDTH),
        ..inner
    };
    Layout::horizontal(widths.iter().copied())
        .spacing(COLUMN_SPACING)
        .split(area)
        .get(index)
        .map_or(0, |column| usize::from(column.width))
}

/// Branches that releases usually go to, highlighted as merge targets.
fn is_main_branch(branch: &str) -> bool {
    matches!(branch, "main" | "master" | "production" | "prod")
}

/// `source → target` in `width` characters. The target is what tells merge requests apart
/// (develop, main, release/…), so the source gives up characters first.
fn branches(mr: &MergeRequest, width: usize) -> Line<'static> {
    const ARROW: &str = " → ";
    let target_style = if is_main_branch(&mr.target_branch) {
        Style::new().fg(Color::Yellow).bold()
    } else {
        Style::new().fg(Color::Blue)
    };
    let target_len = mr.target_branch.chars().count();
    let room = width.saturating_sub(target_len + ARROW.chars().count());
    // Too narrow to show a useful part of the source: the target alone.
    if room < 4 {
        return Line::from(Span::styled(
            format::shorten_middle(&mr.target_branch, width),
            target_style,
        ));
    }
    Line::from(vec![
        Span::raw(format::shorten_middle(&mr.source_branch, room)).fg(Color::Magenta),
        Span::raw(ARROW).dark_gray(),
        Span::styled(mr.target_branch.clone(), target_style),
    ])
}

fn render_merge_requests(frame: &mut Frame, area: Rect, state: &AppState, view: &mut ViewState) {
    let tab = state.active();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(Color::Blue));
    let visible = state.visible();

    if visible.is_empty() {
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let [middle] = Layout::vertical([Constraint::Length(1)])
            .flex(Flex::Center)
            .areas(inner);
        let message = Paragraph::new(empty_message(state, tab))
            .dark_gray()
            .alignment(Alignment::Center);
        frame.render_widget(message, middle);
        return;
    }

    // The project column is redundant on a project tab.
    let show_project = !matches!(tab.list, MrList::Project(_));
    let prefix = state.forge.reference_prefix().to_string();
    let mut headers = vec![prefix.as_str(), "Title"];
    let mut widths = vec![Constraint::Length(6), Constraint::Fill(4)];
    if show_project {
        headers.push("Project");
        widths.push(Constraint::Fill(2));
    }
    headers.extend(["Author", "Branch", "CI", "Appr", "Updated"]);
    widths.extend([
        Constraint::Length(12),
        Constraint::Fill(3),
        Constraint::Length(2),
        Constraint::Length(4),
        Constraint::Length(7),
    ]);
    let branch_width = column_width(block.inner(area), &widths, widths.len() - 4);

    let now = Utc::now();
    let rows = visible.iter().map(|mr| {
        let mut cells = vec![
            Cell::from(format!("{}{}", state.forge.reference_prefix(), mr.iid)).dark_gray(),
            Cell::from(mr.title.as_str()).style(if mr.draft {
                Style::new().dark_gray()
            } else {
                Style::new()
            }),
        ];
        if show_project {
            cells.push(Cell::from(mr.project_path.as_str()).fg(Color::Cyan));
        }
        cells.extend([
            Cell::from(mr.author.username.as_str()),
            Cell::from(branches(mr, branch_width)),
            pipeline_cell(mr),
            approvals_cell(mr),
            Cell::from(format::relative_time(mr.updated_at, now)).dark_gray(),
        ]);
        Row::new(cells)
    });

    let header = Row::new(headers).style(Style::new().dark_gray().add_modifier(Modifier::BOLD));

    let table = Table::new(rows, widths)
        .header(header)
        .block(block)
        .column_spacing(COLUMN_SPACING)
        .row_highlight_style(Style::new().bg(SELECTED_BG).add_modifier(Modifier::BOLD))
        .highlight_symbol("▶ ");

    // One TableState per tab, so each tab keeps its own scroll position.
    if view.tables.len() < state.tabs.len() {
        view.tables
            .resize_with(state.tabs.len(), TableState::default);
    }
    let table_state = &mut view.tables[state.active_tab];
    table_state.select(Some(tab.selected));
    frame.render_stateful_widget(table, area, table_state);
}

fn empty_message(state: &AppState, tab: &MrTab) -> String {
    if !tab.loaded_once {
        format!("Loading {}…", label(state))
    } else if tab.error.is_some() && tab.merge_requests.is_empty() {
        format!("Could not load {}. Press r to retry.", label(state))
    } else if !state.filter.is_empty() {
        format!("No {} match “{}”.", label(state), state.filter)
    } else {
        format!("No open {} here.", label(state))
    }
}

fn pipeline_cell(mr: &MergeRequest) -> Cell<'static> {
    let (symbol, color) = pipeline_symbol(mr.pipeline_status);
    Cell::from(symbol).fg(color)
}

fn approvals_cell(mr: &MergeRequest) -> Cell<'static> {
    match mr.approved_by.len() {
        0 => Cell::from("–").dark_gray(),
        n => Cell::from(format!("✓{n}")).fg(Color::Green),
    }
}

fn render_filter(frame: &mut Frame, area: Rect, state: &AppState) {
    let mut spans = vec![
        Span::raw(" / ").fg(Color::Blue).bold(),
        Span::raw(state.filter.as_str()),
    ];
    if state.editing_filter {
        spans.push(Span::raw("█").fg(Color::Blue));
    }
    let matches = match state.view {
        View::MergeRequests => state.visible().len(),
        View::Pipelines => state.visible_pipelines().len(),
        View::Branches => state.visible_branches().len(),
    };
    spans.push(Span::raw(format!("  {matches} match(es)")).dark_gray());
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// "Check out … ?" with what will happen, so `y` is never a surprise.
fn render_checkout_popup(frame: &mut Frame, pending: &crate::app::checkout::PendingCheckout) {
    use crate::git::Target;
    let request = &pending.request;
    let steps = match &request.target {
        Target::Branch(branch) => format!(
            "Fetches {branch} from {} and switches to it (a local branch of that name is only fast-forwarded).",
            request.remote
        ),
        Target::ChangeRequest {
            source_branch,
            fallback_branch,
            ..
        } => format!(
            "Fetches it from {} and switches to {source_branch}, or to {fallback_branch} if it comes from a fork.",
            request.remote
        ),
    };
    let lines = vec![
        Line::from(vec![
            Span::raw("Check out "),
            Span::raw(pending.label.clone()).bold(),
        ]),
        Line::from(vec![
            Span::raw("in "),
            Span::raw(format::home_relative(&request.root)).fg(Color::Cyan),
            Span::raw("?"),
        ]),
        Line::from(""),
        Line::from(steps).dark_gray(),
        Line::from("Stops without touching anything if you have uncommitted changes.").dark_gray(),
    ];
    let area = frame.area();
    let width = area.width.saturating_sub(8).min(80);
    let paragraph = Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: false });
    // As tall as the wrapped text, so the last line (the safety note) is never cut off.
    let rows = paragraph.line_count(width.saturating_sub(2));
    let height = u16::try_from(rows).unwrap_or(u16::MAX).saturating_add(2);
    let popup = detail::centered(area, width, height.min(area.height));
    frame.render_widget(ratatui::widgets::Clear, popup);
    frame.render_widget(
        paragraph.block(detail::popup_block(
            " Checkout ".to_string(),
            " y check out · n cancel ",
        )),
        popup,
    );
}

/// On a file list next to a diff: whether the diff has the focus. `None` elsewhere.
fn files_focus(state: &AppState) -> Option<bool> {
    if let Some(screen) = &state.branch_screen {
        return Some(screen.diff_focused);
    }
    let detail = state.detail.as_ref()?;
    (detail.section == crate::app::detail::Section::Files).then_some(detail.diff_focused)
}

fn render_footer(frame: &mut Frame, area: Rect, state: &AppState) {
    let keys: &[(&str, &str)] = match state.key_context() {
        KeyContext::List => &[
            ("s", "switch"),
            ("1/2/3", "views"),
            ("j/k", "move"),
            ("Enter", "open"),
            ("[/]", "tab"),
            ("/", "filter"),
            ("C", "checkout"),
            ("o", "browser"),
            ("r", "refresh"),
            ("q", "quit"),
        ],
        KeyContext::Filter => &[("Enter", "apply"), ("Esc", "clear"), ("↑/↓", "move")],
        // The Files section: what `j`/`k` do depends on the focused pane.
        KeyContext::Detail if files_focus(state) == Some(true) => &[
            ("j/k J/K", "line"),
            ("c", "comment on line"),
            ("R", "review"),
            ("h/Esc", "files"),
            ("[/]", "section"),
            ("a", "approve"),
            ("m", "merge"),
            ("o", "browser"),
        ],
        KeyContext::Detail if files_focus(state) == Some(false) => &[
            ("Esc", "back"),
            ("[/]", "section"),
            ("j/k", "file"),
            ("Enter/l", "read diff"),
            ("J/K", "scroll"),
            ("a", "approve"),
            ("c", "comment"),
            ("m", "merge"),
            ("o", "browser"),
        ],
        KeyContext::Detail => &[
            ("Esc", "back"),
            ("[/]", "section"),
            ("j/k", "move"),
            ("J/K ^d/^u", "scroll"),
            ("a", "approve"),
            ("c", "comment"),
            ("m", "merge"),
            ("C", "checkout"),
            ("o", "browser"),
            ("r", "refresh"),
        ],
        KeyContext::DetailPipelines if state.forge == ForgeKind::GitHub => &[
            ("Esc", "back"),
            ("j/k", "job"),
            ("Enter", "log"),
            ("J/K/G", "scroll"),
            ("f", "follow"),
            ("R", "retry job"),
            ("^R/X", "retry/cancel run"),
            ("o", "browser"),
        ],
        KeyContext::DetailPipelines => &[
            ("Esc", "back"),
            ("j/k", "job"),
            ("Enter", "log"),
            ("J/K/G", "scroll"),
            ("f", "follow"),
            ("R/C", "retry/cancel job"),
            ("^R/X", "retry/cancel pipeline"),
            ("o", "browser"),
        ],
        KeyContext::BranchBase => &[
            ("type", "filter"),
            ("↑/↓", "move"),
            ("Enter", "compare"),
            ("Esc", "cancel"),
        ],
        KeyContext::Branch if files_focus(state) == Some(true) => &[
            ("j/k J/K ^d/^u", "move in diff"),
            ("h/Esc", "files"),
            ("o", "browser"),
            ("r", "refresh"),
            ("q", "quit"),
        ],
        KeyContext::Branch => &[
            ("Esc", "back"),
            ("j/k", "file"),
            ("Enter/l", "read diff"),
            ("J/K ^d/^u", "scroll"),
            ("b", "compare with…"),
            ("C", "checkout"),
            ("o", "browser"),
            ("r", "refresh"),
            ("q", "quit"),
        ],
        KeyContext::CommentEditor => &[("Ctrl+S", "send"), ("Esc", "close (keeps draft)")],
        KeyContext::ReviewEditor => &[
            ("type", "summary"),
            ("Tab", "verdict"),
            ("Ctrl+S", "send review"),
            ("Esc", "close (kept)"),
        ],
        KeyContext::Confirm => &[("y", "yes"), ("n/Esc", "cancel")],
        KeyContext::Picker => &[
            ("j/k", "move"),
            ("Enter", "choose"),
            ("Esc", "back"),
            ("q", "quit"),
        ],
        KeyContext::PickerSearch => &[
            ("type", "filter"),
            ("↑/↓", "move"),
            ("Enter", "open"),
            ("Esc", "back"),
        ],
    };
    let mut spans = Vec::new();
    for (key, label) in keys {
        spans.push(Span::raw(format!(" {key} ")).fg(Color::Blue).bold());
        spans.push(Span::raw(format!("{label} ")).dark_gray());
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);

    let status = if state.checking_out {
        Some("checking out…")
    } else if let Some(detail) = &state.detail {
        detail
            .busy
            .or(detail.ci.busy)
            .or((detail.loading || detail.ci.loading).then_some("loading…"))
    } else if let Some(screen) = &state.pipeline_screen {
        screen.ci.busy.or(screen.ci.loading.then_some("loading…"))
    } else if let Some(screen) = &state.branch_screen {
        screen.loading.then_some("loading…")
    } else if state.view == View::Pipelines {
        state.active_pipeline_tab().loading.then_some("loading…")
    } else if state.view == View::Branches {
        state.branches.loading.then_some("loading…")
    } else {
        state.active().loading.then_some("loading…")
    };
    if let Some(status) = status {
        let status = Line::from(format!("{status} ")).fg(Color::Yellow);
        frame.render_widget(Paragraph::new(status).alignment(Alignment::Right), area);
    }
}

pub(crate) fn pipeline_symbol(status: Option<PipelineStatus>) -> (&'static str, Color) {
    use PipelineStatus::*;
    match status {
        None => ("–", Color::DarkGray),
        Some(Success) => ("✓", Color::Green),
        Some(Failed) => ("✗", Color::Red),
        Some(Running) => ("◐", Color::Yellow),
        Some(
            Created | WaitingForResource | WaitingForCallback | Preparing | Pending | Scheduled,
        ) => ("○", Color::Yellow),
        Some(Manual) => ("⏸", Color::Blue),
        Some(Canceling | Canceled | Skipped | Unknown) => ("⊘", Color::DarkGray),
    }
}

/// "merge requests" or "pull requests", for sentences.
fn label(state: &AppState) -> String {
    state.forge.change_requests().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::User;

    fn mr(source: &str, target: &str) -> MergeRequest {
        MergeRequest {
            iid: 1,
            title: "t".into(),
            draft: false,
            author: User {
                username: "linh".into(),
                name: "Linh".into(),
            },
            project_path: "g/p".into(),
            source_branch: source.into(),
            target_branch: target.into(),
            web_url: String::new(),
            updated_at: Utc::now(),
            pipeline_status: None,
            approved_by: Vec::new(),
        }
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: crossterm::event::KeyModifiers::NONE,
        }
    }

    /// Files at x 0..30, diff at x 30..100, rows 5..25; file 2 of the list is selected.
    fn file_panes() -> ViewState {
        let mut view = ViewState {
            panes: Some(Panes {
                list: Rect::new(0, 5, 30, 20),
                content: Rect::new(30, 5, 70, 20),
                clickable: true,
            }),
            ..ViewState::default()
        };
        view.file_list.select(Some(2));
        view
    }

    #[test]
    fn the_wheel_scrolls_the_pane_under_the_pointer() {
        let view = file_panes();
        let over_diff = mouse(MouseEventKind::ScrollDown, 50, 10);
        assert!(matches!(
            mouse_action(&view, over_diff, KeyContext::Detail),
            Some(Action::ScrollDown(WHEEL_LINES))
        ));
        let over_files = mouse(MouseEventKind::ScrollUp, 10, 10);
        assert!(matches!(
            mouse_action(&view, over_files, KeyContext::Detail),
            Some(Action::SelectFile(1))
        ));
        // Lists without panes (merge requests, pipelines): move the selection.
        let plain = ViewState::default();
        assert!(matches!(
            mouse_action(&plain, over_diff, KeyContext::List),
            Some(Action::SelectNext)
        ));
    }

    #[test]
    fn clicks_pick_a_file_or_focus_the_diff() {
        let view = file_panes();
        // Row 5 is the border; row 8 is the third file.
        let on_file = mouse(MouseEventKind::Down(MouseButton::Left), 10, 8);
        assert!(matches!(
            mouse_action(&view, on_file, KeyContext::Detail),
            Some(Action::SelectFile(2))
        ));
        let on_border = mouse(MouseEventKind::Down(MouseButton::Left), 10, 4);
        assert!(mouse_action(&view, on_border, KeyContext::Detail).is_none());
        let on_diff = mouse(MouseEventKind::Down(MouseButton::Left), 60, 12);
        assert!(matches!(
            mouse_action(&view, on_diff, KeyContext::Branch),
            Some(Action::FocusContent)
        ));
        // Not while writing a comment.
        assert!(mouse_action(&view, on_diff, KeyContext::CommentEditor).is_none());
    }

    fn text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn shows_source_and_target() {
        let line = branches(&mr("fix/login", "develop"), 30);
        assert_eq!(text(&line), "fix/login → develop");
        assert_eq!(line.spans[2].style.fg, Some(Color::Blue));
    }

    #[test]
    fn highlights_main_branches_as_targets() {
        let line = branches(&mr("hotfix/rounding", "main"), 30);
        assert_eq!(line.spans[2].style.fg, Some(Color::Yellow));
    }

    #[test]
    fn shortens_the_source_and_keeps_the_target() {
        let line = branches(&mr("feature/very-long-branch-name", "release/2.4"), 26);
        let shown = text(&line);
        assert!(shown.ends_with(" → release/2.4"), "{shown}");
        assert!(shown.starts_with("featur"), "{shown}");
        assert_eq!(shown.chars().count(), 26);
    }

    #[test]
    fn shows_only_the_target_when_very_narrow() {
        assert_eq!(text(&branches(&mr("feature/x", "develop"), 9)), "develop");
        assert_eq!(text(&branches(&mr("feature/x", "develop"), 5)), "de…op");
    }

    #[test]
    fn column_width_matches_what_the_table_draws() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let widths = [
            Constraint::Length(6),
            Constraint::Fill(4),
            Constraint::Fill(3),
            Constraint::Length(4),
        ];
        let long = "x".repeat(200);
        let mut terminal = Terminal::new(TestBackend::new(90, 3)).unwrap();
        let area = Rect::new(0, 0, 90, 3);
        let expected = column_width(area, &widths, 2);
        terminal
            .draw(|frame| {
                let row = Row::new(["id", "title", &long, "ci"]);
                let table = Table::new([row], widths)
                    .column_spacing(COLUMN_SPACING)
                    .highlight_symbol("▶ ");
                let mut state = TableState::default().with_selected(Some(0));
                frame.render_stateful_widget(table, area, &mut state);
            })
            .unwrap();
        let drawn = terminal.backend().buffer().content()[..90]
            .iter()
            .filter(|cell| cell.symbol() == "x")
            .count();
        assert_eq!(drawn, expected);
    }
}
