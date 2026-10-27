use std::sync::atomic::AtomicU64;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Terminal,
    backend::TestBackend,
    layout::Rect,
    style::{Color, Modifier},
    text::Text,
};
use review::{
    app::{App, Focus},
    hunks::{Hunk, HunkLocation},
    renderer::{RenderOptions, RendererConfig, render_pair},
    state::Review,
    tree, ui,
    vcs::{Change, Comparison, Entry, LineCounts},
};

fn change(path: &str) -> Change {
    Change {
        path: path.into(),
        old_path: path.into(),
        status: "M".into(),
        old: Entry {
            object_id: "old".into(),
            kind: "file".into(),
            executable: false,
        },
        new: Entry {
            object_id: "new".into(),
            kind: "file".into(),
            executable: false,
        },
    }
}

fn review() -> Review {
    Review::new(
        Comparison {
            base_ref: "main".into(),
            head_ref: "feature".into(),
            base_id: "aaa".into(),
            head_id: "bbb".into(),
            base_change_id: None,
            head_change_id: None,
            mode: Default::default(),
            line_counts: Some(LineCounts::default()),
            file_line_counts: Some(
                [
                    (
                        "README.md".into(),
                        LineCounts {
                            added: 2,
                            removed: 1,
                        },
                    ),
                    (
                        "src/a.rs".into(),
                        LineCounts {
                            added: 3,
                            removed: 2,
                        },
                    ),
                    (
                        "src/nested/b.rs".into(),
                        LineCounts {
                            added: 5,
                            removed: 4,
                        },
                    ),
                ]
                .into(),
            ),
        },
        vec![
            change("README.md"),
            change("src/a.rs"),
            change("src/nested/b.rs"),
        ],
    )
}

fn key(app: &mut App, code: KeyCode) {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE), 10, 100, 100);
}

fn hunk_app(count: usize) -> App {
    let mut review = review();
    review.view.selected = "src/".into();
    let mut app = App::new(review);
    app.hunks = (0..count)
        .map(|index| Hunk {
            rows: index * 6..index * 6 + 4,
            location: HunkLocation {
                path: if index % 2 == 0 {
                    "src/a.rs"
                } else {
                    "src/nested/b.rs"
                }
                .into(),
                old_line: Some(index + 1),
                new_line: Some(index + 1),
            },
        })
        .collect();
    app
}

#[test]
fn numbered_hunk_shortcuts_are_absolute_one_based_and_keep_relative_navigation() {
    let mut app = hunk_app(12);
    for (sequence, index) in [
        ("]4h", 3),
        ("[4h", 3),
        ("]12h", 11),
        ("[10h", 9),
        ("]04h", 3),
        ("]1h", 0),
        ("]h", 1),
        ("[h", 0),
        ("[h", 11),
        ("]h", 0),
    ] {
        for ch in sequence.chars() {
            key(&mut app, KeyCode::Char(ch));
        }
        assert_eq!(app.selected_hunk(), Some(&app.hunks[index]), "{sequence}");
        assert_eq!(app.focus, Focus::Diff);
        assert!(app.message.starts_with(&format!("Hunk {}/12", index + 1)));
        if index == 3 {
            assert_eq!(app.position().vertical, 15);
            assert_eq!(app.editor_location().unwrap().path, "src/nested/b.rs");
            assert_eq!(app.editor_location().unwrap().new_line, Some(4));
        }
    }
}

#[test]
fn invalid_hunk_numbers_preserve_selection_and_scrolling_and_overflow_safely() {
    let mut app = hunk_app(4);
    for ch in "]2h".chars() {
        key(&mut app, KeyCode::Char(ch));
    }
    app.position().vertical = 35;
    app.position().horizontal = 7;
    for sequence in [
        "]0h".to_owned(),
        "[5h".into(),
        format!("]{}h", "9".repeat(100)),
    ] {
        for ch in sequence.chars() {
            key(&mut app, KeyCode::Char(ch));
        }
        assert_eq!(app.selected_hunk(), Some(&app.hunks[1]));
        assert_eq!(app.position().vertical, 35);
        assert_eq!(app.position().horizontal, 7);
        assert!(app.message.contains("between 1 and 4"), "{}", app.message);
    }
    for ch in "]h".chars() {
        key(&mut app, KeyCode::Char(ch));
    }
    assert_eq!(app.selected_hunk(), Some(&app.hunks[2]));
    let mut empty = hunk_app(0);
    for ch in "]1h".chars() {
        key(&mut empty, KeyCode::Char(ch));
    }
    assert!(empty.selected_hunk().is_none());
    assert!(empty.message.contains("no mapped hunks"));
}

#[test]
fn numbered_hunk_shortcuts_queue_in_order_while_the_preview_is_rendering() {
    for (sequence, index) in [("]4h", 3), ("]4h]h[2h[h", 0), ("[12h]h", 0)] {
        let mut app = hunk_app(12);
        app.rendering = true;
        let hunks = std::mem::take(&mut app.hunks);
        for ch in sequence.chars() {
            key(&mut app, KeyCode::Char(ch));
        }
        assert!(!app.apply_hunk_navigation(10, 100));
        assert!(app.selected_hunk().is_none());
        app.hunks = hunks;
        app.rendering = false;
        assert!(app.apply_hunk_navigation(10, 100));
        assert_eq!(app.selected_hunk(), Some(&app.hunks[index]), "{sequence}");
        assert!(!app.apply_hunk_navigation(10, 100));
    }
    let mut app = hunk_app(4);
    app.rendering = true;
    for ch in "]4h".chars() {
        key(&mut app, KeyCode::Char(ch));
    }
    key(&mut app, KeyCode::Esc);
    app.rendering = false;
    assert!(!app.apply_hunk_navigation(10, 100));
    assert!(app.selected_hunk().is_none());
}

#[test]
fn numbered_hunk_prefixes_cancel_cleanly_and_prompt_text_remains_literal() {
    let mut app = hunk_app(12);
    for ch in "]4".chars() {
        key(&mut app, KeyCode::Char(ch));
    }
    key(&mut app, KeyCode::Esc);
    for ch in "]h]4f".chars() {
        key(&mut app, KeyCode::Char(ch));
    }
    assert_eq!(app.review.view.active, "src/");
    assert_eq!(app.selected_hunk(), Some(&app.hunks[0]));
    key(&mut app, KeyCode::Char('/'));
    for ch in "]4h".chars() {
        key(&mut app, KeyCode::Char(ch));
    }
    assert_eq!(app.diff_search, "]4h");
    assert_eq!(app.selected_hunk(), Some(&app.hunks[0]));
    key(&mut app, KeyCode::Esc);
    key(&mut app, KeyCode::Char('s'));
    for ch in "]4h".chars() {
        key(&mut app, KeyCode::Char(ch));
    }
    assert_eq!(app.flash.as_ref().unwrap().query, "]4h");
    key(&mut app, KeyCode::Esc);
    for ch in "]4".chars() {
        key(&mut app, KeyCode::Char(ch));
    }
    app.handle_key(
        KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL),
        10,
        100,
        100,
    );
    assert_eq!(app.focus, Focus::Tree);
    assert_eq!(app.selected_hunk(), Some(&app.hunks[0]));
    for ch in "]h]f".chars() {
        key(&mut app, KeyCode::Char(ch));
    }
    assert_eq!(app.review.view.active, "src/a.rs");
    assert!(app.hunks.is_empty());
}

#[test]
fn changing_the_preview_cancels_an_unfinished_hunk_number() {
    let mut app = hunk_app(12);
    for ch in "]4".chars() {
        key(&mut app, KeyCode::Char(ch));
    }
    app.review.view.selected = "src/nested/b.rs".into();
    app.rebuild();
    app.hunks = hunk_app(4).hunks;
    key(&mut app, KeyCode::Char('h'));
    assert!(app.selected_hunk().is_none());
}

