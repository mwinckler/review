mod common;
#[path = "common/session.rs"]
mod session_fixture;

use std::{fs, path::Path, process::Command};

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
        let saved = session_fixture::write(
            &store,
            &repo,
            id,
            Review::new(comparison.clone(), changes.clone()),
        );
        let mut record = serde_json::to_value(saved).unwrap();
        record["updated_at"] = updated_at.into();
        if let Some(opened_at) = opened_at {
            record["opened_at"] = opened_at.into();
        } else {
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
    for id in ["r1", "r2"] {
        let output = cli(repo_dir.path(), state.path())
            .args(["--resume", id])
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
    for (prefix, message) in [
        ("abc", "Ambiguous session ID prefix"),
        ("f", "No saved review matches"),
        ("abc1", "interactive terminal"),
    ] {
        let output = cli(repo_dir.path(), state.path())
            .args(["--resume", prefix])
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
    let output = cli(repo.path(), state.path())
        .arg("--resume")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("No saved review"));
    assert!(error.contains("Start one with review"));
}
