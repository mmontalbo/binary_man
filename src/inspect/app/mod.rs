mod actions;
mod state;
mod view;

use super::data::InspectData;
use super::Tab;
use crate::enrich;
use std::path::PathBuf;

pub(super) struct App {
    doc_pack_root: PathBuf,
    summary: enrich::StatusSummary,
    data: InspectData,
    tab: Tab,
    selection: [usize; 3],
    show_all: [bool; 3],
    message: Option<String>,
    show_help: bool,
    detail_view: bool,
    detail_scroll: u16,
    /// When true, browse tab has focus on preview pane (right side)
    browse_preview_focus: bool,
}
