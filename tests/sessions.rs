#[path = "common/session.rs"]
mod session_fixture;

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
fn new_session_ids_are_compact_lowercase_hex_and_distinct() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path(), &repo(dir.path())).unwrap();
    let mut ids = std::collections::BTreeSet::new();
    for _ in 0..32 {
        let (saved, _lock) = store.create(review()).unwrap();
        assert_eq!(saved.id.len(), 12);
        assert!(
            saved
                .id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        );
        assert!(ids.insert(saved.id));
    }
    assert_eq!(store.list().unwrap().len(), 32);
}

#[test]
fn forgetting_sessions_resolves_and_deduplicates_prefixes_and_preserves_shared_files() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo(dir.path());
    let store = SessionStore::new(dir.path(), &repo).unwrap();
    for id in ["a1", "b2", "c3"] {
        session_fixture::write(&store, &repo, id, review());
    }
    fs::create_dir(store.directory.join("snapshots")).unwrap();
    fs::write(
        store.directory.join("snapshots/editor.rs"),
        "reviewed contents",
    )
    .unwrap();
    fs::write(dir.path().join("working-file"), "working contents").unwrap();
    assert_eq!(
        store
            .forget(&["a".into(), "a1".into(), "b".into()])
            .unwrap(),
        2
    );
    assert_eq!(store.list().unwrap()[0].id, "c3");
    assert!(store.directory.join("a1.lock").exists());
    assert!(store.directory.join("b2.lock").exists());
    assert_eq!(
        fs::read_to_string(store.directory.join("snapshots/editor.rs")).unwrap(),
        "reviewed contents"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("working-file")).unwrap(),
        "working contents"
    );
    assert!(store.open("a1").is_err());
}

#[test]
fn forgetting_preflights_all_ids_and_locks_before_removing_any_session() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo(dir.path());
    let store = SessionStore::new(dir.path(), &repo).unwrap();
    for id in ["a1", "b2", "b3"] {
        session_fixture::write(&store, &repo, id, review());
    }
    let (_, lock) = store.open("b2").unwrap();
    assert!(store.is_open("b2").unwrap());
    for ids in [
        vec!["a", "b"],
        vec!["a", "b2"],
        vec!["a", "f"],
        vec!["a", "../"],
        vec![],
    ] {
        let ids: Vec<String> = ids.into_iter().map(str::to_owned).collect();
        assert!(store.forget(&ids).is_err());
        assert_eq!(store.list().unwrap().len(), 3);
        assert!(!store.is_open("a1").unwrap());
    }
    drop(lock);
    assert!(!store.is_open("b2").unwrap());
    assert_eq!(store.forget(&["a".into(), "b2".into()]).unwrap(), 2);
    assert_eq!(store.list().unwrap()[0].id, "b3");
}

#[test]
fn forgetting_corrupt_records_is_scoped_to_the_repository_and_backend() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo(dir.path());
    let store = SessionStore::new(dir.path(), &repo).unwrap();
    let other_repo = Repository {
        root: dir.path().join("other"),
        backend: repo.backend,
    };
    let other = SessionStore::new(dir.path(), &other_repo).unwrap();
    let git_repo = Repository {
        backend: Backend::Git,
        ..repo.clone()
    };
    let git = SessionStore::new(dir.path(), &git_repo).unwrap();
    session_fixture::write(&other, &other_repo, "a1", review());
    session_fixture::write(&git, &git_repo, "a1", review());
    fs::write(store.directory.join("a1.json"), "broken JSON").unwrap();
    assert_eq!(store.forget(&["a".into()]).unwrap(), 1);
    assert_eq!(other.list().unwrap()[0].id, "a1");
    assert_eq!(git.list().unwrap()[0].id, "a1");
}

