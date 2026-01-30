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
    selection: [usize; 4],
    show_all: [bool; 4],
    message: Option<String>,
    show_help: bool,
}
