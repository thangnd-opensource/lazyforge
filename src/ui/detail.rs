//! The merge request detail screen and its popups.

use chrono::Utc;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};

use super::{Panes, ViewState, ci, format, pipeline_symbol};
use crate::app::ci::CiState;
use crate::app::detail::{DetailState, Popup, Section};
use crate::forge::diff::DiffLineKind;
use crate::forge::{CiAction, Discussion, FileDiff, MrDetail};

pub(super) fn render(frame: &mut Frame, area: Rect, state: &DetailState, view: &mut ViewState) {
    let [title, sections, body] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(area);

    render_title(frame, title, state);
    render_sections(frame, sections, state);

    let Some(detail) = &state.detail else {
        let message = if state.error.is_some() {
            format!(
                "Could not load this {}. Press r to retry.",
                singular(state.forge)
            )
        } else {
            format!("Loading {}…", singular(state.forge))
        };
        let block = border();
        let inner = block.inner(body);
        frame.render_widget(block, body);
        let [middle] = Layout::vertical([Constraint::Length(1)])
            .flex(Flex::Center)
            .areas(inner);
        frame.render_widget(
            Paragraph::new(message)
                .dark_gray()
                .alignment(Alignment::Center),
            middle,
        );
        return;
    };

    match state.section {
        Section::Overview => {
            let lines = overview_lines(detail);
            render_scrollable(frame, body, border(), lines, state.scroll, view);
        }
        Section::Files => {
            render_files(
                frame,
                body,
                FilesPane {
                    files: &detail.files,
                    discussions: &detail.discussions,
                    selected: state.selected_file,
                    scroll: state.scroll,
                    cursor: state.diff_focused.then_some(state.diff_cursor),
                },
                view,
            );
        }
        Section::Discussion => {
            let lines = discussion_lines(detail);
            render_scrollable(frame, body, border(), lines, state.scroll, view);
        }
        Section::Pipelines => ci::render(frame, body, &state.ci, view),
    }
}

/// The pane that has the keyboard focus.
fn focused_border() -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(Color::Green).bold())
}

pub(super) fn border() -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(Color::Blue))
}

fn render_title(frame: &mut Frame, area: Rect, state: &DetailState) {
    let mut spans = vec![
        Span::raw(format!(" {} ", state.reference)).dark_gray(),
        Span::raw(state.title.clone()).bold(),
    ];
    if let Some(detail) = &state.detail {
        let (symbol, color) = pipeline_symbol(detail.pipeline_status);
        spans.extend([
            Span::raw(format!(
                "  {} → {}  ",
                detail.source_branch, detail.target_branch
            ))
            .dark_gray(),
            Span::raw(format!("CI {symbol}")).fg(color),
            Span::raw("  "),
            approvals_span(detail),
        ]);
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn approvals_span(detail: &MrDetail) -> Span<'static> {
    let approvals = &detail.approvals;
    let count = approvals.approved_by.len();
    let mut text = match approvals.approvals_required {
        Some(required) if required > 0 => format!("✓ {count}/{required}"),
        _ => format!("✓ {count}"),
    };
    if approvals.user_has_approved {
        text.push_str(" (you)");
    }
    if count > 0 {
        Span::raw(text).fg(Color::Green)
    } else {
        Span::raw(text).dark_gray()
    }
}

fn render_sections(frame: &mut Frame, area: Rect, state: &DetailState) {
    let mut spans = vec![Span::raw(" ")];
    for (i, section) in Section::ALL.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" │ ").dark_gray());
        }
        let label = match (section, &state.detail) {
            (Section::Overview, _) => " Overview ".to_string(),
            (Section::Files, Some(d)) => format!(" Files ({}) ", d.files.len()),
            (Section::Files, None) => " Files ".to_string(),
            (Section::Discussion, Some(d)) => format!(" Discussion ({}) ", thread_count(d)),
            (Section::Discussion, None) => " Discussion ".to_string(),
            (Section::Pipelines, _) => match &state.ci.snapshot {
                Some(snapshot) => format!(
                    " {} ({}) ",
                    state.forge.pipelines(),
                    snapshot.pipelines.len()
                ),
                None => format!(" {} ", state.forge.pipelines()),
            },
        };
        spans.push(if *section == state.section {
            Span::raw(label).bold().fg(Color::Black).bg(Color::Blue)
        } else {
            Span::raw(label).dark_gray()
        });
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Draws wrapped text that scrolls, and records how far it can scroll.
fn render_scrollable(
    frame: &mut Frame,
    area: Rect,
    block: Block,
    lines: Vec<Line>,
    scroll: u16,
    view: &mut ViewState,
) {
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    // Number of rows after wrapping at this width (ratatui feature `unstable-rendered-line-info`).
    let total = u16::try_from(paragraph.line_count(inner.width)).unwrap_or(u16::MAX);
    let max = total.saturating_sub(inner.height);
    view.scroll_limit = Some(usize::from(max));
    frame.render_widget(paragraph.scroll((scroll.min(max), 0)), inner);
}

