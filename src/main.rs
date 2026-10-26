use std::{
    io::{self, IsTerminal},
    path::PathBuf,
    time::Duration,
};

use anyhow::{Result, ensure};
use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{layout::Rect, text::Text};
use review::{
    app::{App, Focus},
    renderer::{RenderOptions, RenderWorker, RendererConfig},
    state::Review,
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
    base: String,
    /// Head revision (defaults to HEAD for Git, @ for Jujutsu)
    #[arg(long)]
    head: Option<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    ensure!(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "review needs an interactive terminal"
    );
    let repo = Repository::discover(&cli.repo, cli.backend)?;
    let head = cli.head.as_deref().unwrap_or(match repo.backend {
        Backend::Git => "HEAD",
        Backend::Jj => "@",
    });
    let comparison = repo.snapshot(&cli.base, head)?;
    let changes = repo.changes(&comparison)?;
    let mut app = App::new(Review::new(comparison, changes));
    let worker = RenderWorker::new(RendererConfig::default());
    ratatui::run(|terminal| run(terminal, &repo, &mut app, &worker))
}

fn run(
    terminal: &mut ratatui::DefaultTerminal,
    repo: &Repository,
    app: &mut App,
    worker: &RenderWorker,
) -> Result<()> {
    let mut diff = Text::raw("Select a changed file");
    let mut requested = None;
    let mut request_id = 0;
    let mut redraw = true;
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
        if event::poll(Duration::from_millis(30))? {
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    let search = app.focus == Focus::Diff
                        && !app.searching
                        && matches!(key.code, KeyCode::Char('n' | 'N'));
                    let search_enter =
                        app.focus == Focus::Diff && app.searching && key.code == KeyCode::Enter;
                    app.handle_key(key, page, diff.lines.len());
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
                    app.review.viewed.retain(|path, hash| {
                        changes
                            .iter()
                            .any(|change| change.path == *path && change.fingerprint() == *hash)
                    });
                    app.review.changes = changes;
                    app.review.comparison = comparison;
                    app.rebuild();
                    app.message = "Snapshots refreshed; matching review progress retained".into();
                    requested = None;
                }
                Err(error) => {
                    app.message = format!("Refresh failed: {error:#}");
                }
            }
            redraw = true;
        }
    }
    Ok(())
}
