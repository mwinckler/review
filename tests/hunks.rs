mod common;

use std::{fs, sync::atomic::AtomicU64, time::Duration};

use ratatui::{style::Color, text::Text};
use review::{
    hunks::{self, HunkFormat},
    renderer::{RenderOptions, RenderWorker, RendererConfig, render_pair},
    vcs::{Backend, Change, Entry, Repository},
};

fn change() -> Change {
    Change {
        path: "src/example.rs".into(),
        old_path: "src/example.rs".into(),
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

#[test]
fn unified_hunks_point_to_the_first_changed_line_and_keep_their_screen_ranges() {
    let text = Text::raw(
        "--- old\n+++ new\n@@ -8,4 +8,5 @@\n context\n-old\n+new\n+extra\n context\n context\n@@ -30 +31,0 @@\n-gone\n",
    );
    let hunks = hunks::parse(&text, &change(), HunkFormat::Unified);
    assert_eq!(hunks.len(), 2);
    assert_eq!(hunks[0].rows, 2..9);
    assert_eq!(hunks[0].location.old_line, Some(9));
    assert_eq!(hunks[0].location.new_line, Some(9));
    assert_eq!(hunks[1].location.old_line, Some(30));
    assert_eq!(hunks[1].location.new_line, Some(32));
    let mut removed = change();
    removed.new.kind.clear();
    assert_eq!(
        hunks::parse(&text, &removed, HunkFormat::Unified)[0]
            .location
            .new_line,
        None
    );
    assert!(hunks::parse(&text, &change(), HunkFormat::None).is_empty());
}

#[test]
fn difftastic_maps_colored_inline_split_and_wrapped_hunks_without_changing_the_output() {
    let old = (1..=70)
        .map(|line| format!("let value_{line} = \"old_{line}\";\n"))
        .collect::<String>();
    let new = old
        .replace("old_10\"", "new_10 with a longer value\"")
        .replace("old_60\"", "new_60\"");
    for inline in [false, true] {
        for width in [45, 100] {
            let text = render_pair(
                &RendererConfig::default(),
                &change(),
                old.as_bytes(),
                new.as_bytes(),
                RenderOptions { width, inline },
                &AtomicU64::new(1),
                1,
            )
            .unwrap();
            let before = text.clone();
            let hunks = hunks::parse(&text, &change(), HunkFormat::Difftastic);
            assert_eq!(hunks.len(), 2, "{text}");
            assert_eq!(hunks[0].location.new_line, Some(10), "{text}");
            assert_eq!(hunks[1].location.new_line, Some(60), "{text}");
            assert_eq!(hunks[0].location.old_line, Some(10));
            assert!(hunks[0].rows.end <= hunks[1].rows.start);
            assert!(hunks[1].rows.end <= text.lines.len());
            assert_eq!(text, before);
            assert!(
                text.lines
                    .iter()
                    .flat_map(|line| &line.spans)
                    .any(|span| matches!(span.style.fg, Some(Color::LightGreen | Color::Green)))
            );
        }
    }
}

#[test]
fn single_sided_difftastic_changes_have_safe_locations() {
    for (old, new, expected) in [
        ("", "added\n", Some(1)),
        ("a\nb\nc\nd\ne\nf\ng\nh\n", "a\nb\nc\ne\nf\ng\nh\n", Some(4)),
    ] {
        let mut file = change();
        if old.is_empty() {
            file.old.kind.clear();
        }
        let text = render_pair(
            &RendererConfig::default(),
            &file,
            old.as_bytes(),
            new.as_bytes(),
            RenderOptions {
                width: 80,
                inline: false,
            },
            &AtomicU64::new(1),
            1,
        )
        .unwrap();
        let hunks = hunks::parse(&text, &file, HunkFormat::Difftastic);
        assert_eq!(hunks.len(), 1, "{text}");
        assert_eq!(hunks[0].location.new_line, expected, "{text}");
    }
}

#[test]
fn numeric_source_lines_are_not_mistaken_for_difftastic_line_number_columns() {
    let old = (1..=70).map(|line| format!("{line}\n")).collect::<String>();
    let new = old
        .replace("10\n", "changed-ten\n")
        .replace("60\n", "changed-sixty\n");
    let mut file = change();
    file.path = "numbers.txt".into();
    file.old_path = file.path.clone();
    for inline in [false, true] {
        let text = render_pair(
            &RendererConfig::default(),
            &file,
            old.as_bytes(),
            new.as_bytes(),
            RenderOptions { width: 80, inline },
            &AtomicU64::new(1),
            1,
        )
        .unwrap();
        let hunks = hunks::parse(&text, &file, HunkFormat::Difftastic);
        assert_eq!(
            hunks
                .iter()
                .map(|hunk| hunk.location.new_line)
                .collect::<Vec<_>>(),
            [Some(10), Some(60)],
            "{text}"
        );
    }
}

#[test]
fn unified_source_lines_map_context_additions_and_deletions_without_header_or_eof_markers() {
    let text = Text::raw(
        "--- old\n+++ new\n@@ -8,3 +8,3 @@\n context\n-old\n+new\n last\n\\ No newline at end of file\n",
    );
    let lines = hunks::source_lines(&text, &change(), HunkFormat::Unified, (10, 10));
    assert_eq!(
        lines
            .iter()
            .map(|line| (line.row, line.location.old_line, line.location.new_line))
            .collect::<Vec<_>>(),
        [
            (3, Some(8), Some(8)),
            (4, Some(9), None),
            (5, None, Some(9)),
            (6, Some(10), Some(10)),
        ]
    );
    assert!(
        lines
            .iter()
            .all(|line| line.location.path == "src/example.rs")
    );
    assert!(hunks::source_lines(&text, &change(), HunkFormat::None, (10, 10)).is_empty());
}

#[test]
fn difftastic_source_lines_have_exact_sides_for_split_inline_added_and_deleted_files() {
    for (old, new) in [
        (
            "fn f() {\n    old();\n    same();\n}\n",
            "fn f() {\n    new();\n    same();\n}\n",
        ),
        ("", "fn f() {\n    new();\n}\n"),
        ("fn f() {\n    old();\n}\n", ""),
    ] {
        let mut file = change();
        if old.is_empty() {
            file.old.kind.clear();
        }
        if new.is_empty() {
            file.new.kind.clear();
        }
        for inline in [false, true] {
            for width in [30, 90] {
                let text = render_pair(
                    &RendererConfig::default(),
                    &file,
                    old.as_bytes(),
                    new.as_bytes(),
                    RenderOptions { width, inline },
                    &AtomicU64::new(1),
                    1,
                )
                .unwrap();
                let lines = hunks::source_lines(
                    &text,
                    &file,
                    HunkFormat::Difftastic,
                    (old.lines().count(), new.lines().count()),
                );
                assert!(!lines.is_empty(), "{text}");
                for (source, exists, side) in
                    [(old, !old.is_empty(), true), (new, !new.is_empty(), false)]
                {
                    let numbers: Vec<_> = lines
                        .iter()
                        .filter_map(|line| {
                            if side {
                                line.location.old_line
                            } else {
                                line.location.new_line
                            }
                        })
                        .collect();
                    assert_eq!(!numbers.is_empty(), exists, "{text}");
                    assert!(
                        numbers
                            .iter()
                            .all(|number| *number > 0 && *number <= source.lines().count()),
                        "{text}"
                    );
                    if exists {
                        assert!(numbers.contains(&2), "{text}");
                    }
                }
                assert!(
                    lines
                        .iter()
                        .all(|line| !text.lines[line.row].to_string().contains(" --- "))
                );
            }
        }
    }
}

#[test]
fn directory_results_and_cached_results_keep_hunk_paths_and_row_offsets() {
    let dir = common::fixture();
    let repo = Repository::discover(dir.path(), Some(Backend::Git)).unwrap();
    let comparison = repo.snapshot_expression("main..feature").unwrap();
    fs::write(dir.path().join("src/main.rs"), "uncommitted\n").unwrap();
    let changes = repo.changes(&comparison).unwrap();
    let worker = RenderWorker::new(RendererConfig::default());
    for _ in 0..2 {
        worker.request(
            &repo,
            &comparison,
            "./",
            changes.clone(),
            RenderOptions {
                width: 90,
                inline: false,
            },
        );
        let result = worker.results.recv_timeout(Duration::from_secs(5)).unwrap();
        let text = result.text.unwrap();
        let hunk = result
            .hunks
            .iter()
            .find(|hunk| hunk.location.path == "src/main.rs")
            .unwrap();
        assert_eq!(hunk.location.new_line, Some(1));
        assert!(
            text.lines[hunk.rows.start]
                .to_string()
                .contains("src/main.rs")
        );
        assert!(!text.to_string().contains("uncommitted"));
        let source = result
            .source_lines
            .iter()
            .find(|line| line.location.path == "src/main.rs" && line.location.new_line == Some(1))
            .unwrap();
        assert!(source.row > hunk.rows.start);
        assert!(text.lines[source.row].to_string().contains("println!"));
    }
}