fn label(text: &str) -> Span<'static> {
    Span::raw(format!("{text:<11}")).dark_gray()
}

fn overview_lines(detail: &MrDetail) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    lines.push(Line::from(vec![
        label("Author"),
        Span::raw(format!(
            "@{} ({})",
            detail.author.username, detail.author.name
        )),
    ]));
    lines.push(Line::from(vec![
        label("Branches"),
        Span::raw(detail.source_branch.clone()).fg(Color::Magenta),
        Span::raw(" → "),
        Span::raw(detail.target_branch.clone()).fg(Color::Magenta),
    ]));
    let mut state = detail.state.clone();
    if detail.draft {
        state.push_str(", draft");
    }
    lines.push(Line::from(vec![label("State"), Span::raw(state)]));

    let merge_status = if detail.has_conflicts {
        Span::raw("has conflicts").fg(Color::Red)
    } else {
        match detail.merge_status.as_deref() {
            Some(status @ ("mergeable" | "can_be_merged")) => {
                Span::raw(status.replace('_', " ")).fg(Color::Green)
            }
            Some(status) => Span::raw(status.replace('_', " ")).fg(Color::Yellow),
            None => Span::raw("unknown").dark_gray(),
        }
    };
    lines.push(Line::from(vec![label("Mergeable"), merge_status]));

    let (symbol, color) = pipeline_symbol(detail.pipeline_status);
    let pipeline = match detail.pipeline_status {
        Some(status) => format!("{symbol} {status:?}").to_lowercase(),
        None => "no pipeline".to_string(),
    };
    lines.push(Line::from(vec![
        label("Pipeline"),
        Span::raw(pipeline).fg(color),
    ]));

    let approvals = &detail.approvals;
    let mut approval_text = if approvals.approved_by.is_empty() {
        "none".to_string()
    } else {
        approvals
            .approved_by
            .iter()
            .map(|u| format!("@{}", u.username))
            .collect::<Vec<_>>()
            .join(", ")
    };
    if let Some(left) = approvals.approvals_left.filter(|left| *left > 0) {
        approval_text.push_str(&format!("  ({left} more needed)"));
    }
    lines.push(Line::from(vec![
        label("Approvals"),
        Span::raw(approval_text),
    ]));

    let added: usize = detail.files.iter().map(|f| f.added).sum();
    let removed: usize = detail.files.iter().map(|f| f.removed).sum();
    lines.push(Line::from(vec![
        label("Changes"),
        Span::raw(format!("{} files  ", detail.files.len())),
        Span::raw(format!("+{added}")).fg(Color::Green),
        Span::raw(" "),
        Span::raw(format!("−{removed}")).fg(Color::Red),
    ]));

    lines.push(Line::from(""));
    if detail.description.trim().is_empty() {
        lines.push(Line::from("No description.").dark_gray());
    } else {
        // Shown as raw Markdown for now.
        lines.extend(
            detail
                .description
                .lines()
                .map(|line| Line::from(format::expand_tabs(line))),
        );
    }
    lines
}

/// What `render_files` draws.
pub(super) struct FilesPane<'a> {
    pub files: &'a [FileDiff],
    /// Threads to show under their diff lines (none for a branch comparison).
    pub discussions: &'a [Discussion],
    pub selected: usize,
    pub scroll: u16,
    /// The line cursor, when the diff has the focus.
    pub cursor: Option<usize>,
}

