//! LM response application logic.
//!
//! Functions for invoking LM and applying its responses to doc packs.

use crate::enrich::{
    self, append_lm_log, next_cycle_number, store_lm_content, LmInvocationKind, LmLogBuilder,
};
use crate::scenarios;
use crate::surface;
use crate::workflow::lm_client::{invoke_lm_for_behavior, LmClientConfig};
use crate::workflow::lm_response::validate_responses;
use anyhow::{anyhow, Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

/// Invoke LM and apply its responses.
/// Returns (applied_count, updated_scenario_ids) where updated_scenario_ids
/// are scenarios that were modified and should be rerun.
pub(super) fn invoke_lm_and_apply(
    doc_pack_root: &Path,
    lm_config: &LmClientConfig,
    summary: &enrich::StatusSummary,
    payload: &enrich::BehaviorNextActionPayload,
    verbose: bool,
) -> Result<(usize, Vec<String>)> {
    let paths = enrich::DocPackPaths::new(doc_pack_root.to_path_buf());
    let cycle = next_cycle_number(&paths).unwrap_or(1);

    // Determine if this is a retry based on payload
    let is_retry = payload.retry_count.unwrap_or(0) > 0;
    let kind = if is_retry {
        LmInvocationKind::BehaviorRetry
    } else {
        LmInvocationKind::Behavior
    };

    // Build log entry
    let log_builder = LmLogBuilder::new(cycle, kind).with_items(payload.target_ids.clone());

    // Invoke LM
    let invocation = match invoke_lm_for_behavior(lm_config, summary, payload) {
        Ok(inv) => inv,
        Err(e) => {
            // Log failure
            let entry = log_builder.failed(e.to_string());
            let _ = append_lm_log(&paths, &entry);
            return Err(e);
        }
    };

    let batch = invocation.result;

    if verbose {
        eprintln!("apply: LM returned {} responses", batch.responses.len());
        // Store full prompt/response in verbose mode
        let _ = store_lm_content(
            &paths,
            cycle,
            kind,
            &invocation.prompt,
            &invocation.raw_response,
        );
    }

    // Load surface inventory for validation
    let paths = enrich::DocPackPaths::new(doc_pack_root.to_path_buf());
    let surface_path = paths.surface_path();
    if !surface_path.is_file() {
        return Err(anyhow!("surface.json not found"));
    }
    let surface_inventory: surface::SurfaceInventory =
        serde_json::from_str(&fs::read_to_string(&surface_path)?)?;
    let binary_name = surface_inventory
        .binary_name
        .clone()
        .unwrap_or_else(|| "<binary>".to_string());

    // Build verification ledger for validation
    let scenarios_path = paths.scenarios_plan_path();
    let template_path = paths
        .root()
        .join(enrich::VERIFICATION_FROM_SCENARIOS_TEMPLATE_REL);
    let ledger = scenarios::build_verification_ledger(
        &binary_name,
        &surface_inventory,
        paths.root(),
        &scenarios_path,
        &template_path,
        None,
        Some(paths.root()),
    )?;

    // Build set of valid unverified surface_ids
    let valid_surface_ids: BTreeSet<String> = ledger
        .entries
        .iter()
        .filter(|e| e.behavior_status != "verified" && e.behavior_status != "excluded")
        .map(|e| e.surface_id.clone())
        .collect();

    // Validate responses
    let (validated, result) = validate_responses(&batch, &valid_surface_ids);

    if verbose {
        eprintln!(
            "apply: validated {} responses ({} skipped, {} errors)",
            result.valid_count,
            result.skipped_count,
            result.errors.len()
        );
        for error in &result.errors {
            eprintln!("  error: {}: {}", error.surface_id, error.message);
        }
    }

    if result.valid_count == 0 {
        // Log partial/failed result
        let entry = if result.errors.is_empty() {
            log_builder
                .with_prompt_preview(&invocation.prompt)
                .success("no responses to apply")
        } else {
            log_builder.with_prompt_preview(&invocation.prompt).partial(
                0,
                result.errors.len(),
                format!("{} validation errors", result.errors.len()),
            )
        };
        let _ = append_lm_log(&paths, &entry);
        return Ok((0, Vec::new()));
    }

    // Apply scenarios
    let mut applied_count = 0;
    let mut updated_scenario_ids = Vec::new();
    if !validated.scenarios_to_upsert.is_empty() {
        let plan_path = paths.scenarios_plan_path();
        let mut plan = scenarios::load_plan(&plan_path, paths.root())?;

        // Ensure baseline scenario exists
        let needs_baseline = validated
            .scenarios_to_upsert
            .iter()
            .any(|s| s.baseline_scenario_id.as_deref() == Some("baseline"));
        let has_baseline = plan.scenarios.iter().any(|s| s.id == "baseline");

        if needs_baseline && !has_baseline {
            let baseline = scenarios::ScenarioSpec {
                id: "baseline".to_string(),
                kind: scenarios::ScenarioKind::Behavior,
                publish: false,
                argv: Vec::new(),
                env: BTreeMap::new(),
                stdin: None,
                seed: None,
                cwd: None,
                timeout_seconds: None,
                net_mode: None,
                no_sandbox: None,
                no_strace: None,
                snippet_max_lines: None,
                snippet_max_bytes: None,
                coverage_tier: Some("behavior".to_string()),
                baseline_scenario_id: None,
                assertions: Vec::new(),
                covers: Vec::new(),
                coverage_ignore: true,
                expect: scenarios::ScenarioExpect::default(),
            };
            plan.scenarios.push(baseline);
            if verbose {
                eprintln!("  added baseline scenario");
            }
        }

        for scenario in &validated.scenarios_to_upsert {
            if let Some(existing) = plan.scenarios.iter_mut().find(|s| s.id == scenario.id) {
                *existing = scenario.clone();
                if verbose {
                    eprintln!("  updated scenario: {}", scenario.id);
                }
            } else {
                plan.scenarios.push(scenario.clone());
                if verbose {
                    eprintln!("  added scenario: {}", scenario.id);
                }
            }
            updated_scenario_ids.push(scenario.id.clone());
            applied_count += 1;
        }

        let plan_json = serde_json::to_string_pretty(&plan)?;
        fs::write(&plan_path, plan_json.as_bytes())?;
    }

    // Apply overlays
    let has_overlays = !validated.value_examples.is_empty()
        || !validated.requires_argv.is_empty()
        || !validated.exclusions.is_empty();

    if has_overlays {
        let invalidated = apply_lm_overlays(&paths, &validated, &ledger)?;
        applied_count += validated.value_examples.len()
            + validated.requires_argv.len()
            + validated.exclusions.len();
        if verbose && !invalidated.is_empty() {
            eprintln!(
                "apply: {} surface(s) had overlay changes, deleted stale scenarios",
                invalidated.len()
            );
        }
    }

    // Log success
    let entry = if result.errors.is_empty() {
        log_builder
            .with_prompt_preview(&invocation.prompt)
            .success(format!(
                "{} responses applied ({} scenarios)",
                result.valid_count,
                validated.scenarios_to_upsert.len()
            ))
    } else {
        log_builder.with_prompt_preview(&invocation.prompt).partial(
            result.valid_count,
            result.errors.len(),
            format!(
                "{} applied, {} errors",
                result.valid_count,
                result.errors.len()
            ),
        )
    };
    let _ = append_lm_log(&paths, &entry);

    Ok((applied_count, updated_scenario_ids))
}

/// Apply an LM response file to the doc pack.
pub(super) fn apply_lm_response(doc_pack: &Path, lm_response_path: &Path) -> Result<()> {
    use crate::workflow::lm_response::load_lm_response;

    let doc_pack_root = crate::docpack::ensure_doc_pack_root(doc_pack, false)?;
    let paths = enrich::DocPackPaths::new(doc_pack_root);

    // Load LM response
    let batch = load_lm_response(lm_response_path)?;
    eprintln!(
        "lm-response: loaded {} responses from {}",
        batch.responses.len(),
        lm_response_path.display()
    );

    // Load surface inventory
    let surface_path = paths.surface_path();
    if !surface_path.is_file() {
        return Err(anyhow!(
            "surface.json not found; run `bman apply` first to generate surface inventory"
        ));
    }
    let surface_inventory: surface::SurfaceInventory =
        serde_json::from_str(&fs::read_to_string(&surface_path).context("read surface inventory")?)
            .context("parse surface inventory")?;
    let binary_name = surface_inventory
        .binary_name
        .clone()
        .unwrap_or_else(|| "<binary>".to_string());

    // Load scenarios plan
    let scenarios_path = paths.scenarios_plan_path();
    if !scenarios_path.is_file() {
        return Err(anyhow!(
            "scenarios/plan.json not found; run `bman apply` first"
        ));
    }

    // Build verification ledger on-the-fly
    let template_path = paths
        .root()
        .join(enrich::VERIFICATION_FROM_SCENARIOS_TEMPLATE_REL);
    let ledger = scenarios::build_verification_ledger(
        &binary_name,
        &surface_inventory,
        paths.root(),
        &scenarios_path,
        &template_path,
        None,
        Some(paths.root()),
    )
    .context("compute verification ledger for LM response validation")?;

    // Build set of valid unverified surface_ids
    let valid_surface_ids: BTreeSet<String> = ledger
        .entries
        .iter()
        .filter(|e| e.behavior_status != "verified" && e.behavior_status != "excluded")
        .map(|e| e.surface_id.clone())
        .collect();

    // Validate responses
    let (validated, result) = validate_responses(&batch, &valid_surface_ids);

    eprintln!(
        "lm-response: validated {} responses ({} skipped, {} errors)",
        result.valid_count,
        result.skipped_count,
        result.errors.len()
    );

    for error in &result.errors {
        eprintln!("  error: {}: {}", error.surface_id, error.message);
    }

    if result.valid_count == 0 {
        if result.errors.is_empty() {
            eprintln!("lm-response: no actionable responses to apply");
            return Ok(());
        }
        return Err(anyhow!(
            "all {} responses failed validation",
            result.errors.len()
        ));
    }

    // Apply scenarios to plan.json
    if !validated.scenarios_to_upsert.is_empty() {
        let plan_path = paths.scenarios_plan_path();
        let mut plan = scenarios::load_plan(&plan_path, paths.root())?;

        for scenario in &validated.scenarios_to_upsert {
            // Upsert: replace existing or add new
            if let Some(existing) = plan.scenarios.iter_mut().find(|s| s.id == scenario.id) {
                *existing = scenario.clone();
                eprintln!("  updated scenario: {}", scenario.id);
            } else {
                plan.scenarios.push(scenario.clone());
                eprintln!("  added scenario: {}", scenario.id);
            }
        }

        let plan_json = serde_json::to_string_pretty(&plan).context("serialize plan")?;
        fs::write(&plan_path, plan_json.as_bytes()).context("write plan.json")?;
        eprintln!(
            "lm-response: wrote {} scenario(s) to {}",
            validated.scenarios_to_upsert.len(),
            plan_path.display()
        );
    }

    // Apply assertion fixes to existing scenarios
    if !validated.assertion_fixes.is_empty() {
        let plan_path = paths.scenarios_plan_path();
        let mut plan = scenarios::load_plan(&plan_path, paths.root())?;

        for (scenario_id, assertions) in &validated.assertion_fixes {
            if let Some(scenario) = plan.scenarios.iter_mut().find(|s| s.id == *scenario_id) {
                scenario.assertions = assertions.clone();
                eprintln!("  fixed assertions in scenario: {}", scenario_id);
            } else {
                eprintln!(
                    "  warning: scenario {} not found for assertion fix",
                    scenario_id
                );
            }
        }

        let plan_json = serde_json::to_string_pretty(&plan).context("serialize plan")?;
        fs::write(&plan_path, plan_json.as_bytes()).context("write plan.json")?;
    }

    // Apply overlays (value_examples, requires_argv, exclusions)
    let has_overlays = !validated.value_examples.is_empty()
        || !validated.requires_argv.is_empty()
        || !validated.exclusions.is_empty();

    if has_overlays {
        let invalidated = apply_lm_overlays(&paths, &validated, &ledger)?;
        if !invalidated.is_empty() {
            eprintln!(
                "lm-response: {} surface(s) had overlay changes, deleted stale scenarios",
                invalidated.len()
            );
        }
    }

    Ok(())
}

/// Apply overlay changes from LM responses.
/// Returns surface IDs that had overlay changes requiring scenario regeneration.
pub(super) fn apply_lm_overlays(
    paths: &enrich::DocPackPaths,
    validated: &crate::workflow::lm_response::ValidatedResponses,
    ledger: &scenarios::VerificationLedger,
) -> Result<Vec<String>> {
    use crate::workflow::lm_response::ExclusionReasonCode;

    let overlays_path = paths.surface_overlays_path();

    // Load existing overlays or create new structure
    let mut overlays: serde_json::Value = if overlays_path.is_file() {
        serde_json::from_str(&fs::read_to_string(&overlays_path)?)?
    } else {
        serde_json::json!({
            "schema_version": 3,
            "items": [],
            "overlays": []
        })
    };

    let overlays_array = overlays["overlays"]
        .as_array_mut()
        .ok_or_else(|| anyhow!("overlays must be an array"))?;

    // Track surface IDs that need scenario regeneration
    let mut invalidated_surface_ids: Vec<String> = Vec::new();

    // Helper to find or create overlay for a surface_id
    let find_or_create_overlay = |arr: &mut Vec<serde_json::Value>, surface_id: &str| -> usize {
        if let Some(idx) = arr
            .iter()
            .position(|o| o["id"].as_str() == Some(surface_id))
        {
            idx
        } else {
            arr.push(serde_json::json!({
                "id": surface_id,
                "kind": "option",
                "invocation": {}
            }));
            arr.len() - 1
        }
    };

    // Apply value_examples (these change how scenarios should be generated)
    for (surface_id, examples) in &validated.value_examples {
        let idx = find_or_create_overlay(overlays_array, surface_id);
        let existing: Option<Vec<String>> = overlays_array[idx]["invocation"]["value_examples"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            });
        let new_values: Vec<&str> = examples.iter().map(|s| s.as_str()).collect();
        let changed = existing
            .as_ref()
            .map(|e| e.iter().map(|s| s.as_str()).collect::<Vec<_>>())
            != Some(new_values.clone());
        overlays_array[idx]["invocation"]["value_examples"] = serde_json::json!(examples);
        // Mark for regeneration if overlay changed
        if changed {
            invalidated_surface_ids.push(surface_id.to_string());
        }
        eprintln!("  added value_examples for {}: {:?}", surface_id, examples);
    }

    // Apply requires_argv (these change how scenarios should be generated)
    for (surface_id, argv) in &validated.requires_argv {
        let idx = find_or_create_overlay(overlays_array, surface_id);
        let existing: Option<Vec<String>> = overlays_array[idx]["invocation"]["requires_argv"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            });
        let new_values: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
        let changed = existing
            .as_ref()
            .map(|e| e.iter().map(|s| s.as_str()).collect::<Vec<_>>())
            != Some(new_values.clone());
        overlays_array[idx]["invocation"]["requires_argv"] = serde_json::json!(argv);
        // Mark for regeneration if overlay changed
        if changed {
            invalidated_surface_ids.push(surface_id.to_string());
        }
        eprintln!("  added requires_argv for {}: {:?}", surface_id, argv);
    }

    // Apply exclusions (only if we have delta evidence)
    for (surface_id, (reason_code, note)) in &validated.exclusions {
        // Get delta evidence from ledger for this surface_id
        let ledger_entry = ledger.entries.iter().find(|e| e.surface_id == *surface_id);
        let delta_variant_path = ledger_entry
            .and_then(|e| e.delta_evidence_paths.first().cloned())
            .filter(|p| !p.is_empty());

        // Skip exclusions without delta evidence - they need to run scenarios first
        let Some(delta_variant_path) = delta_variant_path else {
            eprintln!(
                "  deferred exclusion for {} (no delta evidence yet, will retry after scenario runs)",
                surface_id
            );
            continue;
        };

        let idx = find_or_create_overlay(overlays_array, surface_id);

        let reason_code_str = match reason_code {
            ExclusionReasonCode::FixtureGap => "fixture_gap",
            ExclusionReasonCode::AssertionGap => "assertion_gap",
            ExclusionReasonCode::Nondeterministic => "nondeterministic",
            ExclusionReasonCode::RequiresInteractiveTty => "requires_interactive_tty",
            ExclusionReasonCode::UnsafeSideEffects => "unsafe_side_effects",
            ExclusionReasonCode::BlocksIndefinitely => "blocks_indefinitely",
        };

        overlays_array[idx]["behavior_exclusion"] = serde_json::json!({
            "reason_code": reason_code_str,
            "note": note,
            "evidence": {
                "delta_variant_path": delta_variant_path
            }
        });
        eprintln!(
            "  added exclusion for {}: {} ({})",
            surface_id, reason_code_str, note
        );
    }

    // Write updated overlays
    let overlays_json = serde_json::to_string_pretty(&overlays)?;
    fs::write(&overlays_path, overlays_json.as_bytes())?;
    eprintln!("lm-response: wrote overlays to {}", overlays_path.display());

    // Delete scenarios covering invalidated surface IDs so they get regenerated
    if !invalidated_surface_ids.is_empty() {
        let plan_path = paths.scenarios_plan_path();
        if plan_path.is_file() {
            let mut plan = scenarios::load_plan(&plan_path, paths.root())?;
            let invalidated_set: BTreeSet<&str> =
                invalidated_surface_ids.iter().map(|s| s.as_str()).collect();
            let before_count = plan.scenarios.len();
            plan.scenarios.retain(|scenario| {
                let dominated = scenario
                    .covers
                    .iter()
                    .all(|covered| invalidated_set.contains(covered.as_str()));
                if dominated && !scenario.covers.is_empty() {
                    eprintln!(
                        "  removing scenario {} (overlay changed for {:?})",
                        scenario.id, scenario.covers
                    );
                    false
                } else {
                    true
                }
            });
            if plan.scenarios.len() != before_count {
                let plan_json = serde_json::to_string_pretty(&plan)?;
                fs::write(&plan_path, plan_json.as_bytes())?;
            }
        }
    }

    Ok(invalidated_surface_ids)
}
