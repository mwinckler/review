use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    widgets::{Block, Clear, Paragraph, Wrap},
};

use crate::{
    input,
    ui::{display_path, draw_single_line_field},
    vcs::{Backend, Comparison, ComparisonMode},
};

pub struct RevisionEdit {
    pub base: String,
    pub head: String,
    pub mode: ComparisonMode,
    pub working_tree: bool,
}

struct Field {
    value: String,
    cursor: usize,
}

impl Field {
    fn new(value: &str) -> Self {
        Self {
            value: value.to_owned(),
            cursor: value.len(),
        }
    }
}

pub struct RevisionDialog {
    base: Field,
    head: Field,
    editing_head: bool,
    mode: ComparisonMode,
    working_tree: Option<bool>,
    pub pending: bool,
    request: Option<RevisionEdit>,
    error: Option<String>,
}

impl RevisionDialog {
    pub(crate) fn new(comparison: &Comparison, backend: Backend) -> Self {
        let single = comparison.mode == ComparisonMode::SingleRevision;
        Self {
            base: Field::new(if single { "" } else { &comparison.base_ref }),
            head: Field::new(&comparison.head_ref),
            editing_head: single,
            mode: comparison.mode,
            working_tree: (backend == Backend::Git).then_some(comparison.working_tree.is_some()),
            pending: false,
            request: None,
            error: None,
        }
    }

    fn field(&mut self) -> &mut Field {
        if self.editing_head {
            &mut self.head
        } else {
            &mut self.base
        }
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> bool {
        if self.pending {
            return true;
        }
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => return false,
            (KeyCode::Enter, KeyModifiers::NONE) => {
                self.error = None;
                self.pending = true;
                self.request = Some(RevisionEdit {
                    base: self.base.value.trim().to_owned(),
                    head: self.head.value.trim().to_owned(),
                    mode: self.mode,
                    working_tree: self.working_tree.unwrap_or(false),
                });
            }
            (KeyCode::Tab | KeyCode::BackTab, _) => {
                if self.mode != ComparisonMode::SingleRevision {
                    self.editing_head = !self.editing_head;
                }
            }
            (KeyCode::Char('o'), KeyModifiers::CONTROL) => {
                self.mode = match self.mode {
                    ComparisonMode::Direct => ComparisonMode::MergeBase,
                    ComparisonMode::MergeBase => ComparisonMode::SingleRevision,
                    ComparisonMode::SingleRevision => ComparisonMode::Direct,
                };
                if self.mode == ComparisonMode::SingleRevision {
                    self.editing_head = true;
                }
            }
            (KeyCode::Char('w'), KeyModifiers::CONTROL) => {
                if let Some(enabled) = &mut self.working_tree {
                    *enabled = !*enabled;
                }
            }
            _ => {
                let field = self.field();
                input::single_line(&mut field.value, &mut field.cursor, key);
            }
        }
        true
    }

    pub fn paste(&mut self, text: &str) {
        if !self.pending {
            let field = self.field();
            input::insert_single_line(&mut field.value, &mut field.cursor, text);
        }
    }

    pub fn take_request(&mut self) -> Option<RevisionEdit> {
        self.request.take()
    }

    pub fn fail(&mut self, error: String) {
        self.pending = false;
        self.error = Some(error);
    }
}

impl RevisionDialog {
    pub(crate) fn draw(&self, frame: &mut Frame) {
        let screen = frame.area();
        let width = screen.width.min(78);
        let option_height = u16::from(self.working_tree.is_some());
        let dialog_height = if self.error.is_some() { 11 } else { 7 };
        let height = screen.height.min(dialog_height + option_height);
        let area = Rect::new(
            screen.x + (screen.width - width) / 2,
            screen.y + (screen.height - height) / 2,
            width,
            height,
        );
        frame.render_widget(Clear, area);
        let block = Block::bordered()
            .title(" Edit review revisions ")
            .border_style(Color::Cyan);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let single = self.mode == ComparisonMode::SingleRevision;
        let [mode, base, head, working_tree, status, controls] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(u16::from(!single)),
            Constraint::Length(1),
            Constraint::Length(option_height),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .areas(inner);
        let mode_name = match self.mode {
            ComparisonMode::Direct => "Direct comparison",
            ComparisonMode::MergeBase => "Merge-base comparison",
            ComparisonMode::SingleRevision => "Single revision against its parent",
        };
        frame.render_widget(Paragraph::new(mode_name), mode);
        if !single {
            draw_single_line_field(
                frame,
                base,
                "Base: ",
                &self.base.value,
                (!self.editing_head && !self.pending).then_some(self.base.cursor),
            );
        }
        draw_single_line_field(
            frame,
            head,
            if single { "Revision: " } else { "Head: " },
            &self.head.value,
            (self.editing_head && !self.pending).then_some(self.head.cursor),
        );
        if let Some(enabled) = self.working_tree {
            let checked = if enabled { 'x' } else { ' ' };
            frame.render_widget(
                Paragraph::new(format!("[{checked}] Include Git working tree (Ctrl-w)"))
                    .style(Style::default().fg(Color::Cyan)),
                working_tree,
            );
        }
        let controls_text = if self.pending {
            "Resolving review revisions…"
        } else {
            "Tab switch fields · Ctrl-o change mode · Enter apply · Esc cancel"
        };
        frame.render_widget(
            Paragraph::new(controls_text).style(Style::default().fg(Color::Cyan)),
            controls,
        );
        let message = self.error.as_ref().map_or_else(
            || {
                if self.working_tree == Some(true) {
                    "Use HEAD to compare against the checked-out commit and local edits.".to_owned()
                } else {
                    "An empty field uses the current revision.".to_owned()
                }
            },
            |error| format!("Revision change failed: {}", display_path(error)),
        );
        frame.render_widget(
            Paragraph::new(message)
                .wrap(Wrap { trim: true })
                .style(Style::default().fg(if self.error.is_some() {
                    Color::Yellow
                } else {
                    Color::Gray
                })),
            status,
        );
    }
}