/// Changed files on the left, the selected file's diff on the right. Shared by merge
/// requests and branch comparisons.
pub(super) fn render_files(frame: &mut Frame, area: Rect, pane: FilesPane, view: &mut ViewState) {
    let FilesPane {
        files,
        discussions,
        selected,
        scroll,
        cursor,
    } = pane;
    let diff_focused = cursor.is_some();
    let list_width = (area.width / 3).clamp(24, 50);
    let [list_area, diff_area] =
        Layout::horizontal([Constraint::Length(list_width), Constraint::Min(0)]).areas(area);
    view.panes = Some(Panes {
        list: list_area,
        content: diff_area,
        clickable: true,
    });
    // The pane that `j`/`k` act on has a bright border.
    let (list_border, diff_border) = if diff_focused {
        (border(), focused_border())
    } else {
        (focused_border(), border())
    };

    let items: Vec<ListItem> = files.iter().map(file_item).collect();
    let list = List::new(items)
        .block(list_border.title(" Files "))
        .highlight_style(
            Style::new()
                .bg(super::SELECTED_BG)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");
    view.file_list.select(Some(selected));
    frame.render_stateful_widget(list, list_area, &mut view.file_list);

    let Some(file) = files.get(selected) else {
        frame.render_widget(
            Paragraph::new(" No changed files.").block(diff_border),
            diff_area,
        );
        return;
    };
    let title = if file.renamed_file {
        format!(" {} → {} ", file.old_path, file.new_path)
    } else {
        format!(" {} ", file.new_path)
    };
    let block = diff_border.title(title);
    let (lines, starts) = diff_with_threads(file, discussions);
    match cursor {
        // Lines that are only a message ("Diff too large…") have no cursor.
        Some(cursor) if !starts.is_empty() => {
            let key = (selected, file.new_path.clone());
            render_with_cursor(frame, diff_area, block, lines, &starts, cursor, key, view);
        }
        _ => render_scrollable(frame, diff_area, block, lines, scroll, view),
    }
}

/// The diff's lines with the threads on each line right below it, and for each diff line
/// where it starts and how many display lines it takes with its threads.
fn diff_with_threads(
    file: &FileDiff,
    discussions: &[Discussion],
) -> (Vec<Line<'static>>, Vec<(usize, usize)>) {
    let base = diff_lines(file);
    if file.too_large || file.lines.is_empty() {
        return (base, Vec::new());
    }
    let now = Utc::now();
    let mut lines = Vec::with_capacity(base.len());
    let mut starts = Vec::with_capacity(base.len());
    for (line, diff_line) in base.into_iter().zip(&file.lines) {
        let start = lines.len();
        lines.push(line);
        for thread in discussions
            .iter()
            .filter(|d| d.position().is_some_and(|p| p.is_on(file, diff_line)))
        {
            lines.extend(thread_lines(thread, now));
        }
        starts.push((start, lines.len() - start));
    }
    (lines, starts)
}

/// A thread under its diff line: a yellow bar, who wrote what and when.
fn thread_lines(thread: &Discussion, now: chrono::DateTime<Utc>) -> Vec<Line<'static>> {
    const INDENT: &str = "          ";
    let bar = || Span::raw(format!("{INDENT}┃ ")).fg(Color::Yellow);
    let mut lines = Vec::new();
    for (i, note) in thread.notes.iter().filter(|n| !n.system).enumerate() {
        let marker = if i == 0 { "● " } else { "↳ " };
        lines.push(Line::from(vec![
            bar(),
            Span::raw(marker).fg(Color::Yellow),
            Span::raw(format!("@{}", note.author.username)).bold(),
            Span::raw(format!(
                " · {}",
                format::relative_time(note.created_at, now)
            ))
            .dark_gray(),
        ]));
        for text in note.body.lines() {
            lines.push(Line::from(vec![
                bar(),
                Span::raw(format!("  {}", format::expand_tabs(text))),
            ]));
        }
    }
    lines
}

