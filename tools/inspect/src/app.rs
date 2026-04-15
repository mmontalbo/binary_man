use ratatui::widgets::ListState;

use crate::data::{CellState, CharacterizationLog, CycleResponseMap, Experiment, Cell, Surface, Transcript};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Experiments,
    Cells,
    CellView,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    SurfaceList,
    Detail,
}

pub struct App {
    pub experiments: Vec<Experiment>,
    pub experiment_list_state: ListState,
    pub cell_list_state: ListState,
    pub surface_list_state: ListState,
    pub cell_state: Option<CellState>,
    pub transcripts: Vec<Transcript>,
    pub cycle_responses: CycleResponseMap,
    pub char_logs: Vec<CharacterizationLog>,
    pub focused_event: usize,
    pub event_cycles: Vec<u32>,
    pub focus: Focus,
    pub active_pane: Pane,
    pub detail_scroll: u16,
    pub expanded: bool,
    pub should_quit: bool,
    pub filter: String,
    pub filter_active: bool,
    /// Indices into cell_state.surfaces matching current filter.
    pub filtered_indices: Vec<usize>,
}

impl App {
    pub fn new(experiments: Vec<Experiment>) -> Self {
        let mut experiment_list_state = ListState::default();
        if !experiments.is_empty() {
            experiment_list_state.select(Some(0));
        }
        let mut cell_list_state = ListState::default();
        if experiments.first().is_some_and(|e| !e.cells.is_empty()) {
            cell_list_state.select(Some(0));
        }

        Self {
            experiments,
            experiment_list_state,
            cell_list_state,
            surface_list_state: ListState::default(),
            cell_state: None,
            transcripts: Vec::new(),
            cycle_responses: CycleResponseMap::new(),
            char_logs: Vec::new(),
            focused_event: 0,
            event_cycles: Vec::new(),
            focus: Focus::Experiments,
            active_pane: Pane::SurfaceList,
            detail_scroll: 0,
            expanded: false,
            should_quit: false,
            filter: String::new(),
            filter_active: false,
            filtered_indices: Vec::new(),
        }
    }

    pub fn selected_experiment(&self) -> Option<&Experiment> {
        self.experiment_list_state
            .selected()
            .and_then(|i| self.experiments.get(i))
    }

    pub fn selected_cell(&self) -> Option<&Cell> {
        self.selected_experiment().and_then(|exp| {
            self.cell_list_state
                .selected()
                .and_then(|i| exp.cells.get(i))
        })
    }

    /// Get the currently selected surface (through the filtered index).
    pub fn selected_surface(&self) -> Option<&Surface> {
        let state = self.cell_state.as_ref()?;
        let list_idx = self.surface_list_state.selected()?;
        let real_idx = *self.filtered_indices.get(list_idx)?;
        state.surfaces.get(real_idx)
    }

    /// Rebuild the cached event_cycles vec for the currently selected surface.
    pub fn rebuild_event_cycles(&mut self) {
        self.event_cycles.clear();
        self.focused_event = 0;
        if let Some(surface) = self.selected_surface().cloned() {
            let mut events: Vec<u32> = Vec::new();
            for p in &surface.probes {
                events.push(p.cycle);
            }
            for a in &surface.attempts {
                events.push(a.cycle);
            }
            events.sort();
            events.dedup();
            self.event_cycles = events;
        }
    }

    pub fn next_event(&mut self) {
        if self.event_cycles.is_empty() {
            return;
        }
        self.focused_event = (self.focused_event + 1) % self.event_cycles.len();
        self.detail_scroll = u16::MAX;
    }

    pub fn prev_event(&mut self) {
        if self.event_cycles.is_empty() {
            return;
        }
        if self.focused_event == 0 {
            self.focused_event = self.event_cycles.len() - 1;
        } else {
            self.focused_event -= 1;
        }
        self.detail_scroll = u16::MAX;
    }

    // -- Filter --

    pub fn rebuild_filtered_indices(&mut self) {
        let state = match &self.cell_state {
            Some(s) => s,
            None => {
                self.filtered_indices.clear();
                return;
            }
        };

        let filter_lower = self.filter.to_lowercase();
        let mut indices: Vec<usize> = (0..state.surfaces.len())
            .filter(|&i| {
                if filter_lower.is_empty() {
                    return true;
                }
                let s = &state.surfaces[i];
                s.id.to_lowercase().contains(&filter_lower)
                    || s.description.to_lowercase().contains(&filter_lower)
            })
            .collect();

        indices.sort_by_key(|&i| status_order(&state.surfaces[i].status));

        self.filtered_indices = indices;
        if !self.filtered_indices.is_empty() {
            let sel = self
                .surface_list_state
                .selected()
                .unwrap_or(0)
                .min(self.filtered_indices.len() - 1);
            self.surface_list_state.select(Some(sel));
        } else {
            self.surface_list_state.select(None);
        }
    }

