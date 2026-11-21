use crate::{
    app::{App, Focus},
    comment_dialog::{CommentConfirmation, CommentDialog, DraftField},
    comments::DiffSide,
    gitlab_comments::CommentEntry,
    gitlab_ui, markdown,
    restore::RestoreState,
    session_dialog::SessionDialog,
    state::Review,
    tree,
    vcs::{Change, Comparison, LineCounts},
    vim::VimEditor,
};
use edtui::{EditorMode, EditorTheme, EditorView};
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
use std::cell::RefCell;

pub struct Panes {
    mr_header: Rect,
    header: Rect,
    pub tree: Rect,
    pub diff: Rect,
    pub comments: Rect,
    footer: Rect,
}

pub fn panes(area: Rect, app: &App) -> Panes {
    let [mr_header, header, body, footer] = Layout::vertical([
        Constraint::Length(u16::from(app.review.gitlab.is_some())),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(area);
    let (tree, diff, comments) = if app.review.view.fullscreen {
        (Rect::default(), body, Rect::default())
    } else if area.width < 55 {
        match app.focus {
            Focus::Tree => (body, Rect::default(), Rect::default()),
            Focus::Diff => (Rect::default(), body, Rect::default()),
            Focus::Comments => (Rect::default(), Rect::default(), body),
        }
    } else if app.review.view.comments_visible && (area.width >= 80 || app.focus != Focus::Tree) {
        let tree_width = if area.width >= 80 {
            app.review.view.tree_width(area.width).min(area.width - 48)
        } else {
            0
        };
        let comments_width = app
            .review
            .view
            .comments_width(area.width)
            .min(area.width - tree_width - 24);
        let [tree, diff, comments] = Layout::horizontal([
            Constraint::Length(tree_width),
            Constraint::Min(0),
            Constraint::Length(comments_width),
        ])
        .areas(body);
        (tree, diff, comments)
    } else {
        let [tree, diff] = Layout::horizontal([
            Constraint::Length(app.review.view.tree_width(area.width)),
            Constraint::Min(1),
        ])
        .areas(body);
        (tree, diff, Rect::default())
    };
    Panes {
        mr_header,
        header,
        tree,
        diff,
        comments,
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

fn draw_render_throbber(frame: &mut Frame, viewport: Rect, phase: usize) {
    let height = viewport.height.min(2);
    let area = Rect::new(
        viewport.x,
        viewport.y + (viewport.height - height) / 2,
        viewport.width,
        height,
    );
    let label = Line::from("Calculating diff...").centered();
    if height < 2 {
        frame.render_widget(label, area);
        return;
    }
    const DOTS: [&str; 5] = ["● ", "● ", "● ", "● ", "●"];
    let dots = Line::from(
        DOTS.into_iter()
            .enumerate()
            .map(|(index, dot)| {
                let color = if index == phase % DOTS.len() {
                    Color::Cyan
                } else {
                    Color::DarkGray
                };
                Span::styled(dot, Style::default().fg(color))
            })
            .collect::<Vec<_>>(),
    );
    frame.render_widget(Paragraph::new(vec![dots.centered(), label]), area);
}

pub fn draw(frame: &mut Frame, app: &App, diff: &Text<'static>) {
    let panes = panes(frame.area(), app);
    let comments: Vec<_> = app
        .review
        .comment_entries()
        .map(|comment| (comment, comment.target(&app.review)))
        .collect();
    let viewed = app.review.viewed.len();
    let comparison = &app.review.comparison;
    if let Some(gitlab) = &app.review.gitlab {
        let mr = &gitlab.merge_request;
        let mut identity = format!(
            " GitLab !{} {} → {} · {} · {}",
            mr.iid, mr.source_branch, mr.target_branch, mr.title, gitlab.project.path
        );
        if let Some((_, name)) = app.session_label.split_once(" · ")
            && name != gitlab.label()
        {
            identity.push_str(&format!(" · {name}"));
        }
        frame.render_widget(
            Paragraph::new(display_path(&identity)).style(Style::default().fg(Color::Cyan)),
            panes.mr_header,
        );
    }
    let session_label = if app.review.gitlab.is_some() {
        app.session_label.split(" · ").next().unwrap_or_default()
    } else {
        &app.session_label
    };
    let mut header = Line::from(vec![
        Span::styled(
            " review ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            if session_label.is_empty() {
                String::new()
            } else {
                format!("{}  ", display_path(session_label))
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
        app.review
            .gitlab
            .as_ref()
            .map(|_| "base")
            .unwrap_or(&comparison.base_ref),
        &comparison.base_id,
        comparison.base_change_id.as_deref(),
    );
    let head = revision_label(
        app.review
            .gitlab
            .as_ref()
            .map(|_| "head")
            .unwrap_or_else(|| comparison.head_label()),
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
        let diff_file = if app.in_folder_diff() && app.file_headings.len() > 1 {
            app.diff_cursor_row()
                .and_then(|row| app.file_heading_at(row))
                .map(|heading| heading.path.as_str())
        } else {
            None
        };
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
                    .filter(|(comment, _)| {
                        comment.path(&app.review).is_some_and(|path| {
                            path == row.path
                                || (row.path.ends_with('/') && path.starts_with(&row.path))
                        })
                    })
                    .count();
                if count > 0 {
                    spans.push(Span::styled(
                        format!("  ●{count}"),
                        Style::default().fg(Color::Magenta),
                    ));
                }
                let mut style = Style::default();
                if row.viewed == row.total {
                    style = style.add_modifier(Modifier::DIM);
                }
                if diff_file == Some(row.path.as_str()) {
                    style = style.bg(Color::Rgb(21, 27, 36));
                }
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
            let (title, metadata) = change_heading(change);
            let mut title = Line::raw(title);
            let ids = app.revision_ids();
            if !ids.is_empty() {
                title.spans.push(Span::raw("["));
                for (index, id) in ids.iter().enumerate() {
                    if index > 0 {
                        title.spans.push(Span::raw(", "));
                    }
                    let style = match app.revisions {
                        Some(selection) if selection.cursor == index => Style::default()
                            .fg(Color::Black)
                            .bg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                        Some(selection) if selection.range().contains(&index) => Style::default()
                            .fg(Color::Yellow)
                            .bg(Color::Rgb(55, 50, 25)),
                        _ => Style::default(),
                    };
                    title.spans.push(Span::styled(id.clone(), style));
                }
                title.spans.push(Span::raw("] "));
            }
            (title, metadata)
        } else if app.review.view.active.ends_with('/') {
            (
                Line::raw(format!(" {} ", display_path(&app.review.view.active))),
                format!(" Files: {} ", app.preview_changes().count()),
            )
        } else {
            (Line::raw(" Review "), String::new())
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .title_bottom(metadata)
            .border_style(border(app.focus == Focus::Diff));
        let inner = block.inner(panes.diff);
        frame.render_widget(block, panes.diff);
        let [filter, viewport] = Layout::vertical([
            Constraint::Length(u16::from(app.revisions.is_some())),
            Constraint::Min(0),
        ])
        .areas(inner);
        if let Some(range) = app.revision_range_label() {
            frame.render_widget(
                Paragraph::new(format!("Only showing changes from {range}"))
                    .style(Style::default().fg(Color::Yellow)),
                filter,
            );
        }
        let waiting = app.waiting_for_preview();
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
                    if app.is_rendering_row(index) {
                        return Line::default();
                    }
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
        let paragraph = Paragraph::new(visible);
        let paragraph = if app.render_failed {
            paragraph.wrap(Wrap { trim: true })
        } else {
            paragraph.scroll((0, position.horizontal))
        };
        if waiting {
            draw_render_throbber(frame, viewport, app.render_activity);
        } else {
            frame.render_widget(paragraph, viewport);
        }
        for rows in &app.rendering_rows {
            let start = rows.start.max(position.vertical);
            let end = rows.end.min(position.vertical + height);
            if start < end {
                let y = viewport.y + (start - position.vertical) as u16;
                const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
                let glyph = SPINNER[app.render_activity % SPINNER.len()];
                frame.render_widget(
                    Span::styled(
                        format!("{glyph} Rendering…"),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Rect::new(viewport.x, y, viewport.width, 1),
                );
            }
        }
        let first_heading = app
            .file_headings
            .partition_point(|heading| heading.row < position.vertical);
        for heading in app.file_headings[first_heading..]
            .iter()
            .take_while(|heading| heading.row < position.vertical + height)
        {
            if let Some(line) = diff.lines.get(heading.row) {
                let row = heading.row - position.vertical;
                let start = line.width().saturating_sub(position.horizontal as usize);
                if start < viewport.width as usize {
                    let width = viewport.width - start as u16;
                    frame.render_widget(
                        Span::styled(
                            "─".repeat(width as usize),
                            Style::default().fg(Color::DarkGray),
                        ),
                        Rect::new(viewport.x + start as u16, viewport.y + row as u16, width, 1),
                    );
                }
            }
        }
        let selection_style = Style::default().bg(Color::Rgb(55, 50, 25));
        if !app.rendering
            && let Some(row) = app
                .diff_cursor_row()
                .filter(|_| {
                    app.focus == Focus::Diff
                        || app
                            .line_selection
                            .as_ref()
                            .is_some_and(|selection| selection.anchor == selection.cursor)
                })
                .filter(|row| (position.vertical..position.vertical + height).contains(row))
        {
            frame.buffer_mut().set_style(
                Rect::new(
                    viewport.x,
                    viewport.y + (row - position.vertical) as u16,
                    viewport.width,
                    1,
                ),
                selection_style,
            );
        }
        for source in app.source_lines.iter().filter(|source| {
            !app.rendering
                && source.row >= position.vertical
                && source.row < position.vertical + height
                && !app.is_rendering_row(source.row)
        }) {
            let y = viewport.y + (source.row - position.vertical) as u16;
            if let Some(selection) = &app.line_selection
                && selection.anchor != selection.cursor
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
                    selection_style,
                );
            }
            let has_comment = comments.iter().any(|(_, target)| {
                target.as_ref().is_some_and(|target| {
                    target.path == source.location.path
                        && source.number(target.side).is_some_and(|line| {
                            (target.start_line..=target.end_line).contains(&line)
                        })
                })
            });
            if has_comment {
                frame.render_widget(
                    Span::styled("●", Style::default().fg(Color::Magenta)),
                    Rect::new(panes.diff.x, y, 1, 1),
                );
            }
        }
        if !waiting && !viewport.is_empty() && diff.lines.len() > height {
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
    crate::comments_pane::draw(frame, app, panes.comments);
    let update_notice = || {
        app.update_available
            .as_ref()
            .map(|version| format!("Run `review update` to install {version}."))
    };
    let message_with_update = || {
        let mut message = display_path(&app.message);
        if let Some(notice) = update_notice()
            && Span::raw(message.as_str()).width() + 3 + notice.len() <= panes.footer.width as usize
        {
            message.push_str(" · ");
            message.push_str(&notice);
        }
        message
    };
    let footer = if matches!(
        app.restore,
        Some(RestoreState::Apply(_) | RestoreState::Applying)
    ) {
        "Restoring changes…".into()
    } else if let Some(flash) = &app.flash {
        if flash.query.is_empty() {
            "Flash: type a filename fragment · Esc cancel".into()
        } else {
            format!(
                "Flash /{} · {} matches · label jump · Enter first · Esc cancel",
                display_path(&flash.query),
                flash.matches.len()
            )
        }
    } else if app.focus == Focus::Comments && app.submission_dialog.is_some() {
        "Use Ctrl-o to choose a decision, Ctrl-s to preview, or Esc to close.".into()
    } else if app.comment_editor_active() {
        "Use Ctrl-h/j/k/l to change panes, Ctrl-s to save, or Ctrl-q to discard.".into()
    } else if app.focus == Focus::Comments {
        if app.message.is_empty() {
            update_notice().unwrap_or_else(|| {
                "Use j/k to select a thread, Enter to read it, or t to hide this pane.".into()
            })
        } else {
            message_with_update()
        }
    } else if app.searching {
        let value = if app.focus == Focus::Tree {
            &app.review.view.filter
        } else {
            &app.diff_search
        };
        format!("/{}▏", display_path(value))
    } else if !app.message.is_empty() {
        message_with_update()
    } else if let Some(selection) = &app.line_selection {
        let target = selection.target();
        let (range, movement, cancel) = if selection.extending {
            (
                format!("{}-{}", target.start_line, target.end_line),
                "j/k extend · v single",
                " · Esc cancel range",
            )
        } else {
            (target.start_line.to_string(), "j/k move · v range", "")
        };
        format!(
            "{}:{range} ({}) · {movement} · h/l old/new · D restore · o thread · c comment{cancel}",
            display_path(&target.path),
            target.side.label()
        )
    } else if !app.review.view.filter.is_empty() {
        format!(
            "Filter: {}  Esc clear · ? help",
            display_path(&app.review.view.filter)
        )
    } else {
        update_notice().unwrap_or_else(|| {
            "Press Space to mark viewed, t for threads, c to comment, or ? for help.".into()
        })
    };
    frame.render_widget(
        Paragraph::new(footer).style(Style::default().fg(Color::Cyan)),
        panes.footer,
    );
    crate::thread::draw(frame, app, panes.comments, panes.footer);
    if app.help {
        let width = frame.area().width.min(74);
        let height = frame.area().height.min(39);
        let area = Rect::new(
            frame.area().x + (frame.area().width - width) / 2,
            frame.area().y + (frame.area().height - height) / 2,
            width,
            height,
        );
        frame.render_widget(Clear, area);
        let mut text = String::from(concat!(
            "j/k, up/down     Move the selection or diff cursor.\n",
            "Shift-j/k        Scroll the diff by one line from any pane.\n",
            "Ctrl-e/y         Scroll the diff down/up without moving the cursor.\n",
            "Ctrl-n/p         Select the next/previous tree row from any pane.\n",
            "h/l              Collapse/expand a directory or choose the diff side.\n",
            "Shift-h/l        Scroll the diff horizontally.\n",
            "Tab, Ctrl-h/j/k/l Move between panes.\n",
            "Ctrl-arrows      Resize the active split.\n",
            "gg / G           First/last row or page\n",
            "Ctrl-d/u         Scroll the diff by half a page from either pane.\n",
            "Ctrl-f/b         Scroll the focused pane by a full page.\n",
            "Space            Mark viewed and advance, or unmark and expand a file.\n",
            ", / .            Select the previous/next file revision.\n",
            "< / >            Extend the selected revision range.\n",
            "]f / [f          Next/previous file\n",
            "]u / [u          Next/previous unviewed file\n",
            "]h / [h          Select next/previous diff hunk\n",
            "]Nh / [Nh        Select hunk number N (1-based)\n",
            "e                Open the selected diff line or file in the editor.\n",
            "Y                Copy selected file/folder's full path\n",
            "D                Restore the selected file or lines.\n",
            "Enter            Focus a file's diff or collapse a line range.\n",
            "c / v / C        Comment / select line range / comments\n",
            "t                Toggle the source-aligned comments pane.\n",
            "o / ]c / [c      Expand thread / next/previous thread\n",
            "Alt-p            Preview and submit a GitLab review.\n",
            "/                Filter filenames / search diff\n",
            "s                Flash jump in the tree from either pane\n",
            "S                Manage / rename / forget review sessions\n",
            "B                Edit the base and head of a local review.\n",
            "n / Alt-n        Go to the next or previous search match.\n",
            "Shift-n/p        Go to the next or previous file in a folder diff.\n",
            "u                Toggle unviewed-only files\n",
            "i / z            Toggle inline / full-width diff\n",
            "+ / -            Change context for the selected diff file.\n",
            "r                Refresh revisions, keep matching progress\n",
            "Esc              Clear filters\n",
            "q / Ctrl-c       Save and quit; q stays in a focused input.\n",
        ));
        if !app.bindings_help.is_empty() {
            text.push_str(&format!("\nCustom: {}\n", display_path(&app.bindings_help)));
        }
        text.push_str("\nPress Esc to close.");
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
    if let Some(dialog) = &app.revision_dialog {
        dialog.draw(frame);
    }
    if let Some(dialog) = &app.comment_dialog {
        draw_comment_dialog(frame, app, dialog);
    }
    if let Some(dialog) = &app.submission_dialog {
        gitlab_ui::draw_submission(frame, app, dialog);
    }
    if let Some(RestoreState::Confirm(restore)) = &app.restore {
        let screen = frame.area();
        let width = screen.width.min(76);
        let height = screen.height.min(7);
        let area = Rect::new(
            screen.x + (screen.width - width) / 2,
            screen.y + (screen.height - height) / 2,
            width,
            height,
        );
        let block = Block::bordered()
            .title(" Confirm restore ")
            .border_style(Color::Yellow);
        let inner = block.inner(area);
        let [keys, body] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(inner);
        frame.render_widget(Clear, area);
        frame.render_widget(block, area);
        frame.render_widget(
            Paragraph::new("Press y to restore or n / Esc to cancel."),
            keys,
        );
        frame.render_widget(
            Paragraph::new(format!(
                "{} from the review base into {}?",
                restore.label(),
                if app.backend == crate::vcs::Backend::Git {
                    "the working tree"
                } else {
                    "@"
                }
            ))
            .wrap(Wrap { trim: false }),
            body,
        );
    }
}

pub(crate) fn max_scroll(paragraph: &Paragraph<'_>, area: Rect) -> usize {
    paragraph
        .line_count(area.width)
        .saturating_sub(area.height as usize)
        .min(u16::MAX as usize)
}

pub(crate) fn comment_heading(entry: CommentEntry<'_>, review: &Review) -> String {
    let todo = if entry
        .local(review)
        .is_some_and(|comment| comment.source_todo.is_some())
    {
        " · TODO written"
    } else {
        ""
    };
    match entry {
        CommentEntry::Local(comment) => {
            let anchor = &comment.anchor;
            let status = if anchor.is_outdated(&review.changes) {
                " · outdated"
            } else {
                ""
            };
            format!(
                "{}:{}-{} ({}){status}{todo}{}",
                display_path(&anchor.path),
                anchor.start_line,
                anchor.end_line,
                anchor.side.label(),
                publication_status(review, &comment.id)
            )
        }
        CommentEntry::GitLab(discussion) => {
            let note = discussion.comments().next().expect("visible discussion");
            let location = if let Some(target) = discussion.location(review) {
                format!(
                    "{}:{}-{} ({})",
                    display_path(&target.path),
                    target.start_line,
                    target.end_line,
                    target.side.label()
                )
            } else {
                discussion
                    .path()
                    .map(display_path)
                    .unwrap_or_else(|| "MR discussion".into())
            };
            let status = if note.resolvable {
                if note.resolved {
                    " · resolved"
                } else {
                    " · open"
                }
            } else {
                ""
            };
            let outdated = if discussion.is_outdated(review) {
                " · outdated"
            } else {
                ""
            };
            let pending = crate::gitlab_review::pending_replies(review)
                .filter(|reply| reply.discussion_id == discussion.id)
                .count();
            let pending = if pending > 0 {
                format!(" · {pending} pending")
            } else {
                String::new()
            };
            format!(
                "{location} · GitLab @{}{status}{outdated}{todo} · {} replies{pending}",
                display_path(&note.author.username),
                discussion.comments().count().saturating_sub(1)
            )
        }
    }
}

fn publication_status(review: &Review, id: &str) -> &'static str {
    review
        .gitlab
        .as_ref()
        .and_then(|gitlab| gitlab.publication.comments.get(id))
        .map(|receipt| {
            if receipt.note_id.is_some() {
                " · published"
            } else if receipt.uncertain {
                " · GitLab outcome uncertain"
            } else {
                " · GitLab draft"
            }
        })
        .unwrap_or("")
}

pub(crate) fn comment_text(entry: CommentEntry<'_>, review: &Review, width: u16) -> Text<'static> {
    let mut text = Vec::new();
    match entry {
        CommentEntry::Local(comment) => {
            text.push(Line::styled(
                if comment.anchor.is_outdated(&review.changes) {
                    "This source is outdated."
                } else {
                    "Original source"
                },
                Style::default().fg(Color::DarkGray),
            ));
            text.extend(
                comment
                    .anchor
                    .excerpt
                    .lines()
                    .map(|line| Line::from(display_path(line))),
            );
            text.push(Line::default());
            text.push(Line::styled("Comment", Style::default().fg(Color::Magenta)));
            text.extend(markdown::render(&comment.body, width).lines);
        }
        CommentEntry::GitLab(discussion) => {
            for note in discussion.comments() {
                let created = note
                    .created_at
                    .as_ref()
                    .map(|created| format!(" · {created}"))
                    .unwrap_or_default();
                let status = if note.resolvable {
                    if note.resolved {
                        " · resolved"
                    } else {
                        " · open"
                    }
                } else {
                    ""
                };
                let outdated = if discussion.is_outdated(review) {
                    " · outdated"
                } else {
                    ""
                };
                text.push(Line::styled(
                    display_path(&format!(
                        "@{}{status}{outdated}{created}",
                        note.author.username
                    )),
                    Style::default().fg(Color::Magenta),
                ));
                text.extend(markdown::render(&note.body, width).lines);
                text.push(Line::default());
            }
            if let Some(gitlab) = &review.gitlab {
                for reply in gitlab
                    .replies
                    .iter()
                    .filter(|reply| reply.discussion_id == discussion.id)
                {
                    let receipt = gitlab.publication.comments.get(&reply.id);
                    if receipt
                        .and_then(|receipt| receipt.note_id)
                        .is_some_and(|id| discussion.comments().any(|note| note.id == id))
                    {
                        continue;
                    }
                    let label = if receipt.is_some_and(|receipt| receipt.note_id.is_some()) {
                        "Saved reply"
                    } else {
                        "Pending reply"
                    };
                    text.push(Line::styled(
                        format!("{label}{}", publication_status(review, &reply.id)),
                        Style::default().fg(Color::Yellow),
                    ));
                    text.extend(markdown::render(&reply.body, width).lines);
                    text.push(Line::default());
                }
                if let Some(note) = discussion.comments().next() {
                    text.push(Line::styled(
                        display_path(&format!(
                            "{}#note_{}",
                            gitlab.merge_request.web_url, note.id
                        )),
                        Style::default().fg(Color::Cyan),
                    ));
                }
            }
        }
    }
    Text::from(text)
}

fn draw_markdown_editor(
    frame: &mut Frame,
    area: Rect,
    content: (&str, usize),
    editor: &RefCell<VimEditor>,
    preview: bool,
    focused: bool,
    title: &str,
) {
    let (body, cursor) = content;
    let mut editor = editor.borrow_mut();
    editor.sync(body, cursor);
    let mode = editor.state.mode.name().to_uppercase();
    let mode = if editor.state.mode == EditorMode::Search {
        format!("{mode} /{}", display_path(&editor.state.search_pattern()))
    } else {
        mode
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(
            "{}· {mode} ",
            if preview { " Markdown preview " } else { title }
        ))
        .border_style(border(focused));
    let inner = block.inner(area);
    if inner.is_empty() {
        frame.render_widget(block, area);
        return;
    }
    if preview {
        frame.render_widget(
            Paragraph::new(markdown::render(body, inner.width))
                .wrap(Wrap { trim: false })
                .block(block),
            area,
        );
    } else {
        let mut theme = EditorTheme::default()
            .base(Style::default())
            .block(block)
            .hide_status_line()
            .cursor_style(Style::default().fg(Color::Black).bg(Color::Yellow))
            .selection_style(Style::default().bg(Color::Rgb(55, 50, 25)));
        if !focused {
            theme = theme.hide_cursor();
        }
        frame.render_widget(
            EditorView::new(&mut editor.state).wrap(true).theme(theme),
            area,
        );
    }
}

fn draw_comment_dialog(frame: &mut Frame, app: &App, dialog: &CommentDialog) {
    let area = if dialog.is_editor() {
        panes(frame.area(), app).comments
    } else {
        session_dialog_area(frame.area())
    };
    if area.is_empty() {
        return;
    }
    frame.render_widget(Clear, area);
    let title = match dialog {
        CommentDialog::Compose(draft) if draft.id.is_some() => " Edit line comment ".to_owned(),
        CommentDialog::Compose(_) => " New line comment ".to_owned(),
        CommentDialog::Browse(_) => {
            format!(
                " Comment threads · {} ",
                app.review.comment_entries().count()
            )
        }
        CommentDialog::Reply(_) => " Reply to GitLab thread ".into(),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(border(!dialog.is_editor() || app.focus == Focus::Comments));
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
                Constraint::Length(3),
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
                if app.focus == Focus::Comments && draft.field == field {
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
            draw_markdown_editor(
                frame,
                body,
                (&draft.body, draft.cursor),
                &draft.editor,
                draft.preview,
                app.focus == Focus::Comments && draft.field == DraftField::Body,
                " Comment · Markdown ",
            );
            frame.render_widget(
                Paragraph::new(display_path(&draft.message))
                    .style(Style::default().fg(Color::Yellow)),
                message,
            );
            frame.render_widget(
                Paragraph::new(
                    "Ctrl-s saves; Ctrl-q discards.\nCtrl-p previews; Tab changes fields.",
                )
                .wrap(Wrap { trim: false })
                .style(Style::default().fg(Color::Cyan)),
                controls,
            );
        }
        CommentDialog::Reply(draft) => {
            let [context, body, message, controls] = Layout::vertical([
                Constraint::Percentage(45),
                Constraint::Min(3),
                Constraint::Length(1),
                Constraint::Length(2),
            ])
            .areas(inner);
            if let Some(gitlab) = &app.review.gitlab
                && let Some(discussion) = gitlab
                    .discussions
                    .iter()
                    .find(|discussion| discussion.id == draft.discussion_id)
            {
                let block = Block::default()
                    .borders(Borders::ALL)
                    .title(" Conversation ");
                let content = block.inner(context);
                let paragraph = Paragraph::new(comment_text(
                    CommentEntry::GitLab(discussion),
                    &app.review,
                    content.width,
                ))
                .wrap(Wrap { trim: false });
                let scroll = max_scroll(&paragraph, content);
                frame.render_widget(paragraph.scroll((scroll as u16, 0)).block(block), context);
            }
            draw_markdown_editor(
                frame,
                body,
                (&draft.body, draft.cursor),
                &draft.editor,
                draft.preview,
                app.focus == Focus::Comments,
                " Reply · Markdown ",
            );
            frame.render_widget(
                Paragraph::new(display_path(&draft.message))
                    .style(Style::default().fg(Color::Yellow)),
                message,
            );
            frame.render_widget(
                Paragraph::new("Ctrl-s saves; Ctrl-q discards.\nCtrl-p previews the reply.")
                    .wrap(Wrap { trim: false })
                    .style(Style::default().fg(Color::Cyan)),
                controls,
            );
        }
        CommentDialog::Browse(list) => {
            let comments: Vec<_> = app.review.comment_entries().collect();
            let selection = list.selection.min(comments.len().saturating_sub(1));
            let [entries, message, controls] = Layout::vertical([
                Constraint::Min(0),
                Constraint::Length(1),
                Constraint::Length(2),
            ])
            .areas(inner);
            let visible = tree::visible_range(
                comments.len(),
                selection,
                (entries.height as usize / 2).max(1),
            );
            let offset = visible.start;
            let items: Vec<_> = comments
                .iter()
                .skip(offset)
                .take(visible.len())
                .map(|entry| {
                    ListItem::new(vec![
                        Line::raw(comment_heading(*entry, &app.review)),
                        crate::comments_pane::summary(*entry, app, entries.width, true, 1),
                    ])
                })
                .collect();
            let mut state =
                ListState::default().with_selected(Some(selection.saturating_sub(offset)));
            frame.render_stateful_widget(
                List::new(items).highlight_style(Style::default().bg(Color::Rgb(35, 45, 60))),
                entries,
                &mut state,
            );
            if comments.is_empty() {
                frame.render_widget(
                    Paragraph::new(
                        "No comments yet. Use c to comment on a line or v to select a range.",
                    ),
                    entries,
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
            let actions = if let Some(comment) = comments
                .get(selection)
                .and_then(|entry| entry.local(&app.review))
            {
                if app.review.comment_submitted(&comment.id) {
                    "Press t to write a source TODO."
                } else {
                    "Press e to edit, d to delete, or t to write a source TODO."
                }
            } else if matches!(comments.get(selection), Some(CommentEntry::GitLab(_))) {
                "Press R to reply or r to reload threads."
            } else {
                ""
            };
            frame.render_widget(
                Paragraph::new(format!(
                    "Use j/k to select a thread and Enter to open it in the comments pane.\n{actions}"
                ))
                .style(Style::default().fg(Color::Cyan)),
                controls,
            );
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
                    display_path(saved.review.comparison.head_label()),
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
        draw_single_line_field(frame, rename, &prefix, &prompt.value, Some(prompt.cursor));
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

pub(crate) fn draw_single_line_field(
    frame: &mut Frame,
    area: Rect,
    label: &str,
    value: &str,
    cursor: Option<usize>,
) {
    let mut text = value.to_owned();
    let (scroll, style) = if let Some(cursor) = cursor {
        let column = Span::raw(label).width() + Span::raw(display_path(&text[..cursor])).width();
        text.insert(cursor, '▏');
        (
            column
                .saturating_sub(area.width.saturating_sub(1) as usize)
                .min(u16::MAX as usize) as u16,
            Style::default().fg(Color::Cyan),
        )
    } else {
        (0, Style::default())
    };
    frame.render_widget(
        Paragraph::new(format!("{label}{}", display_path(&text)))
            .scroll((0, scroll))
            .style(style),
        area,
    );
}

fn border(focused: bool) -> Style {
    Style::default().fg(if focused {
        Color::Cyan
    } else {
        Color::DarkGray
    })
}
