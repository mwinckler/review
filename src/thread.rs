//! This module manages source-aligned conversations and their expanded reader.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use crate::{
    app::{App, Focus},
    comment_dialog::{CommentDialog, LineSelection, ReplyDraft},
    gitlab_comments::{CommentEntry, ThreadId},
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
        crate::comments_pane::source_row(self.id.entry(&app.review)?, app)
    }
}

impl App {
    pub(crate) fn open_thread(&mut self, id: ThreadId, full: bool) {
        if self.resume_comment_draft() {
            return;
        }
        let Some(entry) = id.entry(&self.review) else {
            return;
        };
        let target = entry.target(&self.review);
        if let Some(target) = &target {
            if self.review.view.active != target.path
                && !(self.review.view.active.ends_with('/')
                    && self
                        .preview_changes()
                        .any(|change| change.path == target.path && !self.review.is_viewed(change)))
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
            self.directory_cursor = None;
            self.line_selection = Some(LineSelection {
                path: target.path.clone(),
                side: target.side,
                anchor: target.start_line,
                cursor: target.end_line,
                extending: false,
                row: None,
            });
        }
        self.focus_comments();
        self.thread = Some(ThreadView {
            id,
            scroll: 0,
            full: full || target.is_none(),
            confirming_delete: false,
        });
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
            self.open_thread(id, true);
        } else {
            self.message =
                "No thread on this line · ]c / [c next/previous thread · C all comments".into();
        }
    }

    pub(crate) fn jump_thread(&mut self, forward: bool) {
        let ids: Vec<_> = if self.review.view.comments_visible {
            crate::comments_pane::aligned_threads(self)
                .into_iter()
                .map(|(_, entry)| entry.id())
                .collect()
        } else {
            self.review
                .comment_entries()
                .filter(|entry| {
                    entry.path(&self.review).is_some_and(|path| {
                        path == self.review.view.active
                            || (self.review.view.active.ends_with('/')
                                && path.starts_with(&self.review.view.active))
                    })
                })
                .map(CommentEntry::id)
                .collect()
        };
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
        let full =
            self.focus != Focus::Comments || self.thread.as_ref().is_some_and(|thread| thread.full);
        self.open_thread(ids[index].clone(), full);
    }

    pub fn keep_thread_visible(&mut self, page: usize, lines: usize) {
        if self.comment_editor_active() {
            self.keep_line_selection_visible(page, lines);
            return;
        }
        if self.focus == Focus::Comments
            && self
                .thread
                .as_ref()
                .is_none_or(|thread| !thread.full && thread.last_source_row(self).is_none())
        {
            let position = self.position().vertical;
            let id = crate::comments_pane::aligned_threads(self)
                .into_iter()
                .min_by_key(|(row, _)| row.abs_diff(position))
                .map(|(_, entry)| entry.id());
            if let Some(id) = id {
                self.open_thread(id, false);
            }
        }
        if let Some(row) = self
            .thread
            .as_ref()
            .and_then(|thread| thread.last_source_row(self))
        {
            let space = page.max(1);
            let position = self.position();
            if row < position.vertical || row >= position.vertical + space {
                position.vertical = row
                    .saturating_sub((page / 4).min(space - 1))
                    .min(lines.saturating_sub(page));
            }
        }
    }

    pub(crate) fn reply_to_thread(&mut self) {
        if self.resume_comment_draft() {
            return;
        }
        let thread = self.thread.as_ref().expect("open thread");
        match ReplyDraft::new(&self.review, &thread.id) {
            Ok(draft) => {
                self.focus_comments();
                self.thread.as_mut().expect("open thread").full = true;
                self.comment_dialog = Some(CommentDialog::Reply(Box::new(draft)));
            }
            Err(error) => self.message = format!("Reply: {error:#}"),
        }
    }

    pub(crate) fn handle_thread_key(
        &mut self,
        key: KeyEvent,
        page: usize,
        terminal_width: u16,
    ) -> bool {
        let thread = self.thread.as_ref().expect("open thread");
        let Some(entry) = thread.id.entry(&self.review) else {
            self.thread = None;
            self.message = "Thread is no longer available · C comments".into();
            return true;
        };
        let height = page
            .saturating_add(
                4 + usize::from(self.review.gitlab.is_some())
                    + usize::from(self.revisions.is_some()),
            )
            .min(u16::MAX as usize) as u16;
        let area = ui::panes(Rect::new(0, 0, terminal_width, height), self).comments;
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
            return true;
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match (key.code, control) {
            (KeyCode::Esc | KeyCode::Enter | KeyCode::Char('o' | ' '), false) => {
                thread.full = false;
                thread.scroll = 0;
            }
            (KeyCode::Char('C'), false) => {
                self.open_comments_overview();
            }
            (KeyCode::Char('e'), false) => {
                let id = thread.id.clone();
                self.edit_comment(&id);
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
                self.open_submission();
            }
            (KeyCode::Char('n'), false) if key.modifiers == KeyModifiers::ALT => {
                self.jump_thread(false)
            }
            (KeyCode::Char('n' | 'j') | KeyCode::Down, false) => self.jump_thread(true),
            (KeyCode::Char('N' | 'k') | KeyCode::Up, false) => self.jump_thread(false),
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
            _ => {
                return false;
            }
        }
        true
    }
}

pub(crate) fn draw(frame: &mut Frame, app: &App, area: Rect, footer: Rect) {
    let Some(thread) = &app.thread else {
        return;
    };
    if !thread.full || area.is_empty() {
        return;
    }
    let Some(entry) = thread.id.entry(&app.review) else {
        return;
    };
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(
            " Thread · {} ",
            ui::comment_heading(entry, &app.review)
        ))
        .border_style(Style::default().fg(if app.focus == Focus::Comments {
            Color::Magenta
        } else {
            Color::DarkGray
        }));
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
    if app.focus != Focus::Comments || app.comment_editor_active() {
        return;
    }
    let keys = if app.network_busy {
        "Waiting for GitLab… · saved feedback and submission checkpoints retained".to_owned()
    } else if thread.confirming_delete {
        "Delete pending reply? y delete · n / Esc cancel".into()
    } else if !app.message.is_empty() {
        ui::display_path(&app.message)
    } else {
        "Use R to reply, c to comment, j/k for threads, or Esc to collapse.".into()
    };
    frame.render_widget(
        Paragraph::new(keys).style(Style::default().fg(Color::Cyan)),
        footer,
    );
}
