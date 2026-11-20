#[path = "common/session.rs"]
mod session_fixture;

use std::{
    fs,
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use fs2::FileExt;

use review::{
    state::Review,
    store::SessionStore,
    vcs::{Backend, Change, Comparison, ComparisonMode, Entry, LineCounts, Repository},
};

const DAY: Duration = Duration::from_secs(24 * 60 * 60);

fn age_session_lock(store: &SessionStore, id: &str, age: Duration) {
    fs::File::options()
        .write(true)
        .open(store.directory.join(format!("{id}.lock")))
        .unwrap()
        .set_modified(SystemTime::now() - age)
        .unwrap();
}

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
            working_tree: None,
            base_title: None,
            head_title: None,
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
            file_commit_ids: None,
            revisions: None,
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
fn renaming_preserves_identity_review_timestamps_and_lock_files_and_supports_legacy_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo(dir.path());
    let store = SessionStore::new(dir.path(), &repo).unwrap();
    let mut review = review();
    review.toggle_viewed(0);
    review
        .view
        .positions
        .entry("src/a.rs".into())
        .or_default()
        .vertical = 19;
    let saved = session_fixture::write(&store, &repo, "abc123", review);
    let path = store.directory.join("abc123.json");
    let mut legacy = serde_json::to_value(&saved).unwrap();
    legacy.as_object_mut().unwrap().remove("name");
    fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();
    let before = serde_json::to_value(&store.list().unwrap()[0]).unwrap();
    let renamed = store.rename("abc", "  API review λ  ").unwrap();
    assert_eq!(renamed.id, "abc123");
    assert_eq!(renamed.name.as_deref(), Some("API review λ"));
    let mut expected = before.clone();
    expected["name"] = "API review λ".into();
    assert_eq!(serde_json::to_value(&renamed).unwrap(), expected);
    assert_eq!(
        serde_json::to_value(&store.list().unwrap()[0]).unwrap(),
        expected
    );
    assert!(store.directory.join("abc123.lock").is_file());
    assert!(!store.directory.join("abc.lock").exists());
    assert!(!store.directory.join("API review λ.json").exists());
    assert_eq!(store.resolve("abc").unwrap(), "abc123");
    assert_eq!(store.cleanup_candidates().unwrap(), ["abc123"]);
    let renamed = store.rename("abc123", "").unwrap();
    assert!(renamed.name.is_none());
    assert_eq!(
        serde_json::to_value(&store.list().unwrap()[0]).unwrap(),
        before
    );
}

#[test]
fn renaming_is_lock_safe_scoped_and_validates_names_before_changing_records() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo(dir.path());
    let store = SessionStore::new(dir.path(), &repo).unwrap();
    let (mut saved, lock) = store.create(review()).unwrap();
    assert!(saved.name.is_none());
    let before = fs::read(store.directory.join("r1.json")).unwrap();
    assert!(store.rename("r1", "External rename").is_err());
    assert_eq!(fs::read(store.directory.join("r1.json")).unwrap(), before);
    for name in [
        "bad\nname",
        "bad\rname",
        "bad\tname",
        "bad\u{1b}name",
        "bad\u{2028}name",
        "bad\u{2029}name",
    ] {
        assert!(lock.rename(&mut saved, name).is_err());
        assert!(saved.name.is_none());
        assert_eq!(fs::read(store.directory.join("r1.json")).unwrap(), before);
    }
    let opened = saved.opened_at;
    let updated = saved.updated_at;
    saved.review.toggle_viewed(0);
    lock.rename(&mut saved, "Current session").unwrap();
    assert_eq!(saved.name.as_deref(), Some("Current session"));
    assert_eq!(saved.opened_at, opened);
    assert_eq!(saved.updated_at, updated);
    assert_eq!(store.list().unwrap()[0].review.viewed.len(), 1);
    lock.save(&mut saved).unwrap();
    assert_eq!(store.list().unwrap()[0].name, saved.name);
    let other = SessionStore::new(
        dir.path(),
        &Repository {
            backend: Backend::Git,
            ..repo
        },
    )
    .unwrap();
    assert!(other.rename("r1", "Other backend").is_err());
    assert!(other.list().unwrap().is_empty());
}

