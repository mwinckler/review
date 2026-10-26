use std::sync::atomic::AtomicU64;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend, layout::Rect, style::Modifier, text::Text};
use review::{
    app::{App, Focus},
    renderer::{RenderOptions, RendererConfig, render_pair},
    state::Review,
    tree, ui,
    vcs::{Change, Comparison, Entry},
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
fn space_in_the_diff_toggles_only_the_active_file_with_a_folder_selected() {
    let mut app = App::new(review());
    key(&mut app, KeyCode::Char(']'));
    key(&mut app, KeyCode::Char('f'));
    key(&mut app, KeyCode::Char('h'));
    assert_eq!(app.review.view.selected, "src/");
    key(&mut app, KeyCode::Tab);

    key(&mut app, KeyCode::Char(' '));
    assert!(app.review.is_viewed(&app.review.changes[1]));
    assert!(!app.review.is_viewed(&app.review.changes[2]));
    assert_eq!(app.active, Some(2));

    key(&mut app, KeyCode::Char('['));
    key(&mut app, KeyCode::Char('f'));
    key(&mut app, KeyCode::Char(' '));
    assert!(app.review.viewed.is_empty());
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
    let cell = &terminal.backend().buffer()[(5, 6)];
    assert_eq!(cell.symbol(), label.to_ascii_uppercase().to_string());
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
fn collapsed_directories_keep_the_active_diff_and_enter_toggles_them() {
    let mut app = App::new(review());
    key(&mut app, KeyCode::Char(']'));
    key(&mut app, KeyCode::Char('f'));
    assert_eq!(app.active, Some(1));
    key(&mut app, KeyCode::Char('h'));
    assert_eq!(app.review.view.selected, "src/");
    key(&mut app, KeyCode::Char('h'));
    assert!(app.review.view.collapsed.contains("src/"));
    assert_eq!(app.active, Some(1));
    key(&mut app, KeyCode::Enter);
    assert!(!app.review.view.collapsed.contains("src/"));
    key(&mut app, KeyCode::Enter);
    assert!(app.review.view.collapsed.contains("src/"));
    key(&mut app, KeyCode::Tab);
    let resumed = App::new(app.review.clone());
    assert_eq!(resumed.active, Some(1));
    assert_eq!(resumed.focus, Focus::Diff);
}

#[test]
fn placeholders_in_filenames_are_treated_literally() {
    let config = RendererConfig {
        command: vec!["diff".into(), "-u".into(), "{old}".into(), "{new}".into()],
        timeout_ms: 1_000,
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
