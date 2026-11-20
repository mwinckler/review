use std::{
    collections::{BTreeSet, VecDeque},
    ops::Range,
};

use crate::{
    comment_dialog::{CommentAction, CommentDialog, LineSelection},
    comments::DiffSide,
    gitlab_ui::SubmissionDialog,
    gitlab_worker::GitLabJob,
    hunks::{Hunk, HunkLocation, SourceLine},
    renderer::FileHeading,
    restore::{Restore, RestoreState},
    revision_dialog::RevisionDialog,
    session_dialog::{SessionAction, SessionDialog},
    state::{Position, Review},
    tree::{self, Row},
    vcs::{Backend, Change, Comparison, Revision},
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{layout::Rect, text::Text};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Tree,
    Diff,
    Comments,
}

enum HunkNavigation {
    Relative(bool),
    Number(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RevisionSelection {
    pub anchor: usize,
    pub cursor: usize,
}

impl RevisionSelection {
    pub fn range(self) -> Range<usize> {
        self.anchor.min(self.cursor)..self.anchor.max(self.cursor) + 1
    }
}

pub struct Flash {
    pub query: String,
    pub matches: Vec<usize>,
    pub labels: Vec<(usize, char)>,
    previous_focus: Focus,
    previous_fullscreen: bool,
}

impl Flash {
    pub(crate) fn ranges(&self, name: &str) -> Vec<Range<usize>> {
        let query = self.query.to_lowercase();
        if query.is_empty() {
            return Vec::new();
        }
        let lowercase = name.to_lowercase();
        // Case folding can change byte lengths; retain the original UTF-8 boundaries.
        let mut boundaries = Vec::new();
        let mut offset = 0;
        for (index, ch) in name.char_indices() {
            boundaries.push((offset, index));
            offset += ch.to_lowercase().map(char::len_utf8).sum::<usize>();
        }
        boundaries.push((lowercase.len(), name.len()));
        boundaries
            .iter()
            .filter_map(|&(offset, start)| {
                if lowercase[offset..].starts_with(&query) {
                    let end =
                        boundaries.partition_point(|&(index, _)| index < offset + query.len());
                    Some(start..boundaries[end].1)
                } else {
                    None
                }
            })
            .collect()
    }
}

pub struct App {
    pub review: Review,
    pub backend: Backend,
    pub session_label: String,
    pub focus: Focus,
    pub active: Option<usize>,
    pub help: bool,
    pub quit: bool,
    pub refresh: bool,
    pub message: String,
    pub update_available: Option<semver::Version>,
    pub rows: Vec<Row>,
    pub selection: usize,
    pub searching: bool,
    pub diff_search: String,
    pub dirty: bool,
    pub bindings_help: String,
    pub default_diff_context: Option<u32>,
    pub flash: Option<Flash>,
    pub hunks: Vec<Hunk>,
    pub hunk_selection: Option<HunkLocation>,
    pub editor_requested: bool,
    pub copy_path_requested: bool,
    pub restore: Option<RestoreState>,
    pub rendering: bool,
    pub rendering_rows: Vec<Range<usize>>,
    pub render_activity: usize,
    pub render_failed: bool,
    pub session_dialog: Option<SessionDialog>,
    pub revision_dialog: Option<RevisionDialog>,
    pub session_action: Option<SessionAction>,
    pub source_lines: Vec<SourceLine>,
    pub file_headings: Vec<FileHeading>,
    pub(crate) directory_cursor: Option<usize>,
    pub line_selection: Option<LineSelection>,
    pub comment_dialog: Option<CommentDialog>,
    pub comment_action: Option<CommentAction>,
    pub comments_changed: bool,
    pub submission_dialog: Option<SubmissionDialog>,
    pub submission_action: Option<GitLabJob>,
    pub network_busy: bool,
    pub thread: Option<crate::thread::ThreadView>,
    pub revisions: Option<RevisionSelection>,
    pub revision_preview: Option<(Comparison, Vec<Change>)>,
    hunk_actions: VecDeque<HunkNavigation>,
    pending_heading: Option<(String, usize)>,
    follow_diff_selection: bool,
    horizontal_scroll_limit: u16,
    prefix: Option<(char, Option<usize>)>,
    search_original: String,
}

impl App {
    pub fn new(review: Review) -> Self {
        let focus = if review.view.comments_visible && review.view.comments_focused {
            Focus::Comments
        } else if review.view.fullscreen || review.view.diff_focused {
            Focus::Diff
        } else {
            Focus::Tree
        };
        let mut app = Self {
            review,
            backend: Backend::Git,
            session_label: String::new(),
            focus,
            active: None,
            help: false,
            quit: false,
            refresh: false,
            message: String::new(),
            update_available: None,
            rows: Vec::new(),
            selection: 0,
            searching: false,
            diff_search: String::new(),
            dirty: false,
            bindings_help: String::new(),
            default_diff_context: None,
            flash: None,
            hunks: Vec::new(),
            hunk_selection: None,
            editor_requested: false,
            copy_path_requested: false,
            restore: None,
            rendering: false,
            rendering_rows: Vec::new(),
            render_activity: 0,
            render_failed: false,
            session_dialog: None,
            revision_dialog: None,
            session_action: None,
            source_lines: Vec::new(),
            file_headings: Vec::new(),
            directory_cursor: None,
            line_selection: None,
            comment_dialog: None,
            comment_action: None,
            comments_changed: false,
            submission_dialog: None,
            submission_action: None,
            network_busy: false,
            thread: None,
            revisions: None,
            revision_preview: None,
            hunk_actions: VecDeque::new(),
            pending_heading: None,
            follow_diff_selection: true,
            horizontal_scroll_limit: 0,
            prefix: None,
            search_original: String::new(),
        };
        app.rebuild();
        app
    }

    pub fn rebuild(&mut self) {
        let selected = self.review.view.selected.as_str();
        // Keep the headings reachable when the last matching file is marked viewed.
        let retained_directory =
            (self.focus == Focus::Diff && selected.ends_with('/')).then_some(selected);
        self.rows = tree::rows_with_directory(&self.review, retained_directory);
        let selection = self
            .rows
            .iter()
            .position(|row| row.path == selected)
            .or_else(|| {
                if selected.is_empty() {
                    self.rows.iter().position(|row| row.file.is_none())
                } else {
                    None
                }
            })
            .or_else(|| self.rows.iter().position(|row| row.file.is_some()))
            .unwrap_or(0);
        self.select(selection);
    }

    fn select(&mut self, selection: usize) {
        self.selection = selection.min(self.rows.len().saturating_sub(1));
        let selected = self
            .rows
            .get(self.selection)
            .map(|row| (row.path.clone(), row.file));
        if selected
            .as_ref()
            .is_none_or(|(path, _)| self.review.view.active != *path)
        {
            self.clear_revision_filter();
            self.clear_preview();
        }
        if let Some((path, file)) = selected {
            self.review.view.selected = path.clone();
            self.active = file;
            self.review.view.active = path;
        } else {
            self.active = None;
            self.review.view.active.clear();
        }
    }

    pub fn position(&mut self) -> &mut Position {
        self.review
            .view
            .positions
            .entry(self.review.view.active.clone())
            .or_default()
    }

    pub fn update_horizontal_scroll_limit(&mut self, diff_width: usize, viewport_width: u16) {
        if viewport_width == 0 {
            self.horizontal_scroll_limit = 0;
            return;
        }
        self.horizontal_scroll_limit =
            u16::try_from(diff_width.saturating_sub(viewport_width as usize)).unwrap_or(u16::MAX);
        if let Some(position) = self.review.view.positions.get_mut(&self.review.view.active)
            && position.horizontal > self.horizontal_scroll_limit
        {
            position.horizontal = self.horizontal_scroll_limit;
            self.dirty = true;
        }
    }

    pub fn preview_changes(&self) -> impl Iterator<Item = &Change> {
        let path = &self.review.view.active;
        self.review.changes.iter().filter(move |change| {
            change.path == *path || (path.ends_with('/') && change.path.starts_with(path))
        })
    }

    pub(crate) fn file_heading_at(&self, row: usize) -> Option<&FileHeading> {
        let index = self
            .file_headings
            .partition_point(|heading| heading.row <= row);
        self.file_headings.get(index.checked_sub(1)?)
    }

    pub fn in_folder_diff(&self) -> bool {
        self.focus == Focus::Diff && self.review.view.active.ends_with('/')
    }

    pub(crate) fn is_rendering_row(&self, row: usize) -> bool {
        let index = self.rendering_rows.partition_point(|rows| rows.end <= row);
        self.rendering_rows
            .get(index)
            .is_some_and(|rows| rows.contains(&row))
    }

    pub(crate) fn diff_cursor_row(&self) -> Option<usize> {
        self.directory_cursor.or_else(|| {
            self.line_selection
                .as_ref()
                .and_then(|selection| selection.cursor_row(&self.source_lines))
        })
    }

    pub fn clear_file_headings(&mut self) {
        // Restore the cursor within the same file after diff rows have shifted.
        if let Some(row) = self.directory_cursor
            && let Some(heading) = self.file_heading_at(row)
        {
            self.pending_heading = Some((heading.path.clone(), row - heading.row));
        }
        self.file_headings.clear();
        self.directory_cursor = None;
    }

    pub fn set_file_headings(&mut self, headings: Vec<FileHeading>, lines: usize) {
        let row_in_file = |path: &str, offset: usize| {
            let index = headings.iter().position(|heading| heading.path == path)?;
            let end = headings.get(index + 1).map_or(lines, |heading| heading.row);
            Some((headings[index].row + offset).min(end.saturating_sub(1)))
        };
        let top = self.position().vertical;
        let rebased_top = self
            .file_heading_at(top)
            .and_then(|heading| row_in_file(&heading.path, top - heading.row));
        self.clear_file_headings();
        self.directory_cursor = self
            .pending_heading
            .take()
            .and_then(|(path, offset)| row_in_file(&path, offset));
        if let Some(row) = rebased_top {
            self.position().vertical = row;
        }
        self.file_headings = headings;
    }

    pub fn revision_ids(&self) -> &[String] {
        self.active
            .and_then(|index| {
                self.review
                    .comparison
                    .file_commit_ids
                    .as_ref()?
                    .get(&self.review.changes[index].path)
            })
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub fn revision_range_label(&self) -> Option<String> {
        let selection = self.revisions?;
        let ids = self.revision_ids();
        let range = selection.range();
        let first = ids.get(range.start)?;
        let last = ids.get(range.end - 1)?;
        let label = |id: &str| {
            let title = self
                .review
                .comparison
                .revisions
                .as_ref()
                .and_then(|revisions| revisions.get(id))
                .and_then(|revision| revision.title.as_deref())
                .unwrap_or_default()
                .lines()
                .next()
                .unwrap_or_default();
            if title.is_empty() {
                id.to_owned()
            } else {
                format!("{id} ({})", crate::ui::display_path(title))
            }
        };
        Some(if first == last {
            label(first)
        } else {
            format!("{}..{}", label(first), label(last))
        })
    }

    pub fn revision_range(&self) -> Option<(Revision, Revision)> {
        let range = self.revisions?.range();
        let ids = self.revision_ids();
        let revisions = self.review.comparison.revisions.as_ref()?;
        Some((
            revisions.get(ids.get(range.start)?)?.clone(),
            revisions.get(ids.get(range.end - 1)?)?.clone(),
        ))
    }

    pub fn diff_comparison(&self) -> &Comparison {
        self.revision_preview
            .as_ref()
            .map(|(comparison, _)| comparison)
            .unwrap_or(&self.review.comparison)
    }

    pub fn diff_changes(&self) -> &[Change] {
        match &self.revision_preview {
            Some((_, changes)) => changes,
            None if self.revisions.is_some() => &[],
            None => &self.review.changes,
        }
    }

    pub fn clear_revision_filter(&mut self) {
        if self.revisions.take().is_some() {
            self.reset_revision_preview();
        }
        self.prefix = None;
    }

    fn clear_preview(&mut self) {
        self.revision_preview = None;
        self.hunks.clear();
        self.source_lines.clear();
        self.file_headings.clear();
        self.rendering_rows.clear();
        self.directory_cursor = None;
        self.pending_heading = None;
        self.line_selection = None;
        self.follow_diff_selection = true;
        self.hunk_selection = None;
        self.thread = None;
        self.editor_requested = false;
        self.hunk_actions.clear();
    }

    fn reset_revision_preview(&mut self) {
        self.clear_preview();
        self.rendering = true;
        *self.position() = Position::default();
    }

    fn jump_revision(&mut self, forward: bool, extend: bool) {
        let len = self.revision_ids().len();
        if len == 0 {
            return;
        }
        let selection = match self.revisions {
            Some(mut selection) => {
                selection.cursor = if extend {
                    selection
                        .cursor
                        .saturating_add_signed(if forward { 1 } else { -1 })
                        .min(len - 1)
                } else if forward {
                    (selection.cursor + 1) % len
                } else {
                    (selection.cursor + len - 1) % len
                };
                if !extend {
                    selection.anchor = selection.cursor;
                }
                selection
            }
            None => {
                let index = if forward { 0 } else { len - 1 };
                RevisionSelection {
                    anchor: index,
                    cursor: index,
                }
            }
        };
        if self.revisions != Some(selection) {
            self.reset_revision_preview();
            self.revisions = Some(selection);
        }
    }

    pub fn selected_hunk(&self) -> Option<&Hunk> {
        self.hunk_selection
            .as_ref()
            .and_then(|location| self.hunks.iter().find(|hunk| &hunk.location == location))
    }

    pub fn editor_location(&self) -> Option<HunkLocation> {
        if let Some(selection) = &self.line_selection {
            return Some(HunkLocation {
                path: selection.path.clone(),
                old_line: (selection.side == DiffSide::Old).then_some(selection.cursor),
                new_line: (selection.side == DiffSide::New).then_some(selection.cursor),
            });
        }
        if let Some(hunk) = self.selected_hunk() {
            return Some(hunk.location.clone());
        }
        if self.focus == Focus::Diff
            && let Some(row) = self.directory_cursor
            && let Some(heading) = self.file_heading_at(row)
        {
            return self.file_location(&heading.path);
        }
        if self.focus == Focus::Diff {
            let position = self
                .review
                .view
                .positions
                .get(&self.review.view.active)
                .map(|position| position.vertical)
                .unwrap_or(0);
            if let Some(hunk) = self
                .hunks
                .iter()
                .find(|hunk| hunk.rows.end > position)
                .or_else(|| self.hunks.last())
            {
                return Some(hunk.location.clone());
            }
        }
        self.file_location(&self.review.view.active)
    }

    fn file_location(&self, path: &str) -> Option<HunkLocation> {
        self.diff_changes()
            .iter()
            .find(|change| change.path == path)
            .map(|change| HunkLocation {
                path: change.path.clone(),
                old_line: (!change.old.kind.is_empty()).then_some(1),
                new_line: (!change.new.kind.is_empty()).then_some(1),
            })
    }

    pub fn apply_hunk_navigation(&mut self, page: usize, lines: usize) -> bool {
        if self.rendering || self.hunk_actions.is_empty() {
            return false;
        }
        while let Some(navigation) = self.hunk_actions.pop_front() {
            self.jump_hunk(navigation, page, lines);
        }
        true
    }

    fn jump_hunk(&mut self, navigation: HunkNavigation, page: usize, lines: usize) {
        if self.hunks.is_empty() {
            self.message = "Renderer has no mapped hunks for this preview".into();
            return;
        }
        let len = self.hunks.len();
        let index = match navigation {
            HunkNavigation::Number(number) => {
                let Some(index) = number.checked_sub(1).filter(|&index| index < len) else {
                    self.message = format!("Hunk number must be between 1 and {len}");
                    return;
                };
                index
            }
            HunkNavigation::Relative(forward) => {
                let position = self.position().vertical;
                let selected = self.hunk_selection.as_ref().and_then(|location| {
                    self.hunks
                        .iter()
                        .position(|hunk| &hunk.location == location)
                });
                if let Some(index) = selected {
                    if forward {
                        (index + 1) % len
                    } else {
                        (index + len - 1) % len
                    }
                } else if forward {
                    self.hunks
                        .iter()
                        .position(|hunk| hunk.rows.end > position)
                        .unwrap_or(0)
                } else {
                    self.hunks
                        .iter()
                        .rposition(|hunk| hunk.rows.start <= position)
                        .unwrap_or(len - 1)
                }
            }
        };
        let hunk = &self.hunks[index];
        self.line_selection = None;
        self.directory_cursor = (!self.file_headings.is_empty()).then_some(hunk.rows.start);
        let center = hunk.rows.start + hunk.rows.len() / 2;
        let location = hunk.location.clone();
        self.message = format!(
            "Hunk {}/{} · {}:{}",
            index + 1,
            len,
            crate::ui::display_path(&location.path),
            location.new_line.or(location.old_line).unwrap_or(1)
        );
        self.hunk_selection = Some(location);
        self.position().vertical = center
            .saturating_sub(page / 2)
            .min(lines.saturating_sub(page));
        self.position().horizontal = 0;
        self.focus = Focus::Diff;
    }

    pub fn handle_key(
        &mut self,
        key: KeyEvent,
        page: usize,
        diff_lines: usize,
        terminal_width: u16,
    ) {
        self.handle_input(key, page, diff_lines, terminal_width);
        if self.focus == Focus::Tree {
            self.line_selection = None;
            self.directory_cursor = None;
        }
        self.review.view.diff_focused = self.focus == Focus::Diff;
        self.review.view.comments_focused = self.focus == Focus::Comments;
        self.keep_diff_selection_visible(page, diff_lines);
    }

    pub fn keep_diff_selection_visible(&mut self, page: usize, diff_lines: usize) {
        match self.focus {
            Focus::Diff if !self.rendering => {
                if self.follow_diff_selection {
                    self.ensure_diff_cursor(page);
                    self.keep_line_selection_visible(page, diff_lines);
                } else {
                    self.keep_cursor_in_viewport(page, diff_lines);
                }
            }
            Focus::Comments if self.follow_diff_selection => {
                self.keep_thread_visible(page, diff_lines);
            }
            _ => {}
        }
    }

    pub(crate) fn set_focus(&mut self, focus: Focus) {
        self.focus = focus;
        self.follow_diff_selection = true;
        self.review.view.fullscreen = false;
        self.review.view.diff_focused = focus == Focus::Diff;
        self.review.view.comments_focused = focus == Focus::Comments;
    }

    fn handle_pane_key(&mut self, key: KeyEvent, terminal_width: u16) -> bool {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match (key.code, control) {
            (KeyCode::Backspace, _) | (KeyCode::Char('h' | 'k'), true) => {
                self.set_focus(if self.focus == Focus::Comments {
                    Focus::Diff
                } else {
                    Focus::Tree
                });
            }
            (KeyCode::Char('j' | 'l'), true) => {
                self.set_focus(if self.focus == Focus::Tree {
                    Focus::Diff
                } else if self.review.view.comments_visible {
                    Focus::Comments
                } else {
                    Focus::Diff
                });
            }
            (KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down, true) => {
                if terminal_width >= 55 && !self.review.view.fullscreen {
                    let amount = if matches!(key.code, KeyCode::Right | KeyCode::Down) {
                        2
                    } else {
                        -2
                    };
                    let panes = crate::ui::panes(Rect::new(0, 0, terminal_width, 20), self);
                    if panes.comments.width > 0 {
                        if self.focus == Focus::Tree {
                            self.review.view.tree_width = Some(
                                panes
                                    .tree
                                    .width
                                    .saturating_add_signed(amount)
                                    .clamp(24, terminal_width - panes.comments.width - 24),
                            );
                        } else {
                            let amount = if self.focus == Focus::Diff {
                                -amount
                            } else {
                                amount
                            };
                            self.review.view.comments_width = Some(
                                panes
                                    .comments
                                    .width
                                    .saturating_add_signed(amount)
                                    .clamp(24, terminal_width - panes.tree.width - 24),
                            );
                        }
                    } else {
                        self.review.view.tree_width = Some(
                            panes
                                .tree
                                .width
                                .saturating_add_signed(amount)
                                .clamp(24, terminal_width - 24),
                        );
                    }
                }
            }
            (KeyCode::Char('t'), false) if !key.modifiers.contains(KeyModifiers::ALT) => {
                self.review.view.comments_visible = !self.review.view.comments_visible;
                self.review.view.fullscreen = false;
                if self.focus == Focus::Comments || self.thread.is_some() {
                    self.set_focus(Focus::Diff);
                }
            }
            _ => {
                return false;
            }
        }
        self.prefix = None;
        true
    }

    pub fn is_quit_key(key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('q') => key.modifiers == KeyModifiers::NONE,
            KeyCode::Char('c') => key.modifiers.contains(KeyModifiers::CONTROL),
            _ => false,
        }
    }

    fn text_input_focused(&self) -> bool {
        if self.network_busy || self.restore.is_some() {
            return false;
        }
        if let Some(dialog) = &self.session_dialog {
            return dialog.renaming.is_some();
        }
        if let Some(dialog) = &self.revision_dialog {
            return !dialog.pending;
        }
        if self.comments_overview_open() || self.help {
            return false;
        }
        if self.searching || self.flash.is_some() {
            return true;
        }
        if self.focus != Focus::Comments {
            return false;
        }
        if let Some(dialog) = &self.submission_dialog {
            return dialog.accepts_text(&self.review);
        }
        self.comment_dialog
            .as_ref()
            .is_some_and(CommentDialog::accepts_text)
    }

    fn handle_input(&mut self, key: KeyEvent, page: usize, diff_lines: usize, terminal_width: u16) {
        if Self::is_quit_key(key) && (key.code != KeyCode::Char('q') || !self.text_input_focused())
        {
            self.quit = true;
            if self.network_busy {
                self.message = "Quitting after the current GitLab operation finishes…".into();
            }
            return;
        }
        if self.network_busy {
            self.message = "Waiting for the current GitLab operation to finish…".into();
            return;
        }
        if let Some(state) = self.restore.take() {
            self.restore = match (state, key.code, key.modifiers) {
                (RestoreState::Confirm(action), KeyCode::Char('y'), KeyModifiers::NONE) => {
                    Some(RestoreState::Apply(action))
                }
                (RestoreState::Confirm(_), KeyCode::Char('n') | KeyCode::Esc, _) => None,
                (state, _, _) => Some(state),
            };
            return;
        }
        if let Some(dialog) = &mut self.session_dialog {
            self.session_action = dialog.handle_key(key, page);
            return;
        }
        if let Some(dialog) = &mut self.revision_dialog {
            if !dialog.handle_key(key) {
                self.revision_dialog = None;
            }
            return;
        }
        if self.comments_overview_open() {
            self.handle_comment_dialog(key, page);
            return;
        }
        self.dirty = true;
        if self.help {
            self.help = false;
            return;
        }
        if self.searching {
            match key.code {
                KeyCode::Enter => {
                    self.searching = false;
                }
                KeyCode::Esc => {
                    if self.focus == Focus::Tree {
                        self.review.view.filter = self.search_original.clone();
                    } else {
                        self.diff_search = self.search_original.clone();
                    }
                    self.searching = false;
                }
                KeyCode::Backspace => {
                    if self.focus == Focus::Tree {
                        self.review.view.filter.pop();
                    } else {
                        self.diff_search.pop();
                    }
                }
                KeyCode::Char(ch) => {
                    if self.focus == Focus::Tree {
                        self.review.view.filter.push(ch);
                    } else {
                        self.diff_search.push(ch);
                    }
                }
                _ => {}
            }
            if self.focus == Focus::Tree {
                self.rebuild();
            }
            return;
        }
        if self.flash.is_some() {
            self.handle_flash(key, page);
            return;
        }
        if (!self.comment_editor_active() || key.modifiers.contains(KeyModifiers::CONTROL))
            && self.handle_pane_key(key, terminal_width)
        {
            return;
        }
        if self.comment_editor_active() {
            if self.submission_dialog.is_some() {
                self.handle_submission_dialog(key);
            } else {
                self.handle_comment_dialog(key, page);
            }
            return;
        }
        let diff_scroll_step = match (key.code, key.modifiers) {
            (KeyCode::Char('J'), KeyModifiers::NONE)
            | (KeyCode::Char('j' | 'J'), KeyModifiers::SHIFT) => Some(1),
            (KeyCode::Char('K'), KeyModifiers::NONE)
            | (KeyCode::Char('k' | 'K'), KeyModifiers::SHIFT) => Some(-1),
            _ => None,
        };
        if let Some(step) = diff_scroll_step {
            let banner = usize::from(self.focus == Focus::Tree && self.revisions.is_some());
            self.scroll_diff(step, page.saturating_sub(banner).max(1), diff_lines);
            self.follow_diff_selection = false;
            self.prefix = None;
            return;
        }
        self.follow_diff_selection = true;
        let horizontal_step = match (key.code, key.modifiers) {
            (KeyCode::Char('H'), KeyModifiers::NONE)
            | (KeyCode::Char('h' | 'H'), KeyModifiers::SHIFT) => Some(-4),
            (KeyCode::Char('L'), KeyModifiers::NONE)
            | (KeyCode::Char('l' | 'L'), KeyModifiers::SHIFT) => Some(4),
            _ => None,
        };
        if self.focus == Focus::Diff
            && let Some(step) = horizontal_step
        {
            self.scroll_diff_horizontally(step);
            self.prefix = None;
            return;
        }
        if let Some((prefix, number)) = self.prefix.take().filter(|_| {
            !key.modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        }) {
            match (prefix, number, key.code) {
                ('[' | ']', number, KeyCode::Char(digit @ '0'..='9')) => {
                    let number = number
                        .unwrap_or(0)
                        .saturating_mul(10)
                        .saturating_add(digit as usize - '0' as usize);
                    self.prefix = Some((prefix, Some(number)));
                    return;
                }
                ('[' | ']', number, KeyCode::Char('h')) => {
                    let navigation = number.map_or(
                        HunkNavigation::Relative(prefix == ']'),
                        HunkNavigation::Number,
                    );
                    if self.rendering {
                        self.hunk_actions.push_back(navigation);
                        self.focus = Focus::Diff;
                    } else {
                        self.jump_hunk(navigation, page, diff_lines);
                    }
                    return;
                }
                ('[' | ']', None, KeyCode::Char('f' | 'u')) => {
                    self.jump_file(prefix == ']', key.code == KeyCode::Char('u'));
                    return;
                }
                ('[' | ']', None, KeyCode::Char('c')) => {
                    self.jump_thread(prefix == ']');
                    return;
                }
                ('g', None, KeyCode::Char('g')) => {
                    self.move_to(0);
                    return;
                }
                _ => {}
            }
        }
        if self.focus == Focus::Comments && self.handle_comments_key(key, page, terminal_width) {
            return;
        }
        if key.code == KeyCode::Esc && self.revisions.is_some() {
            self.clear_revision_filter();
        } else if self.focus == Focus::Diff && self.handle_line_selection(key, page, diff_lines) {
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('n') => {
                    self.select(self.selection.saturating_add(1));
                }
                KeyCode::Char('p') => {
                    self.select(self.selection.saturating_sub(1));
                }
                KeyCode::Char('d') => {
                    self.scroll_diff((page as isize / 2).max(1), page, diff_lines);
                }
                KeyCode::Char('u') => {
                    self.scroll_diff(-(page as isize / 2).max(1), page, diff_lines);
                }
                KeyCode::Char('f') => {
                    self.move_by(page as isize, page, diff_lines);
                }
                KeyCode::Char('b') => {
                    self.move_by(-(page as isize), page, diff_lines);
                }
                _ => {}
            }
            return;
        }
        if self.in_folder_diff()
            && let Some(forward) = match (key.code, key.modifiers) {
                (KeyCode::Char('N'), KeyModifiers::NONE | KeyModifiers::SHIFT)
                | (KeyCode::Char('n'), KeyModifiers::SHIFT) => Some(true),
                (KeyCode::Char('P'), KeyModifiers::NONE | KeyModifiers::SHIFT)
                | (KeyCode::Char('p'), KeyModifiers::SHIFT) => Some(false),
                _ => None,
            }
        {
            self.jump_diff_file(forward);
            return;
        }
        match key.code {
            KeyCode::Char('o') => self.expand_comment(),
            KeyCode::Char('c') => {
                if self.rendering {
                    self.message = "Wait for the diff to finish rendering".into();
                } else if let Some(target) = self.comment_target() {
                    self.comment_action = Some(CommentAction::Compose(target));
                } else {
                    self.message = "Select a file or mapped source line".into();
                }
            }
            KeyCode::Enter if self.focus == Focus::Diff => {}
            KeyCode::Char('v') => self.start_line_selection(page),
            KeyCode::Char('C') => {
                self.open_comments_overview();
            }
            KeyCode::Char('S') => {
                self.session_action = Some(SessionAction::Open);
            }
            KeyCode::Char('B') if !key.modifiers.contains(KeyModifiers::ALT) => {
                if self.review.gitlab.is_some() {
                    self.message =
                        "Press r to load the latest GitLab merge request revisions.".into();
                } else if !self.resume_comment_draft() {
                    self.revision_dialog =
                        Some(RevisionDialog::new(&self.review.comparison, self.backend));
                }
            }
            KeyCode::Char('P') => {
                self.open_submission();
            }
            KeyCode::Char('p') if key.modifiers == KeyModifiers::ALT => {
                self.open_submission();
            }
            KeyCode::Char('e') => {
                self.editor_requested = true;
            }
            KeyCode::Char('Y') => {
                self.copy_path_requested = true;
            }
            KeyCode::Char('D')
                if self.focus != Focus::Comments && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.confirm_restore();
            }
            KeyCode::Char('?') => {
                self.help = true;
            }
            KeyCode::Tab | KeyCode::BackTab => {
                let focus = if self.review.view.comments_visible {
                    match (self.focus, key.code == KeyCode::Tab) {
                        (Focus::Tree, true) | (Focus::Comments, false) => Focus::Diff,
                        (Focus::Diff, true) | (Focus::Tree, false) => Focus::Comments,
                        _ => Focus::Tree,
                    }
                } else if self.focus == Focus::Tree {
                    Focus::Diff
                } else {
                    Focus::Tree
                };
                self.set_focus(focus);
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.move_by(1, page, diff_lines);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.move_by(-1, page, diff_lines);
            }
            KeyCode::PageDown => {
                self.move_by(page as isize, page, diff_lines);
            }
            KeyCode::PageUp => {
                self.move_by(-(page as isize), page, diff_lines);
            }
            KeyCode::Char('G') | KeyCode::End => {
                let last = if self.focus == Focus::Tree {
                    self.rows.len().saturating_sub(1)
                } else {
                    diff_lines.saturating_sub(page)
                };
                self.move_to(last);
            }
            KeyCode::Home => {
                self.move_to(0);
            }
            KeyCode::Char('<' | '>') => {
                self.jump_revision(key.code == KeyCode::Char('>'), true);
            }
            KeyCode::Char(',' | '.') => {
                self.jump_revision(
                    key.code == KeyCode::Char('.'),
                    key.modifiers.contains(KeyModifiers::SHIFT),
                );
            }
            KeyCode::Char(ch @ ('g' | '[' | ']')) => {
                self.prefix = Some((ch, None));
            }
            KeyCode::Char('h') | KeyCode::Left => {
                if self.focus == Focus::Diff {
                    self.scroll_diff_horizontally(-4);
                } else {
                    self.collapse();
                }
            }
            KeyCode::Char('l') | KeyCode::Right | KeyCode::Enter => {
                if self.focus == Focus::Diff {
                    self.scroll_diff_horizontally(4);
                } else if let Some(row) = self.rows.get(self.selection) {
                    if row.file.is_none() {
                        if key.code == KeyCode::Enter
                            && !self.review.view.collapsed.contains(&row.path)
                        {
                            self.review.view.collapsed.insert(row.path.clone());
                        } else {
                            self.review.view.collapsed.remove(&row.path);
                        }
                        self.rebuild();
                    } else {
                        self.focus = Focus::Diff;
                    }
                }
            }
            KeyCode::Char(' ') => {
                if self.in_folder_diff() {
                    self.toggle_viewed_in_directory();
                    return;
                }
                let file = if self.focus == Focus::Diff {
                    self.active
                } else {
                    self.rows.get(self.selection).and_then(|row| row.file)
                };
                if let Some(index) = file {
                    self.review.toggle_viewed(index);
                    if self.review.is_viewed(&self.review.changes[index])
                        && self.jump_file(true, true)
                    {
                        return;
                    }
                } else if self.review.view.active.ends_with('/') {
                    if self
                        .preview_changes()
                        .all(|change| self.review.is_viewed(change))
                    {
                        self.review
                            .viewed
                            .retain(|path, _| !path.starts_with(&self.review.view.active));
                    } else {
                        self.review.viewed.extend(
                            self.review
                                .changes
                                .iter()
                                .filter(|change| change.path.starts_with(&self.review.view.active))
                                .map(|change| (change.path.clone(), change.fingerprint())),
                        );
                    }
                }
                self.rebuild();
            }
            KeyCode::Char('u') => {
                self.review.view.unviewed_only = !self.review.view.unviewed_only;
                self.rebuild();
            }
            KeyCode::Char('i') => {
                self.review.view.inline = !self.review.view.inline;
            }
            KeyCode::Char('+' | '-') if !key.modifiers.contains(KeyModifiers::ALT) => {
                self.adjust_diff_context(key.code == KeyCode::Char('+'));
            }
            KeyCode::Char('z') => {
                self.review.view.fullscreen = !self.review.view.fullscreen;
                self.focus = Focus::Diff;
            }
            KeyCode::Char('r') => {
                self.refresh = true;
            }
            KeyCode::Char('n' | 'N') if self.focus == Focus::Tree => {
                self.jump_file(
                    key.code == KeyCode::Char('n') && !key.modifiers.contains(KeyModifiers::ALT),
                    false,
                );
            }
            KeyCode::Char('/') => {
                self.searching = true;
                self.search_original = if self.focus == Focus::Tree {
                    self.review.view.filter.clone()
                } else {
                    self.diff_search.clone()
                };
                if self.focus == Focus::Tree {
                    self.review.view.filter.clear();
                    self.rebuild();
                } else {
                    self.diff_search.clear();
                }
            }
            KeyCode::Char('s') => {
                self.flash = Some(Flash {
                    query: String::new(),
                    matches: Vec::new(),
                    labels: Vec::new(),
                    previous_focus: self.focus,
                    previous_fullscreen: self.review.view.fullscreen,
                });
                self.focus = Focus::Tree;
                self.review.view.fullscreen = false;
            }
            KeyCode::Esc => {
                self.clear_revision_filter();
                self.hunk_selection = None;
                self.hunk_actions.clear();
                self.editor_requested = false;
                self.review.view.filter.clear();
                self.review.view.unviewed_only = false;
                self.rebuild();
            }
            _ => {}
        }
    }

