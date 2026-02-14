//! Application state management for the inspector TUI.

use super::super::data::{BrowseEntry, InspectData, WorkItem};
use super::super::{Tab, PREVIEW_LIMIT};
use super::App;
use crate::enrich::{self, LmLogEntry};
use anyhow::Result;
use std::cmp::min;
use std::path::PathBuf;

impl App {
    pub(in crate::inspect) fn new(
        doc_pack_root: PathBuf,
        summary: enrich::StatusSummary,
        data: InspectData,
    ) -> Self {
        Self {
            doc_pack_root,
            summary,
            data,
            tab: Tab::Work,
            selection: [0; 3],
            show_all: [false; 3],
            message: None,
            show_help: false,
            detail_view: false,
            detail_scroll: 0,
            browse_preview_focus: false,
        }
    }

    pub(in crate::inspect) fn refresh(&mut self) -> Result<()> {
        let (summary, data) = super::super::data::load_state(&self.doc_pack_root, &self.show_all)?;
        self.summary = summary;
        self.data = data;
        self.clamp_selection();
        Ok(())
    }

    pub(in crate::inspect) fn toggle_show_all(&mut self) -> Result<()> {
        let idx = self.tab.index();
        self.show_all[idx] = !self.show_all[idx];
        self.refresh()
    }

    pub(in crate::inspect) fn next_tab(&mut self) {
        let idx = (self.tab.index() + 1) % Tab::ALL.len();
        self.tab = Tab::ALL[idx];
        self.detail_view = false;
        self.browse_preview_focus = false;
        self.clamp_selection();
    }

    pub(in crate::inspect) fn prev_tab(&mut self) {
        let idx = if self.tab.index() == 0 {
            Tab::ALL.len() - 1
        } else {
            self.tab.index() - 1
        };
        self.tab = Tab::ALL[idx];
        self.detail_view = false;
        self.browse_preview_focus = false;
        self.clamp_selection();
    }

    pub(in crate::inspect) fn toggle_browse_focus(&mut self) {
        if self.tab == Tab::Browse {
            self.browse_preview_focus = !self.browse_preview_focus;
            self.detail_scroll = 0;
        }
    }

    pub(in crate::inspect) fn is_browse_preview_focused(&self) -> bool {
        self.tab == Tab::Browse && self.browse_preview_focus
    }

    pub(in crate::inspect) fn move_selection(&mut self, delta: isize) {
        let idx = self.tab.index();
        let max = self.visible_items_len(self.tab);
        if max == 0 {
            self.selection[idx] = 0;
            return;
        }
        let current = self.selection[idx] as isize;
        let next = current + delta;
        let clamped = if next < 0 {
            0
        } else if next as usize >= max {
            max as isize - 1
        } else {
            next
        };
        self.selection[idx] = clamped as usize;
    }

    pub(in crate::inspect) fn toggle_help(&mut self) {
        self.show_help = !self.show_help;
    }

    pub(in crate::inspect) fn toggle_detail(&mut self) {
        self.detail_view = !self.detail_view;
        if self.detail_view {
            self.detail_scroll = 0; // Reset scroll on open
        }
    }

    pub(in crate::inspect) fn close_detail(&mut self) {
        self.detail_view = false;
        self.detail_scroll = 0;
    }

    pub(in crate::inspect) fn scroll_detail(&mut self, delta: i16) {
        if self.detail_view || self.browse_preview_focus {
            let new_scroll = self.detail_scroll as i16 + delta;
            self.detail_scroll = new_scroll.max(0) as u16;
        }
    }

    pub(in crate::inspect) fn reset_detail_scroll(&mut self) {
        self.detail_scroll = 0;
    }

    pub(in crate::inspect) fn is_detail_view(&self) -> bool {
        self.detail_view
    }

    pub(in crate::inspect) fn set_message(&mut self, message: String) {
        self.message = Some(message);
    }

    /// Get the selected work item (if on Work tab).
    pub(super) fn selected_work_item(&self) -> Option<&WorkItem> {
        if self.tab != Tab::Work {
            return None;
        }
        let flat_items = self.data.work.flat_items();
        let idx = self.selection[Tab::Work.index()];
        flat_items.get(idx).and_then(|(_, item)| *item)
    }

    /// Get the selected log entry (if on Log tab).
    pub(super) fn selected_log_entry(&self) -> Option<&LmLogEntry> {
        if self.tab != Tab::Log {
            return None;
        }
        self.data.log.get(self.selection[Tab::Log.index()])
    }

    /// Get the selected browse entry (if on Browse tab).
    pub(super) fn selected_browse_entry(&self) -> Option<&BrowseEntry> {
        if self.tab != Tab::Browse {
            return None;
        }
        self.data.browse.get(self.selection[Tab::Browse.index()])
    }

    /// Get copy target for current selection.
    pub(super) fn selected_copy_target(&self) -> Option<String> {
        // If there's a suggested command, use that
        if let enrich::NextAction::Command { command, .. } = &self.summary.next_action {
            return Some(command.clone());
        }

        // Otherwise, copy path of selected item
        match self.tab {
            Tab::Work => self
                .selected_work_item()
                .map(|item| item.surface_id.clone()),
            Tab::Log => self
                .selected_log_entry()
                .map(|entry| format!("cycle {} {:?}", entry.cycle, entry.kind)),
            Tab::Browse => self
                .selected_browse_entry()
                .map(|entry| entry.path.display().to_string()),
        }
    }

    /// Get path to open in editor (if applicable).
    pub(super) fn selected_editor_path(&self) -> Option<PathBuf> {
        match self.tab {
            Tab::Browse => self
                .selected_browse_entry()
                .filter(|e| !e.is_dir)
                .map(|e| e.path.clone()),
            _ => None,
        }
    }

    pub(super) fn visible_items_len(&self, tab: Tab) -> usize {
        let max = match tab {
            Tab::Work => self.data.work.flat_items().len(),
            Tab::Log => self.data.log.len(),
            Tab::Browse => self.data.browse.len(),
        };
        // Browse always shows all files; other tabs respect show_all toggle
        if tab == Tab::Browse || self.show_all[tab.index()] {
            max
        } else {
            min(max, PREVIEW_LIMIT)
        }
    }

    fn clamp_selection(&mut self) {
        for tab in Tab::ALL.iter().copied() {
            let idx = tab.index();
            let max = self.visible_items_len(tab);
            if max == 0 {
                self.selection[idx] = 0;
            } else if self.selection[idx] >= max {
                self.selection[idx] = max - 1;
            }
        }
    }
}