#[test]
fn hunk_navigation_selects_highlights_and_opens_the_correct_directory_descendant() {
    let mut review = review();
    review.view.selected = "src/".into();
    let mut app = App::new(review);
    app.hunks = vec![
        Hunk {
            rows: 3..8,
            location: HunkLocation {
                path: "src/a.rs".into(),
                old_line: Some(10),
                new_line: Some(12),
            },
        },
        Hunk {
            rows: 20..26,
            location: HunkLocation {
                path: "src/nested/b.rs".into(),
                old_line: Some(30),
                new_line: Some(32),
            },
        },
    ];
    for (prefix, expected) in [(']', 0), (']', 1), ('[', 0), ('[', 1)] {
        key(&mut app, KeyCode::Char(prefix));
        key(&mut app, KeyCode::Char('h'));
        assert_eq!(app.selected_hunk(), Some(&app.hunks[expected]));
        assert_eq!(app.focus, Focus::Diff);
    }
    key(&mut app, KeyCode::Char('e'));
    assert!(app.editor_requested);
    assert_eq!(app.editor_location().unwrap().path, "src/nested/b.rs");
    assert_eq!(app.editor_location().unwrap().new_line, Some(32));
    let selected = app.selected_hunk().unwrap().location.clone();
    for hunk in &mut app.hunks {
        hunk.rows.start += 5;
        hunk.rows.end += 5;
    }
    assert_eq!(app.selected_hunk().unwrap().location, selected);
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    let diff = Text::from(
        (0..40)
            .map(|index| {
                ratatui::text::Line::styled(
                    format!("line {index}"),
                    ratatui::style::Style::default().fg(Color::LightGreen),
                )
            })
            .collect::<Vec<_>>(),
    );
    app.position().vertical = app.selected_hunk().unwrap().rows.start;
    terminal.draw(|frame| ui::draw(frame, &app, &diff)).unwrap();
    let pane = ui::panes(Rect::new(0, 0, 100, 24), &app).diff;
    let cell = &terminal.backend().buffer()[(pane.x + 1, pane.y + 1)];
    assert_eq!(cell.fg, Color::LightGreen);
    assert_eq!(cell.bg, Color::Rgb(35, 45, 60));
    key(&mut app, KeyCode::Esc);
    assert!(app.selected_hunk().is_none());
    assert!(app.review.viewed.is_empty());
}

#[test]
fn changing_files_clears_stale_hunks_and_literal_prompt_input_keeps_editor_closed() {
    let mut app = App::new(review());
    app.hunks.push(Hunk {
        rows: 0..5,
        location: HunkLocation {
            path: "README.md".into(),
            old_line: Some(1),
            new_line: Some(1),
        },
    });
    key(&mut app, KeyCode::Char(']'));
    key(&mut app, KeyCode::Char('h'));
    key(&mut app, KeyCode::Char(']'));
    key(&mut app, KeyCode::Char('f'));
    assert!(app.hunks.is_empty());
    assert!(app.selected_hunk().is_none());
    app.focus = Focus::Tree;
    key(&mut app, KeyCode::Char('/'));
    key(&mut app, KeyCode::Char('e'));
    assert!(!app.editor_requested);
    key(&mut app, KeyCode::Esc);
    key(&mut app, KeyCode::Esc);
    let mut empty = review();
    empty.changes.clear();
    let mut app = App::new(empty);
    key(&mut app, KeyCode::Char('e'));
    assert!(app.editor_location().is_none());
}

#[test]
fn hunk_shortcuts_wait_for_the_current_preview_to_finish_rendering() {
    let mut app = App::new(review());
    app.rendering = true;
    for prefix in [']', ']', '['] {
        key(&mut app, KeyCode::Char(prefix));
        key(&mut app, KeyCode::Char('h'));
    }
    assert!(!app.apply_hunk_navigation(10, 100));
    app.hunks = vec![
        Hunk {
            rows: 0..5,
            location: HunkLocation {
                path: "README.md".into(),
                old_line: Some(1),
                new_line: Some(1),
            },
        },
        Hunk {
            rows: 20..25,
            location: HunkLocation {
                path: "README.md".into(),
                old_line: Some(20),
                new_line: Some(22),
            },
        },
    ];
    app.rendering = false;
    assert!(app.apply_hunk_navigation(10, 100));
    assert_eq!(app.selected_hunk().unwrap().location.new_line, Some(1));
    assert!(!app.apply_hunk_navigation(10, 100));
}

#[test]
fn hunk_navigation_centers_the_hunk_and_clamps_to_the_available_scroll_range() {
    for (page, lines, rows, expected) in [
        (10, 100, 40..45, 37),
        (11, 100, 40..44, 37),
        (10, 100, 0..4, 0),
        (10, 100, 97..100, 90),
        (10, 100, 20..70, 40),
        (10, 7, 2..5, 0),
        (1, 100, 40..45, 42),
    ] {
        let mut app = App::new(review());
        app.hunks.push(Hunk {
            rows: rows.clone(),
            location: HunkLocation {
                path: "README.md".into(),
                old_line: Some(1),
                new_line: Some(1),
            },
        });
        for prefix in [']', '['] {
            for ch in [prefix, 'h'] {
                app.handle_key(
                    KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
                    page,
                    lines,
                    100,
                );
            }
            assert_eq!(
                app.position().vertical,
                expected,
                "{rows:?}, page {page}, {lines} lines, {prefix}h"
            );
            assert_eq!(app.selected_hunk().unwrap().rows, rows);
        }
    }
}

#[test]
fn tree_collapses_filters_and_counts_viewed_files() {
    let mut review = review();
    review.toggle_viewed(1);
    let rows = tree::rows(&review);
    let src = rows.iter().find(|row| row.path == "src/").unwrap();
    assert_eq!((src.total, src.viewed), (2, 1));
    review.view.collapsed.insert("src/".into());
    assert_eq!(tree::rows(&review).len(), 2);
    review.view.filter = "b.rs".into();
    let rows = tree::rows(&review);
    assert!(rows.iter().any(|row| row.path == "src/nested/b.rs"));
    assert!(!rows.iter().any(|row| row.path == "README.md"));
    review.view.filter.clear();
    review.view.collapsed.clear();
    review.view.unviewed_only = true;
    assert!(!tree::rows(&review).iter().any(|row| row.path == "src/a.rs"));
}

#[test]
fn vim_navigation_marking_and_unviewed_jumps_work() {
    let mut app = App::new(review());
    assert_eq!(app.active, Some(0));
    key(&mut app, KeyCode::Char(' '));
    assert!(app.review.is_viewed(&app.review.changes[0]));
    assert_eq!(app.active, Some(1));
    key(&mut app, KeyCode::Char(']'));
    key(&mut app, KeyCode::Char('u'));
    assert_eq!(app.active, Some(2));
    key(&mut app, KeyCode::Char('['));
    key(&mut app, KeyCode::Char('u'));
    assert_eq!(app.active, Some(1));
    key(&mut app, KeyCode::Tab);
    assert_eq!(app.focus, Focus::Diff);
    key(&mut app, KeyCode::Char('j'));
    assert_eq!(app.review.view.positions["src/a.rs"].vertical, 1);
    key(&mut app, KeyCode::Char('g'));
    key(&mut app, KeyCode::Char('g'));
    assert_eq!(app.review.view.positions["src/a.rs"].vertical, 0);
    key(&mut app, KeyCode::Char('G'));
    assert_eq!(app.review.view.positions["src/a.rs"].vertical, 90);
}

#[test]
fn control_h_and_l_focus_panes_directly() {
    let mut app = App::new(review());
    app.handle_key(
        KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL),
        10,
        100,
        100,
    );
    assert_eq!(app.focus, Focus::Diff);
    assert!(app.review.view.diff_focused);
    key(&mut app, KeyCode::Char('z'));
    app.handle_key(
        KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL),
        10,
        100,
        100,
    );
    assert_eq!(app.focus, Focus::Tree);
    assert!(!app.review.view.fullscreen);
    assert!(!app.review.view.diff_focused);
}

