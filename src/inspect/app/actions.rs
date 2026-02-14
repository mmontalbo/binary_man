//! Actions for the inspector TUI.

use super::super::external::{
    open_in_editor, open_in_pager, open_man_page, run_external, try_copy_to_clipboard,
    TerminalGuard,
};
use super::super::Tab;
use super::App;
use anyhow::Result;
use ratatui::backend::CrosstermBackend;
use std::io::{self, Write};
use std::path::PathBuf;

impl App {
    pub(in crate::inspect) fn open_selected_in_editor(
        &mut self,
        guard: &mut TerminalGuard,
        terminal: &mut ratatui::Terminal<CrosstermBackend<io::Stdout>>,
    ) -> Result<()> {
        if let Some(path) = self.selected_editor_path() {
            return run_external(guard, terminal, || open_in_editor(&path));
        }
        Err(anyhow::anyhow!("no file selected to edit"))
    }

    pub(in crate::inspect) fn open_selected_in_pager(
        &mut self,
        guard: &mut TerminalGuard,
        terminal: &mut ratatui::Terminal<CrosstermBackend<io::Stdout>>,
    ) -> Result<()> {
        if let Some(entry) = self.selected_browse_entry() {
            if !entry.is_dir {
                return run_external(guard, terminal, || open_in_pager(&entry.path));
            }
            return Err(anyhow::anyhow!("cannot view directory in pager"));
        }
        Err(anyhow::anyhow!("no file selected to view"))
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

    pub(in crate::inspect) fn view_log_prompt(
        &mut self,
        guard: &mut TerminalGuard,
        terminal: &mut ratatui::Terminal<CrosstermBackend<io::Stdout>>,
    ) -> Result<()> {
        if self.tab != Tab::Log {
            return Err(anyhow::anyhow!("prompt view only available in Log tab"));
        }
        let path = self.selected_log_prompt_path()?;
        run_external(guard, terminal, || open_in_pager(&path))
    }

    pub(in crate::inspect) fn view_log_response(
        &mut self,
        guard: &mut TerminalGuard,
        terminal: &mut ratatui::Terminal<CrosstermBackend<io::Stdout>>,
    ) -> Result<()> {
        if self.tab != Tab::Log {
            return Err(anyhow::anyhow!("response view only available in Log tab"));
        }
        let path = self.selected_log_response_path()?;
        run_external(guard, terminal, || open_in_pager(&path))
    }

    fn selected_log_prompt_path(&self) -> Result<PathBuf> {
        let entry = self
            .selected_log_entry()
            .ok_or_else(|| anyhow::anyhow!("no log entry selected"))?;
        let filename = format!("cycle_{:03}_{}_prompt.txt", entry.cycle, entry.kind);
        let path = self
            .doc_pack_root
            .join("enrich")
            .join("lm_log")
            .join(&filename);
        if !path.exists() {
            return Err(anyhow::anyhow!(
                "prompt file not found: {} (run with --verbose to store full content)",
                filename
            ));
        }
        Ok(path)
    }

    fn selected_log_response_path(&self) -> Result<PathBuf> {
        let entry = self
            .selected_log_entry()
            .ok_or_else(|| anyhow::anyhow!("no log entry selected"))?;
        let filename = format!("cycle_{:03}_{}_response.txt", entry.cycle, entry.kind);
        let path = self
            .doc_pack_root
            .join("enrich")
            .join("lm_log")
            .join(&filename);
        if !path.exists() {
            return Err(anyhow::anyhow!(
                "response file not found: {} (run with --verbose to store full content)",
                filename
            ));
        }
        Ok(path)
    }

    /// Load full prompt content for the selected log entry.
    pub(in crate::inspect) fn load_selected_prompt(&self) -> Option<String> {
        let path = self.selected_log_prompt_path().ok()?;
        std::fs::read_to_string(&path).ok()
    }

    /// Load full response content for the selected log entry.
    pub(in crate::inspect) fn load_selected_response(&self) -> Option<String> {
        let path = self.selected_log_response_path().ok()?;
        std::fs::read_to_string(&path).ok()
    }
}