    pub fn toggle_filter(&mut self) {
        self.filter_active = !self.filter_active;
    }

    pub fn filter_push(&mut self, c: char) {
        self.filter.push(c);
        self.rebuild_filtered_indices();
        self.detail_scroll = 0;
    }

    pub fn filter_pop(&mut self) {
        self.filter.pop();
        self.rebuild_filtered_indices();
        self.detail_scroll = 0;
    }

    pub fn filter_clear(&mut self) {
        self.filter.clear();
        self.rebuild_filtered_indices();
        self.detail_scroll = 0;
    }

    // -- Navigation --

    pub fn nav_up(&mut self) {
        match self.focus {
            Focus::Experiments => {
                list_prev(&mut self.experiment_list_state, self.experiments.len());
            }
            Focus::Cells => {
                let len = self.selected_experiment().map_or(0, |e| e.cells.len());
                list_prev(&mut self.cell_list_state, len);
            }
            Focus::CellView => match self.active_pane {
                Pane::SurfaceList => {
                    list_prev(&mut self.surface_list_state, self.filtered_indices.len());
                    self.detail_scroll = 0;
                    self.rebuild_event_cycles();
                }
                Pane::Detail => {
                    self.detail_scroll = self.detail_scroll.saturating_sub(1);
                }
            },
        }
    }

    pub fn nav_down(&mut self) {
        match self.focus {
            Focus::Experiments => {
                list_next(&mut self.experiment_list_state, self.experiments.len());
            }
            Focus::Cells => {
                let len = self.selected_experiment().map_or(0, |e| e.cells.len());
                list_next(&mut self.cell_list_state, len);
            }
            Focus::CellView => match self.active_pane {
                Pane::SurfaceList => {
                    list_next(&mut self.surface_list_state, self.filtered_indices.len());
                    self.detail_scroll = 0;
                    self.rebuild_event_cycles();
                }
                Pane::Detail => {
                    self.detail_scroll = self.detail_scroll.saturating_add(1);
                }
            },
        }
    }

    pub fn page_down(&mut self) {
        match self.focus {
            Focus::Experiments => {
                for _ in 0..20 {
                    list_next(&mut self.experiment_list_state, self.experiments.len());
                }
            }
            Focus::Cells => {
                let len = self.selected_experiment().map_or(0, |e| e.cells.len());
                for _ in 0..20 {
                    list_next(&mut self.cell_list_state, len);
                }
            }
            Focus::CellView => match self.active_pane {
                Pane::SurfaceList => {
                    for _ in 0..20 {
                        list_next(&mut self.surface_list_state, self.filtered_indices.len());
                    }
                    self.detail_scroll = 0;
                    self.rebuild_event_cycles();
                }
                Pane::Detail => {
                    self.detail_scroll = self.detail_scroll.saturating_add(20);
                }
            },
        }
    }

    pub fn page_up(&mut self) {
        match self.focus {
            Focus::Experiments => {
                for _ in 0..20 {
                    list_prev(&mut self.experiment_list_state, self.experiments.len());
                }
            }
            Focus::Cells => {
                let len = self.selected_experiment().map_or(0, |e| e.cells.len());
                for _ in 0..20 {
                    list_prev(&mut self.cell_list_state, len);
                }
            }
            Focus::CellView => match self.active_pane {
                Pane::SurfaceList => {
                    for _ in 0..20 {
                        list_prev(&mut self.surface_list_state, self.filtered_indices.len());
                    }
                    self.detail_scroll = 0;
                    self.rebuild_event_cycles();
                }
                Pane::Detail => {
                    self.detail_scroll = self.detail_scroll.saturating_sub(20);
                }
            },
        }
    }

    pub fn jump_top(&mut self) {
        match self.focus {
            Focus::Experiments => {
                if !self.experiments.is_empty() {
                    self.experiment_list_state.select(Some(0));
                }
            }
            Focus::Cells => {
                self.cell_list_state.select(Some(0));
            }
            Focus::CellView => match self.active_pane {
                Pane::SurfaceList => {
                    if !self.filtered_indices.is_empty() {
                        self.surface_list_state.select(Some(0));
                        self.detail_scroll = 0;
                        self.rebuild_event_cycles();
                    }
                }
                Pane::Detail => {
                    self.detail_scroll = 0;
                }
            },
        }
    }

