use super::app::App;
use super::data::load_state;
use super::external::TerminalGuard;
use super::EVENT_POLL_MS;
use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use ratatui::backend::CrosstermBackend;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

pub(super) fn run_tui(doc_pack_root: PathBuf) -> Result<()> {
    let show_all = [false; 4];
    let (summary, data) = load_state(&doc_pack_root, &show_all)?;
    let mut app = App::new(doc_pack_root, summary, data);

    let mut guard = TerminalGuard::enter()?;
    let mut terminal = {
        let stdout = io::stdout();
        let backend = CrosstermBackend::new(stdout);
        ratatui::Terminal::new(backend).context("init terminal")?
    };

    loop {
        terminal
            .draw(|frame| app.draw(frame))
            .context("draw inspect ui")?;

        if event::poll(Duration::from_millis(EVENT_POLL_MS)).context("poll event")? {
            if let Event::Key(key) = event::read().context("read event")? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if let Some(action) = action_from_key(key) {
                    match action {
                        Action::Quit => break,
                        Action::NextTab => app.next_tab(),
                        Action::PrevTab => app.prev_tab(),
                        Action::Up => app.move_selection(-1),
                        Action::Down => app.move_selection(1),
                        Action::Refresh => {
                            if let Err(err) = app.refresh() {
                                app.set_message(format!("refresh failed: {err}"));
                            } else {
                                app.set_message("refreshed".to_string());
                            }
                        }
                        Action::ToggleHelp => app.toggle_help(),
                        Action::ToggleShowAll => {
                            if let Err(err) = app.toggle_show_all() {
                                app.set_message(format!("show all failed: {err}"));
                            }
                        }
                        Action::OpenEditor => {
                            if let Err(err) = app.open_selected_in_editor(&mut guard, &mut terminal)
                            {
                                app.set_message(format!("open editor failed: {err}"));
                            }
                        }
                        Action::OpenPager => {
                            if let Err(err) = app.open_selected_in_pager(&mut guard, &mut terminal)
                            {
                                app.set_message(format!("open pager failed: {err}"));
                            }
                        }
                        Action::OpenMan => {
                            if let Err(err) = app.open_man_page(&mut guard, &mut terminal) {
                                app.set_message(format!("open man failed: {err}"));
                            }
                        }
                        Action::Copy => {
                            if let Err(err) = app.copy_selected(&mut guard, &mut terminal) {
                                app.set_message(format!("copy failed: {err}"));
                            }
                        }
                    }
                }
            }
        }
    }

    drop(guard);
    terminal.show_cursor().ok();
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum Action {
    Quit,
    NextTab,
    PrevTab,
    Up,
    Down,
    Refresh,
    OpenEditor,
    OpenPager,
    OpenMan,
    Copy,
    ToggleHelp,
    ToggleShowAll,
}

fn action_from_key(key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => Some(Action::Quit),
        KeyCode::Tab => Some(Action::NextTab),
        KeyCode::BackTab => Some(Action::PrevTab),
        KeyCode::Up | KeyCode::Char('k') => Some(Action::Up),
        KeyCode::Down | KeyCode::Char('j') => Some(Action::Down),
        KeyCode::Char('r') => Some(Action::Refresh),
        KeyCode::Char('o') => Some(Action::OpenEditor),
        KeyCode::Enter => Some(Action::OpenPager),
        KeyCode::Char('m') => Some(Action::OpenMan),
        KeyCode::Char('c') => Some(Action::Copy),
        KeyCode::Char('?') => Some(Action::ToggleHelp),
        KeyCode::Char('a') => Some(Action::ToggleShowAll),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    #[test]
    fn key_mapping() {
        let key = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        assert_eq!(action_from_key(key), Some(Action::Quit));
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(action_from_key(key), Some(Action::OpenPager));
        let key = KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE);
        assert_eq!(action_from_key(key), Some(Action::OpenEditor));
    }
}
