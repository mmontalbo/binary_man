//! JSON schema for surface discovery outputs.
//!
//! Surface inventory stays pack-owned so SQL lenses remain the source of truth.
use crate::enrich;
use serde::{Deserialize, Serialize};

fn default_value_arity() -> String {
    "unknown".to_string()
}

fn default_value_separator() -> String {
    "unknown".to_string()
}

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

/// Invocation hints for a surface item.
#[derive(Serialize, Deserialize, Clone)]
pub struct SurfaceInvocation {
    #[serde(default = "default_value_arity")]
    pub value_arity: String,
    #[serde(default = "default_value_separator")]
    pub value_separator: String,
    #[serde(default)]
    pub value_placeholder: Option<String>,
    #[serde(default)]
    pub value_examples: Vec<String>,
    #[serde(default)]
    pub requires_argv: Vec<String>,
}

impl Default for SurfaceInvocation {
    fn default() -> Self {
        Self {
            value_arity: default_value_arity(),
            value_separator: default_value_separator(),
            value_placeholder: None,
            value_examples: Vec::new(),
            requires_argv: Vec::new(),
        }
    }
}

/// Single surface item discovered from help output.
#[derive(Serialize, Deserialize, Clone)]
pub struct SurfaceItem {
    pub id: String,
    pub display: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_argv: Vec<String>,
    #[serde(default)]
    pub forms: Vec<String>,
    #[serde(default)]
    pub invocation: SurfaceInvocation,
    pub evidence: Vec<enrich::EvidenceRef>,
}
