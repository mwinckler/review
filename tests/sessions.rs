use std::{fs, path::Path};

use review::{
    state::Review,
    store::SessionStore,
    vcs::{Backend, Change, Comparison, Entry, LineCounts, Repository},
};

fn repo(root: &Path) -> Repository {
    Repository {
        root: root.to_owned(),
        backend: Backend::Jj,
    }
}

fn review() -> Review {
    Review::new(
        Comparison {
            base_ref: "main".into(),
            head_ref: "@".into(),
            base_id: "base".into(),
            head_id: "head".into(),
            line_counts: Some(LineCounts {
                added: 12,
                removed: 3,
            }),
        },
        vec![Change {
            path: "src/a.rs".into(),
            old_path: "src/a.rs".into(),
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
        }],
    )
}

#[test]
fn session_roundtrip_preserves_snapshots_progress_and_navigation() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo(dir.path());
    let store = SessionStore::new(dir.path(), &repo).unwrap();
    let (mut saved, lock) = store.create(review()).unwrap();
    saved.review.toggle_viewed(0);
    saved.review.view.selected = "src/a.rs".into();
    saved.review.view.collapsed.insert("src/".into());
    saved
        .review
        .view
        .positions
        .entry("src/a.rs".into())
        .or_default()
        .vertical = 300;
    saved.review.view.inline = true;
    saved.review.view.tree_width = Some(40);
    lock.save(&mut saved).unwrap();
    let id = saved.id.clone();
    drop(lock);
    let (loaded, _lock) = store.open(&id).unwrap();
    assert_eq!(loaded.review.comparison.head_id, "head");
    assert!(loaded.review.is_viewed(&loaded.review.changes[0]));
    assert_eq!(loaded.review.view.selected, "src/a.rs");
    assert_eq!(loaded.review.view.positions["src/a.rs"].vertical, 300);
    assert!(loaded.review.view.collapsed.contains("src/"));
    assert!(loaded.review.view.inline);
    assert_eq!(loaded.review.view.tree_width, Some(40));
    assert_eq!(
        loaded.review.comparison.line_counts,
        Some(LineCounts {
            added: 12,
            removed: 3
        })
    );
}

#[test]
fn sessions_without_line_counts_preserve_their_pinned_snapshots_and_viewed_marks() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo(dir.path());
    let store = SessionStore::new(dir.path(), &repo).unwrap();
    let (mut saved, lock) = store.create(review()).unwrap();
    saved.review.toggle_viewed(0);
    let mut legacy = serde_json::to_value(&saved).unwrap();
    legacy["review"]["comparison"]
        .as_object_mut()
        .unwrap()
        .remove("line_counts");
    fs::write(
        store.directory.join(format!("{}.json", saved.id)),
        serde_json::to_vec(&legacy).unwrap(),
    )
    .unwrap();
    drop(lock);

    let (loaded, _lock) = store.open(&saved.id).unwrap();

    assert_eq!(loaded.review.comparison.head_id, "head");
    assert_eq!(loaded.review.comparison.line_counts, None);
    assert!(loaded.review.is_viewed(&loaded.review.changes[0]));
}

#[test]
fn locks_prevent_concurrent_writers_and_sessions_are_repository_scoped() {
    let dir = tempfile::tempdir().unwrap();
    let first = repo(&dir.path().join("first"));
    let second = repo(&dir.path().join("second"));
    let store = SessionStore::new(dir.path(), &first).unwrap();
    let other = SessionStore::new(dir.path(), &second).unwrap();
    let (saved, lock) = store.create(review()).unwrap();
    assert!(store.open(&saved.id).is_err());
    assert_eq!(store.list().unwrap().len(), 1);
    assert!(other.list().unwrap().is_empty());
    drop(lock);
    assert!(store.open(&saved.id).is_ok());
    assert!(store.open("../../arbitrary").is_err());
}

#[test]
fn corrupt_and_future_sessions_are_reported_without_overwriting() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo(dir.path());
    let store = SessionStore::new(dir.path(), &repo).unwrap();
    let (mut saved, lock) = store.create(review()).unwrap();
    saved.version = 999;
    lock.save(&mut saved).unwrap();
    drop(lock);
    assert!(store.open(&saved.id).is_err());
    let path = store.directory.join(format!("{}.json", saved.id));
    fs::write(&path, "broken JSON").unwrap();
    assert!(store.open(&saved.id).is_err());
    assert_eq!(fs::read_to_string(path).unwrap(), "broken JSON");
}

#[test]
fn refresh_preserves_identical_changes_and_invalidates_changes_to_either_side() {
    let mut review = review();
    review.toggle_viewed(0);
    let mut comparison = review.comparison.clone();
    comparison.head_id = "rewritten-history".into();
    assert_eq!(
        review.reconcile(comparison.clone(), review.changes.clone()),
        0
    );
    assert!(review.is_viewed(&review.changes[0]));
    let mut changes = review.changes.clone();
    changes[0].old.object_id = "different base content".into();
    assert_eq!(review.reconcile(comparison.clone(), changes), 1);
    assert!(!review.is_viewed(&review.changes[0]));
    review.toggle_viewed(0);
    let mut changes = review.changes.clone();
    changes[0].new.executable = true;
    assert_eq!(review.reconcile(comparison.clone(), changes), 1);
    review.toggle_viewed(0);
    assert_eq!(review.reconcile(comparison, vec![]), 1);
    assert!(review.viewed.is_empty());
}

#[test]
fn refresh_preserves_directory_scroll_only_while_its_descendants_are_identical() {
    let mut review = review();
    review
        .view
        .positions
        .entry("src/".into())
        .or_default()
        .vertical = 20;
    review
        .view
        .positions
        .entry("src/a.rs".into())
        .or_default()
        .vertical = 3;
    let mut changes = review.changes.clone();
    let mut outside = changes[0].clone();
    outside.path = "outside.txt".into();
    outside.old_path = outside.path.clone();
    changes.push(outside);
    review.reconcile(review.comparison.clone(), changes);
    assert_eq!(review.view.positions["src/"].vertical, 20);
    assert_eq!(review.view.positions["src/a.rs"].vertical, 3);
    let mut changes = review.changes.clone();
    changes[0].new.object_id = "changed".into();
    review.reconcile(review.comparison.clone(), changes);
    assert!(!review.view.positions.contains_key("src/"));
    assert!(!review.view.positions.contains_key("src/a.rs"));
}
