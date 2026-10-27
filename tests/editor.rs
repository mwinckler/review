mod common;

use std::{
    fs,
    path::Path,
    thread,
    time::{Duration, Instant},
};

use review::{
    editor::{self, EditorConfig, EditorWorker},
    hunks::HunkLocation,
    vcs::{Backend, Repository},
};

fn arguments(path: &Path, count: usize) -> Vec<Vec<u8>> {
    let start = Instant::now();
    loop {
        if let Ok(bytes) = fs::read(path) {
            let fields: Vec<_> = bytes
                .split(|byte| *byte == 0)
                .filter(|field| !field.is_empty())
                .map(|field| field.to_vec())
                .collect();
            if bytes.ends_with(&[0]) && fields.len() == count {
                return fields;
            }
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "editor arguments were not written"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn editor_locations_follow_working_copy_insertions_and_use_read_only_snapshots_when_needed() {
    for backend in [Backend::Git, Backend::Jj] {
        let dir = common::fixture();
        let repo = Repository::discover(dir.path(), Some(backend)).unwrap();
        let comparison = repo.snapshot_expression("main..feature").unwrap();
        let changes = repo.changes(&comparison).unwrap();
        let file = changes
            .iter()
            .find(|change| change.path == "src/main.rs")
            .unwrap();
        let source = HunkLocation {
            path: file.path.clone(),
            old_line: Some(1),
            new_line: Some(1),
        };
        let cache = tempfile::tempdir().unwrap();
        let location = editor::location(&repo, &comparison, file, &source, cache.path()).unwrap();
        assert_eq!(location.file, dir.path().join("src/main.rs"));
        assert_eq!(location.line, 1);
        assert!(!location.read_only);
        fs::write(
            dir.path().join("src/main.rs"),
            "// inserted\nfn main() { println!(\"new\"); }\n",
        )
        .unwrap();
        let location = editor::location(&repo, &comparison, file, &source, cache.path()).unwrap();
        assert_eq!(location.line, 2);
        assert!(!location.read_only);
        fs::write(dir.path().join("src/main.rs"), "entirely replaced\n").unwrap();
        let location = editor::location(&repo, &comparison, file, &source, cache.path()).unwrap();
        assert!(location.read_only);
        assert_eq!(location.line, 1);
        assert_eq!(
            fs::read(&location.file).unwrap(),
            b"fn main() { println!(\"new\"); }\n"
        );
        assert!(
            fs::metadata(&location.file)
                .unwrap()
                .permissions()
                .readonly()
        );
        #[cfg(unix)]
        {
            let outside = tempfile::NamedTempFile::new().unwrap();
            fs::write(outside.path(), "outside the repository\n").unwrap();
            fs::remove_file(dir.path().join("src/main.rs")).unwrap();
            std::os::unix::fs::symlink(outside.path(), dir.path().join("src/main.rs")).unwrap();
            let location =
                editor::location(&repo, &comparison, file, &source, cache.path()).unwrap();
            assert!(location.read_only);
            assert_eq!(
                fs::read(location.file).unwrap(),
                b"fn main() { println!(\"new\"); }\n"
            );
            assert_eq!(
                fs::read(outside.path()).unwrap(),
                b"outside the repository\n"
            );
        }
        let removed = changes
            .iter()
            .find(|change| change.path == "removed.txt")
            .unwrap();
        let source = HunkLocation {
            path: removed.path.clone(),
            old_line: Some(1),
            new_line: None,
        };
        let location =
            editor::location(&repo, &comparison, removed, &source, cache.path()).unwrap();
        assert!(location.read_only);
        assert_eq!(fs::read(location.file).unwrap(), b"remove me\n");
        assert!(!dir.path().join("removed.txt").exists());
    }
}

#[test]
fn editor_launcher_passes_arguments_literally_and_reports_failures_asynchronously() {
    let dir = common::fixture();
    let repo = Repository::discover(dir.path(), Some(Backend::Git)).unwrap();
    let comparison = repo.snapshot_expression("main..feature").unwrap();
    let changes = repo.changes(&comparison).unwrap();
    let file = changes
        .iter()
        .find(|change| change.path == "space and\nλ.txt")
        .unwrap();
    let state = tempfile::tempdir().unwrap();
    let log = state.path().join("arguments");
    let config = EditorConfig {
        command: vec![
            "sh".into(),
            "-c".into(),
            "printf '%s\\0' \"$@\" > \"$0\"".into(),
            log.to_str().unwrap().into(),
            "{file}".into(),
            "{line}".into(),
            "{column}".into(),
            "{read_only}".into(),
        ],
    };
    let worker = EditorWorker::new(config.clone(), state.path().join("snapshots"));
    worker
        .request(
            &repo,
            &comparison,
            file,
            HunkLocation {
                path: file.path.clone(),
                old_line: None,
                new_line: Some(1),
            },
        )
        .unwrap();
    let opened = worker
        .results
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    assert!(!opened.read_only);
    assert_eq!(
        arguments(&log, 3),
        vec![
            opened.file.as_os_str().as_encoded_bytes().to_vec(),
            b"1".to_vec(),
            b"1".to_vec()
        ]
    );
    drop(worker);
    fs::remove_file(dir.path().join(&file.path)).unwrap();
    let worker = EditorWorker::new(config, state.path().join("snapshots"));
    worker
        .request(
            &repo,
            &comparison,
            file,
            HunkLocation {
                path: file.path.clone(),
                old_line: None,
                new_line: Some(1),
            },
        )
        .unwrap();
    let opened = worker
        .results
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    assert!(opened.read_only);
    assert_eq!(arguments(&log, 4)[3], b"-R");
    drop(worker);
    assert_eq!(fs::read(opened.file).unwrap(), b"new file\n");
    let worker = EditorWorker::new(
        EditorConfig {
            command: vec!["/missing/review-editor".into()],
        },
        state.path().join("snapshots"),
    );
    worker
        .request(
            &repo,
            &comparison,
            file,
            HunkLocation {
                path: file.path.clone(),
                old_line: None,
                new_line: Some(1),
            },
        )
        .unwrap();
    assert!(
        worker
            .results
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .is_err()
    );
    let worker = EditorWorker::new(
        EditorConfig {
            command: vec!["sh".into(), "-c".into(), "exit 7".into()],
        },
        state.path().join("snapshots"),
    );
    worker
        .request(
            &repo,
            &comparison,
            file,
            HunkLocation {
                path: file.path.clone(),
                old_line: None,
                new_line: Some(1),
            },
        )
        .unwrap();
    assert!(
        worker
            .results
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap_err()
            .contains('7')
    );
}
