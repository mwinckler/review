use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    layout::{Constraint, Layout},
    style::{Color, Style},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};

use crate::{tree, ui::display_path};

pub enum Pick {
    Selected(usize),
    Reload,
    Cancel,
}

/// Startup pickers restore the terminal before transport commands or authentication run.
pub fn pick(title: &str, labels: &[String]) -> Result<Pick> {
    ratatui::run(|terminal| {
        let mut query = String::new();
        let mut searching = false;
        let mut selection = 0;
        loop {
            let rows: Vec<_> = labels
                .iter()
                .enumerate()
                .filter(|(_, label)| label.to_lowercase().contains(&query.to_lowercase()))
                .collect();
            selection = selection.min(rows.len().saturating_sub(1));
            let mut page = 1;
            terminal.draw(|frame| {
                let [list, search, controls] = Layout::vertical([
                    Constraint::Min(0),
                    Constraint::Length(1),
                    Constraint::Length(1),
                ])
                .areas(frame.area());
                let block = Block::default().borders(Borders::ALL).title(format!(
                    " {} · {} ",
                    display_path(title),
                    rows.len()
                ));
                page = block.inner(list).height.max(1) as usize;
                let visible = tree::visible_range(rows.len(), selection, page);
                let items: Vec<_> = rows
                    .iter()
                    .skip(visible.start)
                    .take(visible.len())
                    .map(|(_, label)| ListItem::new(display_path(label)))
                    .collect();
                let mut state = ListState::default()
                    .with_selected(Some(selection.saturating_sub(visible.start)));
                frame.render_stateful_widget(
                    List::new(items)
                        .block(block)
                        .highlight_style(Style::default().bg(Color::Rgb(35, 45, 60))),
                    list,
                    &mut state,
                );
                frame.render_widget(
                    Paragraph::new(format!(
                        "/{}{}",
                        display_path(&query),
                        if searching { "▏" } else { "" }
                    )),
                    search,
                );
                frame.render_widget(
                    Paragraph::new("j/k move · Enter select · / search · r reload · Esc cancel")
                        .style(Style::default().fg(Color::Cyan)),
                    controls,
                );
            })?;
            if !event::poll(Duration::from_millis(100))? {
                continue;
            }
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind == KeyEventKind::Release {
                continue;
            }
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                return Ok(Pick::Cancel);
            }
            if searching {
                match key.code {
                    KeyCode::Enter => searching = false,
                    KeyCode::Esc => {
                        searching = false;
                        query.clear();
                    }
                    KeyCode::Backspace => {
                        query.pop();
                        selection = 0;
                    }
                    KeyCode::Char(ch)
                        if !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        query.push(ch);
                        selection = 0;
                    }
                    _ => {}
                }
                continue;
            }
            match key.code {
                KeyCode::Char('j') | KeyCode::Down => {
                    selection = (selection + 1).min(rows.len().saturating_sub(1))
                }
                KeyCode::Char('k') | KeyCode::Up => selection = selection.saturating_sub(1),
                KeyCode::PageDown => {
                    selection = selection
                        .saturating_add(page)
                        .min(rows.len().saturating_sub(1))
                }
                KeyCode::PageUp => selection = selection.saturating_sub(page),
                KeyCode::Home | KeyCode::Char('g') => selection = 0,
                KeyCode::End | KeyCode::Char('G') => selection = rows.len().saturating_sub(1),
                KeyCode::Char('/') => searching = true,
                KeyCode::Char('r') => return Ok(Pick::Reload),
                KeyCode::Esc | KeyCode::Char('q') => return Ok(Pick::Cancel),
                KeyCode::Enter => {
                    if let Some((index, _)) = rows.get(selection) {
                        return Ok(Pick::Selected(*index));
                    }
                }
                _ => {}
            }
        }
    })
}
