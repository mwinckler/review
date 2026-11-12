use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    style::{Color, Style},
    text::{Line, Text},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::{
    app::{App, Focus},
    gitlab_review::{SubmissionPreview, acknowledge_retry, pending_comments, pending_replies},
    gitlab_worker::GitLabJob,
    input, markdown,
    state::Review,
    tree,
    ui::{display_path, max_scroll, panes},
};

pub struct SubmissionDialog {
    pub summary: String,
    pub cursor: usize,
    pub selection: usize,
    pub scroll: usize,
    pub summary_focused: bool,
    pub preview: Option<SubmissionPreview>,
    pub reset_confirmation: bool,
    pub message: String,
}

impl SubmissionDialog {
    pub fn new(review: &Review) -> Self {
        let summary = review
            .gitlab
            .as_ref()
            .and_then(|gitlab| gitlab.publication.attempt.as_ref())
            .map(|attempt| attempt.summary.clone())
            .unwrap_or_default();
        Self {
            cursor: summary.len(),
            summary,
            selection: 0,
            scroll: 0,
            summary_focused: true,
            preview: None,
            reset_confirmation: false,
            message: String::new(),
        }
    }

    pub fn insert(&mut self, value: &str) {
        if self.preview.is_none() && !self.reset_confirmation && self.summary_focused {
            input::insert(&mut self.summary, &mut self.cursor, value);
        }
    }
}

impl App {
    pub(crate) fn open_submission(&mut self) {
        if self.resume_comment_draft() {
            return;
        }
        if self.review.gitlab.is_none() {
            self.message = "Open a GitLab MR with review gitlab to submit a review.".into();
            return;
        }
        self.focus_comments();
        self.submission_dialog = Some(SubmissionDialog::new(&self.review));
    }

    pub(crate) fn handle_submission_dialog(
        &mut self,
        key: crossterm::event::KeyEvent,
        page: usize,
    ) {
        let Some(mut dialog) = self.submission_dialog.take() else {
            return;
        };
        if dialog.reset_confirmation {
            match key.code {
                KeyCode::Char('y') => {
                    acknowledge_retry(&mut self.review);
                    self.dirty = true;
                    self.comments_changed = true;
                    dialog.reset_confirmation = false;
                    dialog.message =
                        "Retry acknowledged. Ctrl-s rechecks the server before another write."
                            .into();
                }
                KeyCode::Char('n') | KeyCode::Esc => dialog.reset_confirmation = false,
                _ => {}
            }
        } else if let Some(preview) = &dialog.preview {
            match key.code {
                KeyCode::Char('y') => {
                    self.submission_action = Some(GitLabJob::Publish(Box::new(preview.clone())))
                }
                KeyCode::Char('n') | KeyCode::Esc => dialog.preview = None,
                _ => {}
            }
        } else if key.code == KeyCode::Esc {
            return;
        } else if key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.submission_action = Some(GitLabJob::Preview(dialog.summary.clone()));
        } else if key.code == KeyCode::Char('X')
            && self
                .review
                .gitlab
                .as_ref()
                .is_some_and(|gitlab| gitlab.publication.uncertain())
        {
            dialog.reset_confirmation = true;
        } else if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
            dialog.summary_focused = !dialog.summary_focused;
        } else if dialog.summary_focused {
            input::multiline(&mut dialog.summary, &mut dialog.cursor, key);
        } else {
            let count =
                pending_comments(&self.review).count() + pending_replies(&self.review).count();
            match key.code {
                KeyCode::Char('j') | KeyCode::Down => {
                    dialog.selection = (dialog.selection + 1).min(count.saturating_sub(1));
                    dialog.scroll = 0;
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    dialog.selection = dialog.selection.saturating_sub(1);
                    dialog.scroll = 0;
                }
                KeyCode::PageDown => {
                    dialog.scroll = dialog
                        .scroll
                        .saturating_add(page / 2)
                        .min(u16::MAX as usize)
                }
                KeyCode::PageUp => dialog.scroll = dialog.scroll.saturating_sub(page / 2),
                _ => {}
            }
        }
        self.submission_dialog = Some(dialog);
    }
}

