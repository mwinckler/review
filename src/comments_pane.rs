use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};
use time::OffsetDateTime;

use crate::{
    app::{App, Focus},
    comments::DiffSide,
    gitlab_comments::CommentEntry,
    gitlab_review::pending_replies,
    gitlab_worker::GitLabJob,
    markdown, ui,
};

pub(crate) fn source_row(entry: CommentEntry<'_>, app: &App) -> Option<usize> {
    let target = entry.target(&app.review)?;
    if app.revisions.is_some() {
        let original = app
            .review
            .changes
            .iter()
            .find(|change| change.path == target.path)?;
        let current = app
            .diff_changes()
            .iter()
            .find(|change| change.path == target.path)?;
        let matches = match target.side {
            DiffSide::Old => original.old == current.old,
            DiffSide::New => original.new == current.new,
        };
        if !matches {
            return None;
        }
    }
    app.source_lines
        .iter()
        .rfind(|source| {
            source.location.path == target.path
                && source
                    .number(target.side)
                    .is_some_and(|line| (target.start_line..=target.end_line).contains(&line))
        })
        .map(|source| source.row)
}

pub(crate) fn aligned_threads(app: &App) -> Vec<(usize, CommentEntry<'_>)> {
    let mut threads: Vec<_> = app
        .review
        .comment_entries()
        .filter_map(|entry| source_row(entry, app).map(|row| (row, entry)))
        .collect();
    threads.sort_by_key(|(row, _)| *row);
    threads
}

impl App {
    pub(crate) fn handle_comments_key(
        &mut self,
        key: KeyEvent,
        page: usize,
        terminal_width: u16,
    ) -> bool {
        if self.thread.as_ref().is_some_and(|thread| thread.full)
            && self.handle_thread_key(key, page, terminal_width)
        {
            return true;
        }
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return false;
        }
        match key.code {
            KeyCode::Char('j' | 'n') | KeyCode::Down => self.jump_thread(true),
            KeyCode::Char('k' | 'N') | KeyCode::Up => self.jump_thread(false),
            KeyCode::Home | KeyCode::Char('g') => {
                self.thread = None;
                self.jump_thread(true);
            }
            KeyCode::End | KeyCode::Char('G') => {
                self.thread = None;
                self.jump_thread(false);
            }
            KeyCode::Enter | KeyCode::Char('o' | 'f' | ' ') => {
                if let Some(thread) = &mut self.thread {
                    thread.full = true;
                    thread.scroll = 0;
                }
            }
            KeyCode::Char('R') if self.thread.is_some() => self.reply_to_thread(),
            KeyCode::Char('e') if self.thread.is_some() => {
                let id = self.thread.as_ref().expect("selected thread").id.clone();
                self.edit_comment(&id);
            }
            KeyCode::Char('r') if self.review.gitlab.is_some() => {
                self.submission_action = Some(GitLabJob::Discussions);
            }
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') => {
                self.set_focus(Focus::Diff);
            }
            KeyCode::Right | KeyCode::Char('l') => {}
            _ => {
                return false;
            }
        }
        true
    }
}

fn fit(value: &str, width: usize) -> String {
    let span = Span::raw(value);
    if span.width() <= width {
        return value.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let mut columns = 0;
    let mut clipped: String = span
        .styled_graphemes(Style::default())
        .take_while(|grapheme| {
            columns += Span::raw(grapheme.symbol).width();
            columns < width
        })
        .map(|grapheme| grapheme.symbol)
        .collect();
    clipped.push('…');
    clipped
}

fn resolved(entry: CommentEntry<'_>) -> bool {
    match entry {
        CommentEntry::Local(_) => false,
        CommentEntry::GitLab(discussion) => discussion
            .comments()
            .find(|note| note.resolvable)
            .is_some_and(|note| note.resolved),
    }
}

pub(crate) fn summary(
    entry: CommentEntry<'_>,
    app: &App,
    width: u16,
    collapsed: bool,
    count: usize,
) -> Line<'static> {
    let (author, created, body, replies) = match entry {
        CommentEntry::Local(comment) => {
            let created = OffsetDateTime::from_unix_timestamp((comment.created_at / 1_000) as i64)
                .map(|date| {
                    format!(
                        "{:02}-{:02} {:02}:{:02}",
                        date.month() as u8,
                        date.day(),
                        date.hour(),
                        date.minute()
                    )
                })
                .unwrap_or_default();
            ("You".to_owned(), created, comment.body.as_str(), 0)
        }
        CommentEntry::GitLab(discussion) => {
            let note = discussion.comments().next().expect("visible thread");
            let created = ui::display_path(
                &note
                    .created_at
                    .as_deref()
                    .and_then(|date| date.get(5..16))
                    .unwrap_or_default()
                    .replace('T', " "),
            );
            let replies = discussion.comments().count().saturating_sub(1)
                + pending_replies(&app.review)
                    .filter(|reply| reply.discussion_id == discussion.id)
                    .count();
            (
                format!("@{}", note.author.username),
                created,
                note.body.as_str(),
                replies,
            )
        }
    };
    let icon = if resolved(entry) {
        "✓"
    } else if collapsed {
        "▸"
    } else {
        "▾"
    };
    let group = if count > 1 {
        format!("{count} threads · ")
    } else {
        String::new()
    };
    let mut text = format!(
        "{icon} {group}{}",
        fit(&ui::display_path(&author), (width as usize / 3).min(16))
    );
    if replies > 0 {
        text.push_str(&format!(" ↩{replies}"));
    }
    if !created.is_empty() && Span::raw(&text).width() + created.len() + 12 <= width as usize {
        text.push_str(&format!(" · {created}"));
    }
    text.push_str(" · ");
    let preview = body
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    text.push_str(&fit(
        &ui::display_path(&preview),
        (width as usize).saturating_sub(Span::raw(&text).width()),
    ));
    Line::raw(text)
}