/// The diff with the line under the cursor highlighted, scrolled so that line is visible.
/// The scroll offset lives in `ViewState` (like ratatui's `ListState`): only drawing knows
/// how many rows each line takes once wrapped.
#[allow(clippy::too_many_arguments)] // Drawing inputs; a struct would only rename them.
fn render_with_cursor(
    frame: &mut Frame,
    area: Rect,
    block: Block,
    mut lines: Vec<Line<'static>>,
    starts: &[(usize, usize)],
    cursor: usize,
    key: (usize, String),
    view: &mut ViewState,
) {
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let height = usize::from(inner.height);
    let (first, count) = starts[cursor.min(starts.len() - 1)];
    let rows: Vec<usize> = lines
        .iter()
        .map(|line| {
            Paragraph::new(line.clone())
                .wrap(Wrap { trim: false })
                .line_count(inner.width)
                .max(1)
        })
        .collect();
    let start: usize = rows[..first].iter().sum();
    // The line and its threads, so a thread is read with its line.
    let tall: usize = rows[first..first + count].iter().sum();
    let total: usize = rows.iter().sum();

    // Another file: start from the top.
    if view.diff_offset_for.as_ref() != Some(&key) {
        view.diff_offset = 0;
        view.diff_offset_for = Some(key);
    }
    let mut offset = view.diff_offset;
    if start < offset {
        offset = start;
    } else if start + tall > offset + height {
        // As much of the thread as fits, but never past the line itself.
        offset = (start + tall).saturating_sub(height).min(start);
    }
    offset = offset.min(total.saturating_sub(height));
    view.diff_offset = offset;

    if let Some(line) = lines.get_mut(first) {
        *line = std::mem::take(line).patch_style(
            Style::new()
                .bg(super::SELECTED_BG)
                .add_modifier(Modifier::BOLD),
        );
    }
    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((u16::try_from(offset).unwrap_or(u16::MAX), 0));
    frame.render_widget(paragraph, inner);
}

fn file_item(file: &FileDiff) -> ListItem<'static> {
    let (status, color) = if file.new_file {
        ("A", Color::Green)
    } else if file.deleted_file {
        ("D", Color::Red)
    } else if file.renamed_file {
        ("R", Color::Yellow)
    } else {
        ("M", Color::Blue)
    };
    // Show the file name first: long directory paths get cut at the end, not the name.
    let (dir, name) = match file.new_path.rsplit_once('/') {
        Some((dir, name)) => (format!(" {dir}/"), name.to_string()),
        None => (String::new(), file.new_path.clone()),
    };
    ListItem::new(Line::from(vec![
        Span::raw(format!("{status} ")).fg(color),
        Span::raw(name),
        Span::raw(format!(" +{}", file.added)).fg(Color::Green),
        Span::raw(format!(" −{}", file.removed)).fg(Color::Red),
        Span::raw(dir).dark_gray(),
    ]))
}

fn diff_lines(file: &FileDiff) -> Vec<Line<'static>> {
    if file.too_large {
        return vec![
            Line::from("Diff too large to show here. Press o to open it in the browser.")
                .dark_gray(),
        ];
    }
    if file.lines.is_empty() {
        return vec![
            Line::from("No text changes (binary file, or only the file mode changed).").dark_gray(),
        ];
    }
    let number = |n: Option<u32>| n.map_or("    ".to_string(), |n| format!("{n:>4}"));
    file.lines
        .iter()
        .map(|line| {
            let gutter = format!("{} {} ", number(line.old_line), number(line.new_line));
            match line.kind {
                DiffLineKind::HunkHeader => Line::from(line.text.clone()).fg(Color::Cyan),
                DiffLineKind::Meta => Line::from(line.text.clone()).dark_gray(),
                // With syntax colors: the sign says added or removed, the code keeps its colors.
                _ if !line.tokens.is_empty() => {
                    let (sign, sign_color, dim) = match line.kind {
                        DiffLineKind::Added => ("+", Color::Green, false),
                        // Removed code is dimmed, so the old and new versions stand apart.
                        DiffLineKind::Removed => ("-", Color::Red, true),
                        _ => (" ", Color::Reset, false),
                    };
                    let mut spans = vec![
                        Span::raw(gutter).dark_gray(),
                        Span::raw(sign).fg(sign_color).bold(),
                    ];
                    spans.extend(line.tokens.iter().map(|token| {
                        let mut style = Style::new();
                        if let Some(color) = token.color {
                            style = style.fg(Color::Indexed(color));
                        }
                        if dim {
                            style = style.add_modifier(Modifier::DIM);
                        }
                        Span::styled(format::expand_tabs(&token.text), style)
                    }));
                    Line::from(spans)
                }
                DiffLineKind::Context => Line::from(vec![
                    Span::raw(gutter).dark_gray(),
                    Span::raw(format!(" {}", format::expand_tabs(&line.text))),
                ]),
                DiffLineKind::Added => Line::from(vec![
                    Span::raw(gutter).dark_gray(),
                    Span::raw(format!("+{}", format::expand_tabs(&line.text))).fg(Color::Green),
                ]),
                DiffLineKind::Removed => Line::from(vec![
                    Span::raw(gutter).dark_gray(),
                    Span::raw(format!("-{}", format::expand_tabs(&line.text))).fg(Color::Red),
                ]),
            }
        })
        .collect()
}

