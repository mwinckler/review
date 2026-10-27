use std::{fs, path::Path};

use review::{
    state::Review,
    store::SessionStore,
    vcs::{Backend, Change, Comparison, ComparisonMode, Entry, LineCounts, Repository},
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
            base_change_id: None,
            head_change_id: None,
            mode: ComparisonMode::MergeBase,
            line_counts: Some(LineCounts {
                added: 12,
                removed: 3,
            }),
            file_line_counts: Some(
                [(
                    "src/a.rs".into(),
                    LineCounts {
                        added: 12,
                        removed: 3,
                    },
                )]
                .into(),
            ),
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
    assert_eq!(
        loaded.review.comparison.file_line_counts,
        saved.review.comparison.file_line_counts
    );
}

#[test]
fn sessions_preserve_change_ids_and_accept_legacy_comparisons() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo(dir.path());
    let store = SessionStore::new(dir.path(), &repo).unwrap();
    let mut review = review();
    review.comparison.base_change_id = Some("ukrwmnxyzqst".into());
    review.comparison.head_change_id = Some("vqmrstuvwxyz".into());
    review.toggle_viewed(0);
    let (saved, lock) = store.create(review).unwrap();
    drop(lock);
    let (loaded, lock) = store.open(&saved.id).unwrap();
    assert_eq!(
        loaded.review.comparison.base_change_id.as_deref(),
        Some("ukrwmnxyzqst")
    );
    assert_eq!(
        loaded.review.comparison.head_change_id.as_deref(),
        Some("vqmrstuvwxyz")
    );
    let mut legacy = serde_json::to_value(&loaded).unwrap();
    for field in ["base_change_id", "head_change_id"] {
        legacy["review"]["comparison"]
            .as_object_mut()
            .unwrap()
            .remove(field);
    }
    fs::write(
        store.directory.join(format!("{}.json", saved.id)),
        serde_json::to_vec(&legacy).unwrap(),
    )
    .unwrap();
    drop(lock);
    let (loaded, _lock) = store.open(&saved.id).unwrap();
    assert_eq!(loaded.review.comparison.base_id, "base");
    assert_eq!(loaded.review.comparison.head_id, "head");
    assert_eq!(loaded.review.comparison.base_change_id, None);
    assert_eq!(loaded.review.comparison.head_change_id, None);
    assert!(loaded.review.is_viewed(&loaded.review.changes[0]));
}

#[test]
fn sessions_preserve_comparison_modes_and_default_legacy_sessions_to_merge_base() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo(dir.path());
    let store = SessionStore::new(dir.path(), &repo).unwrap();
    let mut review = review();
    review.comparison.mode = ComparisonMode::Direct;
    let (saved, lock) = store.create(review).unwrap();
    drop(lock);
    let (loaded, lock) = store.open(&saved.id).unwrap();
    assert_eq!(loaded.review.comparison.mode, ComparisonMode::Direct);
    let mut legacy = serde_json::to_value(&loaded).unwrap();
    legacy["review"]["comparison"]
        .as_object_mut()
        .unwrap()
        .remove("mode");
    fs::write(
        store.directory.join(format!("{}.json", saved.id)),
        serde_json::to_vec(&legacy).unwrap(),
    )
    .unwrap();
    drop(lock);
    let (loaded, _lock) = store.open(&saved.id).unwrap();
    assert_eq!(loaded.review.comparison.mode, ComparisonMode::MergeBase);
    assert_eq!(
        loaded.review.comparison.base_id,
        saved.review.comparison.base_id
    );
    assert_eq!(
        loaded.review.comparison.head_id,
        saved.review.comparison.head_id
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
    for field in ["line_counts", "file_line_counts"] {
        legacy["review"]["comparison"]
            .as_object_mut()
            .unwrap()
            .remove(field);
    }
    fs::write(
        store.directory.join(format!("{}.json", saved.id)),
        serde_json::to_vec(&legacy).unwrap(),
    )
    .unwrap();
    drop(lock);

    let (loaded, _lock) = store.open(&saved.id).unwrap();

    assert_eq!(loaded.review.comparison.head_id, "head");
    assert_eq!(loaded.review.comparison.line_counts, None);
    assert_eq!(loaded.review.comparison.file_line_counts, None);
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
