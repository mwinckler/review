use std::{
    env,
    io::{Read, Seek, SeekFrom},
    path::{self, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
};

use anyhow::{Context, Result, anyhow, bail, ensure};

use crate::{
    command,
    vcs::{Backend, Repository},
};

pub struct Conductor {
    program: PathBuf,
}

pub struct ConductorStartup(Receiver<Result<()>>);

impl ConductorStartup {
    pub fn take_finished(&self) -> Option<Result<()>> {
        match self.0.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(anyhow!(
                "Conductor startup worker stopped unexpectedly."
            ))),
        }
    }
}

impl Conductor {
    pub fn find() -> Option<Self> {
        let search_path = env::var_os("PATH")?;
        let executable = format!("conductor{}", env::consts::EXE_SUFFIX);
        let (program, status) = env::split_paths(&search_path).find_map(|directory| {
            // Resolve relative PATH entries before changing to the checkout's root.
            let program = path::absolute(directory.join(&executable)).ok()?;
            if !program.is_file() {
                return None;
            }
            let status = Command::new(&program)
                .arg("--version")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .ok()?;
            Some((program, status))
        })?;
        status.success().then_some(Self { program })
    }

    pub(crate) fn start(
        self,
        repo: &Repository,
        branch: &str,
        remote: &str,
        expected_head: String,
    ) -> Result<ConductorStartup> {
        let repo = repo.clone();
        let branch = branch.to_owned();
        let remote = remote.to_owned();
        let (sender, result) = mpsc::channel();
        thread::Builder::new()
            .name("conductor-startup".into())
            .spawn(move || {
                let _ = sender.send(self.checkout(&repo, &branch, &remote, &expected_head));
            })
            .context("Could not launch the Conductor startup worker.")?;
        Ok(ConductorStartup(result))
    }

    fn checkout(
        &self,
        repo: &Repository,
        branch: &str,
        remote: &str,
        expected_head: &str,
    ) -> Result<()> {
        let mut stderr = tempfile::tempfile().context("Could not capture Conductor output.")?;
        let status = command::launch(
            Command::new(&self.program)
                .current_dir(&repo.root)
                .args(["checkout", branch, "--remote", remote])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(stderr.try_clone()?),
            None,
            Command::status,
        )?;
        if !status.success() {
            const LIMIT: u64 = 4096;
            stderr.seek(SeekFrom::Start(
                stderr.metadata()?.len().saturating_sub(LIMIT),
            ))?;
            let mut bytes = Vec::new();
            stderr.take(LIMIT).read_to_end(&mut bytes)?;
            let text = String::from_utf8_lossy(&bytes);
            if let Some(detail) = text
                .lines()
                .rev()
                .map(str::trim)
                .find(|line| !line.is_empty())
            {
                bail!("Conductor startup failed: {detail}");
            }
            bail!("Conductor could not check out and start the workspace ({status}).");
        }
        let revision = match repo.backend {
            Backend::Git => "HEAD",
            Backend::Jj => "@-",
        };
        let head = repo
            .resolve(revision, false)
            .context("Could not verify the workspace revision after Conductor checkout.")?;
        ensure!(
            head == expected_head,
            "Conductor started a different revision than GitLab reported. Stop the workspace and retry."
        );
        Ok(())
    }
}