/// Threads with at least one comment written by a person.
fn thread_count(detail: &MrDetail) -> usize {
    detail
        .discussions
        .iter()
        .filter(|d| d.notes.iter().any(|n| !n.system))
        .count()
}

fn discussion_lines(detail: &MrDetail) -> Vec<Line<'static>> {
    let now = Utc::now();
    let mut lines = Vec::new();
    for discussion in &detail.discussions {
        let notes: Vec<_> = discussion.notes.iter().filter(|n| !n.system).collect();
        let Some(first) = notes.first() else {
            continue;
        };

        let mut header = vec![
            Span::raw("● ").fg(Color::Blue),
            Span::raw(format!("@{}", first.author.username)).bold(),
            Span::raw(format!(
                " · {}",
                format::relative_time(first.created_at, now)
            ))
            .dark_gray(),
        ];
        if let Some(position) = &first.position {
            let path = position.new_path.clone().unwrap_or_default();
            let line = position.new_line.or(position.old_line);
            let location = match line {
                Some(line) => format!("  {path}:{line}"),
                None => format!("  {path}"),
            };
            header.push(Span::raw(location).fg(Color::Cyan));
        }
        if first.resolvable {
            header.push(if notes.iter().all(|n| n.resolved) {
                Span::raw("  resolved").fg(Color::Green)
            } else {
                Span::raw("  unresolved").fg(Color::Yellow)
            });
        }
        lines.push(Line::from(header));
        lines.extend(
            first
                .body
                .lines()
                .map(|l| Line::from(format!("  {}", format::expand_tabs(l)))),
        );

        for reply in &notes[1..] {
            lines.push(Line::from(vec![
                Span::raw("  ↳ ").dark_gray(),
                Span::raw(format!("@{}", reply.author.username)).bold(),
                Span::raw(format!(
                    " · {}",
                    format::relative_time(reply.created_at, now)
                ))
                .dark_gray(),
            ]));
            lines.extend(
                reply
                    .body
                    .lines()
                    .map(|l| Line::from(format!("    {}", format::expand_tabs(l)))),
            );
        }
        lines.push(Line::from(""));
    }
    if lines.is_empty() {
        lines.push(Line::from("No comments yet. Press c to write one.").dark_gray());
    }
    lines
}

pub(super) fn render_popup(frame: &mut Frame, state: &DetailState) {
    match state.popup {
        Some(Popup::Comment) => render_comment_popup(frame, state),
        Some(Popup::ConfirmMerge) => render_merge_popup(frame, state),
        Some(Popup::ConfirmCi(action)) => render_ci_popup(frame, &state.ci, action),
        None => {}
    }
}

pub(super) fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let [row] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);
    let [cell] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(row);
    cell
}

pub(super) fn popup_block(title: String, hint: &'static str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(Color::Yellow))
        .title(Line::from(title).bold())
        .title_bottom(Line::from(hint).dark_gray())
}

fn render_comment_popup(frame: &mut Frame, state: &DetailState) {
    let area = frame.area();
    let popup = centered(area, area.width.saturating_sub(8).min(90), 12);
    let title = match &state.comment_target {
        Some(target) => target.title(),
        None => format!("Comment on {}", state.reference),
    };
    let block = popup_block(
        format!(" {title} "),
        " Ctrl+S send · Enter new line · Esc close (draft is kept) ",
    );
    let inner = block.inner(popup);
    // `Clear` erases what is underneath, otherwise the list would show through.
    frame.render_widget(Clear, popup);
    frame.render_widget(block, popup);

    let text = format!("{}█", state.draft);
    let paragraph = Paragraph::new(text).wrap(Wrap { trim: false });
    // Keep the end of the text (where the cursor is) visible.
    let total = u16::try_from(paragraph.line_count(inner.width)).unwrap_or(u16::MAX);
    let scroll = total.saturating_sub(inner.height);
    frame.render_widget(paragraph.scroll((scroll, 0)), inner);
}