#[test]
fn opening_records_a_persistent_timestamp_while_saving_and_listing_preserve_it() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path(), &repo(dir.path())).unwrap();
    let started = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let (mut saved, lock) = store.create(review()).unwrap();
    let opened_at = saved.opened_at.unwrap();
    assert!(opened_at >= started);
    saved.review.toggle_viewed(0);
    lock.save(&mut saved).unwrap();
    assert_eq!(saved.opened_at, Some(opened_at));
    drop(lock);

    let path = store.directory.join(format!("{}.json", saved.id));
    saved.opened_at = Some(1);
    fs::write(&path, serde_json::to_vec(&saved).unwrap()).unwrap();
    let (resumed, lock) = store.open(&saved.id).unwrap();
    let reopened_at = resumed.opened_at.unwrap();
    assert!(reopened_at >= opened_at);
    assert_eq!(resumed.review.viewed.len(), 1);
    let persisted: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(persisted["opened_at"], reopened_at);
    let before = fs::read(&path).unwrap();
    assert!(store.is_open(&saved.id).unwrap());
    assert!(store.open(&saved.id).is_err());
    store.list().unwrap();
    drop(lock);
    assert!(!store.is_open(&saved.id).unwrap());
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn cleanup_preflights_records_before_deleting_any_review() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo(dir.path());
    let store = SessionStore::new(dir.path(), &repo).unwrap();
    for id in ["r1", "r2"] {
        session_fixture::write(&store, &repo, id, review());
    }
    let before = fs::read(store.directory.join("r1.json")).unwrap();
    fs::write(store.directory.join("r2.json"), "broken JSON").unwrap();
    assert!(store.cleanup(&["r1".into(), "r2".into()]).is_err());
    assert_eq!(fs::read(store.directory.join("r1.json")).unwrap(), before);
    assert_eq!(
        fs::read_to_string(store.directory.join("r2.json")).unwrap(),
        "broken JSON"
    );
}

#[test]
fn cleanup_rechecks_confirmed_ids_and_tolerates_missing_or_recent_reviews() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo(dir.path());
    let store = SessionStore::new(dir.path(), &repo).unwrap();
    for id in ["r1", "r2"] {
        session_fixture::write(&store, &repo, id, review());
    }
    let ids = store.cleanup_candidates().unwrap();
    assert_eq!(ids.len(), 2);
    store.forget(&["r1".into()]).unwrap();
    store.open("r2").unwrap();
    session_fixture::write(&store, &repo, "r3", review());
    assert_eq!(store.cleanup(&ids).unwrap(), 0);
    assert!(store.directory.join("r2.json").exists());
    assert_eq!(store.cleanup(&["r3".into(), "r3".into()]).unwrap(), 1);
    assert_eq!(store.list().unwrap().len(), 1);
}

#[test]
fn new_session_ids_are_short_consecutive_review_numbers() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path(), &repo(dir.path())).unwrap();
    for number in 1..=32 {
        let (saved, _lock) = store.create(review()).unwrap();
        assert_eq!(saved.id, format!("r{number}"));
    }
    assert_eq!(store.list().unwrap().len(), 32);
}

#[test]
fn session_numbers_survive_reopening_and_remain_reserved_during_the_deletion_cooldown() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo(dir.path());
    let store = SessionStore::new(dir.path(), &repo).unwrap();
    let (first, lock) = store.create(review()).unwrap();
    assert_eq!(first.id, "r1");
    drop(lock);
    let store = SessionStore::new(dir.path(), &repo).unwrap();
    let (second, lock) = store.create(review()).unwrap();
    assert_eq!(second.id, "r2");
    drop(lock);
    assert_eq!(store.forget(&[first.id, second.id]).unwrap(), 2);
    assert!(store.list().unwrap().is_empty());
    let (third, _lock) = store.create(review()).unwrap();
    assert_eq!(third.id, "r3");
}

#[test]
fn deleted_session_numbers_are_reused_after_seven_days_from_deletion() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo(dir.path());
    let store = SessionStore::new(dir.path(), &repo).unwrap();
    let mut original = review();
    original.toggle_viewed(0);
    let (saved, lock) = store.create(original).unwrap();
    drop(lock);
    age_session_lock(&store, &saved.id, DAY * 8);
    let reservation = fs::File::options()
        .read(true)
        .write(true)
        .open(store.directory.join("r1.lock"))
        .unwrap();
    let deleted_at = SystemTime::now();
    store.forget(&[saved.id]).unwrap();
    assert!(reservation.metadata().unwrap().modified().unwrap() >= deleted_at);
    assert_eq!(store.create(review()).unwrap().0.id, "r2");
    age_session_lock(&store, "r1", DAY * 7 - Duration::from_secs(60 * 60));
    assert_eq!(store.create(review()).unwrap().0.id, "r3");
    age_session_lock(&store, "r1", DAY * 7);
    let store = SessionStore::new(dir.path(), &repo).unwrap();
    let (reused, lock) = store.create(review()).unwrap();
    assert_eq!(reused.id, "r1");
    assert!(reused.review.viewed.is_empty());
    assert!(reservation.try_lock_exclusive().is_err());
    drop(lock);
    reservation.try_lock_exclusive().unwrap();
    assert_eq!(store.list().unwrap().len(), 3);
}

