use std::{path::Path, process::Command};

use anyhow::{Result, ensure};

use crate::{
    command,
    restore::{PartialRestore, Restore},
    vcs::Repository,
};

pub(crate) fn apply(repo: &Repository, restore: &Restore) -> Result<()> {
    let change = restore.change();
    let head = &restore.comparison().head_id;
    let paths: Vec<_> = restore.paths().collect();
    ensure!(
        [&change.old, &change.new]
            .iter()
            .all(|entry| entry.kind != "git-submodule"),
        "Restore Git submodules from their own working trees."
    );
    ensure_current_head(repo, head)?;
    ensure_clean_index(repo, head, &paths)?;
    restore.check_missing_paths(repo)?;

    // Use a private index so Git can enforce its index/worktree consistency
    // checks, and apply binary patches and checkout filters, without staging.
    let scratch = tempfile::tempdir()?;
    let index = scratch.path().join("index");
    run(
        temporary_index(repo, &index).args(["read-tree", head]),
        None,
    )?;
    let patch = if let Some(partial) = restore.partial(repo)? {
        set_partial_entry(repo, &index, &change.path, head.len(), partial)?;
        let patch = diff(repo, Some(&index), &["--cached", head], &paths)?;
        run(
            temporary_index(repo, &index).args(["read-tree", head]),
            None,
        )?;
        patch
    } else {
        diff(repo, None, &[head, &restore.comparison().base_id], &paths)?
    };
    ensure!(
        !patch.is_empty(),
        "The selected paths already match the restore source."
    );

    if !change.new.kind.is_empty() {
        run(
            temporary_index(repo, &index).args(["add", "--refresh", "--", &change.path]),
            None,
        )?;
        check_clean(
            temporary_index(repo, &index).args([
                "diff-files",
                "--quiet",
                "--no-ext-diff",
                "--",
                &change.path,
            ]),
            "The selected file has local changes in the working tree.",
        )?;
    }

    ensure_current_head(repo, head)?;
    ensure_clean_index(repo, head, &paths)?;
    run(
        temporary_index(repo, &index).args([
            "apply",
            "--index",
            "--no-ignore-whitespace",
            "--whitespace=nowarn",
        ]),
        Some(&patch),
    )?;
    Ok(())
}

fn ensure_current_head(repo: &Repository, head: &str) -> Result<()> {
    ensure!(
        repo.resolve("HEAD", false)? == head,
        "Git HEAD differs from the reviewed head. Review the checked-out commit before restoring."
    );
    Ok(())
}

fn ensure_clean_index(repo: &Repository, head: &str, paths: &[&str]) -> Result<()> {
    check_clean(
        git(repo)
            .args(["diff", "--cached", "--quiet", "--no-ext-diff", head, "--"])
            .args(paths),
        "The selected paths have local changes in the Git index.",
    )
}

fn set_partial_entry(
    repo: &Repository,
    index: &Path,
    path: &str,
    hash_len: usize,
    partial: PartialRestore,
) -> Result<()> {
    let entry = if let Some(content) = partial.content {
        let object = run(
            git(repo).args(["hash-object", "-w", "--stdin"]),
            Some(&content),
        )?;
        let object = std::str::from_utf8(&object)?.trim();
        let mode = if partial.executable {
            "100755"
        } else {
            "100644"
        };
        format!("{mode} {object}\t{path}\0")
    } else {
        format!("0 {}\t{path}\0", "0".repeat(hash_len))
    };
    run(
        temporary_index(repo, index).args(["update-index", "-z", "--index-info"]),
        Some(entry.as_bytes()),
    )?;
    Ok(())
}

fn diff(
    repo: &Repository,
    index: Option<&Path>,
    revisions: &[&str],
    paths: &[&str],
) -> Result<Vec<u8>> {
    let mut command = match index {
        Some(index) => temporary_index(repo, index),
        None => git(repo),
    };
    run(
        command
            .args([
                "diff",
                "--binary",
                "--full-index",
                "--no-renames",
                "--no-ext-diff",
                "--no-textconv",
                "--no-relative",
                "--color=never",
                "--unified=3",
                "--src-prefix=a/",
                "--dst-prefix=b/",
            ])
            .args(revisions)
            .arg("--")
            .args(paths),
        None,
    )
}

fn git(repo: &Repository) -> Command {
    let mut command = repo.command(false);
    command.arg("--literal-pathspecs");
    command
}

fn temporary_index(repo: &Repository, path: &Path) -> Command {
    let mut command = git(repo);
    command.env("GIT_INDEX_FILE", path).args([
        "-c",
        "core.splitIndex=false",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.sparseCheckout=false",
    ]);
    command
}

fn check_clean(command: &mut Command, dirty: &str) -> Result<()> {
    let output = command::output(command, None)?;
    if output.status.code() == Some(1) && output.stderr.is_empty() {
        anyhow::bail!("{dirty}");
    }
    ensure!(
        output.status.success(),
        "Could not check Git paths: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}

fn run(command: &mut Command, input: Option<&[u8]>) -> Result<Vec<u8>> {
    let output = command::output(command, input)?;
    ensure!(
        output.status.success(),
        "Could not restore Git paths: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(output.stdout)
}
