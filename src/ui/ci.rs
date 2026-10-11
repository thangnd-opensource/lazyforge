//! The Pipelines section: a stage → job tree on the left, the selected job's log on the right.

use ansi_to_tui::IntoText;
use chrono::Utc;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};

use super::{Panes, ViewState, format, pipeline_symbol};
use crate::app::ci::CiState;
use crate::forge::{ForgeKind, Job, PipelineStatus};

fn border() -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(Color::Blue))
}

fn centered_message(frame: &mut Frame, area: Rect, message: &str) {
    let block = border();
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [middle] = Layout::vertical([Constraint::Length(1)])
        .flex(Flex::Center)
        .areas(inner);
    frame.render_widget(
        Paragraph::new(message.to_string())
            .dark_gray()
            .alignment(Alignment::Center),
        middle,
    );
}

pub(super) fn render(frame: &mut Frame, area: Rect, ci: &CiState, view: &mut ViewState) {
    let Some(pipeline) = ci.pipeline() else {
        let (pipelines, change_request) = match ci.forge {
            ForgeKind::GitLab => ("pipelines", "merge request"),
            ForgeKind::GitHub => ("workflow runs", "pull request"),
        };
        let message = if ci.snapshot.is_some() {
            format!("No {pipelines} for this {change_request}.")
        } else if ci.error.is_some() {
            format!("Could not load {pipelines}. Press r to retry.")
        } else {
            format!("Loading {pipelines}…")
        };
        centered_message(frame, area, &message);
        return;
    };

    let [header, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);

    let (symbol, color) = pipeline_symbol(Some(pipeline.status));
    let mut spans = vec![
        Span::raw(match &pipeline.name {
            // GitHub: the workflow's name.
            Some(name) => format!(" {name} #{} ", pipeline.id),
            None => format!(" Pipeline #{} ", pipeline.id),
        })
        .bold(),
        Span::raw(format!("{symbol} {}", status_text(pipeline.status))).fg(color),
        Span::raw(format!(
            "  · {} · {}",
            pipeline.git_ref,
            format::relative_time(pipeline.created_at, Utc::now())
        ))
        .dark_gray(),
    ];
    if let Some((index, count)) = ci.pipeline_position()
        && count > 1
    {
        spans
            .push(Span::raw(format!("  ({} of {count}: P older, p newer)", index + 1)).dark_gray());
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), header);

    let list_width = (body.width / 3).clamp(28, 48);
    let [jobs_area, log_area] =
        Layout::horizontal([Constraint::Length(list_width), Constraint::Min(0)]).areas(body);
    // The wheel scrolls the log; the job list has stage rows, so clicks do not pick jobs.
    view.panes = Some(Panes {
        list: jobs_area,
        content: log_area,
        clickable: false,
    });
    render_jobs(frame, jobs_area, ci, view);
    render_log(frame, log_area, ci, view);
}

fn render_jobs(frame: &mut Frame, area: Rect, ci: &CiState, view: &mut ViewState) {
    let jobs = ci.jobs();
    if jobs.is_empty() {
        frame.render_widget(
            Paragraph::new(" No jobs.")
                .dark_gray()
                .block(border().title(" Jobs ")),
            area,
        );
        return;
    }

    // Stage headers are list items too, so the selected job's row index is shifted by the
    // number of headers above it.
    let mut items = Vec::new();
    let mut selected_row = 0;
    let mut current_stage = None;
    for (index, job) in jobs.iter().enumerate() {
        if current_stage != Some(job.stage.as_str()) {
            current_stage = Some(job.stage.as_str());
            items.push(ListItem::new(Line::from(job.stage.clone()).bold()));
        }
        if index == ci.selected_job {
            selected_row = items.len();
        }
        items.push(job_item(job, ci.log.as_ref().map(|l| l.job_id)));
    }

    let list = List::new(items)
        .block(border().title(" Jobs "))
        .highlight_style(
            Style::new()
                .bg(super::SELECTED_BG)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");
    view.job_list.select(Some(selected_row));
    frame.render_stateful_widget(list, area, &mut view.job_list);
}

fn job_item(job: &Job, open_job: Option<u64>) -> ListItem<'static> {
    let (symbol, color) = if job.status == PipelineStatus::Failed && job.allow_failure {
        // Failed but allowed to: a warning, not an error.
        ("!", Color::Yellow)
    } else {
        pipeline_symbol(Some(job.status))
    };
    let mut spans = vec![
        Span::raw(format!("  {symbol} ")).fg(color),
        Span::raw(job.name.clone()),
        Span::raw(format!("  {}", job_duration(job))).dark_gray(),
    ];
    if open_job == Some(job.id) {
        spans.push(Span::raw("  ◂").fg(Color::Blue));
    }
    ListItem::new(Line::from(spans))
}

/// Finished jobs: their duration. Running jobs: time since they started.
fn job_duration(job: &Job) -> String {
    match (job.duration, job.started_at) {
        (Some(seconds), _) => format::duration(seconds),
        (None, Some(started)) => {
            let seconds = (Utc::now() - started).num_milliseconds() as f64 / 1000.0;
            format::duration(seconds)
        }
        (None, None) => String::new(),
    }
}

fn render_log(frame: &mut Frame, area: Rect, ci: &CiState, view: &mut ViewState) {
    let Some(log) = &ci.log else {
        let hint = match ci.selected_job() {
            Some(job) => format!("Press Enter to show the log of {}.", job.name),
            None => "Select a job to show its log.".to_string(),
        };
        centered_message(frame, area, &hint);
        return;
    };

    let (symbol, color) = pipeline_symbol(Some(log.status));
    let mut title = vec![
        Span::raw(format!(" {} ", log.job_name)).bold(),
        Span::raw(format!("{symbol} {} ", status_text(log.status))).fg(color),
    ];
    if log.follow {
        title.push(Span::raw("[following] ").fg(Color::Yellow));
    }
    let block = border().title(Line::from(title));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if log.lines.is_empty() {
        let message = if let Some(error) = &log.error {
            format!("Could not load the log: {error}")
        } else if log.status.is_active() && !ci.forge.supports_live_logs() {
            // Never fetched while the job runs (see `CiState::open_log`).
            format!(
                "{} serves job logs once the job has finished. This updates when it does.",
                ci.forge.name()
            )
        } else if log.loading || !log.loaded_once {
            "Loading log…".to_string()
        } else if log.status.is_active() {
            "Waiting for the job to start…".to_string()
        } else {
            "This job has no log.".to_string()
        };
        frame.render_widget(Paragraph::new(message).dark_gray(), inner);
        return;
    }

    // Logs can be tens of thousands of lines: only convert and draw what fits on screen.
    // Long lines are cut at the edge rather than wrapped, so line numbers and scroll
    // positions stay simple.
    let height = usize::from(inner.height);
    let max = log.lines.len().saturating_sub(height);
    view.scroll_limit = Some(max);
    let start = if log.follow { max } else { log.scroll.min(max) };
    let visible: Vec<Line> = log.lines[start..(start + height).min(log.lines.len())]
        .iter()
        .map(|line| ansi_line(line))
        .collect();
    frame.render_widget(Paragraph::new(visible), inner);
}

/// Converts one log line with ANSI color codes into a styled line.
fn ansi_line(line: &str) -> Line<'static> {
    let line = format::expand_tabs(line);
    match line.as_bytes().into_text() {
        Ok(text) => text.lines.into_iter().next().unwrap_or_default(),
        // Unparsable escape sequences: show the text as it is.
        Err(_) => Line::from(line),
    }
}

fn status_text(status: PipelineStatus) -> String {
    format!("{status:?}").to_lowercase()
}
