use std::{
    collections::{BTreeSet, VecDeque},
    ops::Range,
};

use crate::{
    hunks::{Hunk, HunkLocation},
    state::{Position, Review},
    tree::{self, Row},
    vcs::Change,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::Text;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Tree,
    Diff,
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
    pub focus: Focus,
    pub active: Option<usize>,
    pub help: bool,
    pub quit: bool,
    pub refresh: bool,
    pub message: String,
    pub rows: Vec<Row>,
    pub selection: usize,
    pub searching: bool,
    pub diff_search: String,
    pub dirty: bool,
    pub bindings_help: String,
    pub flash: Option<Flash>,
    pub hunks: Vec<Hunk>,
    pub hunk_selection: Option<HunkLocation>,
    pub editor_requested: bool,
    pub rendering: bool,
    hunk_actions: VecDeque<bool>,
    prefix: Option<char>,
    search_original: String,
}

impl App {
    pub fn new(review: Review) -> Self {
        let focus = if review.view.fullscreen || review.view.diff_focused {
            Focus::Diff
        } else {
            Focus::Tree
        };
        let mut app = Self {
            review,
            focus,
            active: None,
            help: false,
            quit: false,
            refresh: false,
            message: String::new(),
            rows: Vec::new(),
            selection: 0,
            searching: false,
            diff_search: String::new(),
            dirty: false,
            bindings_help: String::new(),
            flash: None,
            hunks: Vec::new(),
            hunk_selection: None,
            editor_requested: false,
            rendering: false,
            hunk_actions: VecDeque::new(),
            prefix: None,
            search_original: String::new(),
        };
        app.rebuild();
        app
    }

    pub fn rebuild(&mut self) {
        self.rows = tree::rows(&self.review);
        let selection = self
            .rows
            .iter()
            .position(|row| row.path == self.review.view.selected)
            .or_else(|| self.rows.iter().position(|row| row.file.is_some()))
            .unwrap_or(0);
        self.select(selection);
    }

    fn select(&mut self, selection: usize) {
        self.selection = selection.min(self.rows.len().saturating_sub(1));
        if let Some(row) = self.rows.get(self.selection) {
            if self.review.view.active != row.path {
                self.hunks.clear();
                self.hunk_selection = None;
                self.editor_requested = false;
                self.hunk_actions.clear();
            }
            self.review.view.selected = row.path.clone();
            self.active = row.file;
            self.review.view.active = row.path.clone();
        } else {
            self.active = None;
            self.review.view.active.clear();
            self.hunks.clear();
            self.hunk_selection = None;
            self.editor_requested = false;
            self.hunk_actions.clear();
        }
    }

    pub fn position(&mut self) -> &mut Position {
        self.review
            .view
            .positions
            .entry(self.review.view.active.clone())
            .or_default()
    }

    pub fn preview_changes(&self) -> impl Iterator<Item = &Change> {
        let path = &self.review.view.active;
        self.review.changes.iter().filter(move |change| {
            change.path == *path || (path.ends_with('/') && change.path.starts_with(path))
        })
    }

    pub fn selected_hunk(&self) -> Option<&Hunk> {
        self.hunk_selection
            .as_ref()
            .and_then(|location| self.hunks.iter().find(|hunk| &hunk.location == location))
    }

