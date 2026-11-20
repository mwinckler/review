use std::{
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

fn git_path(repo: &Repository, name: &str) -> Result<PathBuf> {
    let path =
        run(repo
            .command(false)
            .args(["rev-parse", "--path-format=absolute", "--git-path", name]))?;
    let path = std::str::from_utf8(&path)?;
    Ok(PathBuf::from(path.strip_suffix('\n').unwrap_or(path)))
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

pub(crate) fn working_tree(repo: &Repository, objects: &Path) -> Result<String> {
    let real_index = git_path(repo, "index")?;
    let repo_objects = git_path(repo, "objects")?;
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
    let scratch = tempfile::tempdir()?;
    let index = scratch.path().join("index");
    let snapshot_command = || -> Result<Command> {
        let mut command = command(repo, &index);
        command.env("GIT_OBJECT_DIRECTORY", objects);
        add_object_alternate(&mut command, &repo_objects)?;
        Ok(command)
    };
    match fs::copy(real_index, &index) {
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {
            run(snapshot_command()?.args(["read-tree", "HEAD"]))?;
        }
        Err(error) => return Err(error).context("copying the Git index for review"),
    }
    // Keep the snapshot objects in Review state, separate from the repository.
    let mut add = snapshot_command()?;
    add.args(["add", "--all", "--", "."]);
    // Review state can be inside the working tree; never include it in a snapshot.
    let state = objects.parent().and_then(|repository_state| {
        repository_state
            .parent()
            .filter(|state| *state != repo.root)
            .unwrap_or(repository_state)
            .strip_prefix(&repo.root)
            .ok()
    });
    if let Some(state) = state {
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
    let tree = run(snapshot_command()?.arg("write-tree"))?;
    Ok(std::str::from_utf8(&tree)?.trim_end().to_owned())
}
