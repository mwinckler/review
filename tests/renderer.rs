mod common;

use std::{fs, thread, time::Duration};

use ratatui::text::Text;
use review::{
    renderer::{RenderOptions, RenderWorker, RendererConfig},
    vcs::{Backend, Comparison, Repository},
};
use tempfile::TempDir;

const OPTIONS: RenderOptions = RenderOptions {
    width: 80,
    inline: false,
};

fn fixture() -> TempDir {
    let dir = common::fixture();
    common::jj(dir.path(), &["new", "feature"]);
    fs::create_dir_all(dir.path().join("src/nested")).unwrap();
    fs::create_dir(dir.path().join("src-other")).unwrap();
    fs::write(
        dir.path().join("src/lib.rs"),
        "pub const DIRECTORY_TOP: u8 = 1;\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("src/nested/lib.rs"),
        "pub const DIRECTORY_NESTED: u8 = 2;\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("src-other/outside.rs"),
        "pub const OUTSIDE: u8 = 3;\n",
    )
    .unwrap();
    common::jj(dir.path(), &["commit", "-m", "directory changes"]);
    common::jj(dir.path(), &["bookmark", "create", "directory", "-r", "@-"]);
    dir
}

fn render(
    worker: &RenderWorker,
    repo: &Repository,
    comparison: &Comparison,
    path: &str,
) -> Result<Text<'static>, String> {
    let changes = repo
        .changes(comparison)
        .unwrap()
        .into_iter()
        .filter(|change| {
            change.path == path || (path.ends_with('/') && change.path.starts_with(path))
        })
        .collect();
    let id = worker.request(repo, comparison, path, changes, OPTIONS);
    let result = worker
        .results
        .recv_timeout(Duration::from_secs(30))
        .unwrap();
    assert_eq!(result.id, id);
    result.text
}

#[test]
fn directory_jobs_render_every_descendant_with_headings_from_pinned_contents() {
    let dir = fixture();
    for backend in [Backend::Git, Backend::Jj] {
        let repo = Repository::discover(dir.path(), Some(backend)).unwrap();
        let comparison = repo.snapshot("main", "directory").unwrap();
        fs::write(dir.path().join("src/lib.rs"), "uncommitted contents\n").unwrap();
        let worker = RenderWorker::new(RendererConfig::default());
        let output = render(&worker, &repo, &comparison, "src/")
            .unwrap()
            .to_string();
        for path in ["src/lib.rs", "src/main.rs", "src/nested/lib.rs"] {
            assert!(output.contains(path), "{output}");
        }
        assert!(output.contains("DIRECTORY_TOP"));
        assert!(output.contains("DIRECTORY_NESTED"));
        assert!(!output.contains("OUTSIDE"));
        assert!(!output.contains("uncommitted contents"));
        let nested = render(&worker, &repo, &comparison, "src/nested/")
            .unwrap()
            .to_string();
        assert!(nested.contains("src/nested/lib.rs"));
        assert!(nested.contains("absent → file"));
        assert!(!nested.contains("DIRECTORY_TOP"));
    }
}

#[test]
fn directory_cache_keys_include_every_descendant() {
    let dir = fixture();
    let log = tempfile::NamedTempFile::new().unwrap();
    let config = RendererConfig {
        command: vec![
            "sh".into(),
            "-c".into(),
            "printf . >> \"$1\"; cat \"$2\"".into(),
            "review-render".into(),
            log.path().to_str().unwrap().into(),
            "{new}".into(),
        ],
        timeout_ms: 5_000,
        ..Default::default()
    };
    let worker = RenderWorker::new(config);
    let repo = Repository::discover(dir.path(), Some(Backend::Git)).unwrap();
    let comparison = repo.snapshot("main", "directory").unwrap();
    render(&worker, &repo, &comparison, "src/").unwrap();
    assert_eq!(fs::metadata(log.path()).unwrap().len(), 3);
    render(&worker, &repo, &comparison, "src/").unwrap();
    assert_eq!(fs::metadata(log.path()).unwrap().len(), 3);

    common::jj(dir.path(), &["new", "directory"]);
    fs::write(
        dir.path().join("src/nested/lib.rs"),
        "pub const NESTED_UPDATED: u8 = 4;\n",
    )
    .unwrap();
    common::jj(dir.path(), &["commit", "-m", "update nested file"]);
    common::jj(dir.path(), &["bookmark", "create", "updated", "-r", "@-"]);
    let comparison = repo.snapshot("main", "updated").unwrap();
    let output = render(&worker, &repo, &comparison, "src/")
        .unwrap()
        .to_string();
    assert!(output.contains("NESTED_UPDATED"));
    assert!(!output.contains("DIRECTORY_NESTED"));
    assert_eq!(fs::metadata(log.path()).unwrap().len(), 6);
}

#[test]
fn directory_jobs_share_one_timeout_budget() {
    let dir = fixture();
    let repo = Repository::discover(dir.path(), Some(Backend::Git)).unwrap();
    let comparison = repo.snapshot("main", "directory").unwrap();
    let worker = RenderWorker::new(RendererConfig {
        command: vec!["sleep".into(), "0.07".into()],
        timeout_ms: 160,
        ..Default::default()
    });
    let error = render(&worker, &repo, &comparison, "src/").unwrap_err();
    assert!(error.contains("timed out"), "{error}");
}

#[test]
fn selecting_a_file_cancels_a_directory_job() {
    let dir = fixture();
    let repo = Repository::discover(dir.path(), Some(Backend::Git)).unwrap();
    let comparison = repo.snapshot("main", "directory").unwrap();
    let changes = repo.changes(&comparison).unwrap();
    let worker = RenderWorker::new(RendererConfig {
        command: vec!["sleep".into(), "0.2".into()],
        timeout_ms: 2_000,
        ..Default::default()
    });
    worker.request(
        &repo,
        &comparison,
        "src/",
        changes
            .iter()
            .filter(|change| change.path.starts_with("src/"))
            .cloned()
            .collect(),
        OPTIONS,
    );
    thread::sleep(Duration::from_millis(30));
    let file = changes
        .iter()
        .find(|change| change.path == "src/main.rs")
        .unwrap();
    let id = worker.request(&repo, &comparison, &file.path, vec![file.clone()], OPTIONS);
    let result = worker.results.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(result.id, id);
    assert_eq!(result.text.unwrap().to_string(), "File metadata changed");
}

#[test]
fn directory_jobs_bound_the_combined_output_size() {
    let dir = fixture();
    common::jj(dir.path(), &["new", "directory"]);
    let content = "x".repeat(6 * 1024 * 1024);
    for path in ["src/lib.rs", "src/main.rs", "src/nested/lib.rs"] {
        fs::write(dir.path().join(path), &content).unwrap();
    }
    common::jj(dir.path(), &["commit", "-m", "large directory"]);
    common::jj(dir.path(), &["bookmark", "create", "large", "-r", "@-"]);
    let repo = Repository::discover(dir.path(), Some(Backend::Git)).unwrap();
    let comparison = repo.snapshot("main", "large").unwrap();
    let worker = RenderWorker::new(RendererConfig {
        command: vec!["cat".into(), "{new}".into()],
        timeout_ms: 30_000,
        ..Default::default()
    });
    let error = render(&worker, &repo, &comparison, "src/").unwrap_err();
    assert!(error.contains("exceeded 16 MiB"), "{error}");
}