#[test]
fn backspace_focuses_the_tree_but_edits_search_prompts() {
    let mut app = App::new(review());
    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::Backspace);
    assert_eq!(app.focus, Focus::Tree);
    key(&mut app, KeyCode::Char('/'));
    key(&mut app, KeyCode::Char('a'));
    key(&mut app, KeyCode::Backspace);
    assert!(app.searching);
    assert!(app.review.view.filter.is_empty());
}

#[test]
fn marking_a_file_advances_to_the_next_unviewed_file_from_either_pane() {
    for focus in [Focus::Tree, Focus::Diff] {
        let mut review = review();
        review.toggle_viewed(1);
        review
            .view
            .collapsed
            .extend(["src/".into(), "src/nested/".into()]);
        review
            .view
            .positions
            .entry("src/nested/b.rs".into())
            .or_default()
            .vertical = 7;
        let mut app = App::new(review);
        app.focus = focus;
        app.review.view.fullscreen = focus == Focus::Diff;

        key(&mut app, KeyCode::Char(' '));

        assert!(app.review.is_viewed(&app.review.changes[0]));
        assert!(!app.review.is_viewed(&app.review.changes[2]));
        assert_eq!(app.active, Some(2));
        assert_eq!(app.review.view.selected, "src/nested/b.rs");
        assert_eq!(app.rows[app.selection].file, Some(2));
        assert!(app.review.view.collapsed.is_empty());
        assert_eq!(app.position().vertical, 7);
        assert_eq!(app.focus, focus);
        assert_eq!(app.review.view.fullscreen, focus == Focus::Diff);
    }
}

#[test]
fn marking_a_file_wraps_to_an_earlier_unviewed_file() {
    let mut review = review();
    review.view.selected = "src/nested/b.rs".into();
    let mut app = App::new(review);

    key(&mut app, KeyCode::Char(' '));

    assert!(app.review.is_viewed(&app.review.changes[2]));
    assert_eq!(app.active, Some(0));
    assert_eq!(app.review.view.selected, "README.md");
}

#[test]
fn marking_in_unviewed_only_mode_advances_relative_to_the_marked_file() {
    for focus in [Focus::Tree, Focus::Diff] {
        for index in 0..3 {
            let mut review = review();
            review.view.selected = review.changes[index].path.clone();
            review.view.unviewed_only = true;
            let mut app = App::new(review);
            app.focus = focus;

            key(&mut app, KeyCode::Char(' '));

            let next = (index + 1) % 3;
            assert_eq!(app.active, Some(next));
            assert_eq!(app.review.view.selected, app.review.changes[next].path);
            assert_eq!(app.rows[app.selection].file, Some(next));
            assert!(app.review.is_viewed(&app.review.changes[index]));
            assert!(app.rows.iter().all(|row| row.file != Some(index)));
            assert_eq!(app.focus, focus);
        }
    }
}

#[test]
fn automatic_advance_respects_filename_filters() {
    let mut review = review();
    review.view.filter = ".rs".into();
    review.changes = vec![change("a/one.rs"), change("b/skip.txt"), change("c/two.rs")];
    let mut app = App::new(review);

    key(&mut app, KeyCode::Char(' '));

    assert_eq!(app.active, Some(2));
    assert_eq!(app.review.view.selected, "c/two.rs");
    assert!(!app.review.is_viewed(&app.review.changes[1]));
    assert_eq!(app.review.view.filter, ".rs");

    key(&mut app, KeyCode::Char(' '));
    assert_eq!(app.active, Some(2));
    assert_eq!(app.review.view.selected, "c/two.rs");
    assert!(app.message.contains("All matching files have been viewed"));
}

#[test]
fn unmarking_a_file_keeps_it_selected() {
    for focus in [Focus::Tree, Focus::Diff] {
        let mut review = review();
        review.toggle_viewed(1);
        review.view.selected = "src/a.rs".into();
        let mut app = App::new(review);
        app.focus = focus;

        key(&mut app, KeyCode::Char(' '));

        assert!(!app.review.is_viewed(&app.review.changes[1]));
        assert_eq!(app.active, Some(1));
        assert_eq!(app.review.view.selected, "src/a.rs");
        assert_eq!(app.focus, focus);
    }
}

#[test]
fn marking_the_last_unviewed_file_handles_completion_with_or_without_filtering() {
    for unviewed_only in [false, true] {
        let mut review = review();
        review.toggle_viewed(0);
        review.toggle_viewed(1);
        review.view.selected = "src/nested/b.rs".into();
        review.view.unviewed_only = unviewed_only;
        let mut app = App::new(review);

        key(&mut app, KeyCode::Char(' '));

        assert_eq!(app.review.viewed.len(), 3);
        assert!(app.message.contains("All matching files have been viewed"));
        if unviewed_only {
            assert_eq!(app.active, None);
            assert!(app.rows.is_empty());
        } else {
            assert_eq!(app.active, Some(2));
            assert_eq!(app.review.view.selected, "src/nested/b.rs");
        }
    }
}

#[test]
fn space_marks_all_descendants_of_a_collapsed_folder_viewed() {
    let mut review = review();
    review.changes.push(change("src-other/c.rs"));
    review.toggle_viewed(1);
    review.view.selected = "src/".into();
    review.view.collapsed.insert("src/".into());
    let mut app = App::new(review);

    key(&mut app, KeyCode::Char(' '));

    assert!(app.review.is_viewed(&app.review.changes[1]));
    assert!(app.review.is_viewed(&app.review.changes[2]));
    assert!(!app.review.is_viewed(&app.review.changes[0]));
    assert!(!app.review.is_viewed(&app.review.changes[3]));
    let folder = app.rows.iter().find(|row| row.path == "src/").unwrap();
    assert_eq!((folder.viewed, folder.total), (2, 2));
    assert!(app.review.view.collapsed.contains("src/"));
    assert!(app.dirty);

    key(&mut app, KeyCode::Char(' '));
    assert_eq!(app.review.viewed.len(), 2);
}

#[test]
fn space_on_a_folder_includes_files_hidden_by_filters() {
    for (filter, unviewed_only) in [("a.rs", false), ("", true)] {
        let mut review = review();
        review.toggle_viewed(1);
        review.view.selected = "src/".into();
        review.view.filter = filter.into();
        review.view.unviewed_only = unviewed_only;
        let mut app = App::new(review);

        key(&mut app, KeyCode::Char(' '));

        assert!(app.review.is_viewed(&app.review.changes[1]));
        assert!(app.review.is_viewed(&app.review.changes[2]));
        assert!(!app.review.is_viewed(&app.review.changes[0]));
        if unviewed_only {
            assert_eq!(app.active, Some(0));
            assert!(app.rows.iter().all(|row| !row.path.starts_with("src/")));
        }
    }
}

#[test]
fn space_on_a_nested_folder_marks_only_its_descendants() {
    let mut review = review();
    review.changes.push(change("src/nested-other/c.rs"));
    review.view.selected = "src/nested/".into();
    let mut app = App::new(review);

    key(&mut app, KeyCode::Char(' '));

    assert!(app.review.is_viewed(&app.review.changes[2]));
    assert_eq!(app.review.viewed.len(), 1);
}

#[test]
fn space_in_a_directory_diff_marks_all_descendants_viewed() {
    let mut app = App::new(review());
    key(&mut app, KeyCode::Char(']'));
    key(&mut app, KeyCode::Char('f'));
    key(&mut app, KeyCode::Char('h'));
    assert_eq!(app.review.view.selected, "src/");
    key(&mut app, KeyCode::Tab);

    key(&mut app, KeyCode::Char(' '));
    assert!(app.review.is_viewed(&app.review.changes[1]));
    assert!(app.review.is_viewed(&app.review.changes[2]));
    assert!(!app.review.is_viewed(&app.review.changes[0]));
    assert_eq!(app.review.view.active, "src/");

    key(&mut app, KeyCode::Char(' '));
    assert_eq!(app.review.viewed.len(), 2);
}

