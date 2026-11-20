use std::{
    collections::HashMap,
    env,
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use semver::Version;
use serde::{Deserialize, Serialize};

use crate::command;

const UPSTREAM: &str = env!("CARGO_PKG_REPOSITORY");
const CURRENT: &str = env!("CARGO_PKG_VERSION");
const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const TIMEOUT: Duration = Duration::from_secs(5);

pub fn run(check: bool) -> Result<()> {
    let Some(latest) = latest_release(&|| false)? else {
        println!("No stable releases are published. You are running Review {CURRENT}.");
        return Ok(());
    };
    if !is_newer(&latest) {
        println!("Review {CURRENT} is up to date. The latest stable release is {latest}.");
        return Ok(());
    }
    if check {
        println!("Review {CURRENT} is installed. Run `review update` to install {latest}.");
        return Ok(());
    }

    let root = cargo_root().with_context(|| {
        format!(
            "Could not verify a Cargo install for this executable. Use your package manager \
             to upgrade a packaged copy, or install Review separately with \
             `cargo install --locked --git {UPSTREAM} --tag v{latest} review`."
        )
    })?;
    let directory = tempfile::tempdir().context("creating a directory for the Cargo update")?;
    eprintln!("Updating Review from {CURRENT} to {latest}…");
    let status = command::launch(
        Command::new("cargo")
            .current_dir(directory.path())
            .args(["install", "--locked", "--root"])
            .arg(root)
            .args(["--git", UPSTREAM, "--tag", &format!("v{latest}"), "review"]),
        None,
        Command::status,
    )?;
    ensure!(
        status.success(),
        "Cargo could not update Review ({status})."
    );
    println!("Updated Review to {latest}.");
    Ok(())
}

fn stable_version(value: &str) -> Option<Version> {
    let version = Version::parse(value).ok()?;
    (version.pre.is_empty() && version.build.is_empty()).then_some(version)
}

fn is_newer(version: &Version) -> bool {
    let current = Version::parse(CURRENT).expect("Cargo supplies a semantic version");
    version.cmp_precedence(&current).is_gt()
}

fn latest_release(cancelled: &impl Fn() -> bool) -> Result<Option<Version>> {
    let directory = tempfile::tempdir().context("creating a directory for the release check")?;
    let output = command::output_with_timeout(
        Command::new("git")
            .current_dir(directory.path())
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_COMMON_DIR")
            .env_remove("GIT_ASKPASS")
            .env_remove("SSH_ASKPASS")
            .env("GIT_TERMINAL_PROMPT", "0")
            .args([
                "-c",
                "core.askPass=",
                "-c",
                "credential.helper=",
                "-c",
                "http.lowSpeedLimit=1",
                "-c",
                "http.lowSpeedTime=5",
                "ls-remote",
                "--tags",
                "--refs",
                UPSTREAM,
                "refs/tags/v*",
            ]),
        None,
        TIMEOUT,
        cancelled,
    )
    .context("Checking Review releases")?;
    ensure!(
        output.status.success(),
        "Could not check Review releases ({}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let (_, reference) = line.split_once('\t')?;
            stable_version(reference.strip_prefix("refs/tags/v")?)
        })
        .max())
}

#[derive(Deserialize)]
struct CargoMetadata {
    installs: HashMap<String, CargoInstall>,
}

#[derive(Deserialize)]
struct CargoInstall {
    bins: Vec<String>,
}

fn cargo_root() -> Result<PathBuf> {
    let executable = env::current_exe()?.canonicalize()?;
    let name = format!("review{}", env::consts::EXE_SUFFIX);
    let bin = executable.parent().context("Missing bin directory.")?;
    ensure!(
        executable.ends_with(Path::new("bin").join(&name)),
        "The running executable is not named bin/{name}."
    );
    let root = bin.parent().context("Missing Cargo install root.")?;
    let metadata: CargoMetadata = serde_json::from_reader(File::open(root.join(".crates2.json"))?)?;
    let package = format!("review {CURRENT} (");
    ensure!(
        metadata
            .installs
            .iter()
            .any(|(id, install)| { id.starts_with(&package) && install.bins.contains(&name) }),
        "Cargo has no matching install record for Review {CURRENT}."
    );
    Ok(root.to_path_buf())
}

pub struct UpdateCheck {
    results: Receiver<Version>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl UpdateCheck {
    pub fn start() -> Option<Self> {
        let directory = directories::BaseDirs::new()?.cache_dir().join("review");
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let (sender, results) = mpsc::channel();
        let thread = thread::Builder::new()
            .spawn(move || {
                if let Ok(Some(version)) =
                    cached_release(&directory, &|| stopped.load(Ordering::Relaxed))
                    && is_newer(&version)
                {
                    let _ = sender.send(version);
                }
            })
            .ok()?;
        Some(Self {
            results,
            stop,
            thread: Some(thread),
        })
    }

    pub fn take_available(&self) -> Option<Version> {
        self.results.try_recv().ok()
    }
}

impl Drop for UpdateCheck {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Cache {
    latest: Option<String>,
}

fn read_cache(path: &Path) -> Option<Cache> {
    let file = File::open(path).ok()?;
    let age = file.metadata().ok()?.modified().ok()?.elapsed().ok()?;
    if age >= MAX_AGE {
        return None;
    }
    serde_json::from_reader(file).ok()
}

fn write_cache(path: &Path, directory: &Path, version: Option<&Version>) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    serde_json::to_writer(
        file.as_file_mut(),
        &Cache {
            latest: version.map(ToString::to_string),
        },
    )?;
    file.persist(path)?;
    Ok(())
}

fn cached_release(directory: &Path, cancelled: &impl Fn() -> bool) -> Result<Option<Version>> {
    let cache = directory.join("update.json");
    if let Some(found) = read_cache(&cache) {
        return Ok(found.latest.as_deref().and_then(stable_version));
    }
    fs::create_dir_all(directory)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(directory.join("update.lock"))?;
    lock.try_lock_exclusive()?;
    if let Some(found) = read_cache(&cache) {
        return Ok(found.latest.as_deref().and_then(stable_version));
    }
    if cancelled() {
        return Ok(None);
    }
    // Count failed and interrupted checks so opening another review stays offline for a day.
    write_cache(&cache, directory, None)?;
    let found = latest_release(cancelled)?;
    let _ = write_cache(&cache, directory, found.as_ref());
    Ok(found)
}
