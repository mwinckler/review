use std::{
    collections::HashSet,
    env,
    ffi::OsString,
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, ensure};

use crate::vcs::Repository;

pub(crate) fn command(repo: &Repository, index: &Path) -> Command {
    let mut command = repo.command(false);
    command.env("GIT_INDEX_FILE", index).args([
        "-c",
        "core.splitIndex=false",
        "-c",
        "core.fsmonitor=false",
    ]);
    command
}

fn run(command: &mut Command) -> Result<Vec<u8>> {
    let output = crate::command::output(command, None)?;
    ensure!(
        output.status.success(),
        "Git working-tree snapshot: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(output.stdout)
}

pub(crate) fn git_path(repo: &Repository, args: &[&str]) -> Result<PathBuf> {
    let output = run(repo
        .command(false)
        .args(["rev-parse", "--path-format=absolute"])
        .args(args))?;
    let path = std::str::from_utf8(&output)?;
    Ok(PathBuf::from(path.strip_suffix('\n').unwrap_or(path)))
}

pub(crate) fn global_ignore(repo: &Repository) -> Option<PathBuf> {
    let mut command = repo.command(false);
    command.args(["config", "--path", "--null", "--get", "core.excludesFile"]);
    if let Ok(bytes) = run(&mut command) {
        return Some(repo.root.join(worktree_path(bytes.strip_suffix(&[0])?)));
    }
    let config_home = env::var_os("XDG_CONFIG_HOME")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| directories::BaseDirs::new().map(|dirs| dirs.home_dir().join(".config")))?;
    Some(config_home.join("git").join("ignore"))
}

pub(crate) fn add_object_alternate(command: &mut Command, path: &Path) -> Result<()> {
    // Git accepts C-quoted paths in the alternate list, including path separators.
    let path = path
        .to_str()
        .context("Git snapshot object directory must be UTF-8")?;
    let mut quoted = String::from("\"");
    for ch in path.chars() {
        match ch {
            '"' | '\\' => {
                quoted.push('\\');
                quoted.push(ch);
            }
            '\n' => quoted.push_str("\\n"),
            _ => quoted.push(ch),
        }
    }
    quoted.push('"');
    let mut alternates = OsString::from(quoted);
    if let Some(previous) =
        env::var_os("GIT_ALTERNATE_OBJECT_DIRECTORIES").filter(|value| !value.is_empty())
    {
        alternates.push(if cfg!(windows) { ";" } else { ":" });
        alternates.push(previous);
    }
    command.env("GIT_ALTERNATE_OBJECT_DIRECTORIES", alternates);
    Ok(())
}

pub(crate) fn excluded_state<'a>(repo: &Repository, objects: &'a Path) -> Option<&'a Path> {
    let repository_state = objects.parent()?;
    repository_state
        .parent()
        .filter(|state| *state != repo.root)
        .unwrap_or(repository_state)
        .strip_prefix(&repo.root)
        .ok()
}

/// Owns a scratch index for the lifetime of a working-tree comparison.
#[derive(Default)]
pub(crate) struct SnapshotIndex(Option<CachedIndex>);

impl SnapshotIndex {
    pub(crate) fn capture(
        &mut self,
        repo: &Repository,
        objects: &Path,
        head: &str,
    ) -> Result<String> {
        let mut index = match self.0.take() {
            Some(index) if index.can_reuse(repo, objects, head)? => index,
            _ => CachedIndex::new(repo, objects, head)?,
        };
        let tree = index.capture(repo)?;
        self.0 = Some(index);
        Ok(tree)
    }
}

struct CachedIndex {
    scratch: tempfile::TempDir,
    objects: PathBuf,
    repo_objects: PathBuf,
    head: String,
    real_index: PathBuf,
    real_metadata: Option<fs::Metadata>,
    tracked: HashSet<Vec<u8>>,
    missing_tracked: Vec<PathBuf>,
    last_tree: String,
}

fn index_metadata(path: &Path) -> Result<Option<fs::Metadata>> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context("reading Git index metadata"),
    }
}

fn same_index(before: Option<&fs::Metadata>, after: Option<&fs::Metadata>) -> bool {
    match (before, after) {
        (Some(before), Some(after)) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if (
                    before.dev(),
                    before.ino(),
                    before.ctime(),
                    before.ctime_nsec(),
                ) != (after.dev(), after.ino(), after.ctime(), after.ctime_nsec())
                {
                    return false;
                }
            }
            before.len() == after.len() && before.modified().ok() == after.modified().ok()
        }
        (None, None) => true,
        _ => false,
    }
}