    pub fn editor_location(&self) -> Option<HunkLocation> {
        if let Some(hunk) = self.selected_hunk() {
            return Some(hunk.location.clone());
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
        self.active.map(|index| {
            let change = &self.review.changes[index];
            HunkLocation {
                path: change.path.clone(),
                old_line: (!change.old.kind.is_empty()).then_some(1),
                new_line: (!change.new.kind.is_empty()).then_some(1),
            }
        })
    }

    pub fn apply_hunk_navigation(&mut self, page: usize, lines: usize) -> bool {
        if self.rendering || self.hunk_actions.is_empty() {
            return false;
        }
        while let Some(forward) = self.hunk_actions.pop_front() {
            self.jump_hunk(forward, page, lines);
        }
        true
    }

    fn jump_hunk(&mut self, forward: bool, page: usize, lines: usize) {
        if self.hunks.is_empty() {
            self.message = "Renderer has no mapped hunks for this preview".into();
            return;
        }
        let len = self.hunks.len();
        let position = self.position().vertical;
        let selected = self.hunk_selection.as_ref().and_then(|location| {
            self.hunks
                .iter()
                .position(|hunk| &hunk.location == location)
        });
        let index = if let Some(index) = selected {
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
        };
        let hunk = &self.hunks[index];
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
        self.review.view.diff_focused = self.focus == Focus::Diff;
    }

    fn handle_input(&mut self, key: KeyEvent, page: usize, diff_lines: usize, terminal_width: u16) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
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
        if let Some(prefix) = self.prefix.take() {
            match (prefix, key.code) {
                ('[' | ']', KeyCode::Char('h')) => {
                    if self.rendering {
                        self.hunk_actions.push_back(prefix == ']');
                        self.focus = Focus::Diff;
                    } else {
                        self.jump_hunk(prefix == ']', page, diff_lines);
                    }
                    return;
                }
                (']', KeyCode::Char('f' | 'u')) => {
                    self.jump_file(true, key.code == KeyCode::Char('u'));
                    return;
                }
                ('[', KeyCode::Char('f' | 'u')) => {
                    self.jump_file(false, key.code == KeyCode::Char('u'));
                    return;
                }
                ('g', KeyCode::Char('g')) => {
                    self.move_to(0);
                    return;
                }
                _ => {}
            }
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('h') => {
                    self.focus = Focus::Tree;
                    self.review.view.fullscreen = false;
                }
                KeyCode::Char('l') => {
                    self.focus = Focus::Diff;
                }
                KeyCode::Left | KeyCode::Right if terminal_width >= 55 => {
                    let width = self.review.view.tree_width(terminal_width);
                    self.review.view.tree_width = Some(if key.code == KeyCode::Left {
                        width.saturating_sub(2).max(24)
                    } else {
                        width.saturating_add(2).min(terminal_width - 24)
                    });
                }
                KeyCode::Char('d') => {
                    self.move_by((page as isize / 2).max(1), page, diff_lines);
                }
                KeyCode::Char('u') => {
                    self.move_by(-(page as isize / 2).max(1), page, diff_lines);
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
        match key.code {
            KeyCode::Char('e') => {
                self.editor_requested = true;
            }
            KeyCode::Backspace => {
                self.focus = Focus::Tree;
                self.review.view.fullscreen = false;
            }
            KeyCode::Char('q') => {
                self.quit = true;
            }
            KeyCode::Char('?') => {
                self.help = true;
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = if self.focus == Focus::Tree {
                    Focus::Diff
                } else {
                    Focus::Tree
                };
                if self.focus == Focus::Tree {
                    self.review.view.fullscreen = false;
                }
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
            KeyCode::Char(ch @ ('g' | '[' | ']')) => {
                self.prefix = Some(ch);
            }
            KeyCode::Char('h') | KeyCode::Left => {
                if self.focus == Focus::Diff {
                    self.position().horizontal = self.position().horizontal.saturating_sub(4);
                } else {
                    self.collapse();
                }
            }
            KeyCode::Char('l') | KeyCode::Right | KeyCode::Enter => {
                if self.focus == Focus::Diff {
                    self.position().horizontal = self.position().horizontal.saturating_add(4);
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
                    self.review.viewed.extend(
                        self.review
                            .changes
                            .iter()
                            .filter(|change| change.path.starts_with(&self.review.view.active))
                            .map(|change| (change.path.clone(), change.fingerprint())),
                    );
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
            KeyCode::Char('z') => {
                self.review.view.fullscreen = !self.review.view.fullscreen;
                self.focus = Focus::Diff;
            }
            KeyCode::Char('r') => {
                self.refresh = true;
            }
            KeyCode::Char('n' | 'N') if self.focus == Focus::Tree => {
                self.jump_file(key.code == KeyCode::Char('n'), false);
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

    fn move_to(&mut self, position: usize) {
        if self.focus == Focus::Tree {
            self.select(position);
        } else {
            self.position().vertical = position;
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
        let labels = "asdfghjklqwertyuiopzxcvbnm"
            .chars()
            .filter(|label| !continuations.contains(label));
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
                if let Some((index, _)) = flash
                    .labels
                    .iter()
                    .find(|(_, label)| *label == ch.to_ascii_lowercase())
                {
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
            self.position().vertical = self
                .position()
                .vertical
                .saturating_add_signed(amount)
                .min(lines.saturating_sub(page));
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
        let start = self.position().vertical.min(len - 1);
        let needle = self.diff_search.to_lowercase();
        for offset in 1..=len {
            let index = if forward {
                (start + offset) % len
            } else {
                (start + len - offset) % len
            };
            if text.lines[index]
                .to_string()
                .to_lowercase()
                .contains(&needle)
            {
                self.position().vertical = index;
                return;
            }
        }
        self.message = format!("Search: {:?} — 0 matches", self.diff_search);
    }
}
