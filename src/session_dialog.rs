use std::collections::BTreeSet;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::store::{SavedSession, SessionStore};

#[derive(Debug, PartialEq, Eq)]
pub enum SessionAction {
    Open,
    Close,
    Reload,
    Forget(Vec<String>),
}

pub struct SessionDialog {
    pub sessions: Vec<SavedSession>,
    pub current_id: String,
    pub open: BTreeSet<String>,
    pub selected: BTreeSet<String>,
    pub selection: usize,
    pub confirming: bool,
    pub message: String,
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

    pub fn reload(&mut self, store: &SessionStore) -> Result<()> {
        let mut updated = Self::load(store, &self.current_id)?;
        updated.selected = updated
            .sessions
            .iter()
            .filter(|saved| self.selected.contains(&saved.id) && !updated.open.contains(&saved.id))
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

    pub fn handle_key(&mut self, key: KeyEvent, page: usize) -> Option<SessionAction> {
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
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('n' | 'N' | 'q') => {
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
            KeyCode::Esc | KeyCode::Char('q') => return Some(SessionAction::Close),
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
                    .sessions
                    .iter()
                    .filter(|saved| !self.open.contains(&saved.id))
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

    fn move_by(&mut self, amount: isize) {
        self.selection = self
            .selection
            .saturating_add_signed(amount)
            .min(self.sessions.len().saturating_sub(1));
    }
}
