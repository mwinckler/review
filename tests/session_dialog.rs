#[path = "common/session.rs"]
mod session_fixture;

use std::collections::BTreeSet;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend, text::Text};
use review::{
    app::{App, Focus},
    session_dialog::{SessionAction, SessionDialog},
    state::Review,
    store::{SavedSession, SessionStore},
    ui,
    vcs::{Backend, Comparison, Repository},
};

fn review() -> Review {
    Review::new(
        Comparison {
            base_ref: "master".into(),
            head_ref: "feature".into(),
            base_id: "base".into(),
            head_id: "head".into(),
            base_change_id: None,
            head_change_id: None,
            mode: Default::default(),
            line_counts: None,
            file_line_counts: None,
        },
        Vec::new(),
    )
}

fn sessions(count: usize) -> Vec<SavedSession> {
    (0..count)
        .map(|index| SavedSession {
            version: 1,
            id: format!("{index:012x}"),
            repository: "/review-fixture".into(),
            backend: Backend::Git,
            updated_at: 0,
            review: review(),
        })
        .collect()
}

fn dialog() -> SessionDialog {
    SessionDialog::new(
        sessions(30),
        "000000000000".into(),
        BTreeSet::from(["000000000001".into()]),
    )
}

fn key(dialog: &mut SessionDialog, code: KeyCode) -> Option<SessionAction> {
    dialog.handle_key(KeyEvent::new(code, KeyModifiers::NONE), 10)
}

#[test]
fn multiselect_and_select_all_cover_every_closed_session_and_protect_open_sessions() {
    let mut dialog = dialog();
    key(&mut dialog, KeyCode::Char(' '));
    assert!(dialog.selected.is_empty());
    key(&mut dialog, KeyCode::Char('j'));
    key(&mut dialog, KeyCode::Char(' '));
    assert!(dialog.selected.is_empty());
    key(&mut dialog, KeyCode::Char('j'));
    key(&mut dialog, KeyCode::Char(' '));
    assert_eq!(dialog.selected, BTreeSet::from(["000000000002".into()]));
    key(&mut dialog, KeyCode::Char('a'));
    assert_eq!(dialog.selected.len(), 28);
    assert!(dialog.selected.contains("00000000001d"));
    assert!(!dialog.selected.contains("000000000000"));
    assert!(!dialog.selected.contains("000000000001"));
    key(&mut dialog, KeyCode::Char('a'));
    assert!(dialog.selected.is_empty());
    key(&mut dialog, KeyCode::Char('G'));
    assert_eq!(dialog.selection, 29);
    key(&mut dialog, KeyCode::Char(' '));
    assert!(dialog.selected.contains("00000000001d"));
    key(&mut dialog, KeyCode::Char('g'));
    key(&mut dialog, KeyCode::Char('g'));
    assert_eq!(dialog.selection, 0);
    dialog.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL), 10);
    assert_eq!(dialog.selection, 10);
}

#[test]
fn deletion_requires_an_explicit_confirmation_and_cancellation_keeps_selection() {
    let mut dialog = dialog();
    key(&mut dialog, KeyCode::Char('d'));
    assert!(!dialog.confirming);
    key(&mut dialog, KeyCode::Char('j'));
    key(&mut dialog, KeyCode::Char('j'));
    key(&mut dialog, KeyCode::Char(' '));
    for cancel in [KeyCode::Enter, KeyCode::Esc, KeyCode::Char('n')] {
        assert!(key(&mut dialog, KeyCode::Char('d')).is_none());
        assert!(dialog.confirming);
        assert!(key(&mut dialog, cancel).is_none());
        assert!(!dialog.confirming);
        assert_eq!(dialog.selected.len(), 1);
    }
    key(&mut dialog, KeyCode::Delete);
    assert_eq!(
        key(&mut dialog, KeyCode::Char('y')),
        Some(SessionAction::Forget(vec!["000000000002".into()]))
    );
    assert_eq!(
        key(&mut dialog, KeyCode::Char('q')),
        Some(SessionAction::Close)
    );
}