#[test]
fn recycled_session_numbers_are_chosen_lowest_first() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path(), &repo(dir.path())).unwrap();
    for _ in 0..4 {
        store.create(review()).unwrap();
    }
    store.forget(&["r1".into(), "r3".into()]).unwrap();
    age_session_lock(&store, "r1", DAY * 8);
    age_session_lock(&store, "r3", DAY * 9);
    for expected in ["r1", "r3", "r5"] {
        assert_eq!(store.create(review()).unwrap().0.id, expected);
    }
    assert_eq!(store.list().unwrap().len(), 5);
}

#[test]
fn session_number_reuse_preserves_active_sessions_and_held_reservations() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path(), &repo(dir.path())).unwrap();
    for _ in 0..3 {
        store.create(review()).unwrap();
    }
    let active = fs::read(store.directory.join("r1.json")).unwrap();
    age_session_lock(&store, "r1", DAY * 8);
    store.forget(&["r2".into(), "r3".into()]).unwrap();
    age_session_lock(&store, "r2", DAY * 8);
    let held = fs::File::options()
        .read(true)
        .write(true)
        .open(store.directory.join("r2.lock"))
        .unwrap();
    held.try_lock_exclusive().unwrap();
    assert_eq!(store.create(review()).unwrap().0.id, "r4");
    assert_eq!(fs::read(store.directory.join("r1.json")).unwrap(), active);
    assert!(!store.directory.join("r3.json").exists());
    drop(held);
    assert_eq!(store.create(review()).unwrap().0.id, "r2");
}

#[test]
fn abandoned_reservations_age_out_and_future_dated_locks_stay_reserved() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path(), &repo(dir.path())).unwrap();
    fs::write(store.directory.join("r1.lock"), "").unwrap();
    age_session_lock(&store, "r1", DAY * 8);
    let future = fs::File::options()
        .create_new(true)
        .read(true)
        .write(true)
        .open(store.directory.join("r2.lock"))
        .unwrap();
    let future_time = SystemTime::now() + DAY;
    future.set_modified(future_time).unwrap();
    let reclaimed_at = SystemTime::now();
    assert_eq!(store.create(review()).unwrap().0.id, "r1");
    let modified = fs::metadata(store.directory.join("r1.lock"))
        .unwrap()
        .modified()
        .unwrap();
    assert!(modified >= reclaimed_at);
    assert_eq!(store.create(review()).unwrap().0.id, "r3");
    assert_eq!(future.metadata().unwrap().modified().unwrap(), future_time);
}

#[test]
fn expired_numbers_are_reusable_even_at_the_numeric_limit() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path(), &repo(dir.path())).unwrap();
    fs::write(store.directory.join(format!("r{}.lock", u64::MAX)), "").unwrap();
    fs::write(store.directory.join("r1.lock"), "").unwrap();
    age_session_lock(&store, "r1", DAY * 8);
    assert_eq!(store.create(review()).unwrap().0.id, "r1");
}

#[test]
fn session_numbers_skip_existing_records_and_reservations() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo(dir.path());
    let store = SessionStore::new(dir.path(), &repo).unwrap();
    session_fixture::write(&store, &repo, "r5", review());
    let previous = fs::read(store.directory.join("r5.json")).unwrap();
    fs::write(store.directory.join("r8.lock"), "").unwrap();
    fs::write(store.directory.join("r99.tmp"), "unrelated file").unwrap();
    let (saved, _lock) = store.create(review()).unwrap();
    assert_eq!(saved.id, "r9");
    assert_eq!(fs::read(store.directory.join("r5.json")).unwrap(), previous);
    assert_eq!(store.list().unwrap().len(), 2);
}

