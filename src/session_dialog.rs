use std::collections::BTreeSet;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::{
    input,
    store::{SavedSession, SessionStore},
};

#[derive(Debug, PartialEq, Eq)]
pub enum SessionAction {
    Open,
    Close,
    Reload,
    Forget(Vec<String>),
    Rename { id: String, name: String },
}

pub struct RenamePrompt {
    pub id: String,
    pub value: String,
    pub cursor: usize,
}

impl RenamePrompt {
    fn insert(&mut self, text: &str) {
        let text: String = text
            .replace("\r\n", " ")
            .replace(['\r', '\n', '\t', '\u{2028}', '\u{2029}'], " ")
            .chars()
            .filter(|ch| !ch.is_control())
            .collect();
        self.value.insert_str(self.cursor, &text);
        self.cursor += text.len();
    }

    fn input(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('u') {
            self.value.clear();
            self.cursor = 0;
            return;
        }
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return;
        }
        if input::edit(&mut self.value, &mut self.cursor, key.code) {
            return;
        }
        if let KeyCode::Char(ch) = key.code {
            self.insert(&ch.to_string());
        }
    }
}

pub struct SessionDialog {
    pub sessions: Vec<SavedSession>,
    pub current_id: String,
    pub open: BTreeSet<String>,
    pub selected: BTreeSet<String>,
    pub selection: usize,
    pub confirming: bool,
    pub message: String,
    pub renaming: Option<RenamePrompt>,
    first: bool,
}

impl SessionDialog {
    pub fn new(
        sessions: Vec<SavedSession>,
        current_id: String,
        mut open: BTreeSet<String>,
    ) -> Self {
        open.insert(current_id.clone());
        Self {
            sessions,
            current_id,
            open,
            selected: BTreeSet::new(),
            selection: 0,
            confirming: false,
            message: String::new(),
            renaming: None,
            first: false,
        }
    }

    pub fn load(store: &SessionStore, current_id: &str) -> Result<Self> {
        let sessions = store.list()?;
        let mut open = BTreeSet::new();
        for saved in &sessions {
            if store.is_open(&saved.id)? {
                open.insert(saved.id.clone());
            }
        }
        Ok(Self::new(sessions, current_id.to_owned(), open))
    }

    pub fn closed_sessions(&self) -> impl Iterator<Item = &SavedSession> {
        self.sessions
            .iter()
            .filter(|saved| !self.open.contains(&saved.id))
    }

    pub fn reload(&mut self, store: &SessionStore) -> Result<()> {
        let mut updated = Self::load(store, &self.current_id)?;
        updated.selected = updated
            .closed_sessions()
            .filter(|saved| self.selected.contains(&saved.id))
            .map(|saved| saved.id.clone())
            .collect();
        updated.selection = self
            .sessions
            .get(self.selection)
            .and_then(|saved| {
                updated
                    .sessions
                    .iter()
                    .position(|other| other.id == saved.id)
            })
            .unwrap_or(self.selection.min(updated.sessions.len().saturating_sub(1)));
        *self = updated;
        Ok(())
    }

    pub fn forget(&mut self, store: &SessionStore, ids: &[String]) {
        let result = store.forget(ids);
        let reloaded = self.reload(store);
        self.message = match result {
            Ok(count) => format!(
                "Forgot {count} review session{}.",
                if count == 1 { "" } else { "s" }
            ),
            Err(error) => format!("Forget failed: {error:#}"),
        };
        if let Err(error) = reloaded {
            self.message
                .push_str(&format!(" · Reload failed: {error:#}"));
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent, page: usize) -> Option<SessionAction> {
        if let Some(rename) = &mut self.renaming {
            self.message.clear();
            match key.code {
                KeyCode::Enter => {
                    return Some(SessionAction::Rename {
                        id: rename.id.clone(),
                        name: rename.value.clone(),
                    });
                }
                KeyCode::Esc => self.renaming = None,
                _ => rename.input(key),
            }
            return None;
        }
        if self.confirming {
            if key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
            {
                return None;
            }
            match key.code {
                KeyCode::Char('y' | 'Y') => {
                    self.confirming = false;
                    return Some(SessionAction::Forget(
                        self.selected.iter().cloned().collect(),
                    ));
                }
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('n' | 'N') => {
                    self.confirming = false;
                }
                _ => {}
            }
            return None;
        }
        self.message.clear();
        if key.modifiers.contains(KeyModifiers::ALT) {
            return None;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('f') => self.move_by(page as isize),
                KeyCode::Char('b') => self.move_by(-(page as isize)),
                KeyCode::Char('d') => self.move_by((page / 2).max(1) as isize),
                KeyCode::Char('u') => self.move_by(-((page / 2).max(1) as isize)),
                _ => {}
            }
            return None;
        }
        if std::mem::take(&mut self.first) && key.code == KeyCode::Char('g') {
            self.selection = 0;
            return None;
        }
        match key.code {
            KeyCode::Char('R') => {
                self.first = false;
                if let Some(saved) = self.sessions.get(self.selection) {
                    if self.open.contains(&saved.id) && saved.id != self.current_id {
                        self.message = "Session is open in another review.".into();
                    } else {
                        let value = saved.name.clone().unwrap_or_default();
                        self.renaming = Some(RenamePrompt {
                            id: saved.id.clone(),
                            cursor: value.len(),
                            value,
                        });
                    }
                }
            }
            KeyCode::Esc => return Some(SessionAction::Close),
            KeyCode::Char('r') => return Some(SessionAction::Reload),
            KeyCode::Char('j') | KeyCode::Down => self.move_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_by(-1),
            KeyCode::PageDown => self.move_by(page as isize),
            KeyCode::PageUp => self.move_by(-(page as isize)),
            KeyCode::Home => self.selection = 0,
            KeyCode::Char('g') => self.first = true,
            KeyCode::Char('G') | KeyCode::End => {
                self.selection = self.sessions.len().saturating_sub(1)
            }
            KeyCode::Char(' ') => {
                if let Some(saved) = self.sessions.get(self.selection) {
                    if self.open.contains(&saved.id) {
                        self.message = "Open sessions are protected.".into();
                    } else if !self.selected.remove(&saved.id) {
                        self.selected.insert(saved.id.clone());
                    }
                }
            }
            KeyCode::Char('a') => {
                let all: BTreeSet<_> = self
                    .closed_sessions()
                    .map(|saved| saved.id.clone())
                    .collect();
                if self.selected == all {
                    self.selected.clear();
                } else {
                    self.selected = all;
                }
            }
            KeyCode::Char('d') | KeyCode::Delete | KeyCode::Enter => {
                if self.selected.is_empty() {
                    self.message = "Select sessions with Space or a.".into();
                } else {
                    self.confirming = true;
                }
            }
            _ => {}
        }
        None
    }

    pub fn paste(&mut self, text: &str) {
        if let Some(rename) = &mut self.renaming {
            rename.insert(text);
        }
    }

    fn move_by(&mut self, amount: isize) {
        self.selection = self
            .selection
            .saturating_add_signed(amount)
            .min(self.sessions.len().saturating_sub(1));
    }
}
