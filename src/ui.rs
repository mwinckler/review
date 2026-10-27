use crate::{
    app::{App, Focus},
    tree,
    vcs::{Change, LineCounts},
};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
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
                Constraint::Length(app.review.view.tree_width(area.width)),
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

pub fn change_heading(change: &Change) -> (String, String) {
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
}

fn revision_label(reference: &str, commit_id: &str, change_id: Option<&str>) -> String {
    let change_id = change_id
        .map(|id| format!("{} ", id.get(..8).unwrap_or(id)))
        .unwrap_or_default();
    format!(
        "{} ({change_id}{})",
        display_path(reference),
        commit_id.get(..8).unwrap_or(commit_id),
    )
}

fn line_count_spans(counts: Option<LineCounts>) -> Vec<Span<'static>> {
    if let Some(counts) = counts {
        vec![
            Span::styled(
                format!("+{}", counts.added),
                Style::default().fg(Color::Green),
            ),
            Span::raw(" "),
            Span::styled(
                format!("-{}", counts.removed),
                Style::default().fg(Color::Red),
            ),
        ]
    } else {
        vec![Span::styled("+? -?", Style::default().fg(Color::DarkGray))]
    }
}

pub fn draw(frame: &mut Frame, app: &App, diff: &Text<'static>) {
    let panes = panes(frame.area(), app);
    let viewed = app.review.viewed.len();
    let comparison = &app.review.comparison;
    let header = Line::from(vec![
        Span::styled(
            " review ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(
            "{} → {}  ",
            revision_label(
                &comparison.base_ref,
                &comparison.base_id,
                comparison.base_change_id.as_deref()
            ),
            revision_label(
                &comparison.head_ref,
                &comparison.head_id,
                comparison.head_change_id.as_deref()
            ),
        )),
        Span::styled(
            format!("Viewed {viewed}/{}", app.review.changes.len()),
            Style::default().fg(Color::Green),
        ),
    ]);
    let selected_counts = comparison.file_line_counts.as_ref().and_then(|files| {
        app.preview_changes()
            .map(|change| files.get(&change.path).copied())
            .sum()
    });
    let selected_counts = Line::from(line_count_spans(selected_counts));
    let mut counts = vec![Span::raw("(")];
    counts.extend(line_count_spans(comparison.line_counts));
    counts.push(Span::raw(")"));
    let counts = Line::from(counts);
    let [info_area, counts_area] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(counts.width() as u16 + 1),
    ])
    .areas(panes.header);
    let [info_area, selected_counts_area] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(selected_counts.width() as u16 + 1),
    ])
    .areas(info_area);
    frame.render_widget(Paragraph::new(header), info_area);
    frame.render_widget(
        Paragraph::new(selected_counts).alignment(Alignment::Right),
        selected_counts_area,
    );
    frame.render_widget(
        Paragraph::new(counts).alignment(Alignment::Right),
        counts_area,
    );
    if panes.tree.width > 0 {
        let height = panes.tree.height.saturating_sub(2) as usize;
        let visible = tree::visible_range(app.rows.len(), app.selection, height);
        let offset = visible.start;
        let items: Vec<_> = app
            .rows
            .iter()
            .skip(offset)
            .take(visible.len())
            .map(|row| {
                let indent = "  ".repeat(row.depth);
                let (mark, color) = if row.file.is_some() {
                    if row.viewed == 1 {
                        ("✓", Color::Green)
                    } else {
                        ("·", Color::Gray)
                    }
                } else {
                    let arrow = if app.review.view.collapsed.contains(&row.path)
                        && app.review.view.filter.is_empty()
                    {
                        "▸"
                    } else {
                        "▾"
                    };
                    (arrow, Color::Reset)
                };
                let mut spans = vec![
                    Span::raw(indent),
                    Span::styled(format!("{mark} "), Style::default().fg(color)),
                    Span::raw(display_path(row.name())),
                ];
                if let Some(index) = row.file {
                    spans.push(Span::styled(
                        format!("  {}", app.review.changes[index].status),
                        Style::default().fg(Color::Yellow),
                    ));
                } else {
                    spans.push(Span::styled(
                        format!("/ {}/{}", row.viewed, row.total),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
                let style = if row.viewed == row.total {
                    Style::default().add_modifier(Modifier::DIM)
                } else {
                    Style::default()
                };
                ListItem::new(Line::from(spans)).style(style)
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
        let inner = block.inner(panes.tree);
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
        if let Some(flash) = &app.flash {
            let flash_style = Style::default()
                .add_modifier(Modifier::BOLD)
                .remove_modifier(Modifier::DIM);
            for (line, index) in visible.enumerate() {
                let row = &app.rows[index];
                let name = row.name();
                let prefix_width = row.depth * 2 + 2;
                let matches: Vec<_> = flash
                    .ranges(name)
                    .into_iter()
                    .map(|range| {
                        let start =
                            prefix_width + Span::raw(display_path(&name[..range.start])).width();
                        let end =
                            prefix_width + Span::raw(display_path(&name[..range.end])).width();
                        let suffix = Span::raw(display_path(&name[range.end..]));
                        let gap = suffix
                            .styled_graphemes(Style::default())
                            .map(|grapheme| Span::raw(grapheme.symbol).width())
                            .find(|width| *width > 0)
                            .unwrap_or(1);
                        (start..end, end + gap)
                    })
                    .collect();
                let y = inner.y + line as u16;
                for (range, _) in &matches {
                    let start = range.start.min(inner.width as usize);
                    let end = range.end.min(inner.width as usize);
                    frame.buffer_mut().set_style(
                        Rect::new(inner.x + start as u16, y, (end - start) as u16, 1),
                        flash_style.fg(Color::Yellow),
                    );
                }
                if let Some((_, label)) = flash.labels.iter().find(|(row, _)| *row == index) {
                    let label = Span::styled(
                        label.to_string(),
                        flash_style.fg(Color::Black).bg(Color::Yellow),
                    );
                    for (_, column) in &matches {
                        // Keep every match and its following character readable.
                        if *column < inner.width as usize
                            && matches
                                .iter()
                                .all(|(range, next)| !(range.start..*next).contains(column))
                        {
                            frame.render_widget(
                                &label,
                                Rect::new(inner.x + *column as u16, y, 1, 1),
                            );
                        }
                    }
                }
            }
        }
    }
    if panes.diff.width > 0 {
        let (title, metadata) = if let Some(index) = app.active {
            change_heading(&app.review.changes[index])
        } else if app.review.view.active.ends_with('/') {
            (
                format!(" {} ", display_path(&app.review.view.active)),
                format!(" Files: {} ", app.preview_changes().count()),
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
        let selected = app.selected_hunk().map(|hunk| &hunk.rows);
        let visible = Text::from(
            diff.lines
                .iter()
                .enumerate()
                .skip(position.vertical)
                .take(panes.diff.height.saturating_sub(2) as usize)
                .map(|(index, line)| {
                    let mut line = line.clone();
                    if selected.is_some_and(|rows| rows.contains(&index)) {
                        let background = Color::Rgb(35, 45, 60);
                        line.style = line.style.bg(background);
                        for span in &mut line.spans {
                            span.style = span.style.bg(background);
                        }
                    }
                    line
                })
                .collect::<Vec<_>>(),
        );
        frame.render_widget(
            Paragraph::new(visible)
                .scroll((0, position.horizontal))
                .block(block),
            panes.diff,
        );
    }
    let footer = if let Some(flash) = &app.flash {
        if flash.query.is_empty() {
            "Flash: type a filename fragment · Esc cancel".into()
        } else {
            format!(
                "Flash /{} · {} matches · label jump · Enter first · Esc cancel",
                display_path(&flash.query),
                flash.matches.len()
            )
        }
    } else if app.searching {
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
        "Space viewed + next · ]u next unviewed · ]h/[h hunks · e editor · / search · ? help".into()
    };
    frame.render_widget(
        Paragraph::new(footer).style(Style::default().fg(Color::Cyan)),
        panes.footer,
    );
    if app.help {
        let width = frame.area().width.min(74);
        let height = frame.area().height.min(25);
        let area = Rect::new(
            frame.area().x + (frame.area().width - width) / 2,
            frame.area().y + (frame.area().height - height) / 2,
            width,
            height,
        );
        frame.render_widget(Clear, area);
        let mut text = String::from(concat!(
            "j/k, arrows      Move in tree / scroll diff\n",
            "h/l              Collapse/expand / horizontal scroll\n",
            "Tab, Ctrl-h/l    Focus tree/diff\n",
            "Ctrl-Left/Right  Resize tree\n",
            "gg / G           First/last row or page\n",
            "Ctrl-d/u, f/b    Half/full page down/up\n",
            "Space            Toggle file viewed + next / mark folder\n",
            "]f / [f          Next/previous file\n",
            "]u / [u          Next/previous unviewed file\n",
            "]h / [h          Select next/previous diff hunk\n",
            "e                Open selected hunk/file in editor\n",
            "/                Filter filenames / search diff\n",
            "s                Flash jump in the tree from either pane\n",
            "n / N            Next/previous search match\n",
            "u                Toggle unviewed-only files\n",
            "i / z            Toggle inline / full-width diff\n",
            "r                Refresh revisions, keep matching progress\n",
            "Esc              Clear filters\n",
            "q / Ctrl-c       Save and quit\n",
        ));
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