fn paths(list: &[u8]) -> impl Iterator<Item = &[u8]> {
    list.split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
}

fn worktree_path(path: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
        PathBuf::from(OsStr::from_bytes(path))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(path).as_ref())
    }
}

fn create_object_directory(objects: &Path) -> Result<()> {
    let mut directory = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        directory.mode(0o700);
    }
    if let Err(error) = directory.create(objects)
        && error.kind() != ErrorKind::AlreadyExists
    {
        return Err(error).context("creating the working-tree snapshot object directory");
    }
    Ok(())
}

impl CachedIndex {
    fn new(repo: &Repository, objects: &Path, head: &str) -> Result<Self> {
        let real_index = git_path(repo, &["--git-path", "index"])?;
        let repo_objects = git_path(repo, &["--git-path", "objects"])?;
        create_object_directory(objects)?;
        let mut scratch = tempfile::Builder::new();
        scratch.prefix("review-index-");
        let scratch = if env::temp_dir()
            .canonicalize()
            .is_ok_and(|temp| temp.starts_with(&repo.root))
        {
            scratch.tempdir_in(
                objects
                    .parent()
                    .context("opening the Review state directory")?,
            )?
        } else {
            scratch.tempdir()?
        };
        let mut cached = Self {
            scratch,
            objects: objects.to_owned(),
            repo_objects,
            head: head.to_owned(),
            real_metadata: index_metadata(&real_index)?,
            real_index,
            tracked: HashSet::new(),
            missing_tracked: Vec::new(),
            last_tree: String::new(),
        };
        match fs::copy(&cached.real_index, cached.scratch.path().join("index")) {
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {
                run(cached.command(repo)?.args(["read-tree", "HEAD"]))?;
            }
            Err(error) => return Err(error).context("copying the Git index for review"),
        }
        cached.tracked = paths(&cached.tracked_paths(repo)?)
            .map(<[u8]>::to_vec)
            .collect();
        Ok(cached)
    }

    fn can_reuse(&self, repo: &Repository, objects: &Path, head: &str) -> Result<bool> {
        Ok(self.objects == objects
            && self.head == head
            && same_index(
                self.real_metadata.as_ref(),
                index_metadata(&self.real_index)?.as_ref(),
            )
            // An ignored path that was tracked in the real index stays tracked
            // if it reappears after a snapshot recorded its deletion.
            && self.missing_tracked.iter().all(|path| {
                fs::symlink_metadata(repo.root.join(path)).is_err_and(|error| {
                    matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory)
                })
            }))
    }

    fn command(&self, repo: &Repository) -> Result<Command> {
        let mut command = command(repo, &self.scratch.path().join("index"));
        command.env("GIT_OBJECT_DIRECTORY", &self.objects);
        add_object_alternate(&mut command, &self.repo_objects)?;
        Ok(command)
    }

    fn tracked_paths(&self, repo: &Repository) -> Result<Vec<u8>> {
        run(self.command(repo)?.args(["ls-files", "--cached", "-z"]))
    }

    fn capture(&mut self, repo: &Repository) -> Result<String> {
        let mut add = self.command(repo)?;
        add.args(["add", "--all", "--", "."]);
        if let Some(state) = excluded_state(repo, &self.objects) {
            let state = state
                .to_str()
                .context("review state directory must be UTF-8")?;
            ensure!(
                !state.is_empty(),
                "Use a dedicated subdirectory for review state."
            );
            add.arg(format!(":(exclude,top,literal){state}"));
        }
        run(&mut add)?;
        let output = run(self.command(repo)?.arg("write-tree"))?;
        let tree = std::str::from_utf8(&output)?.trim_end();
        if self.last_tree.is_empty() {
            let current = self.tracked_paths(repo)?;
            let current: HashSet<_> = paths(&current).collect();
            self.missing_tracked = self
                .tracked
                .iter()
                .map(Vec::as_slice)
                .filter(|path| !current.contains(*path))
                .map(worktree_path)
                .collect();
        } else if tree != self.last_tree {
            // Git can skip identical subtrees, unlike listing the full index.
            let removed = run(self.command(repo)?.args([
                "diff-tree",
                "-r",
                "--no-commit-id",
                "--no-renames",
                "--ignore-submodules=none",
                "--name-only",
                "-z",
                "--diff-filter=D",
                &self.last_tree,
                tree,
            ]))?;
            self.missing_tracked.extend(
                paths(&removed)
                    .filter(|path| self.tracked.contains(*path))
                    .map(worktree_path),
            );
        }
        self.last_tree = tree.to_owned();
        Ok(self.last_tree.clone())
    }
}
