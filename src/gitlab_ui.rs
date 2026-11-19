use std::{cell::Cell, time::Duration};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    style::{Color, Style, Stylize},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::{
    app::{App, Focus},
    gitlab::{OpenMergeRequest, ReviewStatus},
    gitlab_review::{
        ReviewDecision, SubmissionPreview, acknowledge_retry, pending_comments, pending_replies,
    },
    gitlab_worker::GitLabJob,
    input, markdown,
    state::Review,
    tree,
    ui::{display_path, max_scroll, panes},
};

pub struct SubmissionDialog {
    pub decision: ReviewDecision,
    pub summary: String,
    pub cursor: usize,
    pub selection: usize,
    pub scroll: usize,
    feedback_viewport: Cell<(usize, usize)>,
    pub summary_focused: bool,
    pub preview: Option<SubmissionPreview>,
    pub reset_confirmation: bool,
    pub message: String,
}

impl SubmissionDialog {
    pub fn new(review: &Review) -> Self {
        let attempt = review
            .gitlab
            .as_ref()
            .and_then(|gitlab| gitlab.publication.attempt.as_ref());
        let summary = attempt
            .map(|attempt| attempt.summary.clone())
            .unwrap_or_default();
        Self {
            decision: attempt.map_or(ReviewDecision::default(), |attempt| attempt.decision),
            cursor: summary.len(),
            summary,
            selection: 0,
            scroll: 0,
            feedback_viewport: Cell::default(),
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

    pub(crate) fn handle_submission_dialog(&mut self, key: crossterm::event::KeyEvent) {
        let Some(mut dialog) = self.submission_dialog.take() else {
            return;
        };
        let pending = self
            .review
            .gitlab
            .as_ref()
            .is_some_and(|gitlab| gitlab.publication.pending_decision());
        if dialog.reset_confirmation {
            match key.code {
                KeyCode::Char('y') if key.modifiers == KeyModifiers::NONE => {
                    acknowledge_retry(&mut self.review);
                    self.dirty = true;
                    self.comments_changed = true;
                    dialog.reset_confirmation = false;
                    if pending {
                        dialog.decision = ReviewDecision::Comment;
                        dialog.summary.clear();
                        dialog.cursor = 0;
                        dialog.message =
                            "Pending decision cleared. Published feedback remains on GitLab."
                                .into();
                    } else {
                        dialog.message =
                            "Retry acknowledged. Ctrl-s rechecks the server before another write."
                                .into();
                    }
                }
                KeyCode::Char('n') | KeyCode::Esc => dialog.reset_confirmation = false,
                _ => {}
            }
        } else if let Some(preview) = &dialog.preview {
            match key.code {
                KeyCode::Char('y') if key.modifiers == KeyModifiers::NONE => {
                    self.submission_action = Some(GitLabJob::Publish(Box::new(preview.clone())))
                }
                KeyCode::Char('n') | KeyCode::Esc => dialog.preview = None,
                _ => {}
            }
        } else if key.code == KeyCode::Esc {
            return;
        } else if key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.submission_action = Some(GitLabJob::Preview {
                summary: dialog.summary.clone(),
                decision: dialog.decision,
            });
        } else if key.code == KeyCode::Char('X')
            && (pending
                || self
                    .review
                    .gitlab
                    .as_ref()
                    .is_some_and(|gitlab| gitlab.publication.uncertain()))
        {
            dialog.reset_confirmation = true;
        } else if pending {
            // The receipt fixes the decision once feedback has been published.
        } else if key.code == KeyCode::Char('o') && key.modifiers.contains(KeyModifiers::CONTROL) {
            dialog.decision = dialog.decision.next();
            dialog.message.clear();
        } else if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
            dialog.summary_focused = !dialog.summary_focused;
        } else if dialog.summary_focused {
            input::multiline(&mut dialog.summary, &mut dialog.cursor, key);
        } else {
            let count =
                pending_comments(&self.review).count() + pending_replies(&self.review).count();
            let (height, max_scroll) = dialog.feedback_viewport.get();
            let step = (height / 2).max(1);
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
                    dialog.scroll = dialog.scroll.saturating_add(step).min(max_scroll);
                }
                KeyCode::PageUp => {
                    dialog.scroll = dialog.scroll.min(max_scroll).saturating_sub(step);
                }
                _ => {}
            }
        }
        self.submission_dialog = Some(dialog);
    }
}

