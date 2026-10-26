use crate::{
    state::{Position, Review},
    tree::{self, Row},
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
        let active = review
            .changes
            .iter()
            .position(|change| change.path == review.view.active);
        let mut app = Self {
            review,
            focus,
            active,
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
        let filter = self.review.view.filter.to_lowercase();
        let eligible = |index: usize| {
            self.review.changes.get(index).is_some_and(|change| {
                change.path.to_lowercase().contains(&filter)
                    && (!self.review.view.unviewed_only || !self.review.is_viewed(change))
            })
        };
        if !self.active.is_some_and(eligible) {
            self.active = (0..self.review.changes.len()).find(|index| eligible(*index));
        }
        self.review.view.active = self
            .active
            .map(|index| self.review.changes[index].path.clone())
            .unwrap_or_default();
    }

    fn select(&mut self, selection: usize) {
        self.selection = selection.min(self.rows.len().saturating_sub(1));
        if let Some(row) = self.rows.get(self.selection) {
            self.review.view.selected = row.path.clone();
            if row.file.is_some() {
                self.active = row.file;
                self.review.view.active = row.path.clone();
            }
        }
    }

    pub fn position(&mut self) -> &mut Position {
        let path = self
            .active
            .map(|index| self.review.changes[index].path.clone())
            .unwrap_or_default();
        self.review.view.positions.entry(path).or_default()
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
                    if self.review.is_viewed(&self.review.changes[index]) {
                        self.jump_file(true, true);
                    }
                } else if self.focus == Focus::Tree
                    && let Some(row) = self.rows.get(self.selection)
                {
                    self.review.viewed.extend(
                        self.review
                            .changes
                            .iter()
                            .filter(|change| change.path.starts_with(&row.path))
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
        let query = flash.query.to_lowercase();
        let matches: Vec<_> = tree::visible_range(self.rows.len(), self.selection, height)
            .map(|index| (index, self.rows[index].name().to_lowercase()))
            .filter(|(_, name)| !query.is_empty() && name.contains(&query))
            .collect();
        // Keep query extensions available while assigning home-row-first jump labels.
        let labels = "asdfghjklqwertyuiopzxcvbnm".chars().filter(|label| {
            let extension = format!("{query}{label}");
            matches.iter().all(|(_, name)| !name.contains(&extension))
        });
        flash.matches = matches.iter().map(|(index, _)| *index).collect();
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

    fn jump_file(&mut self, forward: bool, unviewed: bool) {
        let len = self.review.changes.len();
        let start = self.active.unwrap_or(0);
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
                return;
            }
        }
        self.message = "All matching files have been viewed".into();
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
