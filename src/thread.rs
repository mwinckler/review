//! Source-aligned conversation overlays, independent of renderer row coordinates.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use crate::{
    app::{App, Focus},
    comment_dialog::{CommentDialog, CommentList, LineSelection, ReplyDraft},
    gitlab_comments::{CommentEntry, ThreadId},
    gitlab_ui::SubmissionDialog,
    gitlab_worker::GitLabJob,
    ui,
};

pub struct ThreadView {
    pub id: ThreadId,
    pub scroll: usize,
    pub full: bool,
    pub confirming_delete: bool,
}

impl ThreadView {
    fn last_source_row(&self, app: &App) -> Option<usize> {
        let target = self.id.entry(&app.review)?.target(&app.review)?;
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
}

impl App {
    pub(crate) fn open_thread(&mut self, id: ThreadId, full: bool) {
        let Some(entry) = id.entry(&self.review) else {
            return;
        };
        let target = entry.target(&self.review);
        if let Some(target) = &target {
            if self.review.view.active != target.path
                && !(self.review.view.active.ends_with('/')
                    && target.path.starts_with(&self.review.view.active))
            {
                self.review.view.selected.clone_from(&target.path);
                self.review.view.filter.clear();
                self.review.view.unviewed_only = false;
                self.review
                    .view
                    .collapsed
                    .retain(|parent| !target.path.starts_with(parent));
                self.rebuild();
            }
            self.line_selection = Some(LineSelection {
                path: target.path.clone(),
                side: target.side,
                anchor: target.start_line,
                cursor: target.end_line,
                extending: false,
            });
        }
        self.focus = Focus::Diff;
        self.thread = Some(ThreadView {
            id,
            scroll: 0,
            full: full || target.is_none(),
            confirming_delete: false,
        });
        self.dirty = true;
    }

    pub(crate) fn expand_comment(&mut self) {
        if self.rendering {
            self.message = "Wait for the diff to finish rendering".into();
            return;
        }
        let target = self.comment_target();
        let id =
            self.review
                .comment_entries()
                .find(|entry| {
                    entry.target(&self.review).zip(target.as_ref()).is_some_and(
                        |(anchor, target)| {
                            anchor.path == target.path
                                && anchor.side == target.side
                                && anchor.start_line <= target.end_line
                                && anchor.end_line >= target.start_line
                        },
                    )
                })
                .map(CommentEntry::id);
        if let Some(id) = id {
            self.open_thread(id, false);
        } else {
            self.message =
                "No thread on this line · ]c / [c next/previous thread · C all comments".into();
        }
    }

    pub(crate) fn jump_thread(&mut self, forward: bool) {
        let ids: Vec<_> = self
            .review
            .comment_entries()
            .filter(|entry| {
                entry.path(&self.review).is_some_and(|path| {
                    path == self.review.view.active
                        || (self.review.view.active.ends_with('/')
                            && path.starts_with(&self.review.view.active))
                })
            })
            .map(CommentEntry::id)
            .collect();
        if ids.is_empty() {
            self.message = "No threads in this preview · C all comments".into();
            return;
        }
        let current = self
            .thread
            .as_ref()
            .and_then(|thread| ids.iter().position(|id| id == &thread.id));
        let index = current
            .map(|index| {
                if forward {
                    (index + 1) % ids.len()
                } else {
                    (index + ids.len() - 1) % ids.len()
                }
            })
            .unwrap_or(if forward { 0 } else { ids.len() - 1 });
        self.open_thread(ids[index].clone(), false);
    }

    pub fn keep_thread_visible(&mut self, page: usize) {
        if let Some(row) = self
            .thread
            .as_ref()
            .and_then(|thread| thread.last_source_row(self))
        {
            let space = page.saturating_sub(8).max(1);
            let position = self.position();
            if row < position.vertical || row >= position.vertical + space {
                position.vertical = row.saturating_sub((page / 4).min(space - 1));
            }
        }
    }

    pub(crate) fn reply_to_thread(&mut self) {
        let thread = self.thread.as_ref().expect("open thread");
        match ReplyDraft::new(&self.review, &thread.id) {
            Ok(draft) => self.comment_dialog = Some(CommentDialog::Reply(draft)),
            Err(error) => self.message = format!("Reply: {error:#}"),
        }
    }