    fn adjust_diff_context(&mut self, increase: bool) {
        let Some(default) = self.default_diff_context else {
            self.message = "Set {context} in renderer.command to adjust diff context.".into();
            return;
        };
        let path = if self.review.view.active.ends_with('/') {
            let row = self
                .diff_cursor_row()
                .unwrap_or_else(|| self.position().vertical);
            let Some(heading) = self.file_heading_at(row) else {
                self.message = "Select a file after the diff finishes rendering.".into();
                return;
            };
            heading.path.clone()
        } else if self.active.is_some() {
            self.review.view.active.clone()
        } else {
            self.message = "Select a changed file to adjust its diff context.".into();
            return;
        };
        let view = &mut self.review.view;
        let current = view.diff_context_for(&path).unwrap_or(default);
        let lines = if increase {
            current.saturating_add(1)
        } else {
            current.saturating_sub(1)
        };
        self.message = format!("Diff context: {lines} · {}", crate::ui::display_path(&path));
        if lines == view.diff_context.unwrap_or(default) {
            view.file_diff_contexts.remove(&path);
        } else {
            view.file_diff_contexts.insert(path, lines);
        }
    }

    fn jump_diff_file(&mut self, forward: bool) {
        if self.rendering {
            self.message = "Wait for the diff to finish rendering.".into();
            return;
        }
        let row = self
            .diff_cursor_row()
            .unwrap_or_else(|| self.position().vertical);
        let next = self
            .file_headings
            .partition_point(|heading| heading.row <= row);
        let target = if forward {
            self.file_headings.get(next)
        } else {
            next.checked_sub(2)
                .and_then(|index| self.file_headings.get(index))
        };
        if let Some(heading) = target {
            let row = heading.row;
            self.hunk_selection = None;
            self.select_diff_line_at(row);
            self.position().horizontal = 0;
        }
    }

