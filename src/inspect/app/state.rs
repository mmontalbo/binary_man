use super::super::data::{ArtifactEntry, EvidenceEntry, InspectData};
use super::super::format::next_action_copy;
use super::super::{Tab, PREVIEW_LIMIT};
use super::App;
use crate::enrich;
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
            tab: Tab::Intent,
            selection: [0; 4],
            show_all: [false; 4],
            message: None,
            show_help: false,
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
        self.clamp_selection();
    }

    pub(in crate::inspect) fn prev_tab(&mut self) {
        let idx = if self.tab.index() == 0 {
            Tab::ALL.len() - 1
        } else {
            self.tab.index() - 1
        };
        self.tab = Tab::ALL[idx];
        self.clamp_selection();
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

    pub(in crate::inspect) fn set_message(&mut self, message: String) {
        self.message = Some(message);
    }

    pub(super) fn selected_artifact(&self) -> Option<ArtifactEntry> {
        let idx = self.tab.index();
        match self.tab {
            Tab::Intent => self.data.intent.get(self.selection[idx]).cloned(),
            Tab::Outputs => self.data.outputs.get(self.selection[idx]).cloned(),
            Tab::History => self.data.history.get(self.selection[idx]).cloned(),
            Tab::Evidence => None,
        }
    }

    pub(super) fn selected_evidence(&self) -> Option<EvidenceEntry> {
        if self.tab != Tab::Evidence {
            return None;
        }
        self.data
            .evidence
            .entries
            .get(self.selection[self.tab.index()])
            .cloned()
    }

    pub(super) fn selected_copy_target(&self) -> Option<String> {
        if let Some(artifact) = self.selected_artifact() {
            return Some(artifact.path.display().to_string());
        }
        if let Some(evidence) = self.selected_evidence() {
            if let Some(path) = evidence.path.as_ref() {
                return Some(path.display().to_string());
            }
        }
        Some(next_action_copy(&self.summary.next_action))
    }

    pub(super) fn visible_items_len(&self, tab: Tab) -> usize {
        let max = match tab {
            Tab::Intent => self.data.intent.len(),
            Tab::Evidence => self.data.evidence.entries.len(),
            Tab::Outputs => self.data.outputs.len(),
            Tab::History => self.data.history.len(),
        };
        if self.show_all[tab.index()] {
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