#[test]
fn highlighting_a_directory_previews_all_changed_descendants() {
    for (filter, unviewed_only) in [("", false), ("a.rs", false), ("", true)] {
        let mut review = review();
        review.changes.push(change("src-other/c.rs"));
        review.toggle_viewed(2);
        review.view.selected = "src/".into();
        review.view.filter = filter.into();
        review.view.unviewed_only = unviewed_only;
        review
            .view
            .collapsed
            .extend(["src/".into(), "src/nested/".into()]);
        let app = App::new(review);

        assert_eq!(app.review.view.active, "src/");
        assert_eq!(app.active, None);
        assert_eq!(
            app.preview_changes()
                .map(|change| change.path.as_str())
                .collect::<Vec<_>>(),
            ["src/a.rs", "src/nested/b.rs"]
        );
    }
}

#[test]
fn directory_scroll_positions_are_independent_and_resume_with_the_preview() {
    let mut app = App::new(review());
    key(&mut app, KeyCode::Char(']'));
    key(&mut app, KeyCode::Char('f'));
    app.position().vertical = 7;
    key(&mut app, KeyCode::Char('h'));
    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::Char('j'));
    assert_eq!(app.review.view.positions["src/"].vertical, 1);
    assert_eq!(app.review.view.positions["src/a.rs"].vertical, 7);
    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::Char(']'));
    key(&mut app, KeyCode::Char('f'));
    assert_eq!(app.active, Some(1));
    assert_eq!(app.position().vertical, 7);
    key(&mut app, KeyCode::Char('h'));
    key(&mut app, KeyCode::Tab);
    let mut resumed = App::new(app.review.clone());
    assert_eq!(resumed.review.view.active, "src/");
    assert_eq!(resumed.focus, Focus::Diff);
    assert_eq!(resumed.preview_changes().count(), 2);
    assert_eq!(resumed.position().vertical, 1);
}

#[test]
fn file_jumps_from_a_directory_start_at_its_tree_position() {
    let mut review = review();
    review.changes.insert(1, change("docs/a.txt"));
    review.view.selected = "src/".into();
    let mut app = App::new(review);
    key(&mut app, KeyCode::Char(']'));
    key(&mut app, KeyCode::Char('f'));
    assert_eq!(app.review.view.active, "src/a.rs");
    key(&mut app, KeyCode::Char('h'));
    key(&mut app, KeyCode::Char('['));
    key(&mut app, KeyCode::Char('f'));
    assert_eq!(app.review.view.active, "docs/a.txt");
}

#[test]
fn flash_directory_jumps_select_the_combined_preview() {
    let mut app = App::new(review());
    key(&mut app, KeyCode::Char('s'));
    key(&mut app, KeyCode::Char('n'));
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.review.view.active, "src/nested/");
    assert_eq!(app.active, None);
    assert_eq!(
        app.preview_changes()
            .map(|change| change.path.as_str())
            .collect::<Vec<_>>(),
        ["src/nested/b.rs"]
    );
}

#[test]
fn the_directory_preview_title_and_scroll_match_the_selected_folder() {
    let mut review = review();
    review.view.selected = "src/".into();
    let mut app = App::new(review);
    app.position().vertical = 3;
    let diff = Text::from(
        (0..30)
            .map(|index| ratatui::text::Line::raw(format!("directory line {index}")))
            .collect::<Vec<_>>(),
    );
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|frame| ui::draw(frame, &app, &diff)).unwrap();
    let pane = ui::panes(Rect::new(0, 0, 100, 24), &app).diff;
    let buffer = terminal.backend().buffer();
    let title: String = (pane.x..pane.right())
        .map(|x| buffer[(x, pane.y)].symbol())
        .collect();
    assert!(title.contains("src/"), "{title}");
    let body: String = (pane.x..pane.right())
        .map(|x| buffer[(x, pane.y + 1)].symbol())
        .collect();
    assert!(body.contains("directory line 3"));
}

#[test]
fn ratatui_renders_progress_tree_and_diff_at_small_sizes() {
    for (width, height) in [(100, 24), (30, 8), (1, 1)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let mut app = App::new(review());
        app.review.toggle_viewed(0);
        terminal
            .draw(|frame| ui::draw(frame, &app, &Text::raw("diff body")))
            .unwrap();
        if width == 100 {
            let buffer = terminal.backend().buffer();
            let output: String = buffer.content().iter().map(|cell| cell.symbol()).collect();
            assert!(output.contains("Viewed 1/3"));
            assert!(output.contains("README.md"));
            assert!(output.contains("diff body"));
        }
    }
}

#[test]
fn test_line_counts_match_exact_directory_components_and_filename_markers() {
    for (path, is_test) in [
        ("test/check.rs", true),
        ("tests/check.rs", true),
        ("spec/check.rs", true),
        ("specs/check.rs", true),
        ("src/test/nested/check.rs", true),
        ("src/tests/nested/check.rs", true),
        ("src/spec/nested/check.rs", true),
        ("src/specs/λ.rs", true),
        ("check.test.ts", true),
        ("src/check.spec.js", true),
        ("tests/check.test.spec.rs", true),
        (".test.rs", true),
        ("src/.spec.rs", true),
        ("src/tests/line\nbreak.rs", true),
        ("test", false),
        ("tests", false),
        ("src/spec", false),
        ("src/specs", false),
        ("contest/check.rs", false),
        ("test-data/check.rs", false),
        ("specification/check.rs", false),
        ("src/tests_extra/check.rs", false),
        ("src/check.test.folder/ordinary.rs", false),
        ("src/check.spec.folder/ordinary.rs", false),
        ("check.test", false),
        ("check.spec", false),
        ("check.tests.rs", false),
        ("tests.rs", false),
        ("src/Tests/check.rs", false),
        ("check.Test.rs", false),
    ] {
        let mut review = review();
        review.changes = vec![change(path)];
        let counts = LineCounts {
            added: 6,
            removed: 4,
        };
        review.comparison.file_line_counts = Some([(path.to_owned(), counts)].into());
        let expected = if is_test {
            counts
        } else {
            LineCounts::default()
        };
        assert_eq!(review.test_line_counts(), Some(expected), "{path}");
    }
}

#[test]
fn test_line_totals_follow_the_full_diff_while_filtering_marking_and_refreshing() {
    let mut review = review();
    for (path, added, removed) in [
        ("tests/math.test.rs", 10, 3),
        ("src/spec/helper.rs", 4, 2),
        ("src/specs/feature.js", 2, 1),
        ("src/widget.spec.ts", 3, 1),
        ("test/deleted.rs", 0, 7),
        ("specs/moved.rs", 2, 1),
        ("tests/data.bin", 0, 0),
        ("src/contest/ordinary.rs", 100, 100),
    ] {
        let mut file = change(path);
        if path == "test/deleted.rs" {
            file.status = "D".into();
            file.new.kind.clear();
        }
        if path == "specs/moved.rs" {
            file.status = "R".into();
            file.old_path = "test/original.rs".into();
        }
        review.changes.push(file);
        review
            .comparison
            .file_line_counts
            .as_mut()
            .unwrap()
            .insert(path.into(), LineCounts { added, removed });
    }
    review.comparison.line_counts = Some(LineCounts {
        added: 131,
        removed: 122,
    });
    let mut app = App::new(review);
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    for stage in 0..3 {
        if stage == 1 {
            for index in 3..10 {
                app.review.toggle_viewed(index);
            }
            app.review.view.filter = "README".into();
            app.review.view.unviewed_only = true;
            app.review
                .view
                .collapsed
                .extend(["tests/".into(), "src/".into()]);
            app.rebuild();
        }
        if stage == 2 {
            let mut comparison = app.review.comparison.clone();
            comparison.file_line_counts.as_mut().unwrap().insert(
                "tests/math.test.rs".into(),
                LineCounts {
                    added: 12,
                    removed: 4,
                },
            );
            comparison.line_counts = Some(LineCounts {
                added: 133,
                removed: 123,
            });
            app.review.reconcile(comparison, app.review.changes.clone());
            app.rebuild();
        }
        terminal
            .draw(|frame| ui::draw(frame, &app, &Text::raw("diff")))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let top: String = (0..120).map(|x| buffer[(x, 0)].symbol()).collect();
        let expected = if stage == 2 {
            "(+133 -123 tests: +23 -16)"
        } else {
            "(+131 -122 tests: +21 -15)"
        };
        assert!(top.ends_with(expected), "{top}");
        let start = top.rfind("tests: ").unwrap() as u16 + 7;
        assert_eq!(buffer[(start, 0)].fg, Color::Green);
        assert_eq!(buffer[(start + 4, 0)].fg, Color::Red);
    }
}

