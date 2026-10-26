use std::{
    io::{self, IsTerminal},
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{layout::Rect, text::Text};
use review::{
    app::{App, Focus},
    renderer::{RenderOptions, RenderWorker, RendererConfig},
    state::Review,
    store::{SavedSession, SessionLock, SessionStore},
    ui,
    vcs::{Backend, Repository},
};

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Review a whole branch with a file tree, difft, and Vim keys"
)]
struct Cli {
    /// Repository directory (defaults to the current directory)
    #[arg(short = 'C', long, default_value = ".")]
    repo: PathBuf,
    /// Override automatic repository detection
    #[arg(long, value_enum)]
    backend: Option<Backend>,
    /// Base branch or single-revision jj expression; compare from its merge base
    #[arg(long)]
    base: Option<String>,
    /// Head revision (defaults to HEAD for Git, @ for Jujutsu)
    #[arg(long, requires = "base")]
    head: Option<String>,
    /// Resume a saved session by ID, or the latest session when omitted
    #[arg(long, num_args = 0..=1, default_missing_value = "latest", conflicts_with = "base")]
    resume: Option<String>,
    /// List this repository's saved review sessions
    #[arg(long, conflicts_with_all = ["base", "resume"])]
    list: bool,
    /// Override the review state directory
    #[arg(long)]
    state_dir: Option<PathBuf>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let repo = Repository::discover(&cli.repo, cli.backend)?;
    let state_dir = match cli.state_dir {
        Some(directory) => directory,
        None => {
            let base = directories::BaseDirs::new()
                .context("finding the user state directory; use --state-dir")?;
            base.state_dir()
                .unwrap_or(base.data_local_dir())
                .join("review")
        }
    };
    let store = SessionStore::new(&state_dir, &repo)?;
    if cli.list {
        for saved in store.list()? {
            println!(
                "{}  {} → {}  Viewed {}/{}",
                saved.id,
                ui::display_path(&saved.review.comparison.base_ref),
                ui::display_path(&saved.review.comparison.head_ref),
                saved.review.viewed.len(),
                saved.review.changes.len()
            );
        }
        return Ok(());
    }
    let resume_id = if cli.base.is_none() {
        Some(match cli.resume.as_deref() {
            Some(id) if id != "latest" => id.to_owned(),
            _ => store
                .list()?
                .first()
                .context("No saved review. Start one with review --base <branch-or-revision>")?
                .id
                .clone(),
        })
    } else {
        None
    };
    ensure!(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "review needs an interactive terminal"
    );
    let (mut saved, lock) = if let Some(id) = resume_id {
        store.open(&id)?
    } else {
        let head = cli.head.as_deref().unwrap_or(match repo.backend {
            Backend::Git => "HEAD",
            Backend::Jj => "@",
        });
        let comparison = repo.snapshot(
            cli.base
                .as_deref()
                .expect("base provided for a new session"),
            head,
        )?;
        let changes = repo.changes(&comparison)?;
        store.create(&repo, Review::new(comparison, changes))?
    };
    let mut app = App::new(saved.review.clone());
    app.message = format!("Session {} · ? help", saved.id);
    let worker = RenderWorker::new(RendererConfig::default());
    let result =
        ratatui::run(|terminal| run(terminal, &repo, &mut app, &worker, &mut saved, &lock));
    saved.review = app.review;
    let saved_result = lock.save(&mut saved);
    result.and(saved_result)
}

fn run(
    terminal: &mut ratatui::DefaultTerminal,
    repo: &Repository,
    app: &mut App,
    worker: &RenderWorker,
    saved: &mut SavedSession,
    lock: &SessionLock,
) -> Result<()> {
    let mut diff = Text::raw("Select a changed file");
    let mut requested = None;
    let mut request_id = 0;
    let mut redraw = true;
    let mut last_save = Instant::now();
    while !app.quit {
        let size = terminal.size()?;
        let panes = ui::panes(Rect::new(0, 0, size.width, size.height), app);
        let width = panes.diff.width.saturating_sub(2).max(1);
        let page = panes.diff.height.saturating_sub(2).max(1) as usize;
        if let Some(index) = app.active {
            let key = (
                index,
                width,
                app.review.view.inline,
                app.review.comparison.base_id.clone(),
                app.review.comparison.head_id.clone(),
            );
            if requested.as_ref() != Some(&key) {
                request_id = worker.request(
                    repo,
                    &app.review.comparison,
                    &app.review.changes[index],
                    RenderOptions {
                        width,
                        inline: app.review.view.inline,
                    },
                );
                requested = Some(key);
                diff = Text::raw("Rendering…");
                redraw = true;
            }
        } else {
            diff = Text::raw(if app.review.changes.is_empty() {
                "Comparison complete — the snapshots are identical"
            } else {
                "All matching files have been viewed. Press u or Esc to show more files."
            });
        }
        for result in worker.results.try_iter() {
            if result.id == request_id {
                diff = match result.text {
                    Ok(text) => text,
                    Err(error) => Text::raw(format!("Diff error\n\n{error}")),
                };
                app.position().vertical = app
                    .position()
                    .vertical
                    .min(diff.lines.len().saturating_sub(page));
                redraw = true;
            }
        }
        if redraw {
            terminal.draw(|frame| ui::draw(frame, app, &diff))?;
            redraw = false;
        }
        let mut force_save = false;
        if event::poll(Duration::from_millis(30))? {
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    let search = app.focus == Focus::Diff
                        && !app.searching
                        && matches!(key.code, KeyCode::Char('n' | 'N'));
                    let search_enter =
                        app.focus == Focus::Diff && app.searching && key.code == KeyCode::Enter;
                    app.handle_key(key, page, diff.lines.len());
                    force_save = key.code == KeyCode::Char(' ');
                    if search || search_enter {
                        app.search_diff(&diff, key.code != KeyCode::Char('N'));
                    }
                    redraw = true;
                }
                Event::Resize(_, _) => {
                    redraw = true;
                }
                _ => {}
            }
        }
        if app.refresh {
            app.refresh = false;
            let old = &app.review.comparison;
            match repo
                .snapshot(&old.base_ref, &old.head_ref)
                .and_then(|comparison| Ok((repo.changes(&comparison)?, comparison)))
            {
                Ok((changes, comparison)) => {
                    let invalidated = app.review.reconcile(comparison, changes);
                    app.rebuild();
                    app.message = format!(
                        "Snapshots refreshed · {invalidated} viewed files need another look"
                    );
                    requested = None;
                    force_save = true;
                }
                Err(error) => {
                    app.message = format!("Refresh failed: {error:#}");
                }
            }
            redraw = true;
        }
        if app.dirty
            && (force_save || app.quit || last_save.elapsed() >= Duration::from_millis(250))
        {
            saved.review = app.review.clone();
            match lock.save(saved) {
                Ok(()) => {
                    app.dirty = false;
                }
                Err(error) => {
                    if app.quit {
                        return Err(error);
                    }
                    app.message = format!("Save failed; retrying: {error:#}");
                    redraw = true;
                }
            }
            last_save = Instant::now();
        }
    }
    Ok(())
}
