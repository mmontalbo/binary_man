use anyhow::{Context, Result};
use clap::Parser;
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers, MouseEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::prelude::*;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

mod app;
mod data;
mod ui;

#[derive(Parser)]
#[command(about = "Interactive experiment inspector for bman eval data")]
struct Args {
    /// Path to eval_data directory
    #[arg(default_value = "tools/eval_data")]
    data_dir: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let experiments =
        data::discover_experiments(&args.data_dir).context("discover experiments")?;

    if experiments.is_empty() {
        eprintln!("No experiments found in {}", args.data_dir.display());
        return Ok(());
    }

    enable_raw_mode().context("enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        crossterm::event::EnableMouseCapture
    )
    .context("enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("create terminal")?;

    let mut app = app::App::new(experiments);
    let result = run_app(&mut terminal, &mut app);

    disable_raw_mode().context("disable raw mode")?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture
    )
    .context("leave alternate screen")?;
    terminal.show_cursor().context("show cursor")?;

    result
}

enum EventResult {
    Quit,
    Changed,
    Ignored,
}

fn run_app<B: Backend>(terminal: &mut Terminal<B>, app: &mut app::App) -> Result<()> {
    terminal.draw(|frame| ui::draw(frame, app))?;

    loop {
        let ev = event::read().context("read event")?;
        let mut needs_redraw = match handle_event(ev, app)? {
            EventResult::Quit => return Ok(()),
            EventResult::Changed => true,
            EventResult::Ignored => false,
        };

        while event::poll(Duration::ZERO).context("poll event")? {
            let ev = event::read().context("read event")?;
            match handle_event(ev, app)? {
                EventResult::Quit => return Ok(()),
                EventResult::Changed => needs_redraw = true,
                EventResult::Ignored => {}
            }
        }

        if app.should_quit {
            return Ok(());
        }

        if needs_redraw {
            terminal.clear()?;
            terminal.draw(|frame| ui::draw(frame, app))?;
        }
    }
}

fn handle_event(ev: Event, app: &mut app::App) -> Result<EventResult> {
    match ev {
        Event::Key(key) => {
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && key.code == KeyCode::Char('c')
            {
                return Ok(EventResult::Quit);
            }

            if app.filter_active {
                match key.code {
                    KeyCode::Esc | KeyCode::Enter => app.toggle_filter(),
                    KeyCode::Backspace => app.filter_pop(),
                    KeyCode::Char(c) => app.filter_push(c),
                    _ => {}
                }
            } else {
                match key.code {
                    KeyCode::Char('q') => {
                        return Ok(EventResult::Quit);
                    }
                    KeyCode::Esc => {
                        if !app.filter.is_empty()
                            && app.focus == app::Focus::CellView
                        {
                            app.filter_clear();
                        } else {
                            app.back();
                        }
                    }
                    KeyCode::Enter => app.enter(),
                    KeyCode::Up | KeyCode::Char('k') => app.nav_up(),
                    KeyCode::Down | KeyCode::Char('j') => app.nav_down(),
                    KeyCode::Left | KeyCode::Char('h') => app.back(),
                    KeyCode::Right | KeyCode::Char('l') => app.enter(),
                    KeyCode::PageDown => app.page_down(),
                    KeyCode::PageUp => app.page_up(),
                    KeyCode::Char('G') => app.jump_bottom(),
                    KeyCode::Char('g') => app.jump_top(),
                    KeyCode::Char('e') => {
                        if app.focus == app::Focus::CellView {
                            app.expanded = !app.expanded;
                        }
                    }
                    KeyCode::Char('n') => {
                        if app.focus == app::Focus::CellView {
                            app.next_event();
                        }
                    }
                    KeyCode::Char('p') => {
                        if app.focus == app::Focus::CellView {
                            app.prev_event();
                        }
                    }
                    KeyCode::Char('/') => {
                        if app.focus == app::Focus::CellView {
                            app.toggle_filter();
                        }
                    }
                    _ => {}
                }
            }
            Ok(EventResult::Changed)
        }
        Event::Mouse(mouse) => match mouse.kind {
            MouseEventKind::ScrollUp => {
                app.nav_up();
                Ok(EventResult::Changed)
            }
            MouseEventKind::ScrollDown => {
                app.nav_down();
                Ok(EventResult::Changed)
            }
            _ => Ok(EventResult::Ignored),
        },
        Event::Resize(_, _) => Ok(EventResult::Changed),
        _ => Ok(EventResult::Ignored),
    }
}