#[test]
fn overall_line_counts_are_right_justified_and_colored_on_the_top_line() {
    for counts in [
        LineCounts {
            added: 1234,
            removed: 56,
        },
        LineCounts::default(),
    ] {
        for width in [120, 50, 24, 23, 20, 19, 11] {
            let mut review = review();
            review.comparison.base_ref = "a-very-long-base-branch-name".repeat(5);
            review.comparison.line_counts = Some(counts);
            let app = App::new(review);
            let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
            terminal
                .draw(|frame| ui::draw(frame, &app, &Text::raw("diff body")))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let top: String = (0..width).map(|x| buffer[(x, 0)].symbol()).collect();
            let full = format!("(+{} -{} tests: +0 -0)", counts.added, counts.removed);
            let expected = if full.len() <= width as usize {
                full
            } else {
                format!("(+{} -{})", counts.added, counts.removed)
            };
            assert!(top.ends_with(&expected), "{top:?}");
            assert_eq!(
                buffer[(width - expected.len() as u16 + 1, 0)].fg,
                Color::Green
            );
            assert_eq!(
                buffer[(
                    width - expected.len() as u16 + format!("(+{} ", counts.added).len() as u16,
                    0
                )]
                    .fg,
                Color::Red
            );
        }
    }
}

#[test]
fn jj_header_shows_change_ids_alongside_the_pinned_commit_hashes() {
    let mut review = review();
    review.comparison.base_id = "abcdef1234567890".into();
    review.comparison.head_id = "1234567890abcdef".into();
    review.comparison.base_change_id = Some("ukrwmnxyzqst".into());
    review.comparison.head_change_id = Some("vqmrstuvwxyz".into());
    let app = App::new(review);
    for width in [120, 50, 9, 1] {
        let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
        terminal
            .draw(|frame| ui::draw(frame, &app, &Text::raw("diff")))
            .unwrap();
        let top: String = (0..width)
            .map(|x| terminal.backend().buffer()[(x, 0)].symbol())
            .collect();
        if width == 120 {
            assert!(
                top.contains("main (ukrwmnxy abcdef12) → feature (vqmrstuv 12345678)"),
                "{top}"
            );
            assert!(top.contains("Viewed 0/3"));
        }
        if width >= 9 {
            let expected = if width >= 20 {
                "(+0 -0 tests: +0 -0)"
            } else {
                "(+0 -0)"
            };
            assert!(top.ends_with(expected), "{top}");
        }
    }
}

#[test]
fn git_and_legacy_headers_keep_their_commit_hashes() {
    let app = App::new(review());
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal
        .draw(|frame| ui::draw(frame, &app, &Text::raw("diff")))
        .unwrap();
    let top: String = (0..100)
        .map(|x| terminal.backend().buffer()[(x, 0)].symbol())
        .collect();
    assert!(top.contains("main (aaa) → feature (bbb)"), "{top}");
}

#[test]
fn overall_line_counts_cover_the_full_comparison_while_filtering_or_marking() {
    let mut review = review();
    review.comparison.line_counts = Some(LineCounts {
        added: 123,
        removed: 45,
    });
    review.view.filter = "b.rs".into();
    review.view.unviewed_only = true;
    let mut app = App::new(review);
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    for _ in 0..2 {
        terminal
            .draw(|frame| ui::draw(frame, &app, &Text::raw("diff body")))
            .unwrap();
        let top: String = (0..100)
            .map(|x| terminal.backend().buffer()[(x, 0)].symbol())
            .collect();
        assert!(top.ends_with("(+123 -45 tests: +0 -0)"));
        key(&mut app, KeyCode::Char(' '));
    }
}

#[test]
fn highlighted_line_counts_track_file_and_folder_navigation_from_either_pane() {
    let mut app = App::new(review());
    app.review.comparison.line_counts = Some(LineCounts {
        added: 10,
        removed: 7,
    });
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    for (keys, expected) in [
        ("", "+2 -1 (+10 -7 tests: +0 -0)"),
        ("j", "+8 -6 (+10 -7 tests: +0 -0)"),
        ("j", "+3 -2 (+10 -7 tests: +0 -0)"),
        ("j", "+5 -4 (+10 -7 tests: +0 -0)"),
        ("\t", "+5 -4 (+10 -7 tests: +0 -0)"),
        ("j", "+5 -4 (+10 -7 tests: +0 -0)"),
    ] {
        for ch in keys.chars() {
            key(
                &mut app,
                if ch == '\t' {
                    KeyCode::Tab
                } else {
                    KeyCode::Char(ch)
                },
            );
        }
        terminal
            .draw(|frame| ui::draw(frame, &app, &Text::raw("diff")))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let top: String = (0..120).map(|x| buffer[(x, 0)].symbol()).collect();
        assert!(top.ends_with(expected), "{top:?}");
        let start = 120 - expected.len() as u16;
        assert_eq!(buffer[(start, 0)].fg, Color::Green);
        assert_eq!(buffer[(start + 3, 0)].fg, Color::Red);
    }
}

#[test]
fn highlighted_folder_line_counts_include_all_descendants_hidden_by_the_tree() {
    for (filter, unviewed_only) in [("", false), ("a.rs", false), ("", true)] {
        let mut review = review();
        review.changes.push(change("src-other/a.rs"));
        review.comparison.file_line_counts.as_mut().unwrap().insert(
            "src-other/a.rs".into(),
            LineCounts {
                added: 100,
                removed: 100,
            },
        );
        review.toggle_viewed(2);
        review.view.selected = "src/".into();
        review
            .view
            .collapsed
            .extend(["src/".into(), "src/nested/".into()]);
        review.view.filter = filter.into();
        review.view.unviewed_only = unviewed_only;
        let app = App::new(review);
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal
            .draw(|frame| ui::draw(frame, &app, &Text::raw("diff")))
            .unwrap();
        let top: String = (0..100)
            .map(|x| terminal.backend().buffer()[(x, 0)].symbol())
            .collect();
        assert!(top.ends_with("+8 -6 (+0 -0 tests: +0 -0)"), "{top:?}");
    }
}

#[test]
fn empty_selections_and_unavailable_statistics_render_safely() {
    for missing in [false, true] {
        let mut review = review();
        let expected = if missing {
            review.comparison.file_line_counts = None;
            review.comparison.line_counts = None;
            "+? -? (+? -? tests: +? -?)"
        } else {
            review.changes.clear();
            "+0 -0 (+0 -0 tests: +0 -0)"
        };
        let app = App::new(review);
        for width in [120, 20, 9, 1, 0] {
            let mut terminal = Terminal::new(TestBackend::new(width, 8)).unwrap();
            terminal
                .draw(|frame| ui::draw(frame, &app, &Text::raw("diff")))
                .unwrap();
            if width as usize >= expected.len() {
                let top: String = (0..width)
                    .map(|x| terminal.backend().buffer()[(x, 0)].symbol())
                    .collect();
                assert!(top.ends_with(expected), "{top:?}");
            }
        }
    }
}

