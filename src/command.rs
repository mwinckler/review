use std::{
    io::{Read, Seek, Write},
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};

/// Capture external API/transport commands without pipe deadlocks or terminal prompts.
pub(crate) fn output(command: &mut Command, input: Option<&[u8]>) -> Result<Output> {
    const LIMIT: u64 = 32 * 1024 * 1024;
    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;
    if let Some(input) = input {
        let mut file = tempfile::tempfile()?;
        file.write_all(input)?;
        file.rewind()?;
        command.stdin(file);
    } else {
        command.stdin(Stdio::null());
    }
    let mut child = command
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?)
        .spawn()
        .with_context(|| {
            format!(
                "running {}; is it installed?",
                command.get_program().to_string_lossy()
            )
        })?;
    let started = Instant::now();
    let status = (|| -> Result<_> {
        loop {
            ensure!(
                started.elapsed() < Duration::from_secs(90),
                "command timed out after 90 seconds"
            );
            ensure!(
                stdout.metadata()?.len() + stderr.metadata()?.len() <= LIMIT,
                "command output exceeded 32 MiB"
            );
            if let Some(status) = child.try_wait()? {
                return Ok(status);
            }
            thread::sleep(Duration::from_millis(20));
        }
    })();
    if status.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let status = status?;
    stdout.rewind()?;
    stderr.rewind()?;
    let mut out = Vec::new();
    let mut err = Vec::new();
    stdout.take(LIMIT).read_to_end(&mut out)?;
    stderr.take(LIMIT).read_to_end(&mut err)?;
    Ok(Output {
        status,
        stdout: out,
        stderr: err,
    })
}

pub(crate) fn expand(argument: &str, replacements: &[(&str, &str)]) -> String {
    argument
        .split_inclusive('}')
        .map(|part| {
            if let Some((token, value)) =
                replacements.iter().find(|(token, _)| part.ends_with(token))
            {
                format!("{}{value}", &part[..part.len() - token.len()])
            } else {
                part.to_owned()
            }
        })
        .collect()
}
