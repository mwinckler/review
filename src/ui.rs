use crate::{
    app::{App, Focus},
    comment_dialog::{CommentConfirmation, CommentDialog, DraftField},
    comments::DiffSide,
    session_dialog::SessionDialog,
    tree,
    vcs::{Change, Comparison, LineCounts},
};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{
        Block, Borders, Clear, List, ListItem, ListState, Paragraph, Scrollbar,
        ScrollbarOrientation, ScrollbarState, Wrap,
    },
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

fn revision_titles(comparison: &Comparison, available: usize) -> [Span<'static>; 2] {
    let titles = [
        comparison.base_title.as_deref(),
        comparison.head_title.as_deref(),
    ]
    .map(|title| {
        let title: String = title
            .unwrap_or_default()
            .lines()
            .next()
            .unwrap_or_default()
            .chars()
            .take(50)
            .collect();
        if title.is_empty() {
            String::new()
        } else {
            format!(" {}", display_path(&title))
        }
    });
    let [base_width, head_width] = titles
        .each_ref()
        .map(|title| Span::raw(title.as_str()).width());
    let head_width = head_width.min(available - base_width.min(available / 2));
    let base_width = base_width.min(available - head_width);
    [(&titles[0], base_width), (&titles[1], head_width)].map(|(title, width)| {
        let span = Span::raw(title.as_str());
        let mut columns = 0;
        let mut text: String = span
            .styled_graphemes(Style::default())
            .take_while(|grapheme| {
                columns += Span::raw(grapheme.symbol).width();
                columns <= width
            })
            .map(|grapheme| grapheme.symbol)
            .collect();
        if text == " " {
            text.clear();
        }
        Span::styled(text, Style::default().fg(Color::DarkGray))
    })
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
    let comments: Vec<_> = app
        .review
        .comments
        .iter()
        .map(|comment| (comment, comment.anchor.change(&app.review.changes)))
        .collect();
    let viewed = app.review.viewed.len();
    let comparison = &app.review.comparison;
    let mut header = Line::from(vec![
        Span::styled(
            " review ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            if app.session_label.is_empty() {
                String::new()
            } else {
                format!("{}  ", display_path(&app.session_label))
            },
            Style::default().fg(Color::Magenta),
        ),
    ]);
    let selected_counts = comparison.file_line_counts.as_ref().and_then(|files| {
        app.preview_changes()
            .map(|change| files.get(&change.path).copied())
            .sum()
    });
    let mut selected_counts = Line::from(line_count_spans(selected_counts));
    selected_counts.spans.push(Span::raw(" · "));
    let mut counts = line_count_spans(comparison.line_counts);
    counts.push(Span::raw(" total"));
    let total_end = counts.len();
    counts.push(Span::raw(" · "));
    counts.extend(line_count_spans(app.review.test_line_counts()));
    counts.push(Span::raw(" tests"));
    let mut counts = Line::from(counts);
    if counts.width() > panes.header.width as usize {
        counts.spans.truncate(total_end);
    }
    if selected_counts.width() + counts.width() <= panes.header.width as usize {
        selected_counts.spans.extend(counts.spans);
        counts = selected_counts;
    }
    let [info_area, counts_area] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(counts.width() as u16 + 1),
    ])
    .areas(panes.header);
    let base = revision_label(
        &comparison.base_ref,
        &comparison.base_id,
        comparison.base_change_id.as_deref(),
    );
    let head = revision_label(
        &comparison.head_ref,
        &comparison.head_id,
        comparison.head_change_id.as_deref(),
    );
    let progress = Span::styled(
        format!("Viewed {viewed}/{}", app.review.changes.len()),
        Style::default().fg(Color::Green),
    );
    let available = (info_area.width as usize).saturating_sub(
        header.width() + Span::raw(format!("{base} → {head}  ")).width() + progress.width(),
    );
    let [base_title, head_title] = revision_titles(comparison, available);
    header.spans.extend([
        Span::raw(base),
        base_title,
        Span::raw(" → "),
        Span::raw(head),
        head_title,
        Span::raw("  "),
        progress,
    ]);
    frame.render_widget(Paragraph::new(header), info_area);
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
                let count = comments
                    .iter()
                    .filter(|(comment, change)| {
                        let path = change
                            .map(|change| change.path.as_str())
                            .unwrap_or(&comment.anchor.path);
                        path == row.path || (row.path.ends_with('/') && path.starts_with(&row.path))
                    })
                    .count();
                if count > 0 {
                    spans.push(Span::styled(
                        format!("  ●{count}"),
                        Style::default().fg(Color::Magenta),
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
            let change = &app.review.changes[index];
            let (mut title, metadata) = change_heading(change);
            if let Some(ids) = comparison
                .file_commit_ids
                .as_ref()
                .and_then(|files| files.get(&change.path))
                && !ids.is_empty()
            {
                title.push_str(&format!("[{}] ", ids.join(", ")));
            }
            (title, metadata)
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
        let viewport = block.inner(panes.diff);
        let height = viewport.height as usize;
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
                .take(height)
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
        for source in app.source_lines.iter().filter(|source| {
            source.row >= position.vertical && source.row < position.vertical + height
        }) {
            let y = viewport.y + (source.row - position.vertical) as u16;
            if let Some(selection) = &app.line_selection
                && selection.contains(source)
            {
                let (start, end) = match (source.split_column, selection.side) {
                    (Some(split), DiffSide::Old) => (0, split),
                    (Some(split), DiffSide::New) => (
                        split,
                        position.horizontal as usize + viewport.width as usize,
                    ),
                    _ => (0, position.horizontal as usize + viewport.width as usize),
                };
                let start = start
                    .saturating_sub(position.horizontal as usize)
                    .min(viewport.width as usize);
                let end = end
                    .saturating_sub(position.horizontal as usize)
                    .min(viewport.width as usize);
                frame.buffer_mut().set_style(
                    Rect::new(
                        viewport.x + start as u16,
                        y,
                        end.saturating_sub(start) as u16,
                        1,
                    ),
                    Style::default().bg(Color::Rgb(55, 50, 25)),
                );
            }
            let has_comment = comments.iter().any(|(comment, change)| {
                change.is_some_and(|change| change.path == source.location.path)
                    && source.number(comment.anchor.side).is_some_and(|line| {
                        (comment.anchor.start_line..=comment.anchor.end_line).contains(&line)
                    })
            });
            if has_comment {
                frame.render_widget(
                    Span::styled("●", Style::default().fg(Color::Magenta)),
                    Rect::new(panes.diff.x, y, 1, 1),
                );
            }
        }
        if !viewport.is_empty() && diff.lines.len() > height {
            // ScrollbarState counts the possible top-row positions.
            let mut scrollbar = ScrollbarState::new(diff.lines.len() - height + 1)
                .position(position.vertical)
                .viewport_content_length(height);
            frame.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None)
                    .track_symbol(None)
                    .thumb_style(Style::default().fg(Color::Cyan)),
                Rect::new(panes.diff.right() - 1, viewport.y, 1, viewport.height),
                &mut scrollbar,
            );
        }
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
    } else if let Some(selection) = &app.line_selection {
        let target = selection.target();
        let (range, movement) = if selection.extending {
            (
                format!("{}-{}", target.start_line, target.end_line),
                "j/k extend · v single",
            )
        } else {
            (target.start_line.to_string(), "j/k move · v range")
        };
        format!(
            "{}:{range} ({}) · {movement} · h/l old/new · c comment · Esc cancel",
            display_path(&target.path),
            target.side.label()
        )
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
        "Space viewed · Enter line · v range · c comment · C comments · ? help".into()
    };
    frame.render_widget(
        Paragraph::new(footer).style(Style::default().fg(Color::Cyan)),
        panes.footer,
    );
    if app.help {
        let width = frame.area().width.min(74);
        let height = frame.area().height.min(30);
        let area = Rect::new(
            frame.area().x + (frame.area().width - width) / 2,
            frame.area().y + (frame.area().height - height) / 2,
            width,
            height,
        );
        frame.render_widget(Clear, area);
        let mut text = String::from(concat!(
            "j/k, arrows      Move in tree / scroll diff / move highlighted line\n",
            "Ctrl-n/p         Next/previous tree row from either pane\n",
            "h/l              Collapse/expand / horizontal scroll / old/new side\n",
            "Tab, Ctrl-h/l    Focus tree/diff\n",
            "Ctrl-Left/Right  Resize tree\n",
            "gg / G           First/last row or page\n",
            "Ctrl-d/u, f/b    Half/full page down/up\n",
            "Space            Toggle file viewed + next / mark folder\n",
            "]f / [f          Next/previous file\n",
            "]u / [u          Next/previous unviewed file\n",
            "]h / [h          Select next/previous diff hunk\n",
            "]Nh / [Nh        Select hunk number N (1-based)\n",
            "e                Open selected hunk/file in editor\n",
            "Enter            Highlight a source line in the diff\n",
            "c / v / C        Comment / select line range / comments\n",
            "/                Filter filenames / search diff\n",
            "s                Flash jump in the tree from either pane\n",
            "S                Manage / rename / forget review sessions\n",
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
    if let Some(dialog) = &app.session_dialog {
        draw_session_dialog(frame, dialog);
    }
    if let Some(dialog) = &app.comment_dialog {
        draw_comment_dialog(frame, app, dialog);
    }
}

fn draw_comment_dialog(frame: &mut Frame, app: &App, dialog: &CommentDialog) {
    let area = session_dialog_area(frame.area());
    frame.render_widget(Clear, area);
    let title = match dialog {
        CommentDialog::Compose(draft) if draft.id.is_some() => " Edit line comment ".to_owned(),
        CommentDialog::Compose(_) => " New line comment ".to_owned(),
        CommentDialog::Browse(_) => format!(" Comments · {} ", app.review.comments.len()),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    match dialog {
        CommentDialog::Compose(draft) => {
            let source = draft
                .selected_source()
                .unwrap_or_else(|error| error.to_string());
            let [path, range, quote, body, message, controls] = Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(source.lines().count().saturating_add(2).min(6) as u16),
                Constraint::Min(0),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .areas(inner);
            frame.render_widget(
                Paragraph::new(format!(
                    "{} ({})",
                    display_path(&draft.anchor.path),
                    draft.anchor.side.label()
                )),
                path,
            );
            let field_style = |field| {
                if draft.field == field {
                    Style::default().fg(Color::Black).bg(Color::Yellow)
                } else {
                    Style::default().fg(Color::Cyan)
                }
            };
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::raw("Lines: "),
                    Span::styled(display_path(&draft.start), field_style(DraftField::Start)),
                    Span::raw(" through "),
                    Span::styled(display_path(&draft.end), field_style(DraftField::End)),
                    Span::raw(" (inclusive)"),
                ])),
                range,
            );
            let source = Text::from(
                source
                    .lines()
                    .map(|line| Line::from(display_path(line)))
                    .collect::<Vec<_>>(),
            );
            frame.render_widget(
                Paragraph::new(source)
                    .block(Block::default().borders(Borders::ALL).title(" Source ")),
                quote,
            );
            let mut text = draft.body.clone();
            if draft.field == DraftField::Body {
                text.insert(draft.cursor, '▏');
            }
            let text = Text::from(
                text.split('\n')
                    .map(|line| Line::from(display_path(line)))
                    .collect::<Vec<_>>(),
            );
            let row = draft.body[..draft.cursor]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count();
            let column = Span::raw(display_path(
                draft.body[..draft.cursor]
                    .rsplit('\n')
                    .next()
                    .unwrap_or_default(),
            ))
            .width();
            let vertical = row.saturating_sub(body.height.saturating_sub(3) as usize);
            let horizontal = column.saturating_sub(body.width.saturating_sub(3) as usize);
            frame.render_widget(
                Paragraph::new(text)
                    .scroll((
                        vertical.min(u16::MAX as usize) as u16,
                        horizontal.min(u16::MAX as usize) as u16,
                    ))
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(" Comment ")
                            .border_style(border(draft.field == DraftField::Body)),
                    ),
                body,
            );
            frame.render_widget(
                Paragraph::new(display_path(&draft.message))
                    .style(Style::default().fg(Color::Yellow)),
                message,
            );
            frame.render_widget(
                Paragraph::new(
                    "Ctrl-s save locally · Enter newline · Tab line fields · Esc discard",
                )
                .style(Style::default().fg(Color::Cyan)),
                controls,
            );
        }
        CommentDialog::Browse(list) => {
            let [entries, detail, message, controls] = Layout::vertical([
                Constraint::Length(app.review.comments.len().clamp(1, 5) as u16),
                Constraint::Min(0),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .areas(inner);
            let visible = tree::visible_range(
                app.review.comments.len(),
                list.selection,
                entries.height as usize,
            );
            let offset = visible.start;
            let items: Vec<_> = app
                .review
                .comments
                .iter()
                .skip(offset)
                .take(visible.len())
                .map(|comment| {
                    let anchor = &comment.anchor;
                    let status = if anchor.is_outdated(&app.review.changes) {
                        " · outdated"
                    } else {
                        ""
                    };
                    let todo = if comment.source_todo.is_some() {
                        " · TODO written"
                    } else {
                        ""
                    };
                    ListItem::new(format!(
                        "{}:{}-{} ({}){status}{todo}",
                        display_path(&anchor.path),
                        anchor.start_line,
                        anchor.end_line,
                        anchor.side.label()
                    ))
                })
                .collect();
            let mut state =
                ListState::default().with_selected(Some(list.selection.saturating_sub(offset)));
            frame.render_stateful_widget(
                List::new(items).highlight_style(Style::default().bg(Color::Rgb(35, 45, 60))),
                entries,
                &mut state,
            );
            if let Some(comment) = app.review.comments.get(list.selection) {
                let mut text = vec![Line::styled(
                    "Original source",
                    Style::default().fg(Color::DarkGray),
                )];
                text.extend(
                    comment
                        .anchor
                        .excerpt
                        .lines()
                        .map(|line| Line::from(display_path(line))),
                );
                text.push(Line::default());
                text.push(Line::styled("Comment", Style::default().fg(Color::Magenta)));
                text.extend(
                    comment
                        .body
                        .lines()
                        .map(|line| Line::from(display_path(line))),
                );
                frame.render_widget(
                    Paragraph::new(text)
                        .wrap(Wrap { trim: false })
                        .scroll((list.scroll as u16, 0))
                        .block(Block::default().borders(Borders::ALL)),
                    detail,
                );
            } else {
                frame.render_widget(
                    Paragraph::new(
                        "No comments yet. Use c to comment on a line or v to select a range.",
                    ),
                    detail,
                );
            }
            let prompt = match list.confirming {
                Some(CommentConfirmation::Delete) => {
                    "Delete this local comment? y delete · n / Esc cancel".to_owned()
                }
                Some(CommentConfirmation::Todo) => {
                    "Write // TODO: into the working source file? y write · n / Esc cancel"
                        .to_owned()
                }
                None => display_path(&app.message),
            };
            frame.render_widget(
                Paragraph::new(prompt).style(Style::default().fg(Color::Yellow)),
                message,
            );
            frame.render_widget(Paragraph::new("j/k select · Enter jump · e edit · d delete · t TODO · PgUp/PgDn read · Esc close").style(Style::default().fg(Color::Cyan)), controls);
        }
    }
}

