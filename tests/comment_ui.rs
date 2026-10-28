mod common;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend, style::Color, text::Text};
use review::{
    app::{App, Focus},
    comment_dialog::{CommentAction, CommentDialog, CommentDraft, DraftField, LineTarget},
    comments::DiffSide,
    hunks::{self, HunkFormat},
    state::Review,
    ui,
    vcs::{Backend, Repository},
};

fn key(app: &mut App, code: KeyCode) {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE), 10, 100, 100);
}

fn save(app: &mut App) {
    app.handle_key(
        KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
        10,
        100,
        100,
    );
}

fn fixture() -> (tempfile::TempDir, Repository, App, Text<'static>) {
    let root = common::fixture();
    std::fs::write(
        root.path().join("src/main.rs"),
        "fn main() {\n    first();\n    second();\n}\n",
    )
    .unwrap();
    common::jj(root.path(), &["commit", "-m", "range"]);
    let repo = Repository::discover(root.path(), Some(Backend::Git)).unwrap();
    let comparison = repo.snapshot_expression("HEAD~1..HEAD").unwrap();
    let changes = repo.changes(&comparison).unwrap();
    let mut review = Review::new(comparison, changes);
    review.view.selected = "src/main.rs".into();
    let mut app = App::new(review);
    let text = Text::raw(
        "--- old\n+++ new\n@@ -1 +1,4 @@\n-fn main() { println!(\"new\"); }\n+fn main() {\n+    first();\n+    second();\n+}\n",
    );
    let file = &app.review.changes[app.active.unwrap()];
    app.source_lines = hunks::source_lines(&text, file, HunkFormat::Unified, (1, 4));
    app.hunks = hunks::parse(&text, file, HunkFormat::Unified);
    (root, repo, app, text)
}

#[test]
fn visual_selection_starts_on_a_rendered_source_line_when_the_diff_starts_later_in_the_file() {
    let (_root, _repo, mut app, _text) = fixture();
    app.source_lines
        .retain(|source| source.location.new_line.is_some_and(|number| number >= 3));
    key(&mut app, KeyCode::Char('v'));
    assert_eq!(app.line_selection.as_ref().unwrap().target().start_line, 3);
    key(&mut app, KeyCode::Char('j'));
    assert_eq!(app.line_selection.as_ref().unwrap().target().end_line, 4);
}

#[test]
fn line_selection_keeps_pane_shortcuts_and_editor_locations_and_jumps_dirty_the_view() {
    let (_root, repo, mut app, _text) = fixture();
    key(&mut app, KeyCode::Char('v'));
    key(&mut app, KeyCode::Char('j'));
    key(&mut app, KeyCode::Char('j'));
    assert_eq!(app.editor_location().unwrap().new_line, Some(3));
    key(&mut app, KeyCode::Char('G'));
    assert_eq!(app.line_selection.as_ref().unwrap().cursor, 4);
    key(&mut app, KeyCode::Char('g'));
    key(&mut app, KeyCode::Char('g'));
    assert_eq!(app.line_selection.as_ref().unwrap().cursor, 1);
    app.handle_key(
        KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL),
        10,
        100,
        100,
    );
    assert_eq!(app.focus, Focus::Tree);
    assert!(app.line_selection.is_none());
    let target = LineTarget {
        path: "src/main.rs".into(),
        side: DiffSide::New,
        start_line: 2,
        end_line: 2,
    };
    let mut draft = CommentDraft::new(&repo, &app.review, &target).unwrap();
    draft.body = "Jump".into();
    app.comment_dialog = Some(CommentDialog::Compose(Box::new(draft)));
    save(&mut app);
    app.dirty = false;
    key(&mut app, KeyCode::Enter);
    assert!(app.dirty);
}

