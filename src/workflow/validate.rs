//! Workflow validate step.
//!
//! Validation snapshots inputs into a lock so later steps can detect staleness.
use super::EnrichContext;
use crate::cli::ValidateArgs;
use crate::docpack::ensure_doc_pack_root;
use crate::enrich;
use crate::scenarios;
use crate::semantics;
use crate::surface;
use anyhow::{anyhow, Context, Result};
use std::collections::BTreeSet;
use std::fs;

/// Run the validate step and write `enrich/lock.json`.
pub fn run_validate(args: &ValidateArgs) -> Result<()> {
    let doc_pack_root = ensure_doc_pack_root(&args.doc_pack, false)?;
    let ctx = EnrichContext::load(doc_pack_root)?;
    ctx.require_config()?;
    enrich::validate_config(&ctx.config)?;
    let _semantics = semantics::load_semantics(ctx.paths.root())?;
    let _plan = scenarios::load_plan(&ctx.paths.scenarios_plan_path(), ctx.paths.root())?;
    validate_behavior_exclusions(&ctx.paths)?;
    let lock = enrich::build_lock(ctx.paths.root(), &ctx.config, ctx.binary_name())?;
    enrich::write_lock(ctx.paths.root(), &lock)?;
    if args.verbose {
        eprintln!("wrote {}", ctx.paths.lock_path().display());
    }
    Ok(())
}

fn validate_behavior_exclusions(paths: &enrich::DocPackPaths) -> Result<()> {
    let overlays_path = paths.surface_overlays_path();
    let overlays = surface::load_surface_overlays_if_exists(&overlays_path)?;
    let Some(overlays) = overlays else {
        return Ok(());
    };
    let exclusions = surface::collect_behavior_exclusions(&overlays);
    if exclusions.is_empty() {
        return Ok(());
    }

    let surface_path = paths.surface_path();
    if !surface_path.is_file() {
        return Err(anyhow!(
            "behavior exclusions require inventory/surface.json (missing {})",
            surface_path.display()
        ));
    }
    let surface_inventory = surface::load_surface_inventory(&surface_path)
        .with_context(|| format!("read {}", surface_path.display()))?;
    surface::validate_surface_inventory(&surface_inventory)
        .with_context(|| format!("validate {}", surface_path.display()))?;
    let option_ids: BTreeSet<String> = surface_inventory
        .items
        .iter()
        .filter(|item| item.kind == "option")
        .map(|item| item.id.trim())
        .filter(|id| !id.is_empty())
        .map(|id| id.to_string())
        .collect();

    let ledger_path = paths.root().join("verification_ledger.json");
    if !ledger_path.is_file() {
        return Err(anyhow!(
            "behavior exclusions require verification_ledger.json (missing {})",
            ledger_path.display()
        ));
    }
    let ledger_bytes =
        fs::read(&ledger_path).with_context(|| format!("read {}", ledger_path.display()))?;
    let ledger: scenarios::VerificationLedger = serde_json::from_slice(&ledger_bytes)
        .with_context(|| format!("parse {}", ledger_path.display()))?;
    let ledger_entries = ledger
        .entries
        .into_iter()
        .map(|entry| {
            (
                entry.surface_id,
                surface::BehaviorExclusionLedgerEntry {
                    delta_outcome: entry.delta_outcome,
                    delta_evidence_paths: entry.delta_evidence_paths,
                },
            )
        })
        .collect();

    let _validated = surface::validate_behavior_exclusions(
        &exclusions,
        &option_ids,
        &ledger_entries,
        "missing from verification_ledger.json entries",
        "requires delta_outcome evidence in verification_ledger.json",
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_doc_pack_root(name: &str) -> std::path::PathBuf {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("{name}-{}-{now}", std::process::id()));
        std::fs::create_dir_all(root.join("inventory")).expect("create inventory dir");
        root
    }

    #[test]
    fn validate_adapter_rejects_duplicate_behavior_exclusions() {
        let root = temp_doc_pack_root("bman-validate-dup");
        let paths = enrich::DocPackPaths::new(root.clone());

        let overlays = serde_json::json!({
            "schema_version": 3,
            "items": [],
            "overlays": [
                {
                    "kind": "option",
                    "id": "--color",
                    "invocation": {},
                    "behavior_exclusion": {
                        "reason_code": "assertion_gap",
                        "note": "first",
                        "evidence": {
                            "delta_variant_path": "inventory/scenarios/color-after-1.json"
                        }
                    }
                },
                {
                    "kind": "option",
                    "id": "--color",
                    "invocation": {},
                    "behavior_exclusion": {
                        "reason_code": "assertion_gap",
                        "note": "second",
                        "evidence": {
                            "delta_variant_path": "inventory/scenarios/color-after-2.json"
                        }
                    }
                }
            ]
        });
        std::fs::write(
            root.join("inventory").join("surface.overlays.json"),
            serde_json::to_vec_pretty(&overlays).expect("serialize overlays"),
        )
        .expect("write overlays");

        let surface_inventory = surface::SurfaceInventory {
            schema_version: 2,
            generated_at_epoch_ms: 0,
            binary_name: Some("ls".to_string()),
            inputs_hash: None,
            discovery: Vec::new(),
            items: vec![surface::SurfaceItem {
                kind: "option".to_string(),
                id: "--color".to_string(),
                display: "--color".to_string(),
                description: None,
                forms: Vec::new(),
                invocation: surface::SurfaceInvocation::default(),
                evidence: Vec::new(),
            }],
            blockers: Vec::new(),
        };
        std::fs::write(
            root.join("inventory").join("surface.json"),
            serde_json::to_vec_pretty(&surface_inventory).expect("serialize surface"),
        )
        .expect("write surface");

        let verification_ledger = scenarios::VerificationLedger {
            schema_version: 9,
            generated_at_epoch_ms: 0,
            binary_name: "ls".to_string(),
            scenarios_path: "scenarios/plan.json".to_string(),
            surface_path: "inventory/surface.json".to_string(),
            total_count: 1,
            verified_count: 1,
            unverified_count: 0,
            unverified_ids: Vec::new(),
            behavior_verified_count: 1,
            behavior_unverified_count: 0,
            behavior_unverified_ids: Vec::new(),
            excluded_count: 0,
            excluded: Vec::new(),
            entries: vec![scenarios::VerificationEntry {
                surface_id: "--color".to_string(),
                status: "verified".to_string(),
                behavior_status: "verified".to_string(),
                behavior_exclusion_reason_code: None,
                behavior_unverified_reason_code: None,
                behavior_unverified_scenario_id: None,
                behavior_unverified_assertion_kind: None,
                behavior_unverified_assertion_seed_path: None,
                behavior_unverified_assertion_token: None,
                scenario_ids: Vec::new(),
                scenario_paths: Vec::new(),
                behavior_scenario_ids: Vec::new(),
                behavior_assertion_scenario_ids: Vec::new(),
                behavior_scenario_paths: Vec::new(),
                delta_outcome: Some("not_applicable".to_string()),
                delta_evidence_paths: Vec::new(),
                evidence: Vec::new(),
            }],
            warnings: Vec::new(),
        };
        std::fs::write(
            root.join("verification_ledger.json"),
            serde_json::to_vec_pretty(&verification_ledger).expect("serialize ledger"),
        )
        .expect("write verification ledger");

        let result = validate_behavior_exclusions(&paths);
        let _ = std::fs::remove_dir_all(&root);
        let err = result.expect_err("workflow adapter should reject duplicates");
        assert!(err
            .to_string()
            .contains("duplicate behavior_exclusion entries for surface_id --color"));
    }
}