pub fn session_dialog_area(area: Rect) -> Rect {
    let width = area.width.min(100);
    let height = area.height.min(22);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

fn draw_session_dialog(frame: &mut Frame, dialog: &SessionDialog) {
    let area = session_dialog_area(frame.area());
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Review sessions ")
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [info, list, rename, message, controls] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(u16::from(dialog.renaming.is_some())),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);
    let available = dialog.closed_sessions().count();
    frame.render_widget(
        Paragraph::new(format!(
            "{} selected · {available} closed · open sessions protected from deletion",
            dialog.selected.len()
        )),
        info,
    );
    let visible = tree::visible_range(
        dialog.sessions.len(),
        dialog.selection,
        list.height as usize,
    );
    let offset = visible.start;
    let items: Vec<_> = dialog
        .sessions
        .iter()
        .skip(offset)
        .take(visible.len())
        .map(|saved| {
            let open = dialog.open.contains(&saved.id);
            let mark = if open {
                "[-] "
            } else if dialog.selected.contains(&saved.id) {
                "[x] "
            } else {
                "[ ] "
            };
            let prefix = saved.unique_prefix(&dialog.sessions);
            let mut spans = vec![
                Span::raw(mark),
                Span::styled(
                    prefix,
                    Style::default()
                        .fg(Color::Magenta)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    &saved.id[prefix.len()..],
                    Style::default().fg(Color::DarkGray),
                ),
                Span::raw(
                    saved
                        .name
                        .as_ref()
                        .map(|name| format!(" · {}", display_path(name)))
                        .unwrap_or_default(),
                ),
                Span::raw(format!(
                    "  {} → {}  Viewed {}/{}",
                    display_path(&saved.review.comparison.base_ref),
                    display_path(&saved.review.comparison.head_ref),
                    saved.review.viewed.len(),
                    saved.review.changes.len()
                )),
            ];
            if saved.id == dialog.current_id {
                spans.push(Span::styled(
                    "  (current)",
                    Style::default().fg(Color::Cyan),
                ));
            } else if open {
                spans.push(Span::styled("  (open)", Style::default().fg(Color::Yellow)));
            }
            let style = if open {
                Style::default().add_modifier(Modifier::DIM)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(spans)).style(style)
        })
        .collect();
    let mut selection =
        ListState::default().with_selected(Some(dialog.selection.saturating_sub(offset)));
    frame.render_stateful_widget(
        List::new(items).highlight_style(Style::default().bg(Color::Rgb(35, 45, 60))),
        list,
        &mut selection,
    );
    if dialog.sessions.is_empty() {
        frame.render_widget(Paragraph::new("No saved review sessions."), list);
    }
    if let Some(prompt) = &dialog.renaming {
        let prefix = format!("Rename {}: ", display_path(&prompt.id));
        let column = Span::raw(format!(
            "{prefix}{}",
            display_path(&prompt.value[..prompt.cursor])
        ))
        .width();
        let mut value = prompt.value.clone();
        value.insert(prompt.cursor, '▏');
        frame.render_widget(
            Paragraph::new(format!("{prefix}{}", display_path(&value)))
                .scroll((
                    0,
                    column
                        .saturating_sub(rename.width.saturating_sub(1) as usize)
                        .min(u16::MAX as usize) as u16,
                ))
                .style(Style::default().fg(Color::Cyan)),
            rename,
        );
        frame.render_widget(
            Paragraph::new(display_path(&dialog.message)).style(Style::default().fg(Color::Yellow)),
            message,
        );
        frame.render_widget(
            Paragraph::new("Enter save · Esc cancel · Ctrl-u clear · empty name clears")
                .style(Style::default().fg(Color::Cyan)),
            controls,
        );
    } else if dialog.confirming {
        frame.render_widget(
            Paragraph::new(format!(
                "Permanently delete {} saved reviews and their comments?",
                dialog.selected.len()
            ))
            .style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
            message,
        );
        frame.render_widget(
            Paragraph::new("y delete · n / Esc cancel").style(Style::default().fg(Color::Yellow)),
            controls,
        );
    } else {
        frame.render_widget(
            Paragraph::new(display_path(&dialog.message)).style(Style::default().fg(Color::Yellow)),
            message,
        );
        frame.render_widget(
            Paragraph::new(
                "j/k move · Space toggle · a all/none · R rename · d delete · r reload · Esc close",
            )
            .style(Style::default().fg(Color::Cyan)),
            controls,
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