pub(crate) fn draw_submission(frame: &mut Frame, app: &App, dialog: &SubmissionDialog) {
    const SUMMARY_MIN_HEIGHT: u16 = 7 + 2;
    const FEEDBACK_MIN_HEIGHT: u16 = 1 + 2;
    const FEEDBACK_MAX_HEIGHT: u16 = 4 + 2;

    dialog.feedback_viewport.set((0, 0));
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
    let pending = app
        .review
        .gitlab
        .as_ref()
        .is_some_and(|gitlab| gitlab.publication.pending_decision());
    let decision = dialog
        .preview
        .as_ref()
        .map_or(dialog.decision, |preview| preview.decision);
    let (decision_title, color) = match decision {
        ReviewDecision::Comment => (" Comment 🗨 ", Color::Yellow),
        ReviewDecision::Approve => (" Approve ✓ ", Color::Green),
        ReviewDecision::RequestChanges => (" Reject ✗ ", Color::Red),
    };
    let decision_style = Style::default().fg(color);
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
        if pending {
            ("Clear the pending decision from this session? Check GitLab to see whether it already took effect.".into(), "y clear pending decision · n / Esc cancel".into())
        } else {
            ("An earlier write may already be visible. Confirm only after checking the MR on GitLab; retrying can duplicate feedback.".into(), "y acknowledge retry · n / Esc cancel".into())
        }
    } else if let Some(preview) = &dialog.preview {
        if preview.approval_only {
            (
                "Feedback is already published. Approve the reviewed MR version?".into(),
                "y approve · n / Esc cancel".into(),
            )
        } else {
            (
                format!(
                    "Publish {} comments/replies and a summary? Includes {} existing drafts from outside this session. ALL your pending MR drafts will be published.",
                    preview.comment_ids.len(),
                    preview.external_drafts
                ),
                format!("y submit review ({}) · n / Esc edit", decision.label()),
            )
        }
    } else if pending {
        (
            dialog.message.clone(),
            "Ctrl-s recheck decision · X clear pending decision · Esc close".into(),
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
            format!(
                "Ctrl-s preview · Ctrl-o decision · Tab summary/comments · PgUp/PgDn read{retry} · Esc close"
            ),
        )
    };
    let message = Paragraph::new(display_path(&prompt)).wrap(Wrap { trim: false });
    let keys = Paragraph::new(keys).wrap(Wrap { trim: false });
    let line_count =
        |paragraph: &Paragraph<'_>| paragraph.line_count(inner.width).min(u16::MAX as usize) as u16;
    let message_height = if prompt.is_empty() {
        0
    } else {
        line_count(&message)
    };
    let controls_height = line_count(&keys);
    let available = inner
        .height
        .saturating_sub(message_height)
        .saturating_sub(controls_height);
    let identity_height = u16::from(available > SUMMARY_MIN_HEIGHT && app.review.gitlab.is_some());
    let feedback_space = available.saturating_sub(SUMMARY_MIN_HEIGHT + identity_height);
    let detail_min = if !comments.is_empty() && feedback_space > FEEDBACK_MIN_HEIGHT {
        FEEDBACK_MIN_HEIGHT
    } else {
        0
    };
    let entries_height = (feedback_space - detail_min).min(comments.len().min(3) as u16);
    let detail_height = if detail_min > 0 {
        (feedback_space - entries_height).min(FEEDBACK_MAX_HEIGHT)
    } else {
        0
    };
    let [identity, entries, detail, summary, status, controls] = Layout::vertical([
        Constraint::Length(identity_height),
        Constraint::Length(entries_height),
        Constraint::Length(detail_height),
        Constraint::Min(SUMMARY_MIN_HEIGHT.min(available)),
        Constraint::Length(message_height),
        Constraint::Length(controls_height),
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
    if !detail.is_empty()
        && let Some((_, source, body)) = comments.get(selection)
    {
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
        let viewport = block.inner(detail);
        let max_scroll = max_scroll(&paragraph, viewport);
        dialog
            .feedback_viewport
            .set((viewport.height as usize, max_scroll));
        let scroll = dialog.scroll.min(max_scroll);
        frame.render_widget(paragraph.scroll((scroll as u16, 0)).block(block), detail);
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
        && !pending
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
                    .title(" Summary ")
                    .title_top(
                        Line::from(Span::styled(decision_title, decision_style)).right_aligned(),
                    )
                    .border_style(decision_style),
            ),
        summary,
    );
    frame.render_widget(message.style(Style::default().fg(Color::Yellow)), status);
    frame.render_widget(keys.style(Style::default().fg(Color::Cyan)), controls);
}

pub enum Pick {
    Selected(usize),
    Reload,
    Cancel,
}

pub fn pick(title: &str, labels: Vec<String>) -> Result<Pick> {
    let entries: Vec<_> = labels
        .into_iter()
        .map(|label| PickerEntry {
            label,
            section: None,
            dimmed: false,
        })
        .collect();
    pick_entries(title, &entries)
}

pub fn pick_merge_request(requests: &[OpenMergeRequest]) -> Result<Pick> {
    let entries: Vec<_> = requests
        .iter()
        .map(|mr| PickerEntry {
            label: mr.request.label(),
            section: Some(mr.status),
            dimmed: mr.approved_by_me,
        })
        .collect();
    pick_entries("Open merge requests", &entries)
}