#[test]
fn session_numbering_is_repository_and_backend_local_and_independent_of_legacy_ids() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo(dir.path());
    let store = SessionStore::new(dir.path(), &repo).unwrap();
    for id in ["123456789012", "abcdef123456", "18b444222aaa-abc-0"] {
        session_fixture::write(&store, &repo, id, review());
    }
    let (saved, _lock) = store.create(review()).unwrap();
    assert_eq!(saved.id, "r1");
    assert_eq!(store.open("123").unwrap().0.id, "123456789012");
    assert_eq!(store.open("abc").unwrap().0.id, "abcdef123456");
    let other_repo = Repository {
        root: dir.path().join("other"),
        ..repo.clone()
    };
    let other_backend = Repository {
        backend: Backend::Git,
        ..repo
    };
    for repo in [other_repo, other_backend] {
        let other = SessionStore::new(dir.path(), &repo).unwrap();
        assert_eq!(other.create(review()).unwrap().0.id, "r1");
    }
}

#[test]
fn concurrent_creators_reserve_distinct_session_numbers() {
    for recycled in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path(), &repo(dir.path())).unwrap();
        if recycled {
            for number in 1..=16 {
                fs::write(store.directory.join(format!("r{number}.lock")), "").unwrap();
                age_session_lock(&store, &format!("r{number}"), DAY * 8);
            }
        }
        let barrier = std::sync::Barrier::new(16);
        let saved = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..16)
                .map(|index| {
                    let store = &store;
                    let barrier = &barrier;
                    scope.spawn(move || {
                        let mut review = review();
                        review.view.filter = format!("creator-{index}");
                        barrier.wait();
                        store.create(review).unwrap().0
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });
        let ids: std::collections::BTreeSet<_> =
            saved.iter().map(|saved| saved.id.clone()).collect();
        assert_eq!(
            ids,
            (1..=16).map(|number| format!("r{number}")).collect(),
            "recycled: {recycled}"
        );
        assert_eq!(store.list().unwrap().len(), 16);
        for saved in saved {
            let (loaded, _lock) = store.open(&saved.id).unwrap();
            assert_eq!(loaded.review.view.filter, saved.review.view.filter);
        }
    }
}

#[test]
fn numbered_sessions_sort_numerically_and_keep_exact_lookup_and_deletion() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo(dir.path());
    let store = SessionStore::new(dir.path(), &repo).unwrap();
    for id in ["r1", "r2", "r9", "r10", "r11", "abc123000001"] {
        session_fixture::write(&store, &repo, id, review());
    }
    assert_eq!(
        store
            .list()
            .unwrap()
            .iter()
            .map(|saved| saved.id.as_str())
            .collect::<Vec<_>>(),
        ["r11", "r10", "r9", "r2", "r1", "abc123000001"]
    );
    let (mut saved, lock) = store.open("r1").unwrap();
    assert_eq!(saved.id, "r1");
    assert!(store.is_open("r1").unwrap());
    assert!(!store.is_open("r10").unwrap());
    saved.review.toggle_viewed(0);
    lock.save(&mut saved).unwrap();
    assert_eq!(store.list().unwrap()[0].id, "r1");
    drop(lock);
    assert_eq!(store.open("r1").unwrap().0.review.viewed.len(), 1);
    let error = store.resolve("r").unwrap_err().to_string();
    assert!(error.contains("Ambiguous session ID prefix"), "{error}");
    assert!(error.contains("r10"), "{error}");
    assert_eq!(store.forget(&["r1".into(), "r10".into()]).unwrap(), 2);
    assert!(store.open("r11").is_ok());
    assert_eq!(store.open("abc").unwrap().0.id, "abc123000001");
}

#[test]
fn exhausted_session_numbers_report_an_error_and_preserve_existing_files() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path(), &repo(dir.path())).unwrap();
    let reservation = store.directory.join(format!("r{}.lock", u64::MAX));
    fs::write(&reservation, "reserved").unwrap();
    let error = store.create(review()).err().unwrap().to_string();
    assert!(error.contains("session numbers exhausted"), "{error}");
    assert_eq!(fs::read_to_string(reservation).unwrap(), "reserved");
    assert_eq!(fs::read_dir(&store.directory).unwrap().count(), 1);
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
    assert!(error.contains("review list"), "{error}");
    let error = store.open("ff").err().unwrap().to_string();
    assert!(error.contains("No saved review matches"), "{error}");
    assert!(error.contains("ff"), "{error}");
    for prefix in [
        "", "../abc", "abc/../", "abc.json", "r/1", "r-1", "r1.json", "☃",
    ] {
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
        ("r1", "r1"),
        ("r10", "r10"),
        ("r2", "r2"),
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
            assert_ne!(
                store.resolve(&prefix[..prefix.len() - 1]).ok().as_deref(),
                Some(id)
            );
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
