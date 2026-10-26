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
    terminal.send(b" ");
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
    fs::write(fixture.path().join("src/main.rs"), "working tree changed\n").unwrap();
    let mut resume = command();
    resume.args(["--resume", &saved.id]);
    let mut terminal = Pty::spawn(resume);
    terminal.wait_until(|text| text.contains("println"));
    terminal.send(b"v");
    terminal.wait_until(|_| store.list().unwrap()[0].review.viewed.len() == 2);
    let pinned = store.list().unwrap().remove(0);
    assert_eq!(
        pinned.review.comparison.head_id,
        saved.review.comparison.head_id
    );
    common::jj(fixture.path(), &["new", "feature"]);
    fs::write(
        fixture.path().join("src/main.rs"),
        "fn main() { println!(\"updated\"); }\n",
    )
    .unwrap();
    common::jj(fixture.path(), &["commit", "-m", "update feature"]);
    common::jj(fixture.path(), &["bookmark", "set", "feature", "-r", "@-"]);
    terminal.send(b"r");
    terminal.wait_until(|_| {
        let latest = store.list().unwrap().remove(0);
        latest.review.comparison.head_id != saved.review.comparison.head_id
            && latest.review.viewed.len() == 1
    });
    terminal.send(b"\x03");
    assert!(terminal.finish().success());
    let resumed = store.list().unwrap().remove(0);
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
