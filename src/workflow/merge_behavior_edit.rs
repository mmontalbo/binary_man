//! Workflow helper for applying status-provided behavior merge edits.
//!
//! This command removes manual JSON patch handling by validating a status
//! `next_action` and applying the merge contract directly to scenarios/plan.json.
use crate::cli::MergeBehaviorEditArgs;
use crate::docpack::ensure_doc_pack_root;
use crate::enrich;
use crate::scenarios;
use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::fs;
use std::io::Read;

const BEHAVIOR_SCENARIO_EDIT_STRATEGY: &str = "merge_behavior_scenarios";
const SCENARIOS_PLAN_REL_PATH: &str = "scenarios/plan.json";

#[derive(Debug, Deserialize)]
struct StatusEnvelope {
    next_action: StatusNextAction,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum StatusNextAction {
    Command {
        command: String,
        #[allow(dead_code)]
        reason: String,
    },
    Edit {
        path: String,
        content: String,
        #[allow(dead_code)]
        reason: String,
        #[serde(default = "enrich::default_edit_strategy")]
        edit_strategy: String,
    },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct BehaviorMergePatchPayload {
    #[serde(default)]
    defaults: Option<Value>,
    #[serde(default)]
    upsert_scenarios: Vec<scenarios::ScenarioSpec>,
}

#[derive(Debug)]
struct MergeEditAction {
    path: String,
    content: String,
    edit_strategy: String,
}

#[derive(Debug, Default)]
struct MergeOutcome {
    defaults_merged: bool,
    scenarios_inserted: usize,
    scenarios_updated: usize,
}

/// Apply a `merge_behavior_scenarios` status edit into `scenarios/plan.json`.
pub fn run_merge_behavior_edit(args: &MergeBehaviorEditArgs) -> Result<()> {
    let doc_pack_root = ensure_doc_pack_root(&args.doc_pack, false)?;
    let paths = enrich::DocPackPaths::new(doc_pack_root);

    let status_bytes = read_status_json_input(args)?;
    let action = extract_merge_edit_action(&status_bytes)?;
    validate_merge_edit_action(&action)?;

    let payload = parse_behavior_merge_payload(&action.content)?;
    let plan_path = paths.scenarios_plan_path();
    let mut plan = scenarios::load_plan(&plan_path, paths.root())
        .with_context(|| format!("load {}", plan_path.display()))?;
    let outcome = apply_behavior_merge_payload(&mut plan, payload)?;

    let serialized =
        serde_json::to_string_pretty(&plan).context("serialize merged scenario plan")?;
    fs::write(&plan_path, serialized.as_bytes())
        .with_context(|| format!("write {}", plan_path.display()))?;

    println!(
        "merged behavior edit into {} (defaults_merged={}, scenarios_inserted={}, scenarios_updated={})",
        plan_path.display(),
        outcome.defaults_merged,
        outcome.scenarios_inserted,
        outcome.scenarios_updated
    );
    Ok(())
}

fn validate_merge_edit_action(action: &MergeEditAction) -> Result<()> {
    if action.path != SCENARIOS_PLAN_REL_PATH {
        return Err(anyhow!(
            "status next_action.path must be {SCENARIOS_PLAN_REL_PATH} for merge helper (got {})",
            action.path
        ));
    }
    if action.edit_strategy != BEHAVIOR_SCENARIO_EDIT_STRATEGY {
        return Err(anyhow!(
            "status next_action.edit_strategy must be {BEHAVIOR_SCENARIO_EDIT_STRATEGY} for merge helper (got {})",
            action.edit_strategy
        ));
    }
    Ok(())
}

fn read_status_json_input(args: &MergeBehaviorEditArgs) -> Result<Vec<u8>> {
    if let Some(path) = args.status_json.as_ref() {
        return fs::read(path).with_context(|| format!("read status JSON {}", path.display()));
    }
    if !args.from_stdin {
        return Err(anyhow!(
            "provide --status-json <file> or --from-stdin for merge-behavior-edit"
        ));
    }

    let mut bytes = Vec::new();
    std::io::stdin()
        .read_to_end(&mut bytes)
        .context("read status JSON from stdin")?;
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Err(anyhow!("stdin status JSON is empty"));
    }
    Ok(bytes)
}

fn extract_merge_edit_action(status_bytes: &[u8]) -> Result<MergeEditAction> {
    let envelope: StatusEnvelope =
        serde_json::from_slice(status_bytes).context("parse status JSON input")?;
    match envelope.next_action {
        StatusNextAction::Command { command, .. } => Err(anyhow!(
            "status next_action.kind is command; run this first: {command}"
        )),
        StatusNextAction::Edit {
            path,
            content,
            edit_strategy,
            ..
        } => Ok(MergeEditAction {
            path,
            content,
            edit_strategy,
        }),
    }
}

fn parse_behavior_merge_payload(content: &str) -> Result<BehaviorMergePatchPayload> {
    if content.trim().is_empty() {
        return Err(anyhow!("status next_action.content is empty"));
    }
    let payload: BehaviorMergePatchPayload = serde_json::from_str(content)
        .context("parse status next_action.content as merge_behavior_scenarios payload")?;
    if payload.defaults.is_none() && payload.upsert_scenarios.is_empty() {
        return Err(anyhow!(
            "merge payload must include defaults and/or upsert_scenarios"
        ));
    }
    Ok(payload)
}

fn apply_behavior_merge_payload(
    plan: &mut scenarios::ScenarioPlan,
    payload: BehaviorMergePatchPayload,
) -> Result<MergeOutcome> {
    let mut outcome = MergeOutcome::default();

    if let Some(defaults_patch) = payload.defaults.as_ref() {
        outcome.defaults_merged = merge_defaults_patch(plan, defaults_patch)?;
    }

    for mut scenario in payload.upsert_scenarios {
        let scenario_id = scenario.id.trim();
        if scenario_id.is_empty() {
            return Err(anyhow!("upsert_scenarios[].id must not be empty"));
        }
        scenario.id = scenario_id.to_string();
        if let Some(existing) = plan
            .scenarios
            .iter_mut()
            .find(|existing| existing.id == scenario.id)
        {
            *existing = scenario;
            outcome.scenarios_updated += 1;
        } else {
            plan.scenarios.push(scenario);
            outcome.scenarios_inserted += 1;
        }
    }

    Ok(outcome)
}

fn merge_defaults_patch(
    plan: &mut scenarios::ScenarioPlan,
    defaults_patch: &Value,
) -> Result<bool> {
    let defaults_map = defaults_patch
        .as_object()
        .ok_or_else(|| anyhow!("merge payload defaults must be a JSON object"))?;
    if defaults_map.is_empty() {
        return Ok(false);
    }

    let parsed_defaults: scenarios::ScenarioDefaults =
        serde_json::from_value(defaults_patch.clone()).context("parse merge payload defaults")?;
    let defaults = plan
        .defaults
        .get_or_insert_with(scenarios::ScenarioDefaults::default);

    if defaults_map.contains_key("env") {
        defaults.env = parsed_defaults.env;
    }
    if defaults_map.contains_key("seed") {
        defaults.seed = parsed_defaults.seed;
    }
    if defaults_map.contains_key("seed_dir") {
        defaults.seed_dir = parsed_defaults.seed_dir;
    }
    if defaults_map.contains_key("cwd") {
        defaults.cwd = parsed_defaults.cwd;
    }
    if defaults_map.contains_key("timeout_seconds") {
        defaults.timeout_seconds = parsed_defaults.timeout_seconds;
    }
    if defaults_map.contains_key("net_mode") {
        defaults.net_mode = parsed_defaults.net_mode;
    }
    if defaults_map.contains_key("no_sandbox") {
        defaults.no_sandbox = parsed_defaults.no_sandbox;
    }
    if defaults_map.contains_key("no_strace") {
        defaults.no_strace = parsed_defaults.no_strace;
    }
    if defaults_map.contains_key("snippet_max_lines") {
        defaults.snippet_max_lines = parsed_defaults.snippet_max_lines;
    }
    if defaults_map.contains_key("snippet_max_bytes") {
        defaults.snippet_max_bytes = parsed_defaults.snippet_max_bytes;
    }

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::{
        apply_behavior_merge_payload, extract_merge_edit_action, parse_behavior_merge_payload,
        validate_merge_edit_action,
    };
    use crate::scenarios;
    use crate::templates;
    use serde_json::json;

    fn base_plan() -> scenarios::ScenarioPlan {
        serde_json::from_str(templates::SCENARIOS_PLAN_JSON).expect("parse scenario template")
    }

    fn parse_payload(value: serde_json::Value) -> super::BehaviorMergePatchPayload {
        serde_json::from_value(value).expect("parse payload")
    }

    fn help_scenario(id: &str, argv: &[&str]) -> scenarios::ScenarioSpec {
        serde_json::from_value(json!({
            "id": id,
            "kind": "help",
            "publish": false,
            "argv": argv,
            "coverage_ignore": true
        }))
        .expect("parse help scenario")
    }

    #[test]
    fn merge_payload_defaults_patch_preserves_existing_unpatched_fields() {
        let mut plan = base_plan();
        let payload = parse_payload(json!({
            "defaults": {
                "seed": {
                    "entries": [
                        { "path": "work", "kind": "dir" }
                    ]
                }
            }
        }));

        let outcome = apply_behavior_merge_payload(&mut plan, payload).expect("merge payload");
        assert!(outcome.defaults_merged);
        let defaults = plan.defaults.expect("defaults present");
        assert_eq!(defaults.seed_dir.as_deref(), Some("fixtures/empty"));
        assert!(defaults.seed.is_some());
    }

    #[test]
    fn merge_payload_upsert_updates_existing_and_inserts_new_scenarios() {
        let mut plan = base_plan();
        let payload = parse_payload(json!({
            "upsert_scenarios": [
                {
                    "id": "help--help",
                    "kind": "help",
                    "publish": false,
                    "argv": ["--help", "--all"],
                    "coverage_ignore": true
                },
                {
                    "id": "help--extra",
                    "kind": "help",
                    "publish": false,
                    "argv": ["--version"],
                    "coverage_ignore": true
                }
            ]
        }));

        let outcome = apply_behavior_merge_payload(&mut plan, payload).expect("merge payload");
        assert_eq!(outcome.scenarios_updated, 1);
        assert_eq!(outcome.scenarios_inserted, 1);

        let updated = plan
            .scenarios
            .iter()
            .find(|scenario| scenario.id == "help--help")
            .expect("existing scenario updated");
        assert_eq!(
            updated.argv,
            vec!["--help".to_string(), "--all".to_string()]
        );

        let inserted = plan
            .scenarios
            .iter()
            .find(|scenario| scenario.id == "help--extra")
            .expect("new scenario inserted");
        assert_eq!(inserted.argv, vec!["--version".to_string()]);
    }

    #[test]
    fn merge_payload_upsert_is_idempotent_for_plan_content() {
        let payload = parse_payload(json!({
            "upsert_scenarios": [
                {
                    "id": "help--extra",
                    "kind": "help",
                    "publish": false,
                    "argv": ["--version"],
                    "coverage_ignore": true
                }
            ]
        }));
        let mut plan = base_plan();

        apply_behavior_merge_payload(&mut plan, payload.clone()).expect("first merge");
        let first = serde_json::to_string_pretty(&plan).expect("serialize first plan");

        apply_behavior_merge_payload(&mut plan, payload).expect("second merge");
        let second = serde_json::to_string_pretty(&plan).expect("serialize second plan");

        assert_eq!(first, second);
    }

    #[test]
    fn parse_behavior_merge_payload_rejects_empty_content() {
        let err = parse_behavior_merge_payload("   ").expect_err("empty content rejected");
        assert!(err.to_string().contains("next_action.content is empty"));
    }

    #[test]
    fn parse_behavior_merge_payload_rejects_missing_defaults_and_upserts() {
        let err = parse_behavior_merge_payload("{}")
            .expect_err("missing defaults and upsert_scenarios rejected");
        assert!(err
            .to_string()
            .contains("merge payload must include defaults and/or upsert_scenarios"));
    }

    #[test]
    fn parse_behavior_merge_payload_accepts_status_stub_shape() {
        let scenario = help_scenario("help--extra", &["--version"]);
        let payload_json = json!({
            "defaults": {
                "seed": {
                    "entries": [
                        { "path": "work", "kind": "dir" }
                    ]
                }
            },
            "upsert_scenarios": [scenario]
        });
        let payload_text = serde_json::to_string(&payload_json).expect("serialize payload");
        let payload = parse_behavior_merge_payload(&payload_text).expect("parse payload");
        assert!(payload.defaults.is_some());
        assert_eq!(payload.upsert_scenarios.len(), 1);
    }

    #[test]
    fn extract_merge_edit_action_rejects_command_next_action_with_actionable_message() {
        let status = serde_json::json!({
            "next_action": {
                "kind": "command",
                "command": "bman apply --doc-pack .",
                "reason": "verification pending"
            }
        });
        let bytes = serde_json::to_vec(&status).expect("serialize status");
        let err = extract_merge_edit_action(&bytes).expect_err("command next_action rejected");
        assert!(err
            .to_string()
            .contains("run this first: bman apply --doc-pack ."));
    }

    #[test]
    fn validate_merge_edit_action_rejects_wrong_path_and_strategy() {
        let status = serde_json::json!({
            "next_action": {
                "kind": "edit",
                "path": "enrich/config.json",
                "content": "{}",
                "reason": "edit config",
                "edit_strategy": "replace_file"
            }
        });
        let bytes = serde_json::to_vec(&status).expect("serialize status");
        let action = extract_merge_edit_action(&bytes).expect("extract edit action");
        let err = validate_merge_edit_action(&action).expect_err("invalid action rejected");
        assert!(err.to_string().contains("scenarios/plan.json"));
    }

    #[test]
    fn validate_merge_edit_action_rejects_wrong_strategy_with_actionable_message() {
        let status = serde_json::json!({
            "next_action": {
                "kind": "edit",
                "path": "scenarios/plan.json",
                "content": "{}",
                "reason": "replace plan",
                "edit_strategy": "replace_file"
            }
        });
        let bytes = serde_json::to_vec(&status).expect("serialize status");
        let action = extract_merge_edit_action(&bytes).expect("extract edit action");
        let err = validate_merge_edit_action(&action).expect_err("wrong strategy rejected");
        assert!(err.to_string().contains("merge_behavior_scenarios"));
    }
}