    fn toggle_viewed_in_directory(&mut self) {
        if self.rendering {
            self.message = "Wait for the diff to finish rendering.".into();
            return;
        }
        let row = self
            .diff_cursor_row()
            .unwrap_or_else(|| self.position().vertical);
        let Some(heading) = self.file_heading_at(row) else {
            return;
        };
        let Some(index) = self
            .review
            .changes
            .iter()
            .position(|change| change.path == heading.path)
        else {
            return;
        };
        self.directory_cursor = Some(heading.row);
        self.line_selection = None;
        self.hunk_selection = None;
        self.review.toggle_viewed(index);
        self.rebuild();
    }

    fn confirm_restore(&mut self) {
        if self.revisions.is_some() {
            self.message = "Press Esc to return to the full comparison before restoring.".into();
            return;
        }
        let lines = self
            .line_selection
            .as_ref()
            .filter(|_| self.focus == Focus::Diff)
            .map(LineSelection::target);
        if self.rendering && self.focus == Focus::Diff {
            self.message = "Wait for the diff to finish rendering.".into();
            return;
        }
        match Restore::new(&self.review, self.backend, lines) {
            Ok(restore) => {
                self.restore = Some(RestoreState::Confirm(restore));
            }
            Err(error) => {
                self.message = format!("{error:#}");
            }
        }
    }