fn render_merge_popup(frame: &mut Frame, state: &DetailState) {
    let Some(detail) = &state.detail else {
        return;
    };
    let mut lines = vec![
        Line::from(vec![
            Span::raw(format!("Merge {} into ", state.reference)),
            Span::raw(detail.target_branch.clone())
                .fg(Color::Magenta)
                .bold(),
            Span::raw("?"),
        ]),
        Line::from(""),
    ];

    let mut warnings = Vec::new();
    if detail.has_conflicts {
        warnings.push("has merge conflicts".to_string());
    }
    match detail.pipeline_status {
        Some(crate::forge::PipelineStatus::Success) | None => {}
        Some(status) => warnings.push(format!("pipeline is {status:?}").to_lowercase()),
    }
    if let Some(left) = detail.approvals.approvals_left.filter(|l| *l > 0) {
        warnings.push(format!("needs {left} more approval(s)"));
    }
    if let Some(status) = detail
        .merge_status
        .as_deref()
        .filter(|s| !matches!(*s, "mergeable" | "can_be_merged"))
    {
        warnings.push(format!("GitLab says: {}", status.replace('_', " ")));
    }
    if warnings.is_empty() {
        lines.push(Line::from("✓ Ready to merge").fg(Color::Green));
    }
    for warning in warnings {
        lines.push(Line::from(format!("⚠ {warning}")).fg(Color::Yellow));
    }
    if let Some(sha) = &detail.sha {
        let short: String = sha.chars().take(8).collect();
        lines.push(Line::from(""));
        lines.push(
            Line::from(format!(
                "Merging commit {short}. {} refuses if the branch changed since.",
                state.forge.name()
            ))
            .dark_gray(),
        );
    }

    let area = frame.area();
    let height = u16::try_from(lines.len()).unwrap_or(10) + 2;
    let popup = centered(area, area.width.saturating_sub(8).min(70), height);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(popup_block(
                " Confirm merge ".to_string(),
                " y merge · n cancel ",
            )),
        popup,
    );
}

pub(super) fn render_ci_popup(frame: &mut Frame, ci: &CiState, action: CiAction) {
    let what = ci.describe(action);
    let lines = vec![
        Line::from(format!("Cancel {what}?")),
        Line::from(""),
        Line::from("Work in progress is lost; you can retry it later.").dark_gray(),
    ];
    let area = frame.area();
    let popup = centered(area, area.width.saturating_sub(8).min(60), 5);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).block(popup_block(
            " Confirm ".to_string(),
            " y cancel it · n keep running ",
        )),
        popup,
    );
}

/// "merge request" or "pull request".
fn singular(forge: crate::forge::ForgeKind) -> String {
    let plural = forge.change_requests().to_lowercase();
    plural.trim_end_matches('s').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::Note;

    #[test]
    fn threads_appear_below_their_line() {
        let file = FileDiff {
            old_path: "a.rs".into(),
            new_path: "a.rs".into(),
            new_file: false,
            deleted_file: false,
            renamed_file: false,
            too_large: false,
            lines: crate::forge::diff::parse_unified_diff("@@ -1 +1,2 @@\n keep\n+new\n"),
            added: 1,
            removed: 0,
        };
        let note: Note = serde_json::from_value(serde_json::json!({
            "id": 1, "body": "Rename this?\nIt is unclear.",
            "author": {"username": "hoang", "name": "Hoang"},
            "created_at": "2026-10-10T08:00:00Z",
            "position": {"new_path": "a.rs", "new_line": 1, "old_line": 1}
        }))
        .unwrap();
        let thread = Discussion {
            id: "d1".into(),
            notes: vec![note],
        };
        let (lines, starts) = diff_with_threads(&file, &[thread]);
        let text =
            |i: usize| -> String { lines[i].spans.iter().map(|s| s.content.as_ref()).collect() };
        // Header, `keep` and its thread (author + 2 lines), then `new`.
        assert_eq!(starts, vec![(0, 1), (1, 4), (5, 1)]);
        assert!(text(2).contains("@hoang"), "{}", text(2));
        assert!(text(3).ends_with("Rename this?"), "{}", text(3));
        assert!(text(5).contains("new"));
    }
}