#[test]
fn viewed_files_and_fully_viewed_folders_are_dimmed() {
    for complete in [false, true] {
        let mut review = review();
        review.toggle_viewed(0);
        review.toggle_viewed(1);
        if complete {
            review.toggle_viewed(2);
        }
        let app = App::new(review);
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal
            .draw(|frame| ui::draw(frame, &app, &Text::raw("diff")))
            .unwrap();
        for (index, dimmed) in [true, complete, true, complete, complete]
            .into_iter()
            .enumerate()
        {
            let cell = &terminal.backend().buffer()[(1, index as u16 + 2)];
            assert_eq!(
                cell.modifier.contains(Modifier::DIM),
                dimmed,
                "{}",
                app.rows[index].path
            );
        }
        assert!(
            terminal.backend().buffer()[(1, 2)]
                .modifier
                .contains(Modifier::BOLD)
        );
    }
}

#[test]
fn control_arrows_resize_the_tree_from_either_pane_and_keep_both_panes_usable() {
    let area = Rect::new(0, 0, 100, 24);
    for focus in [Focus::Tree, Focus::Diff] {
        let mut app = App::new(review());
        app.focus = focus;
        let right = KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL);
        let left = KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL);
        app.handle_key(right, 20, 100, area.width);
        assert_eq!(ui::panes(area, &app).tree.width, 35);
        assert_eq!(app.focus, focus);
        assert_eq!(app.position().horizontal, 0);
        let resumed = App::new(app.review.clone());
        assert_eq!(ui::panes(area, &resumed).tree.width, 35);

        for _ in 0..100 {
            app.handle_key(right, 20, 100, area.width);
        }
        assert_eq!(ui::panes(area, &app).diff.width, 24);
        app.handle_key(left, 20, 100, area.width);
        assert_eq!(ui::panes(area, &app).diff.width, 26);
        for _ in 0..100 {
            app.handle_key(left, 20, 100, area.width);
        }
        assert_eq!(ui::panes(area, &app).tree.width, 24);
    }
}

#[test]
fn stored_tree_width_is_clamped_for_smaller_terminals() {
    let mut review = review();
    review.view.tree_width = Some(80);
    let mut app = App::new(review);
    let area = Rect::new(0, 0, 60, 24);
    let panes = ui::panes(area, &app);
    assert_eq!((panes.tree.width, panes.diff.width), (36, 24));
    for width in [30, 1, 0] {
        app.handle_key(
            KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL),
            10,
            100,
            width,
        );
        assert_eq!(app.review.view.tree_width, Some(80));
        let mut terminal = Terminal::new(TestBackend::new(width, 8)).unwrap();
        terminal
            .draw(|frame| ui::draw(frame, &app, &Text::raw("diff")))
            .unwrap();
    }
}

#[test]
fn flash_jumps_to_tree_matches_from_either_pane() {
    for focus in [Focus::Tree, Focus::Diff] {
        let mut app = App::new(review());
        app.focus = focus;
        app.review.view.fullscreen = focus == Focus::Diff;
        key(&mut app, KeyCode::Char('s'));
        assert!(app.flash.is_some());
        assert_eq!(app.focus, Focus::Tree);
        assert!(!app.review.view.fullscreen);
        key(&mut app, KeyCode::Char('b'));
        let flash = app.flash.as_ref().unwrap();
        assert_eq!(flash.query, "b");
        let label = flash
            .labels
            .iter()
            .find(|(row, _)| app.rows[*row].path == "src/nested/b.rs")
            .unwrap()
            .1;
        key(&mut app, KeyCode::Char(label));
        assert!(app.flash.is_none());
        assert_eq!(app.review.view.selected, "src/nested/b.rs");
        assert_eq!(app.active, Some(2));
        assert_eq!(app.focus, Focus::Tree);
    }
}

#[test]
fn flash_queries_are_incremental_and_escape_restores_the_previous_layout() {
    let mut app = App::new(review());
    key(&mut app, KeyCode::Char('z'));
    key(&mut app, KeyCode::Char('s'));
    key(&mut app, KeyCode::Char('r'));
    key(&mut app, KeyCode::Char('s'));
    assert_eq!(app.flash.as_ref().unwrap().query, "rs");
    key(&mut app, KeyCode::Backspace);
    assert_eq!(app.flash.as_ref().unwrap().query, "r");
    key(&mut app, KeyCode::Esc);
    assert!(app.flash.is_none());
    assert_eq!(app.focus, Focus::Diff);
    assert!(app.review.view.fullscreen);
    assert_eq!(app.review.view.selected, "README.md");
    assert!(app.review.view.filter.is_empty());
}

#[test]
fn flash_respects_tree_filters_and_collapsed_folders() {
    let mut review = review();
    review.view.collapsed.insert("src/".into());
    let mut app = App::new(review);
    key(&mut app, KeyCode::Char('s'));
    key(&mut app, KeyCode::Char('b'));
    assert!(app.flash.as_ref().unwrap().matches.is_empty());
    key(&mut app, KeyCode::Esc);
    app.review.view.filter = "a.rs".into();
    app.rebuild();
    key(&mut app, KeyCode::Char('s'));
    key(&mut app, KeyCode::Char('b'));
    assert!(app.flash.as_ref().unwrap().matches.is_empty());
    key(&mut app, KeyCode::Backspace);
    key(&mut app, KeyCode::Char('a'));
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.review.view.selected, "src/a.rs");
    assert_eq!(app.review.view.filter, "a.rs");
}

#[test]
fn flash_labels_only_rows_in_the_tree_viewport() {
    let mut review = review();
    review.changes = (0..80)
        .map(|index| change(&format!("folder/file_{index:02}.rs")))
        .collect();
    let mut app = App::new(review);
    key(&mut app, KeyCode::Char('G'));
    key(&mut app, KeyCode::Char('s'));
    key(&mut app, KeyCode::Char('_'));
    let flash = app.flash.as_ref().unwrap();
    assert_eq!(flash.matches.len(), 10);
    assert!(flash.labels.iter().all(|(row, _)| *row >= 71));
    let labels: std::collections::BTreeSet<_> =
        flash.labels.iter().map(|(_, label)| label).collect();
    assert_eq!(labels.len(), flash.labels.len());
}

#[test]
fn flash_jump_keys_require_the_displayed_case() {
    let mut app = App::new(review());
    key(&mut app, KeyCode::Char('s'));
    key(&mut app, KeyCode::Char('b'));
    let label = app.flash.as_ref().unwrap().labels[0].1;
    assert!(label.is_ascii_lowercase());
    app.handle_key(
        KeyEvent::new(
            KeyCode::Char(label.to_ascii_uppercase()),
            KeyModifiers::SHIFT,
        ),
        10,
        100,
        100,
    );
    assert_eq!(
        app.flash.as_ref().unwrap().query,
        format!("b{}", label.to_ascii_uppercase())
    );
    assert_eq!(app.review.view.selected, "README.md");
    key(&mut app, KeyCode::Backspace);
    assert_eq!(app.flash.as_ref().unwrap().query, "b");
    key(&mut app, KeyCode::Char(label));
    assert!(app.flash.is_none());
    assert_eq!(app.review.view.selected, "src/nested/b.rs");
}

#[test]
fn flash_uses_lowercase_then_uppercase_labels_to_jump_to_distinct_rows() {
    let mut review = review();
    review.changes = (0..60)
        .map(|index| change(&format!("file_{index:02}_x_.rs")))
        .collect();
    let mut app = App::new(review);
    for ch in "sX".chars() {
        app.handle_key(
            KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
            60,
            100,
            100,
        );
    }
    let flash = app.flash.as_ref().unwrap();
    assert_eq!(flash.matches.len(), 60);
    assert_eq!(
        flash
            .labels
            .iter()
            .map(|(_, label)| label)
            .collect::<String>(),
        "asdfghjklqwertyuiopzxcvbnmASDFGHJKLQWERTYUIOPZXCVBNM"
    );
    let mut terminal = Terminal::new(TestBackend::new(100, 64)).unwrap();
    terminal
        .draw(|frame| ui::draw(frame, &app, &Text::raw("diff")))
        .unwrap();
    let buffer = terminal.backend().buffer();
    for (row, label) in &flash.labels {
        let cell = &buffer[(13, *row as u16 + 2)];
        assert_eq!(cell.symbol(), label.to_string());
        assert_eq!(cell.bg, Color::Yellow);
    }
    assert_eq!(buffer[(13, 54)].symbol(), ".");
    key(&mut app, KeyCode::Esc);
    for (label, expected_row, modifiers) in [
        ('A', 26, KeyModifiers::SHIFT),
        ('a', 0, KeyModifiers::NONE),
        ('M', 51, KeyModifiers::SHIFT),
        ('m', 25, KeyModifiers::NONE),
    ] {
        for ch in "sx".chars() {
            app.handle_key(
                KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
                60,
                100,
                100,
            );
        }
        app.handle_key(KeyEvent::new(KeyCode::Char(label), modifiers), 60, 100, 100);
        assert!(app.flash.is_none());
        assert_eq!(
            app.review.view.selected,
            format!("file_{expected_row:02}_x_.rs")
        );
    }
}