#[test]
fn pasted_text_is_multiline_and_literal_and_browser_control_scrolls_are_safe() {
    let (_root, repo, mut app, _text) = fixture();
    let target = LineTarget {
        path: "src/main.rs".into(),
        side: DiffSide::New,
        start_line: 1,
        end_line: 1,
    };
    let mut draft = CommentDraft::new(&repo, &app.review, &target).unwrap();
    draft.insert("q\r\nS\rC\u{0}\u{1b}");
    assert_eq!(draft.body, "q\nS\nC");
    app.comment_dialog = Some(CommentDialog::Compose(Box::new(draft)));
    save(&mut app);
    app.handle_key(
        KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
        10,
        100,
        100,
    );
    let Some(CommentDialog::Browse(list)) = &app.comment_dialog else {
        panic!("browser");
    };
    assert!(list.confirming.is_none());
    assert!(list.scroll > 0);
}

#[test]
fn visual_ranges_stay_on_one_side_and_file_and_compose_the_exact_inclusive_lines() {
    let (_root, _repo, mut app, _text) = fixture();
    key(&mut app, KeyCode::Char('v'));
    assert_eq!(app.focus, Focus::Diff);
    assert_eq!(app.line_selection.as_ref().unwrap().target().start_line, 1);
    key(&mut app, KeyCode::Char('j'));
    key(&mut app, KeyCode::Char('j'));
    key(&mut app, KeyCode::Char('c'));
    let Some(CommentAction::Compose(target)) = app.comment_action.take() else {
        panic!("compose requested");
    };
    assert_eq!(
        target,
        LineTarget {
            path: "src/main.rs".into(),
            side: DiffSide::New,
            start_line: 1,
            end_line: 3
        }
    );
    key(&mut app, KeyCode::Char('h'));
    assert_eq!(
        app.line_selection.as_ref().unwrap().target().side,
        DiffSide::Old
    );
    for _ in 0..20 {
        key(&mut app, KeyCode::Char('j'));
    }
    assert_eq!(app.line_selection.as_ref().unwrap().target().end_line, 1);
    key(&mut app, KeyCode::Esc);
    assert!(app.line_selection.is_none());
}

#[test]
fn composer_validates_ranges_edits_unicode_multiline_text_and_saves_only_explicitly() {
    let (_root, repo, mut app, _text) = fixture();
    let target = LineTarget {
        path: "src/main.rs".into(),
        side: DiffSide::New,
        start_line: 2,
        end_line: 3,
    };
    let draft = CommentDraft::new(&repo, &app.review, &target).unwrap();
    assert_eq!(draft.anchor.excerpt, "    first();\n    second();");
    app.comment_dialog = Some(CommentDialog::Compose(Box::new(draft)));
    for ch in "qλj".chars() {
        key(&mut app, KeyCode::Char(ch));
    }
    key(&mut app, KeyCode::Left);
    key(&mut app, KeyCode::Backspace);
    key(&mut app, KeyCode::Char('é'));
    key(&mut app, KeyCode::End);
    key(&mut app, KeyCode::Enter);
    for ch in "S? ]h".chars() {
        key(&mut app, KeyCode::Char(ch));
    }
    assert!(!app.quit);
    assert!(!app.help);
    assert!(app.review.comments.is_empty());
    save(&mut app);
    assert_eq!(app.review.comments[0].body, "qéj\nS? ]h");
    assert_eq!(app.review.comments[0].anchor.start_line, 2);
    assert!(app.comments_changed);
    assert!(matches!(app.comment_dialog, Some(CommentDialog::Browse(_))));
    key(&mut app, KeyCode::Char('e'));
    let Some(CommentDialog::Compose(draft)) = &mut app.comment_dialog else {
        panic!("edit draft");
    };
    assert_eq!(draft.field, DraftField::Body);
    let id = draft.id.clone().unwrap();
    key(&mut app, KeyCode::Char('!'));
    save(&mut app);
    assert_eq!(app.review.comments[0].id, id);
    assert_eq!(app.review.comments.len(), 1);
    key(&mut app, KeyCode::Esc);
    key(&mut app, KeyCode::Char('c'));
    let Some(CommentAction::Compose(target)) = app.comment_action.take() else {
        panic!("compose requested");
    };
    app.comment_dialog = Some(CommentDialog::Compose(Box::new(
        CommentDraft::new(&repo, &app.review, &target).unwrap(),
    )));
    save(&mut app);
    assert!(matches!(
        app.comment_dialog,
        Some(CommentDialog::Compose(_))
    ));
    key(&mut app, KeyCode::Tab);
    let Some(CommentDialog::Compose(draft)) = &mut app.comment_dialog else {
        panic!("draft");
    };
    draft.start = "999".into();
    draft.body = "Range check".into();
    save(&mut app);
    assert_eq!(app.review.comments.len(), 1);
    assert!(matches!(
        app.comment_dialog,
        Some(CommentDialog::Compose(_))
    ));
    key(&mut app, KeyCode::Esc);
    assert!(app.comment_dialog.is_none());
}

