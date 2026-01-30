use anyhow::{anyhow, Context, Result};
use crossterm::cursor::{Hide, Show};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use shell_words::split as shell_split;
use std::io::{self, Write};
use std::path::Path;
use std::process::Command;

pub(super) fn open_in_editor(path: &Path) -> Result<()> {
    let cmd = resolve_command(&["VISUAL", "EDITOR"], "vi");
    run_command(cmd, Some(path))
}

pub(super) fn open_in_pager(path: &Path) -> Result<()> {
    if !path.exists() {
        return Err(anyhow!("missing file {}", path.display()));
    }
    let cmd = resolve_command(&["PAGER"], "less");
    run_command(cmd, Some(path))
}

pub(super) fn open_man_page(path: &Path) -> Result<()> {
    if !path.exists() {
        return Err(anyhow!("missing man page {}", path.display()));
    }
    let cmd = vec![
        "man".to_string(),
        "-l".to_string(),
        path.display().to_string(),
    ];
    run_command(cmd, None)
}

fn resolve_command(vars: &[&str], fallback: &str) -> Vec<String> {
    for var in vars {
        if let Ok(value) = std::env::var(var) {
            if !value.trim().is_empty() {
                if let Ok(parts) = shell_split(&value) {
                    if !parts.is_empty() {
                        return parts;
                    }
                }
            }
        }
    }
    vec![fallback.to_string()]
}

fn run_command(mut cmd: Vec<String>, path: Option<&Path>) -> Result<()> {
    if cmd.is_empty() {
        return Err(anyhow!("missing command"));
    }
    let program = cmd.remove(0);
    let mut command = Command::new(program);
    if !cmd.is_empty() {
        command.args(cmd);
    }
    if let Some(path) = path {
        command.arg(path);
    }
    let status = command.status().with_context(|| "run external command")?;
    if !status.success() {
        return Err(anyhow!("external command failed"));
    }
    Ok(())
}

pub(super) fn try_copy_to_clipboard(text: &str) -> Result<bool> {
    let candidates = vec![
        ("pbcopy", vec![]),
        ("wl-copy", vec![]),
        ("xclip", vec!["-selection", "clipboard"]),
        ("xsel", vec!["--clipboard", "--input"]),
    ];
    for (program, args) in candidates {
        let mut command = Command::new(program);
        command.args(args).stdin(std::process::Stdio::piped());
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(_) => continue,
        };
        if let Some(stdin) = child.stdin.as_mut() {
            stdin.write_all(text.as_bytes()).ok();
        }
        let status = child.wait()?;
        if status.success() {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn run_external<F>(
    guard: &mut TerminalGuard,
    terminal: &mut ratatui::Terminal<CrosstermBackend<io::Stdout>>,
    f: F,
) -> Result<()>
where
    F: FnOnce() -> Result<()>,
{
    guard.suspend()?;
    let result = f();
    guard.resume()?;
    terminal.clear().ok();
    result
}

pub(super) struct TerminalGuard {
    active: bool,
}

impl TerminalGuard {
    pub(super) fn enter() -> Result<Self> {
        enable_raw_mode().context("enable raw mode")?;
        execute!(io::stdout(), EnterAlternateScreen, Hide).context("enter alt screen")?;
        Ok(Self { active: true })
    }

    pub(super) fn suspend(&mut self) -> Result<()> {
        if self.active {
            disable_raw_mode().ok();
            execute!(io::stdout(), LeaveAlternateScreen, Show).ok();
            self.active = false;
        }
        Ok(())
    }

    pub(super) fn resume(&mut self) -> Result<()> {
        if !self.active {
            execute!(io::stdout(), EnterAlternateScreen, Hide).ok();
            enable_raw_mode().ok();
            self.active = true;
        }
        Ok(())
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = self.suspend();
    }
}