    pub(crate) fn handle_thread_key(&mut self, key: KeyEvent, page: usize, terminal_width: u16) {
        let thread = self.thread.as_ref().expect("open thread");
        let Some(entry) = thread.id.entry(&self.review) else {
            self.thread = None;
            self.message = "Thread is no longer available · C comments".into();
            return;
        };
        let height = page
            .saturating_add(4 + usize::from(self.review.gitlab.is_some()))
            .min(u16::MAX as usize) as u16;
        let diff = ui::panes(Rect::new(0, 0, terminal_width, height), self).diff;
        let area = thread_area(diff, self);
        let inner = Block::default().borders(Borders::ALL).inner(area);
        let paragraph = Paragraph::new(ui::comment_text(entry, &self.review, inner.width))
            .wrap(Wrap { trim: false });
        let max_scroll = ui::max_scroll(&paragraph, inner);
        let thread = self.thread.as_mut().expect("open thread");
        thread.id = entry.id();
        thread.scroll = thread.scroll.min(max_scroll);
        if thread.confirming_delete {
            match key.code {
                KeyCode::Char('y') => {
                    if let ThreadId::GitLab(id) = &thread.id {
                        let gitlab = self.review.gitlab.as_mut().expect("GitLab thread");
                        gitlab.replies.retain(|reply| {
                            &reply.discussion_id != id
                                || gitlab.publication.comments.contains_key(&reply.id)
                        });
                        self.comments_changed = true;
                        self.dirty = true;
                    }
                    thread.confirming_delete = false;
                }
                KeyCode::Char('n') | KeyCode::Esc => thread.confirming_delete = false,
                _ => {}
            }
            return;
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match (key.code, control) {
            (KeyCode::Esc | KeyCode::Char('q' | 'o'), false) => self.thread = None,
            (KeyCode::Char('C'), false) => {
                let selection = self
                    .review
                    .comment_entries()
                    .position(|entry| entry.id() == thread.id)
                    .unwrap_or(0);
                self.thread = None;
                self.comment_dialog = Some(CommentDialog::Browse(CommentList {
                    selection,
                    ..Default::default()
                }));
            }
            (KeyCode::Char('f'), false) => {
                thread.full = !thread.full;
                thread.scroll = 0;
            }
            (KeyCode::Char('R'), false) => self.reply_to_thread(),
            (KeyCode::Char('r'), false) => {
                self.submission_action = self.review.gitlab.as_ref().map(|_| GitLabJob::Discussions)
            }
            (KeyCode::Char('P'), false) if self.review.gitlab.is_some() => {
                self.submission_dialog = Some(SubmissionDialog::new(&self.review))
            }
            (KeyCode::Tab | KeyCode::Char('n'), false) => self.jump_thread(true),
            (KeyCode::BackTab | KeyCode::Char('N'), false) => self.jump_thread(false),
            (KeyCode::Char('j') | KeyCode::Down, false) => {
                thread.scroll = thread.scroll.saturating_add(1).min(max_scroll)
            }
            (KeyCode::Char('k') | KeyCode::Up, false) => {
                thread.scroll = thread.scroll.saturating_sub(1)
            }
            (KeyCode::PageDown, _) | (KeyCode::Char('d' | 'f'), true) => {
                thread.scroll = thread
                    .scroll
                    .saturating_add((page / 2).max(1))
                    .min(max_scroll)
            }
            (KeyCode::PageUp, _) | (KeyCode::Char('u' | 'b'), true) => {
                thread.scroll = thread.scroll.saturating_sub((page / 2).max(1))
            }
            (KeyCode::Home | KeyCode::Char('g'), false) => thread.scroll = 0,
            (KeyCode::End | KeyCode::Char('G'), false) => thread.scroll = max_scroll,
            (KeyCode::Char('d') | KeyCode::Delete, false) => {
                if let ThreadId::GitLab(id) = &thread.id
                    && self.review.gitlab.as_ref().is_some_and(|gitlab| {
                        gitlab.replies.iter().any(|reply| {
                            &reply.discussion_id == id
                                && !gitlab.publication.comments.contains_key(&reply.id)
                        })
                    })
                {
                    thread.confirming_delete = true;
                }
            }
            _ => {}
        }
    }
}

fn thread_area(diff: Rect, app: &App) -> Rect {
    let viewport = Block::default().borders(Borders::ALL).inner(diff);
    let thread = app.thread.as_ref().expect("open thread");
    if thread.full {
        return diff;
    }
    let position = app
        .review
        .view
        .positions
        .get(&app.review.view.active)
        .map(|position| position.vertical)
        .unwrap_or(0);
    let Some(offset) = thread
        .last_source_row(app)
        .and_then(|row| row.checked_sub(position))
        .filter(|row| *row < viewport.height as usize)
    else {
        return diff;
    };
    let y = viewport.y + offset as u16 + 1;
    Rect::new(viewport.x, y, viewport.width, viewport.bottom() - y)
}

pub(crate) fn draw(frame: &mut Frame, app: &App, diff: Rect, footer: Rect) {
    let Some(thread) = &app.thread else {
        return;
    };
    let Some(entry) = thread.id.entry(&app.review) else {
        return;
    };
    let area = thread_area(diff, app);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(
            " Thread · {} ",
            ui::comment_heading(entry, &app.review)
        ))
        .border_style(Style::default().fg(Color::Magenta));
    let inner = block.inner(area);
    let paragraph = Paragraph::new(ui::comment_text(entry, &app.review, inner.width))
        .wrap(Wrap { trim: false });
    let max_scroll = ui::max_scroll(&paragraph, inner);
    frame.render_widget(
        paragraph
            .scroll((thread.scroll.min(max_scroll) as u16, 0))
            .block(block),
        area,
    );
    let keys = if app.network_busy {
        "Waiting for GitLab… · saved feedback and submission checkpoints retained".to_owned()
    } else if thread.confirming_delete {
        "Delete pending reply? y delete · n / Esc cancel".into()
    } else if !app.message.is_empty() {
        ui::display_path(&app.message)
    } else if matches!(entry, CommentEntry::GitLab(_)) {
        "R reply · f expand · j/k read · n/N threads · d discard reply · r reload · P publish · o/Esc close".into()
    } else {
        "f expand · j/k read · n/N threads · C comments · o/Esc close".into()
    };
    frame.render_widget(
        Paragraph::new(keys).style(Style::default().fg(Color::Cyan)),
        footer,
    );
}
