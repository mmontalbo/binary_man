//! Scenario scaffold generation for behavior verification.
//!
//! Generates JSON scaffolds for new or modified behavior scenarios that the LM
//! can use as templates for creating test cases.

use crate::enrich;
use crate::scenarios;
use anyhow::{anyhow, Result};

use super::retry::preferred_behavior_scenario_id;
use super::LedgerEntries;

pub(super) const STARTER_SEED_PATH_PLACEHOLDER: &str = "work/item.txt";
pub(super) const STARTER_STDOUT_TOKEN_PLACEHOLDER: &str = "item.txt";
pub(super) const REQUIRED_VALUE_PLACEHOLDER: &str = "__value__";

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BehaviorMergePatchPayload {
    #[serde(default)]
    defaults: Option<serde_json::Value>,
    #[serde(default)]
    upsert_scenarios: Vec<scenarios::ScenarioSpec>,
}

#[derive(serde::Serialize)]
struct BehaviorScaffoldEditPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    defaults: Option<scenarios::ScenarioDefaults>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    upsert_scenarios: Vec<scenarios::ScenarioSpec>,
}

/// Build scaffold context with value_required hints for target IDs.
pub(super) fn build_scaffold_context(
    surface: Option<&crate::surface::SurfaceInventory>,
    target_ids: &[String],
    reason_code: Option<&str>,
) -> Option<enrich::ScaffoldContext> {
    let surface = surface?;
    let is_no_scenario = reason_code == Some("no_scenario");
    let is_outputs_equal = reason_code == Some("outputs_equal");

    if !is_no_scenario && !is_outputs_equal {
        return None;
    }

    let mut value_required = Vec::new();
    for target_id in target_ids {
        let Some(item) = crate::surface::primary_surface_item_by_id(surface, target_id) else {
            continue;
        };
        if item.invocation.value_arity == "required" {
            value_required.push(enrich::ValueRequiredHint {
                option_id: target_id.clone(),
                placeholder: item
                    .invocation
                    .value_placeholder
                    .clone()
                    .unwrap_or_else(|| "VALUE".to_string()),
                description: item.description.clone().unwrap_or_default(),
            });
        }
    }

    let has_value_required = !value_required.is_empty();
    let guidance = if has_value_required && is_outputs_equal {
        Some("Replace __value__ placeholders using examples from option descriptions; options with identical output may need companion flags (-l for details, -s for sizes) or behavior exclusion".to_string())
    } else if has_value_required {
        Some(
            "Replace __value__ placeholders using examples from option descriptions above"
                .to_string(),
        )
    } else if is_outputs_equal {
        Some("Options showing identical output may need companion flags (-l for long format, -s for sizes) or behavior exclusion with appropriate reason_code".to_string())
    } else {
        None
    };

    Some(enrich::ScaffoldContext {
        value_required,
        has_outputs_equal: is_outputs_equal,
        guidance,
    })
}

