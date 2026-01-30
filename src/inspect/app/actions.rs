use super::super::external::{
    open_in_editor, open_in_pager, open_man_page, run_external, try_copy_to_clipboard,
    TerminalGuard,
};
use super::App;
use anyhow::Result;
use ratatui::backend::CrosstermBackend;
use std::io::{self, Write};

impl App {
    pub(in crate::inspect) fn open_selected_in_editor(
        &mut self,
        guard: &mut TerminalGuard,
        terminal: &mut ratatui::Terminal<CrosstermBackend<io::Stdout>>,
    ) -> Result<()> {
        if let Some(artifact) = self.selected_artifact() {
            return run_external(guard, terminal, || open_in_editor(&artifact.path));
        }
        if let Some(evidence) = self.selected_evidence() {
            if let Some(path) = evidence.path.as_ref() {
                return run_external(guard, terminal, || open_in_editor(path));
            }
            return Err(anyhow::anyhow!("no evidence file to open"));
        }
        Err(anyhow::anyhow!("no file selected"))
    }

    pub(in crate::inspect) fn open_selected_in_pager(
        &mut self,
        guard: &mut TerminalGuard,
        terminal: &mut ratatui::Terminal<CrosstermBackend<io::Stdout>>,
    ) -> Result<()> {
        if let Some(artifact) = self.selected_artifact() {
            return run_external(guard, terminal, || open_in_pager(&artifact.path));
        }
        if let Some(evidence) = self.selected_evidence() {
            if let Some(path) = evidence.path.as_ref() {
                return run_external(guard, terminal, || open_in_pager(path));
            }
            return Err(anyhow::anyhow!("no evidence file to open"));
        }
        Err(anyhow::anyhow!("no file selected"))
    }

    pub(in crate::inspect) fn open_man_page(
        &mut self,
        guard: &mut TerminalGuard,
        terminal: &mut ratatui::Terminal<CrosstermBackend<io::Stdout>>,
    ) -> Result<()> {
        let Some(path) = self.data.man_page_path.as_ref() else {
            return Err(anyhow::anyhow!("no man page found"));
        };
        run_external(guard, terminal, || open_man_page(path))
    }

    pub(in crate::inspect) fn copy_selected(
        &mut self,
        guard: &mut TerminalGuard,
        terminal: &mut ratatui::Terminal<CrosstermBackend<io::Stdout>>,
    ) -> Result<()> {
        let Some(target) = self.selected_copy_target() else {
            return Err(anyhow::anyhow!("nothing to copy"));
        };
        if try_copy_to_clipboard(&target)? {
            self.set_message("copied to clipboard".to_string());
            return Ok(());
        }
        let output = format!("copy this: {target}");
        run_external(guard, terminal, || {
            println!("{output}");
            io::stdout().flush().ok();
            Ok(())
        })?;
        self.set_message("copy line printed to stdout".to_string());
        Ok(())
    }
}
