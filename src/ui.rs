use crate::app::{App, Focus};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
};

pub struct Panes {
    header: Rect,
    pub tree: Rect,
    pub diff: Rect,
    footer: Rect,
}

pub fn panes(area: Rect, app: &App) -> Panes {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(area);
    let (tree, diff) =
        if app.review.view.fullscreen || (area.width < 55 && app.focus == Focus::Diff) {
            (Rect::default(), body)
        } else if area.width < 55 {
            (body, Rect::default())
        } else {
            let [tree, diff] = Layout::horizontal([
                Constraint::Length((area.width / 3).clamp(24, 42)),
                Constraint::Min(1),
            ])
            .areas(body);
            (tree, diff)
        };
    Panes {
        header,
        tree,
        diff,
        footer,
    }
}

pub fn display_path(path: &str) -> String {
    let mut displayed = String::with_capacity(path.len());
    for ch in path.chars() {
        if ch.is_control() {
            displayed.extend(ch.escape_default());
        } else {
            displayed.push(ch);
        }
    }
    displayed
}

pub fn draw(frame: &mut Frame, app: &App, diff: &Text<'static>) {
    let panes = panes(frame.area(), app);
    let viewed = app.review.viewed.len();
    let header = Line::from(vec![
        Span::styled(
            " review ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(
            "{} ({}) → {} ({})  ",
            display_path(&app.review.comparison.base_ref),
            app.review
                .comparison
                .base_id
                .get(..8)
                .unwrap_or(&app.review.comparison.base_id),
            display_path(&app.review.comparison.head_ref),
            app.review
                .comparison
                .head_id
                .get(..8)
                .unwrap_or(&app.review.comparison.head_id),
        )),
        Span::styled(
            format!("Viewed {viewed}/{}", app.review.changes.len()),
            Style::default().fg(Color::Green),
        ),
    ]);
    frame.render_widget(Paragraph::new(header), panes.header);
    if panes.tree.width > 0 {
        let height = panes.tree.height.saturating_sub(2) as usize;
        let offset = app
            .selection
            .saturating_sub(height / 2)
            .min(app.rows.len().saturating_sub(height));
        let items: Vec<_> = app
            .rows
            .iter()
            .skip(offset)
            .take(height)
            .map(|row| {
                let indent = "  ".repeat(row.depth);
                let name = row
                    .path
                    .trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .unwrap_or(&row.path);
                let line = if let Some(index) = row.file {
                    let change = &app.review.changes[index];
                    let (mark, color) = if row.viewed == 1 {
                        ("✓", Color::Green)
                    } else {
                        ("·", Color::Gray)
                    };
                    Line::from(vec![
                        Span::raw(indent),
                        Span::styled(format!("{mark} "), Style::default().fg(color)),
                        Span::raw(display_path(name)),
                        Span::styled(
                            format!("  {}", change.status),
                            Style::default().fg(Color::Yellow),
                        ),
                    ])
                } else {
                    let arrow = if app.review.view.collapsed.contains(&row.path)
                        && app.review.view.filter.is_empty()
                    {
                        "▸"
                    } else {
                        "▾"
                    };
                    Line::from(vec![
                        Span::raw(format!("{indent}{arrow} {}/ ", display_path(name))),
                        Span::styled(
                            format!("{}/{}", row.viewed, row.total),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ])
                };
                ListItem::new(line)
            })
            .collect();
        let title = if app.review.view.unviewed_only {
            " Unviewed files "
        } else {
            " Changed files "
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(border(app.focus == Focus::Tree));
        let mut selection = ListState::default().with_selected(Some(app.selection - offset));
        frame.render_stateful_widget(
            List::new(items).block(block).highlight_style(
                Style::default()
                    .bg(Color::Rgb(35, 45, 60))
                    .add_modifier(Modifier::BOLD),
            ),
            panes.tree,
            &mut selection,
        );
    }
    if panes.diff.width > 0 {
        let (title, metadata) = if let Some(index) = app.active {
            let change = &app.review.changes[index];
            let path = if change.old_path != change.path {
                format!(
                    "{} → {}",
                    display_path(&change.old_path),
                    display_path(&change.path)
                )
            } else {
                display_path(&change.path)
            };
            let old = if change.old.kind.is_empty() {
                "absent"
            } else {
                &change.old.kind
            };
            let new = if change.new.kind.is_empty() {
                "absent"
            } else {
                &change.new.kind
            };
            (
                format!(" {} {path} ", change.status),
                format!(
                    " {old}{} → {new}{} ",
                    if change.old.executable { " +x" } else { "" },
                    if change.new.executable { " +x" } else { "" }
                ),
            )
        } else {
            (" Review ".into(), String::new())
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .title_bottom(metadata)
            .border_style(border(app.focus == Focus::Diff));
        let position = app
            .review
            .view
            .positions
            .get(&app.review.view.active)
            .cloned()
            .unwrap_or_default();
        let visible = Text::from(
            diff.lines
                .iter()
                .skip(position.vertical)
                .take(panes.diff.height.saturating_sub(2) as usize)
                .cloned()
                .collect::<Vec<_>>(),
        );
        frame.render_widget(
            Paragraph::new(visible)
                .scroll((0, position.horizontal))
                .block(block),
            panes.diff,
        );
    }
    let footer = if app.searching {
        let value = if app.focus == Focus::Tree {
            &app.review.view.filter
        } else {
            &app.diff_search
        };
        format!("/{}▏", display_path(value))
    } else if !app.message.is_empty() {
        display_path(&app.message)
    } else if !app.review.view.filter.is_empty() {
        format!(
            "Filter: {}  Esc clear · ? help",
            display_path(&app.review.view.filter)
        )
    } else {
        "Space viewed · ]u next unviewed · / search · z full · i inline · r refresh · ? help".into()
    };
    frame.render_widget(
        Paragraph::new(footer).style(Style::default().fg(Color::Cyan)),
        panes.footer,
    );
    if app.help {
        let width = frame.area().width.min(74);
        let height = frame.area().height.min(23);
        let area = Rect::new(
            frame.area().x + (frame.area().width - width) / 2,
            frame.area().y + (frame.area().height - height) / 2,
            width,
            height,
        );
        frame.render_widget(Clear, area);
        let mut text = String::from(
            "j/k, arrows      Move in tree / scroll diff\nh/l              Collapse/expand / horizontal scroll\nTab, Ctrl-h/l    Focus tree/diff\ngg / G           First/last row or page\nCtrl-d/u, f/b    Half/full page down/up\nSpace            Toggle file / mark folder viewed\n]f / [f          Next/previous file\n]u / [u          Next/previous unviewed file\n/                Filter filenames / search diff\nn / N            Next/previous search match\nu                Toggle unviewed-only files\ni / z            Toggle inline / full-width diff\nr                Refresh revisions, keep matching progress\nEsc              Clear filters\nq / Ctrl-c       Save and quit\n",
        );
        if !app.bindings_help.is_empty() {
            text.push_str(&format!("\nCustom: {}\n", display_path(&app.bindings_help)));
        }
        text.push_str("\nPress any key to close");
        frame.render_widget(
            Paragraph::new(text).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Keyboard controls ")
                    .border_style(Style::default().fg(Color::Cyan)),
            ),
            area,
        );
    }
}

fn border(focused: bool) -> Style {
    Style::default().fg(if focused {
        Color::Cyan
    } else {
        Color::DarkGray
    })
}