#[test]
fn comment_browser_confirms_source_writes_and_deletions_and_keeps_outdated_text_readable() {
    let (_root, repo, mut app, _text) = fixture();
    let draft = CommentDraft::new(
        &repo,
        &app.review,
        &LineTarget {
            path: "src/main.rs".into(),
            side: DiffSide::New,
            start_line: 2,
            end_line: 3,
        },
    )
    .unwrap();
    app.comment_dialog = Some(CommentDialog::Compose(Box::new(draft)));
    for ch in "Explain range".chars() {
        key(&mut app, KeyCode::Char(ch));
    }
    save(&mut app);
    key(&mut app, KeyCode::Char('t'));
    assert!(app.comment_action.is_none());
    key(&mut app, KeyCode::Esc);
    assert!(app.comment_action.is_none());
    key(&mut app, KeyCode::Char('t'));
    key(&mut app, KeyCode::Char('y'));
    assert!(matches!(
        app.comment_action.take(),
        Some(CommentAction::WriteTodo(_))
    ));
    key(&mut app, KeyCode::Enter);
    assert!(app.comment_dialog.is_none());
    assert_eq!(app.line_selection.as_ref().unwrap().target().start_line, 2);
    key(&mut app, KeyCode::Esc);
    app.review.changes.clear();
    app.rebuild();
    key(&mut app, KeyCode::Char('C'));
    key(&mut app, KeyCode::Enter);
    assert!(app.message.contains("outdated"));
    let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();
    terminal
        .draw(|frame| ui::draw(frame, &app, &Text::default()))
        .unwrap();
    let display: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(display.contains("outdated"));
    assert!(display.contains("Explain range"));
    assert!(display.contains("first();"));
    key(&mut app, KeyCode::Char('d'));
    key(&mut app, KeyCode::Char('n'));
    assert_eq!(app.review.comments.len(), 1);
    key(&mut app, KeyCode::Char('d'));
    key(&mut app, KeyCode::Char('y'));
    assert!(app.review.comments.is_empty());
}

#[test]
fn range_highlights_and_comment_markers_are_visible_and_small_terminals_render() {
    let (_root, repo, mut app, text) = fixture();
    key(&mut app, KeyCode::Char('v'));
    key(&mut app, KeyCode::Char('j'));
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|frame| ui::draw(frame, &app, &text)).unwrap();
    assert!(
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .any(|cell| cell.bg == Color::Rgb(55, 50, 25))
    );
    let target = app.line_selection.as_ref().unwrap().target();
    let mut draft = CommentDraft::new(&repo, &app.review, &target).unwrap();
    draft.body = "Marker".into();
    app.comment_dialog = Some(CommentDialog::Compose(Box::new(draft)));
    save(&mut app);
    key(&mut app, KeyCode::Esc);
    terminal.draw(|frame| ui::draw(frame, &app, &text)).unwrap();
    assert!(
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .any(|cell| cell.symbol() == "●")
    );
    key(&mut app, KeyCode::Char('C'));
    for (width, height) in [(20, 6), (1, 1), (55, 14)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| ui::draw(frame, &app, &text)).unwrap();
    }
}
