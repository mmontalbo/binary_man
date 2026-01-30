//! JSON schema for surface discovery outputs.
//!
//! Surface inventory stays pack-owned so SQL lenses remain the source of truth.
use crate::enrich;
use serde::{Deserialize, Serialize};

/// Inventory of discovered surface items.
#[derive(Serialize, Deserialize, Clone)]
pub struct SurfaceInventory {
    pub schema_version: u32,
    pub generated_at_epoch_ms: u128,
    pub binary_name: Option<String>,
    #[serde(default)]
    pub inputs_hash: Option<String>,
    pub discovery: Vec<SurfaceDiscovery>,
    pub items: Vec<SurfaceItem>,
    pub blockers: Vec<enrich::Blocker>,
}

/// Discovery event emitted by a surface lens.
#[derive(Serialize, Deserialize, Clone)]
pub struct SurfaceDiscovery {
    pub code: String,
    pub status: String,
    pub evidence: Vec<enrich::EvidenceRef>,
    pub message: Option<String>,
}

/// Single surface item (option, command, subcommand).
#[derive(Serialize, Deserialize, Clone)]
pub struct SurfaceItem {
    pub kind: String,
    pub id: String,
    pub display: String,
    #[serde(default)]
    pub description: Option<String>,
    pub evidence: Vec<enrich::EvidenceRef>,
}