struct PickerEntry {
    label: String,
    section: Option<ReviewStatus>,
    dimmed: bool,
}

enum PickerRow {
    Section(ReviewStatus),
    Entry(usize),
}

fn section_heading(status: ReviewStatus) -> Span<'static> {
    let (label, color) = match status {
        ReviewStatus::Unreviewed => (" Unreviewed ", Color::Magenta),
        ReviewStatus::Approved => (" Approved ", Color::Green),
        ReviewStatus::ChangesRequested => (" Changes requested ", Color::Red),
    };
    Span::raw(label).black().bg(color).bold()
}

fn picker_controls() -> Line<'static> {
    let mut spans = Vec::new();
    for (key, action) in [
        ("j/k", " move"),
        ("Enter", " select"),
        ("/", " search"),
        ("r", " reload"),
        ("Esc", " cancel"),
    ] {
        if !spans.is_empty() {
            spans.push(Span::raw(" · ").dark_gray());
        }
        spans.push(Span::raw(key).magenta());
        spans.push(Span::raw(action).dark_gray());
    }
    Line::from(spans)
}

/// Startup pickers restore the terminal before transport commands or authentication run.
fn pick_entries(title: &str, entries: &[PickerEntry]) -> Result<Pick> {
    let mut order: Vec<_> = (0..entries.len()).collect();
    order.sort_by_key(|&index| entries[index].section);
    ratatui::run(|terminal| {
        let mut query = String::new();
        let mut searching = false;
        let mut selection = 0;
        loop {
            let filter = query.to_lowercase();
            let matches: Vec<_> = order
                .iter()
                .copied()
                .filter(|&index| entries[index].label.to_lowercase().contains(&filter))
                .collect();
            selection = selection.min(matches.len().saturating_sub(1));
            let mut page = 1;
            terminal.draw(|frame| {
                let [list, search, controls] = Layout::vertical([
                    Constraint::Min(0),
                    Constraint::Length(1),
                    Constraint::Length(1),
                ])
                .areas(frame.area());
                // Match jjui's built-in status title, shortcuts, and picker selection.
                let block = Block::default()
                    .borders(Borders::ALL)
                    .title(Line::from(vec![
                        Span::raw(format!(" {} ", display_path(title)))
                            .black()
                            .on_magenta()
                            .bold(),
                        Span::raw(format!(" · {} ", matches.len())).dark_gray(),
                    ]));
                page = block.inner(list).height.max(1) as usize;
                let mut rows = Vec::new();
                let mut section = None;
                let mut selected_row = None;
                for (position, &index) in matches.iter().enumerate() {
                    let entry = &entries[index];
                    if let Some(status) = entry.section
                        && section != Some(status)
                    {
                        section = Some(status);
                        rows.push(PickerRow::Section(status));
                    }
                    if position == selection {
                        selected_row = Some(rows.len());
                    }
                    rows.push(PickerRow::Entry(index));
                }
                let visible = tree::visible_range(rows.len(), selected_row.unwrap_or(0), page);
                let mut state =
                    ListState::default().with_selected(selected_row.map(|row| row - visible.start));
                let items = rows[visible].iter().map(|row| match *row {
                    PickerRow::Section(status) => {
                        ListItem::new(Line::from(section_heading(status)))
                    }
                    PickerRow::Entry(index) => {
                        let entry = &entries[index];
                        let item = ListItem::new(display_path(&entry.label));
                        if entry.dimmed { item.dark_gray() } else { item }
                    }
                });
                frame.render_stateful_widget(
                    List::new(items)
                        .block(block)
                        .highlight_style(Style::default().cyan().on_dark_gray().bold()),
                    list,
                    &mut state,
                );
                frame.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::raw("/").magenta(),
                        Span::raw(display_path(&query)),
                        Span::raw(if searching { "▏" } else { "" }).cyan(),
                    ])),
                    search,
                );
                frame.render_widget(Paragraph::new(picker_controls()), controls);
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
                    selection = (selection + 1).min(matches.len().saturating_sub(1))
                }
                KeyCode::Char('k') | KeyCode::Up => selection = selection.saturating_sub(1),
                KeyCode::PageDown => {
                    selection = selection
                        .saturating_add(page)
                        .min(matches.len().saturating_sub(1))
                }
                KeyCode::PageUp => selection = selection.saturating_sub(page),
                KeyCode::Home | KeyCode::Char('g') => selection = 0,
                KeyCode::End | KeyCode::Char('G') => selection = matches.len().saturating_sub(1),
                KeyCode::Char('/') => searching = true,
                KeyCode::Char('r') => return Ok(Pick::Reload),
                KeyCode::Esc | KeyCode::Char('q') => return Ok(Pick::Cancel),
                KeyCode::Enter => {
                    if let Some(&index) = matches.get(selection) {
                        return Ok(Pick::Selected(index));
                    }
                }
                _ => {}
            }
        }
    })
}
