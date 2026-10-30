//! Vim editing state for locally composed review feedback.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use edtui::{
    EditorEventHandler, EditorMode, EditorState, Index2, Lines, RowIndex,
    actions::{self, Action},
    events::{KeyEventRegister, KeyInput},
};

use crate::input;

pub(crate) struct VimEditor {
    pub state: EditorState,
    handler: EditorEventHandler,
    count: Option<usize>,
    pending: String,
}

impl Default for VimEditor {
    fn default() -> Self {
        Self::new(Lines::default(), EditorMode::Insert)
    }
}

impl VimEditor {
    fn new(lines: Lines, mode: EditorMode) -> Self {
        let mut state = EditorState::new(lines);
        let mut handler = EditorEventHandler::vim_mode();
        if mode == EditorMode::Insert {
            handler.on_key_event(
                KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE),
                &mut state,
            );
        } else {
            state.mode = mode;
        }
        Self {
            state,
            handler,
            count: None,
            pending: String::new(),
        }
    }

    /// Keep the public draft text/cursor and the editor's character indices in sync.
    pub fn sync(&mut self, body: &str, cursor: usize) {
        let text = input::sanitize(body);
        if self.state.lines.to_string() != text {
            *self = Self::new(Lines::from(text), self.state.mode);
        }
        let prefix = input::sanitize(&body[..cursor]);
        self.state.cursor = Index2::new(
            prefix.bytes().filter(|byte| *byte == b'\n').count(),
            prefix
                .rsplit('\n')
                .next()
                .unwrap_or_default()
                .chars()
                .count(),
        );
    }

    pub fn reset_pending(&mut self) {
        self.handler = EditorEventHandler::vim_mode();
        self.count = None;
        self.pending.clear();
    }

    pub fn input(&mut self, body: &mut String, cursor: &mut usize, mut key: KeyEvent) {
        if !matches!(
            key.code,
            KeyCode::Char(_)
                | KeyCode::Up
                | KeyCode::Down
                | KeyCode::Left
                | KeyCode::Right
                | KeyCode::Enter
                | KeyCode::Esc
                | KeyCode::Backspace
                | KeyCode::Delete
                | KeyCode::Tab
                | KeyCode::Home
                | KeyCode::End
                | KeyCode::PageUp
                | KeyCode::PageDown
        ) {
            self.reset_pending();
            return;
        }
        self.sync(body, *cursor);
        if key.code == KeyCode::Char('[') && key.modifiers == KeyModifiers::CONTROL {
            key = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        }
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        if plain && let KeyCode::Char(ch) = key.code {
            key.modifiers.set(
                KeyModifiers::SHIFT,
                ch.is_uppercase() && self.state.mode != EditorMode::Search,
            );
        }
        if self.state.mode == EditorMode::Normal
            && self.pending.is_empty()
            && plain
            && let KeyCode::Char(digit @ '0'..='9') = key.code
            && (digit != '0' || self.count.is_some())
        {
            self.count = Some(
                self.count
                    .unwrap_or(0)
                    .saturating_mul(10)
                    .saturating_add(digit as usize - '0' as usize),
            );
            return;
        }
        let count = self
            .count
            .take()
            .unwrap_or(1)
            .min(body.chars().count().saturating_add(1));
        let mode = self.state.mode;
        let rows = self.state.lines.len();
        let row = self
            .state
            .selection
            .as_ref()
            .map(|selection| selection.start().row)
            .unwrap_or(self.state.cursor.row);
        let character_selection = self.state.selection.as_ref().is_some_and(|selection| {
            !selection.line_mode && selection.start().row == selection.end().row
        });
        let inner_word_delete =
            self.pending == "di" && matches!(key.code, KeyCode::Char('w' | 'W'));
        let counted = if mode == EditorMode::Normal && self.pending.is_empty() && plain && count > 1
        {
            counted_motion(key.code, count)
        } else {
            None
        };
        if let Some(action) = counted {
            let register = KeyEventRegister::n(vec![KeyInput::from(key)]);
            self.handler.key_handler.insert(register.clone(), action);
            self.handler.on_key_event(key, &mut self.state);
            self.handler.key_handler.insert(
                register,
                counted_motion(key.code, 1).expect("counted motion"),
            );
        } else {
            self.handler.on_key_event(key, &mut self.state);
        }
        // Character-wise edits retain the line break when edtui extracts a whole row.
        if self.state.lines.len() + 1 == rows
            && ((mode == EditorMode::Normal && self.state.mode == EditorMode::Insert)
                || (mode == EditorMode::Visual && character_selection)
                || inner_word_delete)
        {
            self.state.lines.insert(RowIndex::new(row), Vec::new());
            self.state.cursor = Index2::new(row, 0);
        }
        if mode == EditorMode::Normal && self.state.mode == EditorMode::Normal && plain {
            if let KeyCode::Char(ch) = key.code {
                match self.pending.as_str() {
                    "" if matches!(ch, 'c' | 'd' | 'y' | 'g' | 'f' | 't') => self.pending.push(ch),
                    "c" | "d" if matches!(ch, 'i' | 'f' | 't') => self.pending.push(ch),
                    _ => self.pending.clear(),
                }
            } else {
                self.pending.clear();
            }
        } else {
            self.pending.clear();
        }
        self.update(body, cursor);
    }

    pub fn paste(&mut self, body: &mut String, cursor: &mut usize, value: &str) {
        self.sync(body, *cursor);
        self.reset_pending();
        self.handler
            .on_paste_event(input::sanitize(value), &mut self.state);
        self.update(body, cursor);
    }

    fn update(&self, body: &mut String, cursor: &mut usize) {
        *body = self.state.lines.to_string();
        let start: usize = body
            .split_inclusive('\n')
            .take(self.state.cursor.row)
            .map(str::len)
            .sum();
        *cursor = start
            + body[start..]
                .split('\n')
                .next()
                .unwrap_or_default()
                .chars()
                .take(self.state.cursor.col)
                .map(char::len_utf8)
                .sum::<usize>();
    }
}

fn counted_motion(code: KeyCode, count: usize) -> Option<Action> {
    Some(match code {
        KeyCode::Char('h') | KeyCode::Left => actions::MoveBackward(count).into(),
        KeyCode::Char('j') | KeyCode::Down => actions::MoveDown(count).into(),
        KeyCode::Char('k') | KeyCode::Up => actions::MoveUp(count).into(),
        KeyCode::Char('l') | KeyCode::Right => actions::MoveForward(count).into(),
        KeyCode::Char('w') => actions::MoveWordForward(count).into(),
        KeyCode::Char('b') => actions::MoveWordBackward(count).into(),
        KeyCode::Char('e') => actions::MoveWordForwardToEndOfWord(count).into(),
        KeyCode::Char('x') => actions::RemoveChar(count).into(),
        _ => {
            return None;
        }
    })
}