/// Build assertion starters for scenarios missing assertions.
pub(super) fn assertion_starters_for_missing_assertions(
    entry: Option<&scenarios::VerificationEntry>,
    include_full: bool,
) -> Vec<enrich::BehaviorAssertionStarter> {
    let seed_path = entry
        .and_then(|entry| entry.behavior_unverified_assertion_seed_path.clone())
        .map(|path| path.trim().to_string())
        .filter(|path| !path.is_empty());
    let mut starters = if let Some(seed_path) = seed_path {
        let stdout_token = entry
            .and_then(|entry| entry.behavior_unverified_assertion_token.clone())
            .map(|token| token.trim().to_string())
            .filter(|token| !token.is_empty())
            .or_else(|| seed_path.rsplit('/').next().map(str::to_string));
        vec![
            enrich::BehaviorAssertionStarter {
                kind: "baseline_stdout_not_contains_seed_path".to_string(),
                seed_path: Some(seed_path.clone()),
                stdout_token: stdout_token.clone(),
            },
            enrich::BehaviorAssertionStarter {
                kind: "variant_stdout_contains_seed_path".to_string(),
                seed_path: Some(seed_path.clone()),
                stdout_token: stdout_token.clone(),
            },
            enrich::BehaviorAssertionStarter {
                kind: "baseline_stdout_contains_seed_path".to_string(),
                seed_path: Some(seed_path.clone()),
                stdout_token: stdout_token.clone(),
            },
            enrich::BehaviorAssertionStarter {
                kind: "variant_stdout_not_contains_seed_path".to_string(),
                seed_path: Some(seed_path),
                stdout_token,
            },
        ]
    } else {
        vec![
            enrich::BehaviorAssertionStarter {
                kind: "baseline_stdout_not_contains_seed_path".to_string(),
                seed_path: Some(STARTER_SEED_PATH_PLACEHOLDER.to_string()),
                stdout_token: Some(STARTER_STDOUT_TOKEN_PLACEHOLDER.to_string()),
            },
            enrich::BehaviorAssertionStarter {
                kind: "variant_stdout_contains_seed_path".to_string(),
                seed_path: Some(STARTER_SEED_PATH_PLACEHOLDER.to_string()),
                stdout_token: Some(STARTER_STDOUT_TOKEN_PLACEHOLDER.to_string()),
            },
            enrich::BehaviorAssertionStarter {
                kind: "baseline_stdout_contains_seed_path".to_string(),
                seed_path: Some(STARTER_SEED_PATH_PLACEHOLDER.to_string()),
                stdout_token: Some(STARTER_STDOUT_TOKEN_PLACEHOLDER.to_string()),
            },
            enrich::BehaviorAssertionStarter {
                kind: "variant_stdout_not_contains_seed_path".to_string(),
                seed_path: Some(STARTER_SEED_PATH_PLACEHOLDER.to_string()),
                stdout_token: Some(STARTER_STDOUT_TOKEN_PLACEHOLDER.to_string()),
            },
        ]
    };
    if !include_full {
        starters.truncate(2);
    }
    starters
}

/// Render scaffold content as pretty-printed JSON.
pub(super) fn render_behavior_scaffold_content(
    defaults: Option<scenarios::ScenarioDefaults>,
    mut upsert_scenarios: Vec<scenarios::ScenarioSpec>,
) -> Option<String> {
    upsert_scenarios.sort_by(|a, b| a.id.cmp(&b.id));
    upsert_scenarios.dedup_by(|a, b| a.id == b.id);
    if defaults.is_none() && upsert_scenarios.is_empty() {
        return None;
    }
    let payload = BehaviorScaffoldEditPayload {
        defaults,
        upsert_scenarios,
    };
    serde_json::to_string_pretty(&payload).ok()
}

/// Merge defaults patch into an existing scenario plan.
fn merge_defaults_patch(
    plan: &mut scenarios::ScenarioPlan,
    defaults_patch: &serde_json::Value,
) -> Result<()> {
    let defaults_map = defaults_patch
        .as_object()
        .ok_or_else(|| anyhow!("merge payload defaults must be a JSON object"))?;
    let mut merged_defaults = serde_json::to_value(plan.defaults.clone().unwrap_or_default())
        .map_err(|err| anyhow!("serialize existing scenario defaults: {err}"))?;
    let merged_map = merged_defaults
        .as_object_mut()
        .ok_or_else(|| anyhow!("existing defaults must serialize as a JSON object"))?;
    for (key, value) in defaults_map {
        merged_map.insert(key.clone(), value.clone());
    }
    let parsed: scenarios::ScenarioDefaults = serde_json::from_value(merged_defaults)
        .map_err(|err| anyhow!("parse merged scenario defaults: {err}"))?;
    plan.defaults = Some(parsed);
    Ok(())
}