    pub fn jump_bottom(&mut self) {
        match self.focus {
            Focus::Experiments => {
                let len = self.experiments.len();
                if len > 0 {
                    self.experiment_list_state.select(Some(len - 1));
                }
            }
            Focus::Cells => {
                let len = self.selected_experiment().map_or(0, |e| e.cells.len());
                if len > 0 {
                    self.cell_list_state.select(Some(len - 1));
                }
            }
            Focus::CellView => match self.active_pane {
                Pane::SurfaceList => {
                    let len = self.filtered_indices.len();
                    if len > 0 {
                        self.surface_list_state.select(Some(len - 1));
                        self.detail_scroll = 0;
                        self.rebuild_event_cycles();
                    }
                }
                Pane::Detail => {
                    self.detail_scroll = u16::MAX; // will be clamped by draw
                }
            },
        }
    }

    /// Move right / drill in. In CellView, switches pane focus.
    pub fn enter(&mut self) {
        match self.focus {
            Focus::Experiments => {
                self.cell_list_state.select(Some(0));
                self.focus = Focus::Cells;
            }
            Focus::Cells => {
                if let Some(cell) = self.selected_cell() {
                    match crate::data::load_cell_state(cell) {
                        Ok(state) => {
                            let transcripts =
                                crate::data::load_transcripts(&state.lm_log_dir)
                                    .unwrap_or_default();
                            let responses =
                                crate::data::load_cycle_responses(&state.lm_log_dir);
                            let char_logs =
                                crate::data::load_characterization_logs(&state.lm_log_dir);
                            self.cell_state = Some(state);
                            self.transcripts = transcripts;
                            self.cycle_responses = responses;
                            self.char_logs = char_logs;
                            self.surface_list_state.select(Some(0));
                            self.focus = Focus::CellView;
                            self.active_pane = Pane::SurfaceList;
                            self.detail_scroll = 0;
                            self.filter.clear();
                            self.filter_active = false;
                            self.rebuild_filtered_indices();
                            self.rebuild_event_cycles();
                        }
                        Err(_e) => {}
                    }
                }
            }
            Focus::CellView => {
                self.active_pane = Pane::Detail;
            }
        }
    }

    /// Move left / back out. In CellView, switches pane focus before exiting.
    /// Clamps at Experiments (doesn't quit).
    pub fn back(&mut self) {
        match self.focus {
            Focus::Experiments => {} // clamp — don't quit
            Focus::Cells => self.focus = Focus::Experiments,
            Focus::CellView => {
                if self.active_pane == Pane::Detail {
                    self.active_pane = Pane::SurfaceList;
                } else {
                    self.cell_state = None;
                    self.transcripts.clear();
                    self.cycle_responses.clear();
                    self.char_logs.clear();
                    self.event_cycles.clear();
                    self.focused_event = 0;
                    self.filter.clear();
                    self.filter_active = false;
                    self.focus = Focus::Cells;
                }
            }
        }
    }
}

fn status_order(status: &str) -> u8 {
    match status {
        "Verified" => 0,
        "Pending" => 1,
        _ => 2,
    }
}

/// Classify a surface's failure mode for grouping.
/// Returns (sort_order, full_label, short_label).
pub fn failure_mode_key(s: &Surface) -> (u8, &'static str, &'static str) {
    if s.status == "Verified" {
        return (0, "Verified", "");
    }

    let all_zero_bytes = !s.probes.is_empty()
        && s.probes.iter().all(|p| {
            !p.outputs_differ
                && !p.setup_failed
                && p.stdout_preview.as_ref().is_none_or(|v| v.is_empty())
                && p.control_stdout_preview
                    .as_ref()
                    .is_none_or(|v| v.is_empty())
        });

    let has_setup_failed = s.probes.iter().any(|p| p.setup_failed)
        || s.attempts.iter().any(|a| a.outcome == "SetupFailed");
    let has_option_error = s.attempts.iter().any(|a| a.outcome == "OptionError");

    if all_zero_bytes {
        (1, "identical (0 bytes)", "0B")
    } else if has_setup_failed {
        (2, "SetupFailed", "setup")
    } else if has_option_error {
        (3, "OptionError", "opt-err")
    } else if s.probes.is_empty() && s.attempts.is_empty() {
        (5, "untouched", "")
    } else {
        (4, "other", "other")
    }
}

fn list_next(state: &mut ListState, len: usize) {
    if len == 0 {
        return;
    }
    let i = state.selected().map_or(0, |i| {
        if i >= len - 1 { 0 } else { i + 1 }
    });
    state.select(Some(i));
}

fn list_prev(state: &mut ListState, len: usize) {
    if len == 0 {
        return;
    }
    let i = state.selected().map_or(0, |i| {
        if i == 0 { len - 1 } else { i - 1 }
    });
    state.select(Some(i));
}
