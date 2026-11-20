use std::{
    env,
    ffi::OsStr,
    fs,
    io::{self, ErrorKind, Read, Seek, Write},
    path::Path,
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow, ensure};

fn missing(program: &OsStr, setting: Option<&str>) -> anyhow::Error {
    let name = Path::new(program).file_name().and_then(OsStr::to_str);
    let name = name.map(|name| name.strip_suffix(env::consts::EXE_SUFFIX).unwrap_or(name));
    let (install, next) = match name {
        Some("glab") => (
            "Install the GitLab CLI (`glab`)".into(),
            " Run `review gitlab login` afterward. See https://docs.gitlab.com/cli/.",
        ),
        Some("git") => ("Install Git".into(), " See https://git-scm.com/install/."),
        Some("jj") => (
            "Install Jujutsu 0.46 or newer".into(),
            " See https://docs.jj-vcs.dev/latest/install-and-setup/.",
        ),
        Some("difft") => (
            "Install Difftastic (`difft`)".into(),
            " See https://difftastic.wilfred.me.uk/.",
        ),
        Some("diff" | "gdiff") => ("Install GNU diffutils".into(), ""),
        Some("nvim") => ("Install Neovim (`nvim`)".into(), " See https://neovim.io/."),
        Some("xdg-terminal-exec") => (
            "Install `xdg-terminal-exec`".into(),
            " See https://github.com/Vladimir-csp/xdg-terminal-exec.",
        ),
        Some("conductor") => (
            "Reinstall Conductor".into(),
            " You can also retry with Review's regular checkout.",
        ),
        Some("sh") => ("Install a POSIX shell that provides `sh`".into(), ""),
        Some("cat" | "chmod" | "mkdir" | "rm") => {
            ("Install your system's core utilities".into(), "")
        }
        _ => (format!("Install {program:?}"), ""),
    };
    let alternative = setting.map_or_else(
        || " and add it to PATH".to_owned(),
        |setting| format!(" or set `{setting}` to an installed program"),
    );
    anyhow!("{install}{alternative}.{next} Could not find executable {program:?}.")
}

pub(crate) fn launch<T>(
    command: &mut Command,
    setting: Option<&str>,
    run: impl FnOnce(&mut Command) -> io::Result<T>,
) -> Result<T> {
    run(command).map_err(|error| {
        let program = command.get_program();
        match error.kind() {
            ErrorKind::NotFound => missing(program, setting),
            ErrorKind::PermissionDenied => anyhow::Error::new(error).context(format!(
                "Could not start {program:?}. Check execute permissions."
            )),
            _ => anyhow::Error::new(error).context(format!("Could not start {program:?}.")),
        }
    })
}

fn executable(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|metadata| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
        }
        #[cfg(not(unix))]
        {
            metadata.is_file()
        }
    })
}

pub(crate) fn require(program: &str, directory: &Path, setting: Option<&str>) -> Result<()> {
    let program = Path::new(program);
    let present = if program.file_name() == Some(program.as_os_str()) {
        let path = env::var_os("PATH")
            .unwrap_or_else(|| if cfg!(unix) { "/bin:/usr/bin" } else { "" }.into());
        env::split_paths(&path).any(|entry| {
            let path = directory.join(entry).join(program);
            executable(&path)
                || (cfg!(windows) && {
                    let mut candidate = path.into_os_string();
                    candidate.push(env::consts::EXE_SUFFIX);
                    executable(Path::new(&candidate))
                })
        })
    } else {
        executable(&directory.join(program))
    };
    if present {
        Ok(())
    } else {
        Err(missing(program.as_os_str(), setting))
    }
}

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
    let mut child = launch(
        command
            .stdout(stdout.try_clone()?)
            .stderr(stderr.try_clone()?),
        None,
        Command::spawn,
    )?;
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
