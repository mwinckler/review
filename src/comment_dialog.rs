use std::{cell::RefCell, collections::BTreeSet};

use anyhow::{Context, Result, ensure};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::{
    app::{App, Focus},
    comments::{Comment, CommentAnchor, DiffSide, excerpt, validate_body},
    gitlab_comments::{CommentEntry, DiscussionReply, ThreadId},
    gitlab_review::pending_replies,
    gitlab_worker::GitLabJob,
    hunks::SourceLine,
    state::Review,
    store::now_millis,
    vcs::{Change, Comparison, Repository},
    vim::VimEditor,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineTarget {
    /// The renderer's comparison path; old-side anchors use the pre-rename path.
    pub path: String,
    pub side: DiffSide,
    pub start_line: usize,
    pub end_line: usize,
}

pub struct LineSelection {
    pub path: String,
    pub side: DiffSide,
    pub anchor: usize,
    pub cursor: usize,
    pub extending: bool,
    pub(crate) row: Option<usize>,
}

enum LineMotion {
    Relative(isize),
    First,
    Last,
}

impl LineMotion {
    fn index(self, current: usize, last: usize) -> usize {
        match self {
            Self::Relative(amount) => current.saturating_add_signed(amount).min(last),
            Self::First => 0,
            Self::Last => last,
        }
    }
}

impl LineSelection {
    fn at_source(source: &SourceLine, side: DiffSide, extending: bool) -> Option<Self> {
        let (side, number) = if let Some(number) = source.number(side) {
            (side, number)
        } else if let Some(number) = source.location.new_line {
            (DiffSide::New, number)
        } else {
            (DiffSide::Old, source.location.old_line?)
        };
        Some(Self {
            path: source.location.path.clone(),
            side,
            anchor: number,
            cursor: number,
            extending,
            row: Some(source.row),
        })
    }

    fn source_index(&self, sources: &[SourceLine]) -> Option<usize> {
        let matches = |source: &SourceLine| {
            source.location.path == self.path && source.number(self.side) == Some(self.cursor)
        };
        // Keep the exact rendered occurrence, but resolve by source location after reflow.
        self.row
            .and_then(|row| sources.binary_search_by_key(&row, |source| source.row).ok())
            .filter(|&index| matches(&sources[index]))
            .or_else(|| sources.iter().position(matches))
    }

    pub(crate) fn source_row(&self, sources: &[SourceLine]) -> Option<usize> {
        self.source_index(sources).map(|index| sources[index].row)
    }

    fn move_cursor(&mut self, sources: &[SourceLine], motion: LineMotion) {
        if !self.extending {
            let Some(last) = sources.len().checked_sub(1) else {
                return;
            };
            let current = self.source_index(sources).or_else(|| {
                self.row
                    .map(|row| sources.partition_point(|source| source.row < row).min(last))
            });
            let index = motion.index(current.unwrap_or(0), last);
            if let Some(next) = Self::at_source(&sources[index], self.side, false) {
                *self = next;
            }
            return;
        }

        // An inclusive comment range stays in one file and on one side.
        let numbers = sources
            .iter()
            .filter(|source| source.location.path == self.path)
            .filter_map(|source| source.number(self.side));
        let number = match motion {
            LineMotion::First => numbers.min(),
            LineMotion::Last => numbers.max(),
            LineMotion::Relative(_) => {
                let numbers: Vec<_> = numbers.collect::<BTreeSet<_>>().into_iter().collect();
                numbers.len().checked_sub(1).map(|last| {
                    let index = numbers
                        .partition_point(|number| *number < self.cursor)
                        .min(last);
                    numbers[motion.index(index, last)]
                })
            }
        };
        if let Some(number) = number
            && self.cursor != number
        {
            self.cursor = number;
            self.row = None;
        }
    }

    pub fn target(&self) -> LineTarget {
        LineTarget {
            path: self.path.clone(),
            side: self.side,
            start_line: self.anchor.min(self.cursor),
            end_line: self.anchor.max(self.cursor),
        }
    }

    pub fn contains(&self, source: &SourceLine) -> bool {
        self.path == source.location.path
            && source.number(self.side).is_some_and(|number| {
                (self.anchor.min(self.cursor)..=self.anchor.max(self.cursor)).contains(&number)
            })
    }
}

pub enum CommentAction {
    Compose(LineTarget),
    WriteTodo(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DraftField {
    Start,
    End,
    Body,
}

pub struct CommentDraft {
    pub id: Option<String>,
    pub anchor: CommentAnchor,
    pub start: String,
    pub end: String,
    pub body: String,
    pub cursor: usize,
    pub field: DraftField,
    pub message: String,
    pub preview: bool,
    pub(crate) editor: RefCell<VimEditor>,
    source: Option<String>,
}

impl CommentDraft {
    pub fn new(repo: &Repository, review: &Review, target: &LineTarget) -> Result<Self> {
        Self::from_preview(repo, &review.comparison, &review.changes, target)
    }

    pub fn from_preview(
        repo: &Repository,
        comparison: &Comparison,
        changes: &[Change],
        target: &LineTarget,
    ) -> Result<Self> {
        let change = changes
            .iter()
            .find(|change| change.path == target.path)
            .context("Selected file is no longer in the comparison")?;
        let content = repo.content(comparison, change, target.side == DiffSide::Old)?;
        let anchor = CommentAnchor::from_content(
            comparison,
            change,
            target.side,
            target.start_line,
            target.end_line,
            &content,
        )?;
        Ok(Self {
            id: None,
            start: target.start_line.to_string(),
            end: target.end_line.to_string(),
            anchor,
            body: String::new(),
            cursor: 0,
            field: DraftField::Body,
            message: String::new(),
            preview: false,
            editor: RefCell::default(),
            source: Some(String::from_utf8(content)?),
        })
    }

    fn edit(comment: &Comment) -> Self {
        Self {
            id: Some(comment.id.clone()),
            anchor: comment.anchor.clone(),
            start: comment.anchor.start_line.to_string(),
            end: comment.anchor.end_line.to_string(),
            body: comment.body.clone(),
            cursor: comment.body.len(),
            field: DraftField::Body,
            message: String::new(),
            preview: false,
            editor: RefCell::default(),
            source: None,
        }
    }

    pub fn selected_source(&self) -> Result<String> {
        if let Some(source) = &self.source {
            excerpt(
                source,
                self.start.parse().context("Start line must be a number")?,
                self.end.parse().context("End line must be a number")?,
            )
        } else {
            Ok(self.anchor.excerpt.clone())
        }
    }

    fn save(&self, review: &mut Review) -> Result<usize> {
        if let Some(id) = &self.id {
            ensure!(
                !review.comment_submitted(id),
                "This comment has a GitLab submission receipt; add a new comment for further feedback"
            );
            let index = review
                .comments
                .iter()
                .position(|comment| &comment.id == id)
                .context("Comment was removed")?;
            review.comments[index].edit(self.body.clone())?;
            Ok(index)
        } else {
            let mut anchor = self.anchor.clone();
            anchor.excerpt = self.selected_source()?;
            anchor.start_line = self.start.parse()?;
            anchor.end_line = self.end.parse()?;
            review
                .comments
                .push(Comment::new(anchor, self.body.clone())?);
            Ok(review.comments.len() - 1)
        }
    }

    pub fn insert(&mut self, text: &str) {
        if self.field == DraftField::Body && !self.preview {
            self.editor
                .get_mut()
                .paste(&mut self.body, &mut self.cursor, text);
        }
    }

    fn input(&mut self, key: KeyEvent) {
        if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) && self.source.is_some() {
            self.editor.get_mut().reset_pending();
            self.field = match (self.field, key.code == KeyCode::Tab) {
                (DraftField::Start, true) | (DraftField::Body, false) => DraftField::End,
                (DraftField::End, true) | (DraftField::Start, false) => DraftField::Body,
                _ => DraftField::Start,
            };
            return;
        }
        if self.field != DraftField::Body {
            let value = if self.field == DraftField::Start {
                &mut self.start
            } else {
                &mut self.end
            };
            match key.code {
                KeyCode::Char(ch @ '0'..='9') => value.push(ch),
                KeyCode::Backspace | KeyCode::Delete => {
                    value.pop();
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    value.clear()
                }
                _ => {}
            }
            return;
        }
        self.editor
            .get_mut()
            .input(&mut self.body, &mut self.cursor, key);
    }
}

#[derive(Clone, Copy)]
pub enum CommentConfirmation {
    Delete,
    Todo,
}

#[derive(Default)]
pub struct CommentList {
    pub selection: usize,
    pub confirming: Option<CommentConfirmation>,
}

pub enum CommentDialog {
    Compose(Box<CommentDraft>),
    Browse(CommentList),
    Reply(Box<ReplyDraft>),
}

impl CommentDialog {
    pub(crate) fn is_editor(&self) -> bool {
        matches!(self, Self::Compose(_) | Self::Reply(_))
    }

    pub(crate) fn accepts_text(&self) -> bool {
        match self {
            Self::Compose(draft) => !draft.preview,
            Self::Reply(draft) => !draft.preview,
            Self::Browse(_) => false,
        }
    }
}

pub struct ReplyDraft {
    pub id: Option<String>,
    pub discussion_id: String,
    pub body: String,
    pub cursor: usize,
    pub preview: bool,
    pub message: String,
    pub(crate) editor: RefCell<VimEditor>,
}

impl ReplyDraft {
    pub(crate) fn new(review: &Review, thread: &ThreadId) -> Result<Self> {
        let Some(CommentEntry::GitLab(discussion)) = thread.entry(review) else {
            anyhow::bail!("Publish the local comment with P to start a GitLab thread");
        };
        let reply = pending_replies(review).find(|reply| reply.discussion_id == discussion.id);
        ensure!(
            reply.is_none_or(|reply| !review.comment_submitted(&reply.id)),
            "This reply has a GitLab submission receipt; P rechecks the pending review"
        );
        let body = reply.map(|reply| reply.body.clone()).unwrap_or_default();
        Ok(Self {
            id: reply.map(|reply| reply.id.clone()),
            discussion_id: discussion.id.clone(),
            cursor: body.len(),
            body,
            preview: false,
            message: String::new(),
            editor: RefCell::default(),
        })
    }

    pub fn insert(&mut self, value: &str) {
        if !self.preview {
            self.editor
                .get_mut()
                .paste(&mut self.body, &mut self.cursor, value);
        }
    }

    fn save(&self, review: &mut Review) -> Result<()> {
        validate_body(&self.body)?;
        ensure!(
            self.id
                .as_ref()
                .is_none_or(|id| !review.comment_submitted(id)),
            "The reply is already staged; P rechecks its receipt"
        );
        let gitlab = review.gitlab.as_mut().context("Open a GitLab MR first")?;
        ensure!(
            gitlab
                .discussions
                .iter()
                .any(|discussion| discussion.id == self.discussion_id),
            "Thread was removed; reload comments"
        );
        if let Some(id) = &self.id {
            let reply = gitlab
                .replies
                .iter_mut()
                .find(|reply| &reply.id == id)
                .context("Reply was removed")?;
            reply.body.clone_from(&self.body);
            reply.updated_at = now_millis()?;
        } else {
            gitlab.replies.push(DiscussionReply::new(
                self.discussion_id.clone(),
                self.body.clone(),
            )?);
        }
        Ok(())
    }
}

impl App {
    fn comment_draft_open(&self) -> bool {
        self.comment_dialog
            .as_ref()
            .is_some_and(CommentDialog::is_editor)
            || self.submission_dialog.is_some()
    }

    pub fn comment_editor_active(&self) -> bool {
        self.focus == Focus::Comments && self.comment_draft_open()
    }

    pub fn comments_overview_open(&self) -> bool {
        matches!(self.comment_dialog, Some(CommentDialog::Browse(_)))
    }

    pub(crate) fn focus_comments(&mut self) {
        self.review.view.comments_visible = true;
        self.set_focus(Focus::Comments);
        self.dirty = true;
    }

    pub(crate) fn resume_comment_draft(&mut self) -> bool {
        if self.comment_draft_open() {
            self.focus_comments();
            self.message = "Finish or discard the current draft before switching comments.".into();
            return true;
        }
        false
    }

    pub fn compose_comment(&mut self, draft: CommentDraft) {
        if self.resume_comment_draft() {
            return;
        }
        self.focus_comments();
        self.comment_dialog = Some(CommentDialog::Compose(Box::new(draft)));
    }

    pub(crate) fn open_comments_overview(&mut self) {
        if self.resume_comment_draft() {
            return;
        }
        let selection = self
            .thread
            .as_ref()
            .and_then(|thread| {
                self.review
                    .comment_entries()
                    .position(|entry| entry.id() == thread.id)
            })
            .unwrap_or(0);
        self.comment_dialog = Some(CommentDialog::Browse(CommentList {
            selection,
            ..Default::default()
        }));
    }

    pub(crate) fn edit_comment(&mut self, id: &ThreadId) {
        if self.resume_comment_draft() {
            return;
        }
        let Some(comment) = id
            .entry(&self.review)
            .and_then(|entry| entry.local(&self.review))
        else {
            self.message = "Select an unpublished local comment to edit.".into();
            return;
        };
        if self.review.comment_submitted(&comment.id) {
            self.message = "This comment has a GitLab submission receipt; add a new comment for further feedback.".into();
            return;
        }
        let draft = CommentDraft::edit(comment);
        self.open_thread(id.clone(), true);
        self.compose_comment(draft);
    }

    pub(crate) fn comment_target(&self) -> Option<LineTarget> {
        if let Some(selection) = &self.line_selection {
            return Some(selection.target());
        }
        let location = self.editor_location()?;
        let position = self
            .review
            .view
            .positions
            .get(&self.review.view.active)
            .map(|position| position.vertical)
            .unwrap_or(0);
        let source = if self.selected_hunk().is_some() {
            self.source_lines
                .iter()
                .find(|source| {
                    source.location.path == location.path
                        && source.location.new_line == location.new_line
                        && location.new_line.is_some()
                })
                .or_else(|| {
                    self.source_lines.iter().find(|source| {
                        source.location.path == location.path
                            && source.location.old_line == location.old_line
                            && location.old_line.is_some()
                    })
                })
        } else if self.focus == Focus::Diff {
            self.source_lines
                .iter()
                .find(|line| line.row >= position)
                .or_else(|| self.source_lines.last())
        } else {
            None
        };
        let location = source.map(|source| &source.location).unwrap_or(&location);
        let (side, line) = if let Some(line) = location.new_line {
            (DiffSide::New, line)
        } else {
            (DiffSide::Old, location.old_line?)
        };
        Some(LineTarget {
            path: location.path.clone(),
            side,
            start_line: line,
            end_line: line,
        })
    }

    pub(crate) fn start_line_selection(&mut self, extending: bool, page: usize, lines: usize) {
        if self.rendering {
            self.message = "Wait for the diff to finish rendering".into();
            return;
        }
        let Some(target) = self.comment_target() else {
            self.message = "Select a file or mapped source line".into();
            return;
        };
        let position = self.position().vertical;
        let source = self
            .source_lines
            .iter()
            .find(|line| {
                line.location.path == target.path
                    && line.number(target.side) == Some(target.start_line)
                    && (position..position + page).contains(&line.row)
            })
            .or_else(|| self.source_lines.iter().find(|line| line.row >= position))
            .or_else(|| self.source_lines.last());
        let Some(selection) =
            source.and_then(|source| LineSelection::at_source(source, target.side, extending))
        else {
            self.message = "Use c and the line-range fields with this renderer".into();
            return;
        };
        self.line_selection = Some(selection);
        self.focus = Focus::Diff;
        self.keep_line_selection_visible(page, lines);
    }

    pub fn keep_line_selection_visible(&mut self, page: usize, lines: usize) {
        let Some(selection) = &mut self.line_selection else {
            return;
        };
        if let Some(row) = selection.source_row(&self.source_lines) {
            selection.row = Some(row);
            let position = self.position();
            if row < position.vertical {
                position.vertical = row;
            } else if row >= position.vertical + page {
                position.vertical = row.saturating_sub(page.saturating_sub(1));
            }
            position.vertical = position.vertical.min(lines.saturating_sub(page));
        }
    }

    pub(crate) fn handle_line_selection(
        &mut self,
        key: KeyEvent,
        page: usize,
        lines: usize,
    ) -> bool {
        let Some(selection) = &mut self.line_selection else {
            return false;
        };
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let amount = match (key.code, control) {
            (KeyCode::Char('j') | KeyCode::Down, false) => Some(1),
            (KeyCode::Char('k') | KeyCode::Up, false) => Some(-1),
            (KeyCode::PageDown, _) | (KeyCode::Char('f'), true) => Some(page as isize),
            (KeyCode::PageUp, _) | (KeyCode::Char('b'), true) => Some(-(page as isize)),
            (KeyCode::Char('d'), true) => Some((page / 2).max(1) as isize),
            (KeyCode::Char('u'), true) => Some(-((page / 2).max(1) as isize)),
            _ => None,
        };
        if let Some(amount) = amount {
            selection.move_cursor(&self.source_lines, LineMotion::Relative(amount));
        } else {
            if key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
            {
                return false;
            }
            match key.code {
                KeyCode::Char('g') | KeyCode::Home => {
                    selection.move_cursor(&self.source_lines, LineMotion::First);
                }
                KeyCode::Char('G') | KeyCode::End => {
                    selection.move_cursor(&self.source_lines, LineMotion::Last);
                }
                KeyCode::Char('h' | 'l') | KeyCode::Left | KeyCode::Right => {
                    let side = if matches!(key.code, KeyCode::Char('h') | KeyCode::Left) {
                        DiffSide::Old
                    } else {
                        DiffSide::New
                    };
                    let row = selection.source_row(&self.source_lines).unwrap_or(0);
                    if let Some(source) = self
                        .source_lines
                        .iter()
                        .filter(|line| {
                            line.location.path == selection.path && line.number(side).is_some()
                        })
                        .min_by_key(|line| line.row.abs_diff(row))
                        && let Some(next) =
                            LineSelection::at_source(source, side, selection.extending)
                    {
                        *selection = next;
                    }
                }
                KeyCode::Char('v') => selection.extending = !selection.extending,
                KeyCode::Enter => selection.extending = false,
                KeyCode::Esc => {
                    self.line_selection = None;
                    return true;
                }
                KeyCode::Char('c') => {
                    self.comment_action = Some(CommentAction::Compose(selection.target()));
                    return true;
                }
                _ => return false,
            }
        }
        if !selection.extending {
            selection.anchor = selection.cursor;
        }
        self.keep_line_selection_visible(page, lines);
        true
    }

    pub(crate) fn handle_comment_dialog(&mut self, key: KeyEvent, page: usize) {
        let Some(mut dialog) = self.comment_dialog.take() else {
            return;
        };
        match &mut dialog {
            CommentDialog::Compose(draft) => {
                if key.code == KeyCode::Char('q') && key.modifiers.contains(KeyModifiers::CONTROL)
                    || key.code == KeyCode::Esc && !draft.preview && draft.field != DraftField::Body
                {
                    return;
                }
                if key.code == KeyCode::Esc && draft.preview {
                    draft.preview = false;
                } else if key.code == KeyCode::Char('s')
                    && key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    match draft.save(&mut self.review) {
                        Ok(index) => {
                            let id = ThreadId::Local(self.review.comments[index].id.clone());
                            self.dirty = true;
                            self.comments_changed = true;
                            self.open_thread(id, true);
                            self.message = "Comment saved locally.".into();
                            return;
                        }
                        Err(error) => draft.message = format!("{error:#}"),
                    }
                } else if key.code == KeyCode::Char('p')
                    && key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    draft.preview = !draft.preview;
                    draft.editor.get_mut().reset_pending();
                } else if !draft.preview {
                    draft.input(key);
                }
            }
            CommentDialog::Reply(draft) => {
                if key.code == KeyCode::Char('q') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    return;
                }
                if key.code == KeyCode::Esc && draft.preview {
                    draft.preview = false;
                } else if key.code == KeyCode::Char('s')
                    && key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    match draft.save(&mut self.review) {
                        Ok(()) => {
                            self.comments_changed = true;
                            self.dirty = true;
                            if let Some(thread) = &mut self.thread {
                                thread.full = true;
                                thread.scroll = usize::MAX;
                            }
                            self.message = "Reply saved locally · P preview and publish".into();
                            return;
                        }
                        Err(error) => draft.message = format!("{error:#}"),
                    }
                } else if key.code == KeyCode::Char('p')
                    && key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    draft.preview = !draft.preview;
                    draft.editor.get_mut().reset_pending();
                } else if !draft.preview {
                    draft
                        .editor
                        .get_mut()
                        .input(&mut draft.body, &mut draft.cursor, key);
                }
            }
            CommentDialog::Browse(list) => {
                let count = self.review.comment_entries().count();
                list.selection = list.selection.min(count.saturating_sub(1));
                let selected = self.review.comment_entries().nth(list.selection);
                let local = selected.and_then(|entry| entry.local(&self.review));
                if let Some(confirmation) = list.confirming {
                    match key.code {
                        KeyCode::Char('y') => {
                            if let Some(comment) = &local {
                                match confirmation {
                                    CommentConfirmation::Delete => {
                                        let id = comment.id.clone();
                                        self.review.comments.retain(|local| local.id != id);
                                        list.selection = list.selection.min(
                                            self.review.comment_entries().count().saturating_sub(1),
                                        );
                                        self.dirty = true;
                                        self.comments_changed = true;
                                        self.message = "Local comment deleted".into();
                                    }
                                    CommentConfirmation::Todo => {
                                        self.comment_action =
                                            Some(CommentAction::WriteTodo(comment.id.clone()))
                                    }
                                }
                            }
                            list.confirming = None;
                        }
                        KeyCode::Char('n') | KeyCode::Esc => list.confirming = None,
                        _ => {}
                    }
                } else {
                    if key.modifiers.contains(KeyModifiers::CONTROL) {
                        match key.code {
                            KeyCode::Char('d' | 'f') => {
                                list.selection = list
                                    .selection
                                    .saturating_add((page / 2).max(1))
                                    .min(count.saturating_sub(1));
                            }
                            KeyCode::Char('u' | 'b') => {
                                list.selection = list.selection.saturating_sub((page / 2).max(1));
                            }
                            _ => {}
                        }
                        self.comment_dialog = Some(dialog);
                        return;
                    }
                    match key.code {
                        KeyCode::Enter | KeyCode::Char('o' | 'f' | 'R') => {
                            if let Some(entry) = selected {
                                let id = entry.id();
                                self.open_thread(id, true);
                                if key.code == KeyCode::Char('R') {
                                    self.reply_to_thread();
                                }
                                return;
                            }
                        }
                        KeyCode::Esc | KeyCode::Char('C') => {
                            return;
                        }
                        KeyCode::Char('j') | KeyCode::Down => {
                            list.selection = (list.selection + 1).min(count.saturating_sub(1));
                        }
                        KeyCode::Char('k') | KeyCode::Up => {
                            list.selection = list.selection.saturating_sub(1);
                        }
                        KeyCode::Home | KeyCode::Char('g') => {
                            list.selection = 0;
                        }
                        KeyCode::End | KeyCode::Char('G') => {
                            list.selection = count.saturating_sub(1);
                        }
                        KeyCode::PageDown => {
                            list.selection = list
                                .selection
                                .saturating_add((page / 2).max(1))
                                .min(count.saturating_sub(1));
                        }
                        KeyCode::PageUp => {
                            list.selection = list.selection.saturating_sub((page / 2).max(1))
                        }
                        KeyCode::Char('d') | KeyCode::Delete if local.is_some() => {
                            if self
                                .review
                                .comment_submitted(&local.as_ref().expect("local comment").id)
                            {
                                self.message =
                                    "Submitted comments are retained with their GitLab receipts"
                                        .into();
                            } else {
                                list.confirming = Some(CommentConfirmation::Delete)
                            }
                        }
                        KeyCode::Char('t') if local.is_some() => {
                            list.confirming = Some(CommentConfirmation::Todo)
                        }
                        KeyCode::Char('e') => {
                            if let Some(entry) = selected {
                                let id = entry.id();
                                self.edit_comment(&id);
                                if self.comment_dialog.is_none() {
                                    self.comment_dialog = Some(dialog);
                                }
                                return;
                            }
                        }
                        KeyCode::Char('r') if self.review.gitlab.is_some() => {
                            self.submission_action = Some(GitLabJob::Discussions);
                        }
                        _ => {}
                    }
                }
            }
        }
        self.comment_dialog = Some(dialog);
    }
}