pub(crate) fn draw(frame: &mut Frame, app: &App, area: Rect) {
    if area.is_empty() {
        return;
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Comments ")
        .border_style(Style::default().fg(if app.focus == Focus::Comments {
            Color::Cyan
        } else {
            Color::DarkGray
        }));
    let mut viewport = block.inner(area);
    let banner = u16::from(app.revisions.is_some()).min(viewport.height);
    viewport.y += banner;
    viewport.height -= banner;
    frame.render_widget(block, area);
    if viewport.is_empty() {
        return;
    }
    let threads = aligned_threads(app);
    if threads.is_empty() {
        frame.render_widget(
            Paragraph::new("No threads are mapped to this preview.").wrap(Wrap { trim: false }),
            viewport,
        );
        return;
    }
    let position = app
        .review
        .view
        .positions
        .get(&app.review.view.active)
        .map(|position| position.vertical)
        .unwrap_or(0);
    let bottom = position.saturating_add(viewport.height as usize);
    let mut groups = threads.chunk_by(|left, right| left.0 == right.0).peekable();
    while let Some(group) = groups.next() {
        let (row, entry) = *group
            .iter()
            .find(|(_, entry)| {
                app.thread
                    .as_ref()
                    .is_some_and(|thread| thread.id == entry.id())
            })
            .unwrap_or(&group[0]);
        if row >= bottom {
            break;
        }
        let content = match entry {
            CommentEntry::Local(comment) => markdown::render(&comment.body, viewport.width),
            CommentEntry::GitLab(_) => ui::comment_text(entry, &app.review, viewport.width),
        };
        let paragraph = Paragraph::new(content).wrap(Wrap { trim: false });
        let height = paragraph.line_count(viewport.width) + 1;
        let available = groups
            .peek()
            .map(|next| next[0].0.saturating_sub(row))
            .unwrap_or(usize::MAX);
        let collapsed = resolved(entry) || group.len() > 1 || height > available;
        let end = row
            .saturating_add(if collapsed { 1 } else { height })
            .min(bottom);
        let start = row.max(position);
        if start >= end {
            continue;
        }
        let color = if resolved(entry) {
            Color::Green
        } else {
            Color::Magenta
        };
        if row >= position {
            frame.render_widget(
                summary(entry, app, viewport.width, collapsed, group.len())
                    .style(Style::default().fg(color)),
                Rect::new(
                    viewport.x,
                    viewport.y + (row - position) as u16,
                    viewport.width,
                    1,
                ),
            );
        }
        let body_start = (row + 1).max(position);
        if !collapsed && body_start < end {
            frame.render_widget(
                paragraph.scroll(((body_start - row - 1).min(u16::MAX as usize) as u16, 0)),
                Rect::new(
                    viewport.x,
                    viewport.y + (body_start - position) as u16,
                    viewport.width,
                    (end - body_start) as u16,
                ),
            );
        }
        if app
            .thread
            .as_ref()
            .is_some_and(|thread| thread.id == entry.id())
        {
            frame.buffer_mut().set_style(
                Rect::new(
                    viewport.x,
                    viewport.y + (start - position) as u16,
                    viewport.width,
                    (end - start) as u16,
                ),
                Style::default()
                    .bg(Color::Rgb(35, 45, 60))
                    .add_modifier(Modifier::BOLD),
            );
        }
    }
}