pub(crate) fn draw_submission(frame: &mut Frame, app: &App, dialog: &SubmissionDialog) {
    let area = panes(frame.area(), app).comments;
    if area.is_empty() {
        return;
    }
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Submit GitLab review ")
        .border_style(Style::default().fg(if app.focus == Focus::Comments {
            Color::Cyan
        } else {
            Color::DarkGray
        }));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let comments: Vec<_> = pending_comments(&app.review)
        .map(|comment| {
            (
                format!(
                    "{}:{}-{} ({}){}",
                    display_path(&comment.anchor.path),
                    comment.anchor.start_line,
                    comment.anchor.end_line,
                    comment.anchor.side.label(),
                    if comment.anchor.is_outdated(&app.review.changes) {
                        " · outdated"
                    } else {
                        ""
                    }
                ),
                comment.anchor.excerpt.as_str(),
                comment.body.as_str(),
            )
        })
        .chain(pending_replies(&app.review).map(|reply| {
            let discussion = app.review.gitlab.as_ref().and_then(|gitlab| {
                gitlab
                    .discussions
                    .iter()
                    .find(|discussion| discussion.id == reply.discussion_id)
            });
            let location = discussion
                .and_then(|discussion| discussion.path())
                .unwrap_or("MR discussion");
            (
                format!(
                    "Reply to {} · {}",
                    display_path(location),
                    display_path(&reply.discussion_id)
                ),
                "",
                reply.body.as_str(),
            )
        }))
        .collect();
    let (prompt, keys) = if app.network_busy {
        (
            "GitLab operation in progress; submission checkpoints are saved.".to_owned(),
            "Waiting for GitLab…".to_owned(),
        )
    } else if dialog.reset_confirmation {
        ("An earlier write may already be visible. Confirm only after checking the MR on GitLab; retrying can duplicate feedback.".to_owned(), "y acknowledge retry · n / Esc cancel".to_owned())
    } else if let Some(preview) = &dialog.preview {
        (
            format!(
                "Publish {} comments/replies and a summary? Includes {} existing drafts from outside this session. ALL your pending MR drafts will be published.",
                preview.comment_ids.len(),
                preview.external_drafts
            ),
            "y publish all pending drafts · n / Esc edit".to_owned(),
        )
    } else {
        let retry = if app
            .review
            .gitlab
            .as_ref()
            .is_some_and(|gitlab| gitlab.publication.uncertain())
        {
            " · X acknowledge retry"
        } else {
            ""
        };
        (
            dialog.message.clone(),
            format!("Ctrl-s preview · Tab summary/comments · PgUp/PgDn read{retry} · Esc close"),
        )
    };
    let prompt = Paragraph::new(display_path(&prompt)).wrap(Wrap { trim: false });
    let keys = Paragraph::new(keys).wrap(Wrap { trim: false });
    let [identity, entries, detail, summary, message, controls] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(comments.len().clamp(1, 3) as u16),
        Constraint::Min(0),
        Constraint::Length(5),
        Constraint::Length(prompt.line_count(inner.width).max(1).min(u16::MAX as usize) as u16),
        Constraint::Length(keys.line_count(inner.width).min(u16::MAX as usize) as u16),
    ])
    .areas(inner);
    if let Some(gitlab) = &app.review.gitlab {
        frame.render_widget(
            Paragraph::new(display_path(&gitlab.merge_request.web_url)),
            identity,
        );
    }
    let selection = dialog.selection.min(comments.len().saturating_sub(1));
    let visible = tree::visible_range(comments.len(), selection, entries.height as usize);
    let items: Vec<_> = comments
        .iter()
        .skip(visible.start)
        .take(visible.len())
        .map(|(label, _, _)| ListItem::new(label.clone()))
        .collect();
    let mut state =
        ListState::default().with_selected(Some(selection.saturating_sub(visible.start)));
    frame.render_stateful_widget(
        List::new(items).highlight_style(Style::default().bg(Color::Rgb(35, 45, 60))),
        entries,
        &mut state,
    );
    if let Some((_, source, body)) = comments.get(selection) {
        let mut text: Vec<_> = source
            .lines()
            .map(|line| Line::from(display_path(line)))
            .collect();
        if !text.is_empty() {
            text.push(Line::default());
        }
        text.extend(markdown::render(body, detail.width.saturating_sub(2)).lines);
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Pending feedback ");
        let paragraph = Paragraph::new(text).wrap(Wrap { trim: false });
        let scroll = dialog
            .scroll
            .min(max_scroll(&paragraph, block.inner(detail)));
        frame.render_widget(paragraph.scroll((scroll as u16, 0)).block(block), detail);
    } else {
        frame.render_widget(
            Paragraph::new("Write a summary to submit a review."),
            detail,
        );
    }
    let mut text = dialog
        .preview
        .as_ref()
        .map(|preview| preview.summary.clone())
        .unwrap_or_else(|| dialog.summary.clone());
    if app.focus == Focus::Comments
        && dialog.preview.is_none()
        && dialog.summary_focused
        && !dialog.reset_confirmation
    {
        text.insert(dialog.cursor, '▏');
    }
    let text = if dialog.preview.is_some() || !dialog.summary_focused {
        markdown::render(&text, summary.width.saturating_sub(2))
    } else {
        Text::from(
            text.split('\n')
                .map(|line| Line::from(display_path(line)))
                .collect::<Vec<_>>(),
        )
    };
    let row = dialog.summary[..dialog.cursor]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count();
    frame.render_widget(
        Paragraph::new(text)
            .wrap(Wrap { trim: false })
            .scroll((
                row.saturating_sub(summary.height.saturating_sub(3) as usize)
                    .min(u16::MAX as usize) as u16,
                0,
            ))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Summary · Markdown ")
                    .border_style(Style::default().fg(
                        if app.focus == Focus::Comments && dialog.summary_focused {
                            Color::Cyan
                        } else {
                            Color::DarkGray
                        },
                    )),
            ),
        summary,
    );
    frame.render_widget(prompt.style(Style::default().fg(Color::Yellow)), message);
    frame.render_widget(keys.style(Style::default().fg(Color::Cyan)), controls);
}

