mod common;
#[path = "common/session.rs"]
mod session_fixture;

use std::{
    fs,
    path::Path,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use fs2::FileExt;

use review::{
    state::Review,
    store::SessionStore,
    vcs::{Backend, Repository},
};

fn cli(repo: &Path, state: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_review"));
    command.env("JJ_CONFIG", "/dev/null").args([
        "-C",
        repo.to_str().unwrap(),
        "--state-dir",
        state.to_str().unwrap(),
        "--backend",
        "git",
    ]);
    command
}

#[test]
fn lists_saved_sessions_without_a_terminal() {
    let repo_dir = common::fixture();
    let state = tempfile::tempdir().unwrap();
    let output = cli(repo_dir.path(), state.path())
        .arg("--list")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let repo = Repository::discover(repo_dir.path(), Some(Backend::Git)).unwrap();
    let comparison = repo.snapshot("main", "feature").unwrap();
    let changes = repo.changes(&comparison).unwrap();
    let store = SessionStore::new(state.path(), &repo).unwrap();
    let (saved, lock) = store.create(Review::new(comparison, changes)).unwrap();
    drop(lock);
    let output = cli(repo_dir.path(), state.path())
        .arg("--list")
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains(&saved.id));
    assert!(text.contains("main → feature"));
    assert!(!text.contains('\u{1b}'));
}