#[test]
fn the_session_dialog_is_modal_and_preserves_the_review_layout_and_navigation() {
    let mut app = App::new(review());
    app.focus = Focus::Diff;
    app.review.view.fullscreen = true;
    app.handle_key(
        KeyEvent::new(KeyCode::Char('S'), KeyModifiers::SHIFT),
        10,
        100,
        100,
    );
    assert_eq!(app.session_action.take(), Some(SessionAction::Open));
    app.session_dialog = Some(dialog());
    for ch in " aj?ser".chars() {
        app.handle_key(
            KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
            10,
            100,
            100,
        );
    }
    assert_eq!(app.session_action.take(), Some(SessionAction::Reload));
    assert_eq!(app.focus, Focus::Diff);
    assert!(app.review.view.fullscreen);
    assert!(app.review.view.positions.is_empty());
    assert!(!app.editor_requested);
    assert!(!app.refresh);
    assert!(!app.help);
    assert!(app.flash.is_none());
    app.handle_key(
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        10,
        100,
        100,
    );
    assert_eq!(app.session_action.take(), Some(SessionAction::Close));
    assert!(!app.quit);
}

#[test]
fn the_dialog_renders_selection_progress_and_protected_sessions_and_scrolls_safely() {
    let mut app = App::new(review());
    app.session_dialog = Some(dialog());
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|frame| ui::draw(frame, &app, &Text::raw("diff")))
        .unwrap();
    let text: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(text.contains("Forget review sessions"));
    assert!(text.contains("current"));
    assert!(text.contains("open"));
    assert!(text.contains("master → feature"));
    let dialog = app.session_dialog.as_mut().unwrap();
    key(dialog, KeyCode::Char('G'));
    key(dialog, KeyCode::Char(' '));
    terminal
        .draw(|frame| ui::draw(frame, &app, &Text::raw("diff")))
        .unwrap();
    let text: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(text.contains("00000000001d"));
    assert!(text.contains("[x]"));
    for (width, height) in [(30, 8), (1, 1), (0, 0)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| ui::draw(frame, &app, &Text::raw("diff")))
            .unwrap();
    }
    let dialog = app.session_dialog.as_mut().unwrap();
    dialog.sessions.clear();
    dialog.selected.clear();
    dialog.selection = 0;
    key(dialog, KeyCode::Char('a'));
    key(dialog, KeyCode::Char('G'));
    key(dialog, KeyCode::Enter);
    assert!(dialog.selected.is_empty());
    assert!(!dialog.confirming);
    terminal
        .draw(|frame| ui::draw(frame, &app, &Text::raw("diff")))
        .unwrap();
}

#[test]
fn reloading_reconciles_deleted_and_newly_open_sessions_before_another_deletion() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository {
        root: dir.path().to_owned(),
        backend: Backend::Git,
    };
    let store = SessionStore::new(dir.path(), &repo).unwrap();
    for saved in sessions(4) {
        session_fixture::write(&store, &repo, &saved.id, saved.review);
    }
    let mut dialog = SessionDialog::load(&store, "000000000000").unwrap();
    key(&mut dialog, KeyCode::Char('a'));
    assert_eq!(dialog.selected.len(), 3);
    let (_, lock) = store.open("000000000001").unwrap();
    assert!(
        store
            .forget(&dialog.selected.iter().cloned().collect::<Vec<_>>())
            .is_err()
    );
    dialog.reload(&store).unwrap();
    assert_eq!(dialog.selected.len(), 2);
    assert!(dialog.open.contains("000000000001"));
    assert_eq!(
        store
            .forget(&dialog.selected.iter().cloned().collect::<Vec<_>>())
            .unwrap(),
        2
    );
    dialog.reload(&store).unwrap();
    assert_eq!(dialog.sessions.len(), 2);
    assert!(dialog.selected.is_empty());
    drop(lock);
    dialog.reload(&store).unwrap();
    key(&mut dialog, KeyCode::Char('a'));
    assert_eq!(dialog.selected, BTreeSet::from(["000000000001".into()]));
}