pub enum Pick {
    Selected(usize),
    Reload,
    Cancel,
}

/// Startup pickers restore the terminal before transport commands or authentication run.
pub fn pick(title: &str, labels: &[String]) -> Result<Pick> {
    ratatui::run(|terminal| {
        let mut query = String::new();
        let mut searching = false;
        let mut selection = 0;
        loop {
            let rows: Vec<_> = labels
                .iter()
                .enumerate()
                .filter(|(_, label)| label.to_lowercase().contains(&query.to_lowercase()))
                .collect();
            selection = selection.min(rows.len().saturating_sub(1));
            let mut page = 1;
            terminal.draw(|frame| {
                let [list, search, controls] = Layout::vertical([
                    Constraint::Min(0),
                    Constraint::Length(1),
                    Constraint::Length(1),
                ])
                .areas(frame.area());
                let block = Block::default().borders(Borders::ALL).title(format!(
                    " {} · {} ",
                    display_path(title),
                    rows.len()
                ));
                page = block.inner(list).height.max(1) as usize;
                let visible = tree::visible_range(rows.len(), selection, page);
                let items: Vec<_> = rows
                    .iter()
                    .skip(visible.start)
                    .take(visible.len())
                    .map(|(_, label)| ListItem::new(display_path(label)))
                    .collect();
                let mut state = ListState::default()
                    .with_selected(Some(selection.saturating_sub(visible.start)));
                frame.render_stateful_widget(
                    List::new(items)
                        .block(block)
                        .highlight_style(Style::default().bg(Color::Rgb(35, 45, 60))),
                    list,
                    &mut state,
                );
                frame.render_widget(
                    Paragraph::new(format!(
                        "/{}{}",
                        display_path(&query),
                        if searching { "▏" } else { "" }
                    )),
                    search,
                );
                frame.render_widget(
                    Paragraph::new("j/k move · Enter select · / search · r reload · Esc cancel")
                        .style(Style::default().fg(Color::Cyan)),
                    controls,
                );
            })?;
            if !event::poll(Duration::from_millis(100))? {
                continue;
            }
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind == KeyEventKind::Release {
                continue;
            }
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                return Ok(Pick::Cancel);
            }
            if searching {
                match key.code {
                    KeyCode::Enter => searching = false,
                    KeyCode::Esc => {
                        searching = false;
                        query.clear();
                    }
                    KeyCode::Backspace => {
                        query.pop();
                        selection = 0;
                    }
                    KeyCode::Char(ch)
                        if !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        query.push(ch);
                        selection = 0;
                    }
                    _ => {}
                }
                continue;
            }
            match key.code {
                KeyCode::Char('j') | KeyCode::Down => {
                    selection = (selection + 1).min(rows.len().saturating_sub(1))
                }
                KeyCode::Char('k') | KeyCode::Up => selection = selection.saturating_sub(1),
                KeyCode::PageDown => {
                    selection = selection
                        .saturating_add(page)
                        .min(rows.len().saturating_sub(1))
                }
                KeyCode::PageUp => selection = selection.saturating_sub(page),
                KeyCode::Home | KeyCode::Char('g') => selection = 0,
                KeyCode::End | KeyCode::Char('G') => selection = rows.len().saturating_sub(1),
                KeyCode::Char('/') => searching = true,
                KeyCode::Char('r') => return Ok(Pick::Reload),
                KeyCode::Esc | KeyCode::Char('q') => return Ok(Pick::Cancel),
                KeyCode::Enter => {
                    if let Some((index, _)) = rows.get(selection) {
                        return Ok(Pick::Selected(*index));
                    }
                }
                _ => {}
            }
        }
    })
}
