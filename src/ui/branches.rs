//! The Branches view and a branch compared with the default branch.

use chrono::Utc;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, List, ListItem, Paragraph, Row, Table};

use super::{COLUMN_SPACING, ViewState, column_width, detail, format, pipeline_symbol};
use crate::app::AppState;
use crate::app::branches::BranchScreen;
use crate::forge::Branch;

fn centered_message(frame: &mut Frame, area: Rect, message: String) {
    let block = detail::border();
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [middle] = Layout::vertical([Constraint::Length(1)])
        .flex(Flex::Center)
        .areas(inner);
    frame.render_widget(
        Paragraph::new(message)
            .dark_gray()
            .alignment(Alignment::Center),
        middle,
    );
}

pub(super) fn render_list(frame: &mut Frame, area: Rect, state: &AppState, view: &mut ViewState) {
    let [heading, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    let list = &state.branches;
    let mut title = vec![Span::raw(" Branches").bold()];
    if let Some(repo) = &state.repo {
        title.push(Span::raw(format!(" of {repo}")).fg(Color::Cyan));
        if list.loaded_once && list.error.is_none() {
            title.push(Span::raw(format!(" ({})", list.branches.len())).dark_gray());
        }
        title.push(
            Span::raw("  · Enter: compare with the default branch · b: with another branch")
                .dark_gray(),
        );
    }
    frame.render_widget(Paragraph::new(Line::from(title)), heading);

    let visible = state.visible_branches();
    if visible.is_empty() {
        centered_message(frame, body, empty_message(state));
        return;
    }

    let widths = [
        Constraint::Fill(3),
        Constraint::Fill(4),
        Constraint::Length(14),
        Constraint::Length(2),
        Constraint::Length(6),
        Constraint::Length(7),
    ];
    let name_width = column_width(detail::border().inner(body), &widths, 0);
    let prefix = state.forge.reference_prefix();
    let now = Utc::now();
    let rows = visible.iter().map(|branch| {
        let (symbol, color) = pipeline_symbol(branch.pipeline_status);
        Row::new([
            Cell::from(name_line(branch, name_width)),
            Cell::from(branch.commit_title.as_str()),
            Cell::from(branch.commit_author.as_deref().unwrap_or("–")).dark_gray(),
            Cell::from(symbol).fg(color),
            match branch.merge_request {
                Some(number) => Cell::from(format!("{prefix}{number}")).fg(Color::Green),
                None => Cell::from("–").dark_gray(),
            },
            Cell::from(
                branch
                    .committed_at
                    .map(|at| format::relative_time(at, now))
                    .unwrap_or_default(),
            )
            .dark_gray(),
        ])
    });
    let mr_header = if prefix == '#' { "PR" } else { "MR" };
    let header = Row::new([
        "Branch",
        "Last commit",
        "Author",
        "CI",
        mr_header,
        "Updated",
    ])
    .style(Style::new().dark_gray().add_modifier(Modifier::BOLD));
    let table = Table::new(rows, widths)
        .header(header)
        .block(detail::border())
        .column_spacing(COLUMN_SPACING)
        .row_highlight_style(
            Style::new()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");
    view.branch_table.select(Some(list.selected));
    frame.render_stateful_widget(table, body, &mut view.branch_table);
}

/// The branch name, shortened in the middle to fit, with a `default` tag.
fn name_line(branch: &Branch, width: usize) -> Line<'static> {
    const TAG: &str = " default";
    if branch.default {
        let room = width.saturating_sub(TAG.len());
        Line::from(vec![
            Span::raw(format::shorten_middle(&branch.name, room))
                .fg(Color::Yellow)
                .bold(),
            Span::raw(TAG).dark_gray(),
        ])
    } else {
        Line::from(format::shorten_middle(&branch.name, width)).fg(Color::Magenta)
    }
}

fn empty_message(state: &AppState) -> String {
    let list = &state.branches;
    if state.repo.is_none() {
        "Branches belong to one repository. Press s to pick one.".to_string()
    } else if !list.loaded_once {
        "Loading branches…".to_string()
    } else if list.error.is_some() && list.branches.is_empty() {
        "Could not load the branches. Press r to retry.".to_string()
    } else if !state.filter.is_empty() {
        format!("No branches match “{}”.", state.filter)
    } else {
        "This repository has no branches yet.".to_string()
    }
}

pub(super) fn render_screen(
    frame: &mut Frame,
    area: Rect,
    screen: &BranchScreen,
    view: &mut ViewState,
) {
    let [title, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    let mut spans = vec![
        Span::raw(format!(" {} ", screen.project))
            .bold()
            .fg(Color::Cyan),
        Span::raw("· "),
        Span::raw(screen.branch.name.clone())
            .fg(Color::Magenta)
            .bold(),
        Span::raw(" compared with ").dark_gray(),
        Span::raw(if screen.base.is_empty() {
            "…".to_string()
        } else {
            screen.base.clone()
        })
        .fg(Color::Yellow),
    ];
    if let Some(comparison) = &screen.comparison {
        let added: usize = comparison.files.iter().map(|f| f.added).sum();
        let removed: usize = comparison.files.iter().map(|f| f.removed).sum();
        spans.extend([
            Span::raw(format!(
                "  · {} commit(s), {} file(s) ",
                comparison.commits,
                comparison.files.len()
            ))
            .dark_gray(),
            Span::raw(format!("+{added}")).fg(Color::Green),
            Span::raw(" "),
            Span::raw(format!("−{removed}")).fg(Color::Red),
        ]);
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), title);

    let Some(comparison) = &screen.comparison else {
        let message = if screen.error.is_some() {
            "Could not compare the branches. Press r to retry."
        } else if !screen.loading {
            "Pick the branch to compare with."
        } else {
            "Comparing…"
        };
        centered_message(frame, body, message.to_string());
        return;
    };
    if comparison.files.is_empty() {
        centered_message(
            frame,
            body,
            format!(
                "No changes: {} has nothing that {} does not have.",
                comparison.head, comparison.base
            ),
        );
        return;
    }
    detail::render_files(
        frame,
        body,
        &comparison.files,
        screen.selected_file,
        screen.scroll,
        screen.diff_focused,
        view,
    );
}

/// The "compare with…" popup: a filter line and the matching branches.
pub(super) fn render_base_picker(frame: &mut Frame, screen: &BranchScreen, view: &mut ViewState) {
    let Some(picker) = &screen.base_picker else {
        return;
    };
    let area = frame.area();
    let width = area.width.saturating_sub(8).min(70);
    let height = area.height.saturating_sub(6).min(20);
    let [row] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);
    let [popup] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(row);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(Color::Yellow))
        .title(Line::from(format!(" Compare {} with… ", screen.branch.name)).bold())
        .title_bottom(
            Line::from(" type to filter · ↑/↓ · Enter compare · Esc cancel ").dark_gray(),
        );
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let [search, list_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(inner);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(" / ").fg(Color::Blue).bold(),
            Span::raw(picker.filter.clone()),
            Span::raw("█").fg(Color::Blue),
        ])),
        search,
    );
    let candidates = screen.base_candidates();
    if candidates.is_empty() {
        frame.render_widget(Paragraph::new(" No branch matches.").dark_gray(), list_area);
        return;
    }
    let items: Vec<ListItem> = candidates
        .iter()
        .map(|name| {
            let mut spans = vec![Span::raw(name.to_string())];
            if *name == screen.base {
                spans.push(Span::raw("  (current)").dark_gray());
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let list = List::new(items)
        .highlight_style(
            Style::new()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");
    view.base_list.select(Some(picker.selected));
    frame.render_stateful_widget(list, list_area, &mut view.base_list);
}