#[test]
fn session_list_shows_last_opened_datetimes_in_utc_and_falls_back_for_legacy_sessions() {
    let root = common::fixture();
    let state = tempfile::tempdir().unwrap();
    let repo = Repository::discover(root.path(), Some(Backend::Git)).unwrap();
    let comparison = repo.snapshot_expression("HEAD~1..HEAD").unwrap();
    let changes = repo.changes(&comparison).unwrap();
    let store = SessionStore::new(state.path(), &repo).unwrap();
    let mut records = Vec::new();
    for (id, opened_at, updated_at, datetime) in [
        ("r1", Some(0), 1_700_000_000_000_u64, "1970-01-01T00:00:00Z"),
        ("r2", Some(1_700_000_000_999_u64), 0, "2023-11-14T22:13:20Z"),
        ("r3", None, 1_000_000_000_000, "2001-09-09T01:46:40Z"),
    ] {
        let mut saved = session_fixture::write(
            &store,
            &repo,
            id,
            Review::new(comparison.clone(), changes.clone()),
        );
        saved.updated_at = updated_at;
        saved.opened_at = opened_at;
        let mut record = serde_json::to_value(saved).unwrap();
        if opened_at.is_none() {
            record.as_object_mut().unwrap().remove("opened_at");
        }
        let path = store.directory.join(format!("{id}.json"));
        let bytes = serde_json::to_vec(&record).unwrap();
        fs::write(&path, &bytes).unwrap();
        records.push((id, datetime, path, bytes));
    }
    let output = cli(root.path(), state.path())
        .env("TZ", "Pacific/Honolulu")
        .arg("--list")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    for (id, datetime, path, bytes) in records {
        assert!(
            text.lines()
                .any(|line| line.starts_with(&format!("{id}  Opened {datetime}  "))),
            "{text}"
        );
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
    assert!(!text.contains('\u{1b}'));
}

#[test]
fn short_session_numbers_are_listed_resumable_and_accepted_when_forgetting() {
    let repo_dir = common::fixture();
    let state = tempfile::tempdir().unwrap();
    let repo = Repository::discover(repo_dir.path(), Some(Backend::Git)).unwrap();
    let comparison = repo.snapshot_expression("HEAD~1..HEAD").unwrap();
    let changes = repo.changes(&comparison).unwrap();
    let store = SessionStore::new(state.path(), &repo).unwrap();
    for number in 1..=2 {
        let (saved, _lock) = store
            .create(Review::new(comparison.clone(), changes.clone()))
            .unwrap();
        assert_eq!(saved.id, format!("r{number}"));
    }
    let output = cli(repo_dir.path(), state.path())
        .arg("--list")
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.lines().any(|line| line.starts_with("r1  ")), "{text}");
    assert!(text.lines().any(|line| line.starts_with("r2  ")), "{text}");
    for args in [
        &["--resume"] as &[&str],
        &["-r"],
        &["--resume", "r1"],
        &["-r", "r1"],
        &["--resume", "r2"],
        &["-r", "r2"],
    ] {
        let output = cli(repo_dir.path(), state.path())
            .args(args)
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&output.stderr).contains("interactive terminal"));
    }
    let output = cli(repo_dir.path(), state.path())
        .args(["--forget", "r1", "--yes"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(store.list().unwrap()[0].id, "r2");
    assert_eq!(store.list().unwrap().len(), 1);
}

#[test]
fn resume_prefix_errors_are_actionable_and_valid_prefixes_reach_the_terminal_check() {
    let repo_dir = common::fixture();
    let state = tempfile::tempdir().unwrap();
    let repo = Repository::discover(repo_dir.path(), Some(Backend::Git)).unwrap();
    let comparison = repo.snapshot_expression("HEAD~1..HEAD").unwrap();
    let changes = repo.changes(&comparison).unwrap();
    let store = SessionStore::new(state.path(), &repo).unwrap();
    for id in ["abc123000001", "abc9ff000002"] {
        session_fixture::write(
            &store,
            &repo,
            id,
            Review::new(comparison.clone(), changes.clone()),
        );
    }
    for flag in ["--resume", "-r"] {
        for (prefix, message) in [
            ("abc", "Ambiguous session ID prefix"),
            ("f", "No saved review matches"),
            ("abc1", "interactive terminal"),
        ] {
            let output = cli(repo_dir.path(), state.path())
                .args([flag, prefix])
                .output()
                .unwrap();
            assert!(!output.status.success());
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(error.contains(message), "{error}");
            if prefix == "abc" {
                assert!(error.contains("abc123000001"), "{error}");
                assert!(error.contains("abc9ff000002"), "{error}");
            }
        }
    }
    assert_eq!(std::fs::read_dir(&store.directory).unwrap().count(), 2);
}

#[test]
fn noninteractive_launch_accepts_comparisons_and_keeps_state_empty() {
    let repo = common::fixture();
    let state = tempfile::tempdir().unwrap();
    for args in [
        &[] as &[&str],
        &["--base", "main"],
        &["main..feature"],
        &["main.."],
        &["..feature"],
        &[".."],
        &["main"],
        &["@"],
        &["main...feature"],
        &["HEAD~1..@"],
    ] {
        let output = cli(repo.path(), state.path()).args(args).output().unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("interactive terminal"), "{args:?}: {error}");
    }
    let repo = Repository::discover(repo.path(), Some(Backend::Git)).unwrap();
    assert!(
        SessionStore::new(state.path(), &repo)
            .unwrap()
            .list()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn forgetting_sessions_requires_confirmation_and_accepts_multiple_unique_prefixes() {
    let root = common::fixture();
    let state = tempfile::tempdir().unwrap();
    let repo = Repository::discover(root.path(), Some(Backend::Git)).unwrap();
    let comparison = repo.snapshot_expression("HEAD~1..HEAD").unwrap();
    let changes = repo.changes(&comparison).unwrap();
    let store = SessionStore::new(state.path(), &repo).unwrap();
    for id in ["a1", "b2", "c3"] {
        session_fixture::write(
            &store,
            &repo,
            id,
            Review::new(comparison.clone(), changes.clone()),
        );
    }
    let output = cli(root.path(), state.path())
        .args(["--forget", "a"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--yes"));
    assert_eq!(store.list().unwrap().len(), 3);
    let output = cli(root.path(), state.path())
        .args(["--forget", "a", "a1", "b", "--yes"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Forgot 2"));
    assert_eq!(store.list().unwrap().len(), 1);
    assert_eq!(store.list().unwrap()[0].id, "c3");
}

#[test]
fn forget_all_preserves_open_sessions_and_explicit_forgetting_rejects_them() {
    let root = common::fixture();
    let state = tempfile::tempdir().unwrap();
    let repo = Repository::discover(root.path(), Some(Backend::Git)).unwrap();
    let comparison = repo.snapshot_expression("HEAD~1..HEAD").unwrap();
    let changes = repo.changes(&comparison).unwrap();
    let store = SessionStore::new(state.path(), &repo).unwrap();
    for id in ["a1", "b2", "c3"] {
        session_fixture::write(
            &store,
            &repo,
            id,
            Review::new(comparison.clone(), changes.clone()),
        );
    }
    let (_, _lock) = store.open("a1").unwrap();
    let output = cli(root.path(), state.path())
        .args(["--forget-all", "-y"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(store.list().unwrap().len(), 1);
    assert_eq!(store.list().unwrap()[0].id, "a1");
    let output = cli(root.path(), state.path())
        .args(["--forget", "a", "--yes"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("already open"));
    assert_eq!(store.list().unwrap().len(), 1);
}

#[test]
fn cleanup_removes_only_stale_closed_reviews_and_preserves_repository_scoping_and_shared_files() {
    let root = common::fixture();
    let state = tempfile::tempdir().unwrap();
    let repo = Repository::discover(root.path(), Some(Backend::Git)).unwrap();
    let comparison = repo.snapshot_expression("HEAD~1..HEAD").unwrap();
    let changes = repo.changes(&comparison).unwrap();
    let store = SessionStore::new(state.path(), &repo).unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let day = 24 * 60 * 60 * 1_000;
    for (id, opened_at, updated_at) in [
        ("r1", Some(now - 8 * day), now),
        ("r2", Some(now - 6 * day), 0),
        ("r3", Some(now + day), 0),
        ("r4", None, now - 8 * day),
        ("r5", None, now - 6 * day),
        ("r6", Some(now - 8 * day), 0),
    ] {
        let mut saved = session_fixture::write(
            &store,
            &repo,
            id,
            Review::new(comparison.clone(), changes.clone()),
        );
        saved.updated_at = updated_at;
        saved.opened_at = opened_at;
        let mut record = serde_json::to_value(saved).unwrap();
        if opened_at.is_none() {
            record.as_object_mut().unwrap().remove("opened_at");
        }
        fs::write(
            store.directory.join(format!("{id}.json")),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
    }
    let active = fs::File::options()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(store.directory.join("r6.lock"))
        .unwrap();
    active.try_lock_exclusive().unwrap();
    fs::create_dir(store.directory.join("snapshots")).unwrap();
    fs::write(
        store.directory.join("snapshots/editor.rs"),
        "editor snapshot",
    )
    .unwrap();
    let contents = fs::read(root.path().join("src/main.rs")).unwrap();
    let other_repo = Repository {
        root: root.path().join("other"),
        ..repo.clone()
    };
    let other_backend = Repository {
        backend: Backend::Jj,
        ..repo.clone()
    };
    let mut others = Vec::new();
    for repository in [other_repo, other_backend] {
        let other = SessionStore::new(state.path(), &repository).unwrap();
        session_fixture::write(
            &other,
            &repository,
            "r1",
            Review::new(comparison.clone(), changes.clone()),
        );
        others.push(other);
    }
    let output = cli(root.path(), state.path())
        .arg("--cleanup")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--yes"));
    assert_eq!(store.list().unwrap().len(), 6);
    let output = cli(root.path(), state.path())
        .args(["--cleanup", "--yes"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Forgot 2"));
    let ids: std::collections::BTreeSet<_> = store
        .list()
        .unwrap()
        .into_iter()
        .map(|saved| saved.id)
        .collect();
    assert_eq!(ids, ["r2", "r3", "r5", "r6"].map(str::to_owned).into());
    assert!(store.directory.join("r1.lock").exists());
    assert!(store.directory.join("r4.lock").exists());
    assert_eq!(
        fs::read(store.directory.join("snapshots/editor.rs")).unwrap(),
        b"editor snapshot"
    );
    assert_eq!(fs::read(root.path().join("src/main.rs")).unwrap(), contents);
    for other in others {
        assert_eq!(other.list().unwrap().len(), 1);
    }
    let (saved, _lock) = store.create(Review::new(comparison, changes)).unwrap();
    assert_eq!(saved.id, "r7");
}

#[test]
fn cleanup_without_stale_reviews_succeeds_without_a_terminal_or_confirmation() {
    let root = common::fixture();
    let state = tempfile::tempdir().unwrap();
    let repo = Repository::discover(root.path(), Some(Backend::Git)).unwrap();
    let comparison = repo.snapshot_expression("HEAD~1..HEAD").unwrap();
    let changes = repo.changes(&comparison).unwrap();
    let store = SessionStore::new(state.path(), &repo).unwrap();
    for create in [false, true] {
        if create {
            store
                .create(Review::new(comparison.clone(), changes.clone()))
                .unwrap();
        }
        let output = cli(root.path(), state.path())
            .arg("--cleanup")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("No stale review sessions"));
    }
}

#[test]
fn forgetting_flags_conflict_with_other_session_and_comparison_modes() {
    for args in [
        vec!["--forget", "a", "--list"],
        vec!["--forget", "a", "--resume"],
        vec!["--forget", "a", "--base", "master"],
        vec!["HEAD~1..HEAD", "--forget", "a"],
        vec!["--forget-all", "--forget", "a"],
        vec!["--forget-all", "--list"],
        vec!["--yes"],
        vec!["--forget"],
        vec!["--cleanup", "--forget", "a"],
        vec!["--cleanup", "--forget-all"],
        vec!["--cleanup", "--list"],
        vec!["--cleanup", "--resume"],
        vec!["--cleanup", "--base", "master"],
        vec!["HEAD~1..HEAD", "--cleanup"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_review"))
            .args(&args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?}");
    }
}

#[test]
fn positional_comparisons_conflict_with_other_comparison_and_session_options() {
    for args in [
        &["main..feature", "--base", "main"] as &[&str],
        &["--base", "main", "main..feature"],
        &["main..feature", "--head", "feature"],
        &["main..feature", "--resume"],
        &["--resume", "latest", "main..feature"],
        &["main..feature", "--list"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_review"))
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("cannot be used with"), "{args:?}: {error}");
    }
}

#[test]
fn resume_without_sessions_explains_how_to_start() {
    let repo = common::fixture();
    let state = tempfile::tempdir().unwrap();
    for flag in ["--resume", "-r"] {
        let output = cli(repo.path(), state.path()).arg(flag).output().unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("No saved review"), "{flag}: {error}");
        assert!(error.contains("Start one with review"), "{flag}: {error}");
    }
}
