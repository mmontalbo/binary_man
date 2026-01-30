use crate::scenarios;
use std::collections::BTreeMap;

pub(crate) fn coverage_stub_from_plan(
    plan: &scenarios::ScenarioPlan,
    uncovered_ids: &[String],
) -> Option<String> {
    let target_id = uncovered_ids.first()?.trim();
    if target_id.is_empty() {
        return None;
    }
    let mut updated = plan.clone();
    let stub_id = coverage_stub_id(&updated);
    let argv = if target_id.starts_with('-') {
        vec![target_id.to_string()]
    } else {
        vec![target_id.to_string(), "--help".to_string()]
    };
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
        covers: vec![target_id.to_string()],
        coverage_ignore: false,
        expect: scenarios::ScenarioExpect::default(),
    });
    serde_json::to_string_pretty(&updated).ok()
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