    fn move_to(&mut self, position: usize) {
        if self.focus == Focus::Tree {
            self.select(position);
        } else {
            self.position().vertical = position;
            if self.focus == Focus::Diff && !self.file_headings.is_empty() {
                self.hunk_selection = None;
                self.select_diff_line_at(position);
            }
        }
    }

    pub fn update_flash(&mut self, height: usize) {
        let Some(flash) = &mut self.flash else {
            return;
        };
        flash.matches.clear();
        let mut continuations = BTreeSet::new();
        for index in tree::visible_range(self.rows.len(), self.selection, height) {
            let name = self.rows[index].name();
            let ranges = flash.ranges(name);
            if !ranges.is_empty() {
                flash.matches.push(index);
            }
            continuations.extend(
                ranges
                    .into_iter()
                    .filter_map(|range| name[range.end..].chars().next())
                    .flat_map(char::to_lowercase),
            );
        }
        // Keep query extensions available while assigning home-row-first jump labels.
        let lowercase = "asdfghjklqwertyuiopzxcvbnm"
            .chars()
            .filter(|label| !continuations.contains(label));
        let labels = lowercase
            .clone()
            .chain(lowercase.map(|label| label.to_ascii_uppercase()));
        flash.labels = flash.matches.iter().copied().zip(labels).collect();
    }

