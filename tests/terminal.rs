#![cfg(unix)]

mod common;

use std::{
    fs::{self, File},
    io::{ErrorKind, Read, Write},
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

use nix::{
    fcntl::{FcntlArg, OFlag, fcntl},
    pty::{Winsize, openpty},
    sys::termios::{LocalFlags, tcgetattr},
};
use review::{
    store::SessionStore,
    vcs::{Backend, Repository},
};

struct Pty {
    child: Child,
    master: File,
    output: String,
}

impl Pty {
    fn spawn(mut command: Command) -> Self {
        let pair = openpty(
            Some(&Winsize {
                ws_row: 28,
                ws_col: 110,
                ws_xpixel: 0,
                ws_ypixel: 0,
            }),
            None,
        )
        .unwrap();
        fcntl(&pair.master, FcntlArg::F_SETFL(OFlag::O_NONBLOCK)).unwrap();
        let slave = File::from(pair.slave);
        let child = command
            .env("TERM", "xterm-256color")
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave))
            .spawn()
            .unwrap();
        Self {
            child,
            master: File::from(pair.master),
            output: String::new(),
        }
    }

    fn pump(&mut self) {
        let mut buffer = [0; 64 * 1024];
        loop {
            match self.master.read(&mut buffer) {
                Ok(0) => {
                    break;
                }
                Ok(count) => {
                    self.output
                        .push_str(&String::from_utf8_lossy(&buffer[..count]));
                }
                Err(error)
                    if error.kind() == ErrorKind::WouldBlock || error.raw_os_error() == Some(5) =>
                {
                    break;
                }
                Err(error) => {
                    panic!("reading PTY: {error}");
                }
            }
        }
    }

    fn wait_until(&mut self, mut ready: impl FnMut(&str) -> bool) {
        let started = Instant::now();
        loop {
            self.pump();
            if ready(&self.output) {
                return;
            }
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "application exited early: {}",
                self.output
            );
            assert!(
                started.elapsed() < Duration::from_secs(8),
                "terminal timed out: {}",
                self.output
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn send(&mut self, keys: &[u8]) {
        self.master.write_all(keys).unwrap();
    }

    fn finish(&mut self) -> ExitStatus {
        let started = Instant::now();
        loop {
            self.pump();
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(
                started.elapsed() < Duration::from_secs(8),
                "application failed to quit: {}",
                self.output
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn git_terminal_autosaves_resumes_refreshes_and_restores_terminal_settings() {
    exercise_terminal(Backend::Git);
}

#[test]
fn jj_terminal_autosaves_resumes_refreshes_and_restores_terminal_settings() {
    exercise_terminal(Backend::Jj);
}

#[test]
fn git_no_argument_launch_starts_at_master_in_the_current_repository() {
    exercise_default_launch(Backend::Git);
}

#[test]
fn jj_no_argument_launch_starts_at_master_in_the_current_repository() {
    exercise_default_launch(Backend::Jj);
}

#[test]
fn git_positional_comparisons_remain_direct_on_resume_and_refresh() {
    exercise_positional_comparison(Backend::Git);
}

#[test]
fn jj_positional_comparisons_remain_direct_on_resume_and_refresh() {
    exercise_positional_comparison(Backend::Jj);
}

fn exercise_positional_comparison(backend: Backend) {
    let fixture = common::fixture();
    common::jj(fixture.path(), &["new", "main"]);
    fs::write(fixture.path().join("base-only.txt"), "base branch\n").unwrap();
    common::jj(fixture.path(), &["commit", "-m", "advance base"]);
    common::jj(fixture.path(), &["bookmark", "set", "main", "-r", "@-"]);
    let state = tempfile::tempdir().unwrap();
    let config = state.path().join("config.toml");
    fs::write(&config, "").unwrap();
    let repo = Repository::discover(fixture.path(), Some(backend)).unwrap();
    let store = SessionStore::new(state.path(), &repo).unwrap();
    let base_id = repo.snapshot("main", "main").unwrap().head_id;
    let command = || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_review"));
        command
            .env("JJ_CONFIG", "/dev/null")
            .current_dir(fixture.path())
            .args([
                "--backend",
                match backend {
                    Backend::Git => "git",
                    Backend::Jj => "jj",
                },
                "--state-dir",
                state.path().to_str().unwrap(),
                "--config",
                config.to_str().unwrap(),
            ]);
        command
    };
    let mut start = command();
    start.arg("main..feature");
    let mut terminal = Pty::spawn(start);
    terminal.wait_until(|text| text.contains("Changed files"));
    terminal.send(b" q");
    assert!(terminal.finish().success(), "{}", terminal.output);
    let saved = store.list().unwrap().remove(0);
    assert_eq!(saved.review.comparison.base_id, base_id);
    assert_eq!(saved.review.comparison.base_ref, "main");
    assert_eq!(saved.review.comparison.head_ref, "feature");
    assert!(
        saved
            .review
            .changes
            .iter()
            .any(|change| { change.path == "base-only.txt" && change.status == "D" })
    );
    assert_eq!(saved.review.viewed.len(), 1);

    common::jj(fixture.path(), &["new", "main"]);
    fs::write(fixture.path().join("new-base-only.txt"), "advanced again\n").unwrap();
    common::jj(fixture.path(), &["commit", "-m", "advance base again"]);
    common::jj(fixture.path(), &["bookmark", "set", "main", "-r", "@-"]);
    let advanced_id = repo.snapshot("main", "main").unwrap().head_id;
    let mut resume = command();
    resume.args(["--resume", &saved.id]);
    let mut terminal = Pty::spawn(resume);
    terminal.wait_until(|text| text.contains(&saved.id));
    let pinned = store.list().unwrap().remove(0);
    assert_eq!(pinned.review.comparison.base_id, base_id);
    assert_eq!(
        pinned.review.comparison.head_id,
        saved.review.comparison.head_id
    );
    assert_eq!(pinned.review.viewed, saved.review.viewed);
    terminal.send(b"r");
    terminal.wait_until(|_| {
        let updated = store.list().unwrap().remove(0);
        updated.review.comparison.base_id == advanced_id
            && updated
                .review
                .changes
                .iter()
                .any(|change| change.path == "new-base-only.txt" && change.status == "D")
    });
    terminal.send(b"q");
    assert!(terminal.finish().success(), "{}", terminal.output);
    assert_eq!(store.list().unwrap()[0].review.viewed, saved.review.viewed);
}

#[test]
fn git_directory_previews_include_hidden_descendants_and_resume_their_scroll() {
    exercise_directory_preview(Backend::Git);
}

#[test]
fn jj_directory_previews_include_hidden_descendants_and_resume_their_scroll() {
    exercise_directory_preview(Backend::Jj);
}

fn exercise_directory_preview(backend: Backend) {
    let fixture = common::fixture();
    common::jj(fixture.path(), &["new", "feature"]);
    fs::create_dir_all(fixture.path().join("src/nested")).unwrap();
    fs::write(
        fixture.path().join("src/lib.rs"),
        "pub const DIRECTORY_TOP: u8 = 1;\n",
    )
    .unwrap();
    let nested = format!("{}// DIRECTORY_END\n", "// DIRECTORY_NESTED\n".repeat(70));
    fs::write(fixture.path().join("src/nested/lib.rs"), nested).unwrap();
    common::jj(fixture.path(), &["commit", "-m", "directory changes"]);
    common::jj(
        fixture.path(),
        &["bookmark", "create", "directory", "-r", "@-"],
    );
    let state = tempfile::tempdir().unwrap();
    let config = state.path().join("config.toml");
    fs::write(&config, "").unwrap();
    let repo = Repository::discover(fixture.path(), Some(backend)).unwrap();
    let store = SessionStore::new(state.path(), &repo).unwrap();
    let command = || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_review"));
        command
            .env("JJ_CONFIG", "/dev/null")
            .current_dir(fixture.path())
            .args([
                "--backend",
                match backend {
                    Backend::Git => "git",
                    Backend::Jj => "jj",
                },
                "--state-dir",
                state.path().to_str().unwrap(),
                "--config",
                config.to_str().unwrap(),
            ]);
        command
    };
    let mut first = command();
    first.args(["--base", "main", "--head", "directory"]);
    let mut terminal = Pty::spawn(first);
    terminal.wait_until(|text| text.contains("Changed files"));
    terminal.send(b"/main.rs\rh");
    terminal.wait_until(|text| text.contains("src/lib.rs") && text.contains("src/nested/lib.rs"));
    terminal.send(b"\x0cG");
    terminal.wait_until(|_| {
        store.list().unwrap()[0]
            .review
            .view
            .positions
            .get("src/")
            .is_some_and(|position| position.vertical > 0)
    });
    terminal.send(b"q");
    assert!(terminal.finish().success(), "{}", terminal.output);
    let saved = store.list().unwrap().remove(0);
    assert_eq!(saved.review.view.active, "src/");
    assert_eq!(saved.review.view.selected, "src/");
    assert_eq!(saved.review.view.filter, "main.rs");
    fs::write(fixture.path().join("src/lib.rs"), "uncommitted\n").unwrap();
    fs::write(fixture.path().join("src/nested/lib.rs"), "uncommitted\n").unwrap();
    let mut resume = command();
    resume.args(["--resume", &saved.id]);
    let mut terminal = Pty::spawn(resume);
    terminal.wait_until(|text| text.contains("DIRECTORY_END"));
    terminal.send(b"q");
    assert!(terminal.finish().success(), "{}", terminal.output);
    let resumed = store.list().unwrap().remove(0);
    assert_eq!(
        resumed.review.comparison.head_id,
        saved.review.comparison.head_id
    );
    assert_eq!(
        resumed.review.view.positions["src/"].vertical,
        saved.review.view.positions["src/"].vertical
    );
}

#[test]
fn terminal_flash_pane_shortcuts_and_tree_resizing_work_together() {
    let fixture = common::fixture();
    let state = tempfile::tempdir().unwrap();
    let config = state.path().join("config.toml");
    fs::write(&config, "[keybindings]\nv = 'clear_filters'\n").unwrap();
    let repo = Repository::discover(fixture.path(), Some(Backend::Git)).unwrap();
    let store = SessionStore::new(state.path(), &repo).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_review"));
    command.current_dir(fixture.path()).args([
        "--backend",
        "git",
        "--base",
        "main",
        "--head",
        "feature",
        "--state-dir",
        state.path().to_str().unwrap(),
        "--config",
        config.to_str().unwrap(),
    ]);
    let mut terminal = Pty::spawn(command);
    terminal.wait_until(|text| text.contains("Changed files"));
    terminal.send(b"\x0c");
    terminal.wait_until(|_| store.list().unwrap()[0].review.view.diff_focused);
    terminal.send(b"zs");
    terminal.wait_until(|_| {
        let saved = store.list().unwrap().remove(0);
        !saved.review.view.diff_focused && !saved.review.view.fullscreen
    });
    terminal.output.clear();
    terminal.send(b"v");
    terminal.wait_until(|text| text.contains('v'));
    assert!(store.list().unwrap()[0].review.viewed.is_empty());
    terminal.send(b"\x7fmain\r");
    terminal.wait_until(|_| store.list().unwrap()[0].review.view.selected == "src/main.rs");
    terminal.send(b"\x0c");
    terminal.wait_until(|_| store.list().unwrap()[0].review.view.diff_focused);
    terminal.send(b"\x1b[1;5C");
    terminal.wait_until(|_| store.list().unwrap()[0].review.view.tree_width == Some(38));
    terminal.send(b"\x1b[1;5D\x08");
    terminal.wait_until(|_| {
        let saved = store.list().unwrap().remove(0);
        saved.review.view.tree_width == Some(36) && !saved.review.view.diff_focused
    });
    terminal.send(b"q");
    assert!(terminal.finish().success(), "{}", terminal.output);
}

fn exercise_default_launch(backend: Backend) {
    let fixture = common::fixture();
    let state = tempfile::tempdir().unwrap();
    common::jj(
        fixture.path(),
        &["bookmark", "create", "master", "-r", "main"],
    );
    if backend == Backend::Git {
        fs::rename(fixture.path().join(".jj"), state.path().join("jj")).unwrap();
    }
    let repo = Repository::discover(fixture.path(), None).unwrap();
    assert_eq!(repo.backend, backend);
    let store = SessionStore::new(&state.path().join("review"), &repo).unwrap();
    let head = match backend {
        Backend::Git => "HEAD",
        Backend::Jj => "@",
    };
    let expected = repo.snapshot("master", head).unwrap();
    let command = || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_review"));
        command
            .current_dir(fixture.path().join("src"))
            .env("JJ_CONFIG", "/dev/null")
            .env("XDG_STATE_HOME", state.path())
            .env("XDG_CONFIG_HOME", state.path());
        command
    };

    let mut terminal = Pty::spawn(command());
    terminal.wait_until(|text| text.contains("Changed files"));
    terminal.send(b" q");
    assert!(terminal.finish().success(), "{}", terminal.output);
    let first = store.list().unwrap().remove(0);
    assert_eq!(first.review.comparison.base_ref, "master");
    assert_eq!(first.review.comparison.head_ref, head);
    assert_eq!(first.review.comparison.base_id, expected.base_id);
    assert_eq!(first.review.comparison.head_id, expected.head_id);
    assert_eq!(first.repository, fixture.path().canonicalize().unwrap());
    assert_eq!(first.review.viewed.len(), 1);
    assert_eq!(first.review.view.selected, first.review.changes[1].path);
    assert_eq!(first.review.view.active, first.review.changes[1].path);

    let mut terminal = Pty::spawn(command());
    terminal.wait_until(|text| text.contains("Changed files"));
    let sessions = store.list().unwrap();
    assert_eq!(sessions.len(), 2);
    assert_ne!(sessions[0].id, first.id);
    assert_eq!(sessions[0].review.comparison.base_ref, "master");
    assert!(sessions[0].review.viewed.is_empty());
    terminal.send(b" q");
    assert!(terminal.finish().success(), "{}", terminal.output);
    let latest = store.list().unwrap().remove(0);

    let mut resume = command();
    resume.arg("--resume");
    let mut terminal = Pty::spawn(resume);
    terminal.wait_until(|text| text.contains(&latest.id));
    terminal.send(b"gg ");
    terminal.wait_until(|_| {
        let sessions = store.list().unwrap();
        sessions.len() == 2 && sessions[0].id == latest.id && sessions[0].review.viewed.is_empty()
    });
    terminal.send(b"q");
    assert!(terminal.finish().success(), "{}", terminal.output);
}

fn exercise_terminal(backend: Backend) {
    let fixture = common::fixture();
    let state = tempfile::tempdir().unwrap();
    let config = state.path().join("config.toml");
    fs::write(&config, "[keybindings]\nv = 'toggle_viewed'\n").unwrap();
    let repo = Repository::discover(fixture.path(), Some(backend)).unwrap();
    let store = SessionStore::new(state.path(), &repo).unwrap();
    let command = || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_review"));
        command.env("JJ_CONFIG", "/dev/null");
        command.args([
            "-C",
            fixture.path().to_str().unwrap(),
            "--backend",
            match backend {
                Backend::Git => "git",
                Backend::Jj => "jj",
            },
            "--state-dir",
            state.path().to_str().unwrap(),
            "--config",
            config.to_str().unwrap(),
        ]);
        command
    };
    let mut first = command();
    first.args(["--base", "main", "--head", "feature"]);
    let mut terminal = Pty::spawn(first);
    terminal.wait_until(|text| text.contains("Changed files"));
    terminal.send(b"v");
    terminal.wait_until(|_| {
        store
            .list()
            .unwrap()
            .first()
            .is_some_and(|saved| saved.review.viewed.len() == 1)
    });
    terminal.send(b"/main.rs\r\t");
    terminal.wait_until(|text| text.contains("println"));
    terminal.send(b"q");
    assert!(terminal.finish().success(), "{}", terminal.output);
    assert!(
        tcgetattr(&terminal.master)
            .unwrap()
            .local_flags
            .contains(LocalFlags::ICANON)
    );
    assert!(terminal.output.contains("\u{1b}[?1049l"));
    let saved = store.list().unwrap().remove(0);
    assert_eq!(saved.review.view.filter, "main.rs");
    assert_eq!(saved.review.view.selected, "src/main.rs");
    assert_eq!(saved.review.viewed.len(), 1);
    for (change_id, commit) in [
        (
            &saved.review.comparison.base_change_id,
            &saved.review.comparison.base_id,
        ),
        (
            &saved.review.comparison.head_change_id,
            &saved.review.comparison.head_id,
        ),
    ] {
        if backend == Backend::Jj {
            let expected = common::jj(
                fixture.path(),
                &["log", "--no-graph", "-r", commit, "-T", "change_id"],
            );
            assert_eq!(change_id.as_deref(), Some(expected.as_str()));
            assert!(terminal.output.contains(&format!("({}", &expected[..8])));
            assert!(terminal.output.contains(&format!("{})", &commit[..8])));
        } else {
            assert!(change_id.is_none());
        }
    }
    let counts = saved
        .review
        .comparison
        .line_counts
        .expect("line counts saved with the snapshots");
    let mut legacy = serde_json::to_value(&saved).unwrap();
    for field in ["line_counts", "base_change_id", "head_change_id"] {
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
    fs::write(fixture.path().join("src/main.rs"), "working tree changed\n").unwrap();
    common::jj(fixture.path(), &["bookmark", "set", "feature", "-r", "@"]);
    let mut resume = command();
    resume.args(["--resume", &saved.id]);
    let mut terminal = Pty::spawn(resume);
    terminal.wait_until(|text| text.contains("println"));
    terminal.send(b"v");
    terminal.wait_until(|_| store.list().unwrap()[0].review.viewed.len() == 2);
    let pinned = store.list().unwrap().remove(0);
    assert_eq!(pinned.review.comparison.line_counts, Some(counts));
    assert_eq!(
        pinned.review.comparison.base_change_id,
        saved.review.comparison.base_change_id
    );
    assert_eq!(
        pinned.review.comparison.head_change_id,
        saved.review.comparison.head_change_id
    );
    assert_eq!(
        pinned.review.comparison.head_id,
        saved.review.comparison.head_id
    );
    common::jj(fixture.path(), &["new", "feature"]);
    fs::write(
        fixture.path().join("src/main.rs"),
        "fn main() {\n    println!(\"updated\");\n}\n",
    )
    .unwrap();
    common::jj(fixture.path(), &["commit", "-m", "update feature"]);
    common::jj(fixture.path(), &["bookmark", "set", "feature", "-r", "@-"]);
    terminal.send(b"r");
    terminal.wait_until(|_| {
        let latest = store.list().unwrap().remove(0);
        latest.review.comparison.head_id != saved.review.comparison.head_id
            && latest.review.viewed.len() == 1
            && latest.review.comparison.line_counts.is_some_and(|updated| {
                updated.added == counts.added + 2 && updated.removed == counts.removed
            })
    });
    terminal.send(b"\x03");
    assert!(terminal.finish().success());
    let resumed = store.list().unwrap().remove(0);
    for (change_id, commit) in [
        (
            &resumed.review.comparison.base_change_id,
            &resumed.review.comparison.base_id,
        ),
        (
            &resumed.review.comparison.head_change_id,
            &resumed.review.comparison.head_id,
        ),
    ] {
        if backend == Backend::Jj {
            let expected = common::jj(
                fixture.path(),
                &["log", "--no-graph", "-r", commit, "-T", "change_id"],
            );
            assert_eq!(change_id.as_deref(), Some(expected.as_str()));
        } else {
            assert!(change_id.is_none());
        }
    }
    assert_ne!(
        resumed.review.comparison.head_id,
        saved.review.comparison.head_id
    );
    assert_eq!(resumed.review.viewed.len(), 1);
    assert!(
        tcgetattr(&terminal.master)
            .unwrap()
            .local_flags
            .contains(LocalFlags::ICANON)
    );
}