#[test]
fn flash_highlights_each_typed_match_and_overlays_labels_after_the_next_letter() {
    let mut review = review();
    review.changes = vec![change("xxTESTabc_TESTdef.rs")];
    let mut app = App::new(review);
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    key(&mut app, KeyCode::Char('s'));
    for (length, ch) in "test".chars().enumerate() {
        key(&mut app, KeyCode::Char(ch));
        let flash = app.flash.as_ref().unwrap();
        assert_eq!(flash.query.len(), length + 1);
        let label = flash.labels[0].1.to_string();
        terminal
            .draw(|frame| ui::draw(frame, &app, &Text::raw("diff")))
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(1, 2)].symbol(), "·");
        assert_eq!(buffer[(3, 2)].symbol(), "x");
        assert_ne!(buffer[(3, 2)].fg, Color::Yellow);
        for start in [2, 10] {
            for offset in start..=start + length {
                let cell = &buffer[(3 + offset as u16, 2)];
                assert_eq!(cell.fg, Color::Yellow);
                assert!(cell.modifier.contains(Modifier::BOLD));
            }
            let next = 3 + (start + length + 1) as u16;
            assert_eq!(
                buffer[(next, 2)].symbol(),
                app.rows[0]
                    .name()
                    .chars()
                    .nth(start + length + 1)
                    .unwrap()
                    .to_string()
            );
            assert_ne!(buffer[(next, 2)].fg, Color::Yellow);
            let cell = &buffer[(next + 1, 2)];
            assert_eq!(cell.symbol(), label);
            assert_eq!(cell.fg, Color::Black);
            assert_eq!(cell.bg, Color::Yellow);
        }
        assert_eq!(buffer[(25, 2)].symbol(), "M");
    }
}

#[test]
fn flash_labels_preserve_overlapping_matches_and_their_continuations() {
    let mut review = review();
    review.changes = vec![change("aaad.rs")];
    let mut app = App::new(review);
    for ch in "saa".chars() {
        key(&mut app, KeyCode::Char(ch));
    }
    let label = app.flash.as_ref().unwrap().labels[0].1;
    assert!(!['a', 'd'].contains(&label));
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal
        .draw(|frame| ui::draw(frame, &app, &Text::raw("diff")))
        .unwrap();
    let buffer = terminal.backend().buffer();
    for x in 3..6 {
        assert_eq!(buffer[(x, 2)].symbol(), "a");
        assert_eq!(buffer[(x, 2)].fg, Color::Yellow);
    }
    assert_eq!(buffer[(6, 2)].symbol(), "d");
    assert_ne!(buffer[(6, 2)].fg, Color::Yellow);
    assert_eq!(buffer[(7, 2)].symbol(), label.to_string());
    key(&mut app, KeyCode::Char('D'));
    assert_eq!(app.flash.as_ref().unwrap().query, "aaD");
}

#[test]
fn flash_label_offsets_follow_unicode_width_case_folding_and_escaped_names() {
    for (name, query, start, width, next, label_column) in [
        ("λa.rs", "λ", 3, 1, "a", 5),
        ("界a.rs", "界", 3, 2, "a", 6),
        ("İTESTa.rs", "test", 4, 4, "a", 9),
        ("q界rest.rs", "q", 3, 1, "界", 6),
        ("e\u{301}a.rs", "e", 3, 1, "a", 5),
        ("one\nTESTa.rs", "test", 8, 4, "a", 13),
    ] {
        let mut review = review();
        review.changes = vec![change(name)];
        let mut app = App::new(review);
        key(&mut app, KeyCode::Char('s'));
        for ch in query.chars() {
            key(&mut app, KeyCode::Char(ch));
        }
        let label = app.flash.as_ref().unwrap().labels[0].1;
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal
            .draw(|frame| ui::draw(frame, &app, &Text::raw("diff")))
            .unwrap();
        let buffer = terminal.backend().buffer();
        for x in start..start + width {
            if buffer[(x, 2)].symbol() != " " {
                assert_eq!(buffer[(x, 2)].fg, Color::Yellow, "{name} at column {x}");
            }
        }
        assert_eq!(buffer[(start + width, 2)].symbol(), next, "{name}");
        assert_ne!(buffer[(start + width, 2)].fg, Color::Yellow, "{name}");
        assert_eq!(
            buffer[(label_column, 2)].symbol(),
            label.to_string(),
            "{name}"
        );
        assert_eq!(buffer[(label_column, 2)].bg, Color::Yellow, "{name}");
    }
}

#[test]
fn flash_labels_at_name_ends_preserve_file_and_folder_metadata_positions() {
    for (path, next, metadata) in [("tail.rs", " ", "M"), ("tail.rs/child", "/", "0")] {
        let mut review = review();
        review.changes = vec![change(path)];
        let mut app = App::new(review);
        for ch in "srs".chars() {
            key(&mut app, KeyCode::Char(ch));
        }
        let label = app.flash.as_ref().unwrap().labels[0].1;
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal
            .draw(|frame| ui::draw(frame, &app, &Text::raw("diff")))
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(10, 2)].symbol(), next);
        assert_eq!(buffer[(11, 2)].symbol(), label.to_string());
        assert_eq!(buffer[(12, 2)].symbol(), metadata);
    }
}

#[test]
fn flash_labels_exclude_continuations_on_every_visible_match_including_unlabeled_rows() {
    let mut review = review();
    review.changes = (0..60)
        .map(|index| change(&format!("file_{index:02}_x_.rs")))
        .chain([change("last_xA_xS_xD.rs")])
        .collect();
    let mut app = App::new(review);
    for ch in "sx".chars() {
        app.handle_key(
            KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
            80,
            100,
            100,
        );
    }
    let flash = app.flash.as_ref().unwrap();
    assert_eq!(flash.matches.len(), 61);
    assert_eq!(flash.labels.len(), 46);
    assert!(
        flash.labels[..23]
            .iter()
            .all(|(_, label)| label.is_ascii_lowercase())
    );
    assert!(
        flash.labels[23..]
            .iter()
            .all(|(_, label)| label.is_ascii_uppercase())
    );
    assert!(
        flash
            .labels
            .iter()
            .all(|(_, label)| !['a', 's', 'd'].contains(&label.to_ascii_lowercase()))
    );
    assert!(flash.labels.iter().all(|(row, _)| *row != 60));
    key(&mut app, KeyCode::Char('S'));
    assert_eq!(app.flash.as_ref().unwrap().query, "xS");
    key(&mut app, KeyCode::Backspace);
    assert_eq!(app.flash.as_ref().unwrap().query, "x");
    let label = app.flash.as_ref().unwrap().labels[0].1;
    key(&mut app, KeyCode::Char(label));
    assert!(app.flash.is_none());
    assert_eq!(app.review.view.selected, "file_00_x_.rs");
}