/// Project a behavior scaffold merge onto a scenario plan.
pub(super) fn project_behavior_scaffold_merge(
    plan: &scenarios::ScenarioPlan,
    doc_pack_root: &std::path::Path,
    content: &str,
) -> Result<scenarios::ScenarioPlan> {
    let payload: BehaviorMergePatchPayload = serde_json::from_str(content).map_err(|err| {
        anyhow!("parse status next_action.content as merge_behavior_scenarios payload: {err}")
    })?;
    if payload.defaults.is_none() && payload.upsert_scenarios.is_empty() {
        return Err(anyhow!(
            "merge payload must include defaults and/or upsert_scenarios"
        ));
    }
    let mut projected = plan.clone();
    if let Some(defaults_patch) = payload.defaults.as_ref() {
        merge_defaults_patch(&mut projected, defaults_patch)?;
    }
    for mut scenario in payload.upsert_scenarios {
        let scenario_id = scenario.id.trim();
        if scenario_id.is_empty() {
            return Err(anyhow!("upsert_scenarios[].id must not be empty"));
        }
        scenario.id = scenario_id.to_string();
        if let Some(existing) = projected
            .scenarios
            .iter_mut()
            .find(|existing| existing.id == scenario.id)
        {
            *existing = scenario;
        } else {
            projected.scenarios.push(scenario);
        }
    }
    scenarios::validate_plan(&projected, doc_pack_root)?;
    Ok(projected)
}

/// Check if content projects as a valid behavior merge.
pub(super) fn content_projects_as_valid_behavior_merge(
    plan: &scenarios::ScenarioPlan,
    doc_pack_root: &std::path::Path,
    content: &str,
) -> bool {
    project_behavior_scaffold_merge(plan, doc_pack_root, content).is_ok()
}

/// Find the first valid scaffold content from candidates.
pub(super) fn first_valid_scaffold_content<I>(
    plan: &scenarios::ScenarioPlan,
    doc_pack_root: &std::path::Path,
    candidates: I,
) -> Option<String>
where
    I: IntoIterator<Item = Option<String>>,
{
    for candidate in candidates {
        let Some(content) = candidate else {
            continue;
        };
        if content_projects_as_valid_behavior_merge(plan, doc_pack_root, &content) {
            return Some(content);
        }
    }
    None
}

/// Get behavior scenario for a surface ID from ledger entries.
fn behavior_scenario_for_surface_id<'a>(
    plan: &'a scenarios::ScenarioPlan,
    ledger_entries: &LedgerEntries,
    surface_id: &str,
) -> Option<&'a scenarios::ScenarioSpec> {
    let entry = ledger_entries.get(surface_id)?;
    let scenario_id = preferred_behavior_scenario_id(entry)?;
    plan.scenarios
        .iter()
        .find(|scenario| scenario.id == scenario_id)
}

/// Build scaffold from existing behavior scenarios for target IDs.
pub(super) fn build_existing_behavior_scenarios_scaffold(
    plan: &scenarios::ScenarioPlan,
    ledger_entries: &LedgerEntries,
    target_ids: &[String],
) -> Option<String> {
    let mut scenarios_by_id = std::collections::BTreeMap::new();
    for surface_id in super::normalize_target_ids(target_ids) {
        let Some(scenario) = behavior_scenario_for_surface_id(plan, ledger_entries, &surface_id)
        else {
            continue;
        };
        scenarios_by_id.insert(scenario.id.clone(), scenario.clone());
    }
    render_behavior_scaffold_content(None, scenarios_by_id.into_values().collect())
}

/// Create a minimal behavior baseline scenario.
fn minimal_behavior_baseline_scenario(id: &str) -> scenarios::ScenarioSpec {
    scenarios::ScenarioSpec {
        id: id.to_string(),
        kind: scenarios::ScenarioKind::Behavior,
        publish: false,
        argv: vec![scenarios::DEFAULT_BEHAVIOR_SEED_DIR.to_string()],
        env: std::collections::BTreeMap::new(),
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
    }
}

