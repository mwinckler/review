use std::{
    io::{self, IsTerminal},
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{layout::Rect, text::Text};
use review::{
    app::{App, Focus},
    config::Config,
    renderer::{RenderOptions, RenderWorker},
    state::Review,
    store::{SavedSession, SessionLock, SessionStore},
    ui,
    vcs::{Backend, Repository},
};

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Review revision comparisons with a file tree, difft, and Vim keys"
)]
struct Cli {
    /// Compare BASE..HEAD directly; REV means REV..@
    ///
    /// Endpoints accept the selected backend's single-revision expressions.
    /// Omitted endpoints and @ select HEAD (Git) or the working copy (Jujutsu).
    /// Git also accepts BASE...HEAD for a merge-base comparison.
    #[arg(value_name = "REVSET", conflicts_with_all = ["base", "head", "resume", "list"])]
    comparison: Option<String>,
    /// Repository directory (defaults to the current directory)
    #[arg(short = 'C', long, default_value = ".")]
    repo: PathBuf,
    /// Override automatic repository detection
    #[arg(long, value_enum)]
    backend: Option<Backend>,
    /// Compare from the merge base of this revision (defaults to master)
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
    /// Load a TOML config (defaults to $XDG_CONFIG_HOME/review/config.toml)
    #[arg(long)]
    config: Option<PathBuf>,
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
    let resume_id = match cli.resume.as_deref() {
        Some("latest") => Some(
            store
                .list()?
                .first()
                .context("No saved review. Start one with review")?
                .id
                .clone(),
        ),
        Some(id) => Some(id.to_owned()),
        None => None,
    };
    ensure!(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "review needs an interactive terminal"
    );
    let config_path = match &cli.config {
        Some(path) => path.clone(),
        None => directories::BaseDirs::new()
            .context("finding the config directory; use --config")?
            .config_dir()
            .join("review/config.toml"),
    };
    let config = Config::load(&config_path, cli.config.is_some())?;
    let (mut saved, lock) = if let Some(id) = resume_id {
        store.open(&id)?
    } else {
        let comparison = if let Some(expression) = cli.comparison {
            repo.snapshot_expression(&expression)?
        } else {
            let head = cli.head.as_deref().unwrap_or(match repo.backend {
                Backend::Git => "HEAD",
                Backend::Jj => "@",
            });
            repo.snapshot(cli.base.as_deref().unwrap_or("master"), head)?
        };
        let changes = repo.changes(&comparison)?;
        store.create(Review::new(comparison, changes))?
    };
    if saved.review.comparison.line_counts.is_none() {
        saved.review.comparison.line_counts = Some(repo.line_counts(&saved.review.comparison)?);
        lock.save(&mut saved)?;
    }
    let mut app = App::new(saved.review.clone());
    app.message = format!("Session {} · ? help", saved.id);
    app.bindings_help = config
        .keybindings
        .iter()
        .map(|(key, action)| format!("{key}: {action}"))
        .collect::<Vec<_>>()
        .join(" · ");
    let worker = RenderWorker::new(config.renderer.clone());
    let result = ratatui::run(|terminal| {
        run(
            terminal, &repo, &mut app, &worker, &mut saved, &lock, &config,
        )
    });
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
    config: &Config,
) -> Result<()> {
    let mut diff = Text::raw("Select a changed file");
    let mut requested = None;
    let mut request_id = 0;
    let mut redraw = true;
    let mut last_save = Instant::now();
    while !app.quit {
        let size = terminal.size()?;
        let panes = ui::panes(Rect::new(0, 0, size.width, size.height), app);
        let width = if panes.diff.width > 0 {
            panes.diff.width
        } else {
            size.width
        }
        .saturating_sub(2)
        .max(1);
        let page = if app.focus == Focus::Tree {
            panes.tree.height
        } else {
            panes.diff.height
        }
        .saturating_sub(2)
        .max(1) as usize;
        if !app.review.view.active.is_empty() {
            let key = (
                app.review.view.active.clone(),
                width,
                app.review.view.inline,
                app.review.comparison.base_id.clone(),
                app.review.comparison.head_id.clone(),
            );
            if requested.as_ref() != Some(&key) {
                request_id = worker.request(
                    repo,
                    &app.review.comparison,
                    &app.review.view.active,
                    app.preview_changes().cloned().collect(),
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
            if requested.take().is_some() {
                request_id = worker.cancel();
                redraw = true;
            }
            diff = Text::raw(if app.review.changes.is_empty() {
                "Comparison complete — the snapshots are identical"
            } else if app.review.view.unviewed_only
                && app.review.viewed.len() == app.review.changes.len()
            {
                "Review complete — every file has been viewed. Press u to show all files."
            } else {
                "0 files match the current filters. Press Esc to clear them."
            });
        }
        for result in worker.results.try_iter() {
            if result.id == request_id {
                diff = match result.text {
                    Ok(text) => text,
                    Err(error) => Text::raw(format!("Diff error\n\n{}", ui::display_path(&error))),
                };
                app.position().vertical = app
                    .position()
                    .vertical
                    .min(diff.lines.len().saturating_sub(page));
                redraw = true;
            }
        }
        if redraw {
            app.update_flash(panes.tree.height.saturating_sub(2) as usize);
            terminal.draw(|frame| ui::draw(frame, app, &diff))?;
            redraw = false;
        }
        let mut force_save = false;
        if event::poll(Duration::from_millis(30))? {
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    app.message.clear();
                    let keys = if app.searching
                        || app.flash.is_some()
                        || app.help
                        || (key.code == KeyCode::Char('c')
                            && key.modifiers.contains(KeyModifiers::CONTROL))
                    {
                        vec![key]
                    } else {
                        config.translate(key)
                    };
                    for key in keys {
                        let search = app.focus == Focus::Diff
                            && !app.searching
                            && !app.help
                            && matches!(key.code, KeyCode::Char('n' | 'N'));
                        let search_enter =
                            app.focus == Focus::Diff && app.searching && key.code == KeyCode::Enter;
                        app.handle_key(key, page, diff.lines.len(), size.width);
                        force_save |= key.code == KeyCode::Char(' ');
                        if search || search_enter {
                            app.search_diff(&diff, key.code != KeyCode::Char('N'));
                        }
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
            match repo
                .refresh(&app.review.comparison)
                .and_then(|comparison| Ok((repo.changes(&comparison)?, comparison)))
            {
                Ok((changes, comparison)) => {
                    let invalidated = app.review.reconcile(comparison, changes);
                    app.rebuild();
                    app.message = format!("Snapshots refreshed · {invalidated} viewed marks reset");
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
