//! The platform → account → repository picker.

use chrono::Utc;
use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};

use super::{ViewState, format};
use crate::app::AppState;
use crate::app::picker::{PLATFORMS, Picker, PickerStep};

pub(super) fn render(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    picker: &Picker,
    view: &mut ViewState,
) {
    let width = area.width.saturating_sub(4).min(100);
    let height = area.height.saturating_sub(2);
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
        .title(Line::from(" Switch account ").bold());
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let [crumbs, gap, body] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(inner);
    render_breadcrumbs(frame, crumbs, state, picker);
    let _ = gap;

    let (items, title) = match picker.step {
        PickerStep::Platform => (platform_items(state), "Choose a platform"),
        PickerStep::Account => (account_items(state, picker), "Choose an account"),
        PickerStep::Repository => {
            let [search, list] =
                Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(body);
            let line = Line::from(vec![
                Span::raw(" / ").fg(Color::Blue).bold(),
                Span::raw(picker.filter.clone()),
                Span::raw("█").fg(Color::Blue),
                Span::raw("  type to filter").dark_gray(),
            ]);
            frame.render_widget(Paragraph::new(line), search);
            render_list(frame, list, repository_items(picker), picker.selected, view);
            return;
        }
    };
    let [heading, list] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(body);
    frame.render_widget(Paragraph::new(format!(" {title}")).dark_gray(), heading);
    render_list(frame, list, items, picker.selected, view);
}

fn render_list(
    frame: &mut Frame,
    area: Rect,
    items: Vec<ListItem<'static>>,
    selected: usize,
    view: &mut ViewState,
) {
    let list = List::new(items)
        .highlight_style(
            Style::new()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");
    view.picker_list.select(Some(selected));
    frame.render_stateful_widget(list, area, &mut view.picker_list);
}

/// `GitHub › work › …`, with the current step highlighted.
fn render_breadcrumbs(frame: &mut Frame, area: Rect, state: &AppState, picker: &Picker) {
    let step_span = |text: String, step: PickerStep| {
        if picker.step == step {
            Span::raw(text).bold().fg(Color::Black).bg(Color::Yellow)
        } else {
            Span::raw(text).dark_gray()
        }
    };
    let account = match picker.step {
        PickerStep::Repository => format!(" {} ", state.profile_name),
        _ => " account ".to_string(),
    };
    let line = Line::from(vec![
        Span::raw(" "),
        step_span(
            format!(" {} ", picker.platform.name()),
            PickerStep::Platform,
        ),
        Span::raw(" › ").dark_gray(),
        step_span(account, PickerStep::Account),
        Span::raw(" › ").dark_gray(),
        step_span(" repository ".to_string(), PickerStep::Repository),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn platform_items(state: &AppState) -> Vec<ListItem<'static>> {
    PLATFORMS
        .iter()
        .map(|kind| {
            let count = state.accounts.iter().filter(|a| a.kind == *kind).count();
            let accounts = match count {
                0 => "no account yet".to_string(),
                1 => "1 account".to_string(),
                n => format!("{n} accounts"),
            };
            ListItem::new(Line::from(vec![
                Span::raw(format!("{:<10}", kind.name())).bold(),
                Span::raw(accounts).dark_gray(),
            ]))
        })
        .collect()
}

fn account_items(state: &AppState, picker: &Picker) -> Vec<ListItem<'static>> {
    let mut items: Vec<ListItem> = picker
        .accounts(&state.accounts)
        .into_iter()
        .map(|account| {
            let mut spans = vec![
                Span::raw(format!("{:<24}", account.name)).bold(),
                Span::raw(account.host.as_str().trim_end_matches('/').to_string()).dark_gray(),
            ];
            if picker.connecting.as_deref() == Some(account.name.as_str()) {
                spans.push(Span::raw("  connecting…").fg(Color::Yellow));
            } else if account.name == state.profile_name {
                spans.push(Span::raw("  (in use)").fg(Color::Green));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    items.push(ListItem::new(
        Line::from(format!("+ Add a {} account…", picker.platform.name())).fg(Color::Blue),
    ));
    items
}

fn repository_items(picker: &Picker) -> Vec<ListItem<'static>> {
    let mut items = vec![ListItem::new(
        Line::from("★ All my repositories").fg(Color::Yellow),
    )];
    if picker.repos_loading {
        items.push(ListItem::new(
            Line::from("  Loading repositories…").dark_gray(),
        ));
    } else if let Some(error) = &picker.repos_error {
        items.push(ListItem::new(
            Line::from(format!("  Could not load repositories: {error}")).fg(Color::Red),
        ));
    }
    let now = Utc::now();
    items.extend(picker.visible_repos().into_iter().map(|repo| {
        let mut spans = vec![Span::raw(format!("{:<44}", repo.path))];
        if let Some(at) = repo.last_activity {
            spans.push(Span::raw(format!("{:>5}  ", format::relative_time(at, now))).dark_gray());
        }
        if let Some(description) = &repo.description {
            spans.push(Span::raw(description.clone()).dark_gray());
        }
        ListItem::new(Line::from(spans))
    }));
    items
}
