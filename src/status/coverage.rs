use crate::scenarios;
use crate::semantics;
use crate::surface;
use std::collections::BTreeMap;

pub(crate) fn coverage_stub_from_plan(
    plan: &scenarios::ScenarioPlan,
    surface: &surface::SurfaceInventory,
    semantics: Option<&semantics::Semantics>,
    uncovered_ids: &[String],
) -> Option<String> {
    let target_id = uncovered_ids.first()?.trim();
    if target_id.is_empty() {
        return None;
    }
    let mut updated = plan.clone();
    let stub_id = coverage_stub_id(&updated);
    let target_kind = surface::primary_surface_item_by_id(surface, target_id)
        .map(|item| item.kind.as_str())
        .unwrap_or("option");
    let argv = coverage_stub_argv(target_id, target_kind, semantics);
    updated.scenarios.push(scenarios::ScenarioSpec {
        id: stub_id,
        kind: scenarios::ScenarioKind::Behavior,
        publish: false,
        argv,
        env: BTreeMap::new(),
        seed_dir: None,
        seed: None,
        cwd: None,
        timeout_seconds: None,
        net_mode: None,
        no_sandbox: None,
        no_strace: None,
        snippet_max_lines: None,
        snippet_max_bytes: None,
        coverage_tier: Some("acceptance".to_string()),
        baseline_scenario_id: None,
        assertions: Vec::new(),
        covers: vec![target_id.to_string()],
        coverage_ignore: false,
        expect: scenarios::ScenarioExpect::default(),
    });
    serde_json::to_string_pretty(&updated).ok()
}

fn coverage_stub_argv(
    target_id: &str,
    target_kind: &str,
    semantics: Option<&semantics::Semantics>,
) -> Vec<String> {
    let mut argv = Vec::new();
    if let Some(semantics) = semantics {
        let (prefix, suffix) = if target_kind == "option" {
            (
                &semantics.verification.option_existence_argv_prefix,
                &semantics.verification.option_existence_argv_suffix,
            )
        } else {
            (
                &semantics.verification.subcommand_existence_argv_prefix,
                &semantics.verification.subcommand_existence_argv_suffix,
            )
        };
        argv.extend(prefix.iter().cloned());
        argv.push(target_id.to_string());
        argv.extend(suffix.iter().cloned());
        return argv;
    }
    vec![target_id.to_string()]
}

fn coverage_stub_id(plan: &scenarios::ScenarioPlan) -> String {
    let base = "coverage-todo";
    if plan.scenarios.iter().all(|scenario| scenario.id != base) {
        return base.to_string();
    }
    let mut idx = 1;
    loop {
        let candidate = format!("{base}-{idx}");
        if plan
            .scenarios
            .iter()
            .all(|scenario| scenario.id != candidate)
        {
            return candidate;
        }
        idx += 1;
    }
}