#[test]
fn unique_prefixes_open_and_save_the_full_session_under_the_same_lock() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo(dir.path());
    let store = SessionStore::new(dir.path(), &repo).unwrap();
    for id in [
        "abc123000001",
        "abc9ff000002",
        "7d8fff000003",
        "18b444222aaa-abc-0",
    ] {
        session_fixture::write(&store, &repo, id, review());
    }
    for (prefix, id) in [
        ("abc1", "abc123000001"),
        ("abc9", "abc9ff000002"),
        ("7", "7d8fff000003"),
        ("18", "18b444222aaa-abc-0"),
    ] {
        let (mut saved, lock) = store.open(prefix).unwrap();
        assert_eq!(saved.id, id);
        assert_eq!(saved.review.comparison.head_id, "head");
        assert!(store.open(id).is_err());
        saved.review.toggle_viewed(0);
        lock.save(&mut saved).unwrap();
        drop(lock);
        let (saved, _lock) = store.open(id).unwrap();
        assert!(saved.review.is_viewed(&saved.review.changes[0]));
        assert!(!store.directory.join(format!("{prefix}.json")).exists());
        assert!(!store.directory.join(format!("{prefix}.lock")).exists());
    }
    assert_eq!(store.list().unwrap().len(), 4);
}

#[test]
fn ambiguous_unknown_and_invalid_prefixes_report_errors_without_creating_files() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo(dir.path());
    let store = SessionStore::new(dir.path(), &repo).unwrap();
    for id in ["abc123000001", "abc9ff000002"] {
        session_fixture::write(&store, &repo, id, review());
    }
    let error = store.open("abc").err().unwrap().to_string();
    assert!(error.contains("Ambiguous session ID prefix"), "{error}");
    assert!(error.contains("abc123000001"), "{error}");
    assert!(error.contains("abc9ff000002"), "{error}");
    assert!(error.contains("review --list"), "{error}");
    let error = store.open("ff").err().unwrap().to_string();
    assert!(error.contains("No saved review matches"), "{error}");
    assert!(error.contains("ff"), "{error}");
    for prefix in ["", "../abc", "abc/../", "abc.json", "☃"] {
        assert!(
            store
                .open(prefix)
                .err()
                .unwrap()
                .to_string()
                .contains("invalid session ID")
        );
    }
    assert_eq!(fs::read_dir(&store.directory).unwrap().count(), 2);
}

#[test]
fn exact_ids_take_precedence_and_prefix_lookup_ignores_unrelated_corrupt_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo(dir.path());
    let store = SessionStore::new(dir.path(), &repo).unwrap();
    let sessions = ["ab", "abcd"].map(|id| session_fixture::write(&store, &repo, id, review()));
    assert_eq!(sessions[0].unique_prefix(&sessions), "ab");
    assert_eq!(sessions[1].unique_prefix(&sessions), "abc");
    fs::write(store.directory.join("ee.json"), "broken JSON").unwrap();
    assert_eq!(store.open("ab").unwrap().0.id, "ab");
    assert_eq!(store.open("abc").unwrap().0.id, "abcd");
    assert!(
        store
            .open("a")
            .err()
            .unwrap()
            .to_string()
            .contains("Ambiguous")
    );
}

#[test]
fn listed_prefixes_are_the_shortest_prefixes_that_resolve_each_session() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo(dir.path());
    let store = SessionStore::new(dir.path(), &repo).unwrap();
    let expected = [
        ("abc123000001", "abc1"),
        ("abc9ff000002", "abc9"),
        ("7d8fff000003", "7"),
        ("18b444222aaa-abc-0", "1"),
    ];
    for (id, _) in expected {
        session_fixture::write(&store, &repo, id, review());
    }
    let sessions = store.list().unwrap();
    for (id, prefix) in expected {
        let saved = sessions.iter().find(|saved| saved.id == id).unwrap();
        assert_eq!(saved.unique_prefix(&sessions), prefix);
        assert_eq!(store.resolve(prefix).unwrap(), id);
        if prefix.len() > 1 {
            assert!(store.resolve(&prefix[..prefix.len() - 1]).is_err());
        }
    }
    let singleton = &sessions[..1];
    assert_eq!(singleton[0].unique_prefix(singleton), &singleton[0].id[..1]);
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
    assert!(other.resolve(&saved.id[..1]).is_err());
    let git = Repository {
        backend: Backend::Git,
        ..first
    };
    assert!(
        SessionStore::new(dir.path(), &git)
            .unwrap()
            .resolve(&saved.id[..1])
            .is_err()
    );
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