    fn handle_flash(&mut self, key: KeyEvent, height: usize) {
        let mut flash = self.flash.take().expect("Flash mode is active");
        match key.code {
            KeyCode::Esc => {
                self.focus = flash.previous_focus;
                self.review.view.fullscreen = flash.previous_fullscreen;
                return;
            }
            KeyCode::Enter => {
                if let Some(index) = flash.matches.first() {
                    self.select(*index);
                    return;
                }
            }
            KeyCode::Backspace => {
                flash.query.pop();
            }
            KeyCode::Char(ch)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                if let Some((index, _)) = flash.labels.iter().find(|(_, label)| *label == ch) {
                    self.select(*index);
                    return;
                }
                flash.query.push(ch);
            }
            _ => {}
        }
        self.flash = Some(flash);
        self.update_flash(height);
    }

    fn move_by(&mut self, amount: isize, page: usize, lines: usize) {
        if self.focus == Focus::Tree {
            self.select(self.selection.saturating_add_signed(amount));
        } else {
            self.scroll_diff(amount, page, lines);
        }
    }

    fn scroll_diff(&mut self, amount: isize, page: usize, lines: usize) {
        let position = self.position();
        position.vertical = position
            .vertical
            .saturating_add_signed(amount)
            .min(lines.saturating_sub(page));
    }

    fn scroll_diff_horizontally(&mut self, amount: i16) {
        if !self.rendering && self.horizontal_scroll_limit > 0 {
            let limit = self.horizontal_scroll_limit;
            let position = self.position();
            position.horizontal = position.horizontal.saturating_add_signed(amount).min(limit);
        }
    }

    fn collapse(&mut self) {
        if let Some(row) = self.rows.get(self.selection) {
            if row.file.is_none() && !self.review.view.collapsed.contains(&row.path) {
                self.review.view.collapsed.insert(row.path.clone());
            } else {
                let path = row.path.trim_end_matches('/');
                if let Some((parent, _)) = path.rsplit_once('/') {
                    let parent = format!("{parent}/");
                    if let Some(index) = self.rows.iter().position(|row| row.path == parent) {
                        self.select(index);
                    }
                }
            }
            self.rebuild();
        }
    }

    fn jump_file(&mut self, forward: bool, unviewed: bool) -> bool {
        let len = self.review.changes.len();
        let start = self.active.unwrap_or_else(|| {
            self.review
                .changes
                .iter()
                .position(|change| change.path.starts_with(&self.review.view.active))
                .map(|first| {
                    if forward {
                        (first + len - 1) % len
                    } else {
                        first
                    }
                })
                .unwrap_or(0)
        });
        let filter = self.review.view.filter.to_lowercase();
        for offset in 1..=len {
            let index = if forward {
                (start + offset) % len
            } else {
                (start + len - offset) % len
            };
            let change = &self.review.changes[index];
            if (!(unviewed || self.review.view.unviewed_only) || !self.review.is_viewed(change))
                && change.path.to_lowercase().contains(&filter)
            {
                self.review.view.selected = change.path.clone();
                self.review
                    .view
                    .collapsed
                    .retain(|parent| !change.path.starts_with(parent));
                self.rebuild();
                return true;
            }
        }
        self.message = "All matching files have been viewed".into();
        false
    }

    pub fn search_diff(&mut self, text: &Text<'_>, forward: bool) {
        let len = text.lines.len();
        if self.diff_search.is_empty() || len == 0 {
            return;
        }
        let start = self
            .diff_cursor_row()
            .unwrap_or_else(|| self.position().vertical)
            .min(len - 1);
        let needle = self.diff_search.to_lowercase();
        for offset in 1..=len {
            let index = if forward {
                (start + offset) % len
            } else {
                (start + len - offset) % len
            };
            if !self.is_rendering_row(index)
                && text.lines[index]
                    .to_string()
                    .to_lowercase()
                    .contains(&needle)
            {
                self.position().vertical = index;
                if !self.file_headings.is_empty() {
                    self.hunk_selection = None;
                }
                self.select_diff_line_at(index);
                return;
            }
        }
        self.message = format!("Search: {:?} — 0 matches", self.diff_search);
    }
}