#[test]
fn flash_overlays_stay_inside_the_tree_pane_when_names_are_clipped() {
    let mut review = review();
    review.changes = vec![change("abcdefghijklmnopqrstu.rs")];
    for (query, columns) in [("qr", 19..21), ("rs", 20..22), ("st", 21..23)] {
        let mut app = App::new(review.clone());
        key(&mut app, KeyCode::Char('s'));
        for ch in query.chars() {
            key(&mut app, KeyCode::Char(ch));
        }
        let mut terminal = Terminal::new(TestBackend::new(55, 8)).unwrap();
        terminal
            .draw(|frame| ui::draw(frame, &app, &Text::raw("diff")))
            .unwrap();
        let pane = ui::panes(Rect::new(0, 0, 55, 8), &app).tree;
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(pane.right() - 1, 2)].symbol(), "│");
        assert_eq!(buffer[(pane.right(), 2)].symbol(), "│");
        assert_eq!(buffer[(pane.right() + 1, 2)].symbol(), "d");
        assert_ne!(buffer[(pane.right() + 1, 2)].bg, Color::Yellow);
        for x in columns.clone() {
            assert_eq!(buffer[(x, 2)].fg, Color::Yellow);
        }
        if columns.end < pane.right() - 1 {
            assert_ne!(buffer[(columns.end, 2)].fg, Color::Yellow);
        }
    }
}

#[test]
fn flash_labels_remain_bright_on_viewed_files() {
    let mut review = review();
    review.toggle_viewed(2);
    let mut app = App::new(review);
    key(&mut app, KeyCode::Char('s'));
    key(&mut app, KeyCode::Char('b'));
    let label = app.flash.as_ref().unwrap().labels[0].1;
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal
        .draw(|frame| ui::draw(frame, &app, &Text::raw("diff")))
        .unwrap();
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(5, 6)].symbol(), "✓");
    assert_eq!(buffer[(7, 6)].fg, Color::Yellow);
    assert!(!buffer[(7, 6)].modifier.contains(Modifier::DIM));
    assert_eq!(buffer[(8, 6)].symbol(), ".");
    assert_ne!(buffer[(8, 6)].fg, Color::Yellow);
    assert!(buffer[(8, 6)].modifier.contains(Modifier::DIM));
    let cell = &buffer[(9, 6)];
    assert_eq!(cell.symbol(), label.to_string());
    assert!(cell.modifier.contains(Modifier::BOLD));
    assert!(!cell.modifier.contains(Modifier::DIM));
    let footer: String = (0..100)
        .map(|x| terminal.backend().buffer()[(x, 23)].symbol())
        .collect();
    assert!(footer.contains("Flash /b"));
}

#[test]
fn flash_supports_unicode_empty_trees_and_literal_prompt_input() {
    let mut review = review();
    review.changes.push(change("src/λ.rs"));
    let mut app = App::new(review);
    key(&mut app, KeyCode::Char('s'));
    key(&mut app, KeyCode::Char('λ'));
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.review.view.selected, "src/λ.rs");
    key(&mut app, KeyCode::Char('/'));
    key(&mut app, KeyCode::Char('s'));
    assert!(app.flash.is_none());
    assert_eq!(app.review.view.filter, "s");
    key(&mut app, KeyCode::Esc);
    app.review.changes.clear();
    app.rebuild();
    key(&mut app, KeyCode::Char('s'));
    key(&mut app, KeyCode::Char('q'));
    assert!(!app.quit);
    assert!(app.flash.as_ref().unwrap().matches.is_empty());
    key(&mut app, KeyCode::Esc);
    assert!(app.flash.is_none());
}

#[test]
fn difft_output_is_embedded_with_ansi_colors() {
    let output = render_pair(
        &RendererConfig::default(),
        &change("example.rs"),
        b"fn old() {}\n",
        b"fn new() {}\n",
        RenderOptions {
            width: 80,
            inline: false,
        },
        &AtomicU64::new(1),
        1,
    )
    .unwrap();
    let plain = output.to_string();
    assert!(plain.contains("old"));
    assert!(plain.contains("new"));
    assert!(!plain.contains('\u{1b}'));
    assert!(
        output
            .lines
            .iter()
            .flat_map(|line| &line.spans)
            .any(|span| span.style.fg.is_some())
    );
}

#[test]
fn external_renderer_supports_placeholders_exit_one_timeout_and_cancellation() {
    let config = RendererConfig {
        command: vec!["diff".into(), "-u".into(), "{old}".into(), "{new}".into()],
        timeout_ms: 1_000,
        ..Default::default()
    };
    let output = render_pair(
        &config,
        &change("a.txt"),
        b"old\n",
        b"new\n",
        RenderOptions {
            width: 80,
            inline: true,
        },
        &AtomicU64::new(1),
        1,
    )
    .unwrap();
    assert!(output.to_string().contains("+new"));
    let sleeping = RendererConfig {
        command: vec!["sleep".into(), "10".into()],
        timeout_ms: 30,
        ..Default::default()
    };
    assert!(
        render_pair(
            &sleeping,
            &change("a.txt"),
            b"",
            b"",
            RenderOptions {
                width: 80,
                inline: false
            },
            &AtomicU64::new(1),
            1
        )
        .unwrap_err()
        .to_string()
        .contains("timed out")
    );
    assert!(
        render_pair(
            &config,
            &change("a.txt"),
            b"",
            b"",
            RenderOptions {
                width: 80,
                inline: false
            },
            &AtomicU64::new(2),
            1
        )
        .is_err()
    );
}

#[test]
fn collapsed_directories_keep_the_directory_preview_and_enter_toggles_them() {
    let mut app = App::new(review());
    key(&mut app, KeyCode::Char(']'));
    key(&mut app, KeyCode::Char('f'));
    assert_eq!(app.active, Some(1));
    key(&mut app, KeyCode::Char('h'));
    assert_eq!(app.review.view.selected, "src/");
    key(&mut app, KeyCode::Char('h'));
    assert!(app.review.view.collapsed.contains("src/"));
    assert_eq!(app.active, None);
    assert_eq!(app.preview_changes().count(), 2);
    key(&mut app, KeyCode::Enter);
    assert!(!app.review.view.collapsed.contains("src/"));
    key(&mut app, KeyCode::Enter);
    assert!(app.review.view.collapsed.contains("src/"));
    key(&mut app, KeyCode::Tab);
    let resumed = App::new(app.review.clone());
    assert_eq!(resumed.active, None);
    assert_eq!(resumed.review.view.active, "src/");
    assert_eq!(resumed.focus, Focus::Diff);
}

#[test]
fn placeholders_in_filenames_are_treated_literally() {
    let config = RendererConfig {
        command: vec!["diff".into(), "-u".into(), "{old}".into(), "{new}".into()],
        timeout_ms: 1_000,
        ..Default::default()
    };
    let output = render_pair(
        &config,
        &change("{new}.rs"),
        b"old\n",
        b"new\n",
        RenderOptions {
            width: 80,
            inline: false,
        },
        &AtomicU64::new(1),
        1,
    )
    .unwrap();
    assert!(output.to_string().contains("+new"));
}

#[test]
fn empty_reviews_and_no_match_filters_are_keyboard_safe() {
    let mut empty = review();
    empty.changes.clear();
    let mut app = App::new(empty);
    for code in [
        KeyCode::Char(']'),
        KeyCode::Char('u'),
        KeyCode::Char(' '),
        KeyCode::Char('G'),
        KeyCode::Char('h'),
        KeyCode::Enter,
    ] {
        key(&mut app, code);
    }
    assert_eq!(app.active, None);
    let mut app = App::new(review());
    key(&mut app, KeyCode::Char('/'));
    for ch in "missing".chars() {
        key(&mut app, KeyCode::Char(ch));
    }
    key(&mut app, KeyCode::Enter);
    assert!(app.rows.is_empty());
    assert_eq!(app.active, None);
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.active, Some(0));
}

#[test]
fn diff_viewport_can_scroll_beyond_u16_line_numbers() {
    let mut app = App::new(review());
    app.position().vertical = 66_000;
    let text = Text::from(
        (0..66_030)
            .map(|line| ratatui::text::Line::raw(format!("line {line}")))
            .collect::<Vec<_>>(),
    );
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|frame| ui::draw(frame, &app, &text)).unwrap();
    let output: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(output.contains("line 66000"));
}
