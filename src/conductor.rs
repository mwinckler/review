use std::{
    env,
    path::{self, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, ensure};

use crate::vcs::{Backend, Repository};

pub struct Conductor {
    program: PathBuf,
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

    pub(crate) fn checkout(
        &self,
        repo: &Repository,
        branch: &str,
        remote: &str,
        expected_head: &str,
    ) -> Result<()> {
        let status = Command::new(&self.program)
            .current_dir(&repo.root)
            .args(["checkout", branch, "--remote", remote])
            .status()
            .context("Could not run Conductor checkout.")?;
        ensure!(
            status.success(),
            "Conductor could not check out and start the workspace ({status})."
        );
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
