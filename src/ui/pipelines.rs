//! The Pipelines view of the dashboard and a pipeline opened from it.

use chrono::Utc;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};

use super::{ViewState, ci, format, pipeline_symbol, render_tab_bar};
use crate::app::AppState;
use crate::app::pipelines::{PipelineScreen, PipelineTab};
use crate::forge::{PipelineList, PipelineSummary};

fn tab_label(list: &PipelineList) -> &str {
    match list {
        PipelineList::Mine => "Mine",
        PipelineList::Recent => "Recent",
        PipelineList::Project(path) => path,
    }
}

pub(super) fn render_list(frame: &mut Frame, area: Rect, state: &AppState, view: &mut ViewState) {
    let [tabs, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    let labels = state.pipeline_tabs.iter().map(|tab| {
        if tab.loaded_once && tab.error.is_none() {
            format!("{} ({})", tab_label(&tab.list), tab.pipelines.len())
        } else {
            tab_label(&tab.list).to_string()
        }
    });
    render_tab_bar(frame, tabs, labels, state.active_pipeline_tab);

    let tab = state.active_pipeline_tab();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(Color::Blue));
    let visible = state.visible_pipelines();
    if visible.is_empty() {
        let inner = block.inner(body);
        frame.render_widget(block, body);
        let [middle] = Layout::vertical([Constraint::Length(1)])
            .flex(Flex::Center)
            .areas(inner);
        frame.render_widget(
            Paragraph::new(empty_message(state, tab))
                .dark_gray()
                .alignment(Alignment::Center),
            middle,
        );
        return;
    }

    // The project column is redundant on a project tab.
    let show_project = !matches!(tab.list, PipelineList::Project(_));
    let now = Utc::now();
    let rows = visible.iter().map(|pipeline| {
        let (symbol, color) = pipeline_symbol(Some(pipeline.status));
        let mut cells = vec![
            Cell::from(symbol).fg(color),
            // GitHub runs are known by their workflow's name; GitLab pipelines by number.
            match &pipeline.name {
                Some(name) => Cell::from(name.clone()),
                None => Cell::from(format!("#{}", pipeline.id)).dark_gray(),
            },
        ];
        if show_project {
            cells.push(Cell::from(pipeline.project_path.as_str()).fg(Color::Cyan));
        }
        cells.extend([
            Cell::from(pipeline.git_ref.as_str()).fg(Color::Magenta),
            Cell::from(pipeline.user.as_deref().unwrap_or("–")),
            Cell::from(duration(pipeline)).dark_gray(),
            Cell::from(format::relative_time(pipeline.created_at, now)).dark_gray(),
        ]);
        Row::new(cells)
    });

    let mut headers = vec![
        "CI",
        if state.forge.supports_live_logs() {
            "Pipeline"
        } else {
            "Workflow"
        },
    ];
    let mut widths = vec![Constraint::Length(2), Constraint::Length(14)];
    if show_project {
        headers.push("Project");
        widths.push(Constraint::Fill(2));
    }
    headers.extend(["Ref", "Triggered by", "Duration", "Created"]);
    widths.extend([
        Constraint::Fill(2),
        Constraint::Length(16),
        Constraint::Length(8),
        Constraint::Length(7),
    ]);
    let header = Row::new(headers).style(Style::new().dark_gray().add_modifier(Modifier::BOLD));
    let table = Table::new(rows, widths)
        .header(header)
        .block(block)
        .column_spacing(2)
        .row_highlight_style(
            Style::new()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    // One TableState per tab, so each tab keeps its own scroll position.
    if view.pipeline_tables.len() < state.pipeline_tabs.len() {
        view.pipeline_tables
            .resize_with(state.pipeline_tabs.len(), TableState::default);
    }
    let table_state = &mut view.pipeline_tables[state.active_pipeline_tab];
    table_state.select(Some(tab.selected));
    frame.render_stateful_widget(table, body, table_state);
}

/// Finished: how long it took. Running: time since it was created.
fn duration(pipeline: &PipelineSummary) -> String {
    match (pipeline.duration, pipeline.status.is_active()) {
        (Some(seconds), _) => format::duration(seconds as f64),
        (None, true) => {
            let seconds = (Utc::now() - pipeline.created_at).num_milliseconds() as f64 / 1000.0;
            format::duration(seconds)
        }
        (None, false) => "–".to_string(),
    }
}

fn empty_message(state: &AppState, tab: &PipelineTab) -> String {
    if !tab.loaded_once {
        "Loading pipelines…".to_string()
    } else if tab.error.is_some() && tab.pipelines.is_empty() {
        "Could not load pipelines. Press r to retry.".to_string()
    } else if !state.filter.is_empty() {
        format!("No pipelines match “{}”.", state.filter)
    } else {
        match tab.list {
            PipelineList::Mine => "No recent pipelines triggered by you.".to_string(),
            _ => "No recent pipelines.".to_string(),
        }
    }
}

pub(super) fn render_screen(
    frame: &mut Frame,
    area: Rect,
    screen: &PipelineScreen,
    view: &mut ViewState,
) {
    let [title, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    let summary = &screen.summary;
    let line = Line::from(vec![
        Span::raw(format!(" {} ", summary.project_path))
            .bold()
            .fg(Color::Cyan),
        Span::raw(format!("· {}", summary.git_ref)).fg(Color::Magenta),
        Span::raw(match &summary.user {
            Some(user) => format!(" · triggered by @{user}"),
            None => String::new(),
        })
        .dark_gray(),
    ]);
    frame.render_widget(Paragraph::new(line), title);
    ci::render(frame, body, &screen.ci, view);
}