/// Add assertion starters to a scenario for a surface ID.
fn scenario_with_assertion_starters(
    plan: &scenarios::ScenarioPlan,
    ledger_entries: &LedgerEntries,
    surface_id: &str,
    baseline_by_id: &mut std::collections::BTreeMap<String, scenarios::ScenarioSpec>,
    upsert_by_id: &mut std::collections::BTreeMap<String, scenarios::ScenarioSpec>,
) {
    let Some(entry) = ledger_entries.get(surface_id) else {
        return;
    };
    let Some(scenario_id) = preferred_behavior_scenario_id(entry) else {
        return;
    };
    let Some(mut scenario) = plan
        .scenarios
        .iter()
        .find(|candidate| candidate.id == scenario_id)
        .cloned()
    else {
        return;
    };

    // Ensure scenario has a baseline
    let baseline_id = scenario
        .baseline_scenario_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .or_else(|| crate::status::verification::find_behavior_baseline_id(plan))
        .unwrap_or_else(|| "baseline".to_string());
    scenario.baseline_scenario_id = Some(baseline_id.clone());
    scenario.coverage_tier = Some("behavior".to_string());

    // Ensure baseline exists in the scaffold
    baseline_by_id.entry(baseline_id).or_insert_with_key(|id| {
        plan.scenarios
            .iter()
            .find(|candidate| candidate.id == *id)
            .cloned()
            .unwrap_or_else(|| minimal_behavior_baseline_scenario(id))
    });

    // Default to outputs_differ - simplest assertion that works for any option
    scenario.assertions = vec![scenarios::BehaviorAssertion::OutputsDiffer {}];
    upsert_by_id.insert(scenario.id.clone(), scenario);
}

/// Build scaffold content for scenarios missing assertions.
pub(super) fn build_missing_assertions_scaffold_content(
    plan: &scenarios::ScenarioPlan,
    ledger_entries: &LedgerEntries,
    target_ids: &[String],
) -> Option<String> {
    let mut baseline_by_id = std::collections::BTreeMap::new();
    let mut upsert_by_id = std::collections::BTreeMap::new();
    for surface_id in super::normalize_target_ids(target_ids) {
        scenario_with_assertion_starters(
            plan,
            ledger_entries,
            &surface_id,
            &mut baseline_by_id,
            &mut upsert_by_id,
        );
    }
    for baseline in baseline_by_id.into_values() {
        upsert_by_id.insert(baseline.id.clone(), baseline);
    }
    render_behavior_scaffold_content(None, upsert_by_id.into_values().collect())
}

/// Get the preferred required value token for a surface item.
pub(super) fn preferred_required_value_token(
    surface: &crate::surface::SurfaceInventory,
    surface_id: &str,
) -> String {
    let Some(item) = crate::surface::primary_surface_item_by_id(surface, surface_id) else {
        return REQUIRED_VALUE_PLACEHOLDER.to_string();
    };
    item.invocation
        .value_examples
        .iter()
        .map(|value| value.trim())
        .find(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            item.invocation
                .value_placeholder
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| REQUIRED_VALUE_PLACEHOLDER.to_string())
}

/// Build a hint for required value argv rewriting.
pub(super) fn required_value_argv_rewrite_hint(
    surface: &crate::surface::SurfaceInventory,
    surface_id: &str,
) -> String {
    let value_token = preferred_required_value_token(surface, surface_id);
    let separator = crate::surface::primary_surface_item_by_id(surface, surface_id)
        .map(|item| item.invocation.value_separator.as_str())
        .unwrap_or("unknown");
    let argv_fragment = match separator {
        "equals" => format!("{surface_id}={value_token}"),
        "space" => format!("{surface_id} {value_token}"),
        _ => format!("{surface_id}={value_token} or {surface_id} {value_token}"),
    };
    format!("scenario.argv should include `{argv_fragment}`")
}
