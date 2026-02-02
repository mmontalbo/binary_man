use super::seed::normalize_seed_path;
use super::{
    ScenarioExpect, ScenarioPlan, ScenarioSpec, DEFAULT_SNIPPET_MAX_BYTES,
    DEFAULT_SNIPPET_MAX_LINES,
};
use crate::util::sha256_hex;
use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::BTreeMap;

pub(super) struct ScenarioRunConfig {
    pub(super) env: BTreeMap<String, String>,
    pub(super) seed: Option<super::ScenarioSeedSpec>,
    pub(super) seed_dir: Option<String>,
    pub(super) cwd: Option<String>,
    pub(super) timeout_seconds: Option<f64>,
    pub(super) net_mode: Option<String>,
    pub(super) no_sandbox: Option<bool>,
    pub(super) no_strace: Option<bool>,
    pub(super) snippet_max_lines: usize,
    pub(super) snippet_max_bytes: usize,
    pub(super) scenario_digest: String,
}

#[derive(Serialize)]
struct ScenarioSeedEntryDigest {
    path: String,
    kind: super::SeedEntryKind,
    contents: Option<String>,
    target: Option<String>,
    mode: Option<u32>,
}

#[derive(Serialize)]
struct ScenarioSeedSpecDigest {
    entries: Vec<ScenarioSeedEntryDigest>,
}

#[derive(Serialize)]
struct ScenarioDigestInput {
    argv: Vec<String>,
    expect: ScenarioExpect,
    seed_dir: Option<String>,
    seed: Option<ScenarioSeedSpecDigest>,
    cwd: Option<String>,
    timeout_seconds: Option<f64>,
    net_mode: Option<String>,
    no_sandbox: Option<bool>,
    no_strace: Option<bool>,
    snippet_max_lines: usize,
    snippet_max_bytes: usize,
    env: BTreeMap<String, String>,
}

pub(super) fn effective_scenario_config(
    plan: &ScenarioPlan,
    scenario: &ScenarioSpec,
) -> Result<ScenarioRunConfig> {
    let defaults = plan.defaults.as_ref();

    let mut env = plan.default_env.clone();
    if let Some(defaults) = defaults {
        env = merge_env(&env, &defaults.env);
    }
    env = merge_env(&env, &scenario.env);

    let seed = if scenario.seed.is_some() {
        scenario.seed.clone()
    } else if scenario.seed_dir.is_some() {
        None
    } else {
        defaults.and_then(|value| value.seed.clone())
    };
    let seed_dir = if seed.is_some() {
        None
    } else {
        scenario
            .seed_dir
            .clone()
            .or_else(|| defaults.and_then(|value| value.seed_dir.clone()))
    };

    let cwd = scenario
        .cwd
        .clone()
        .or_else(|| defaults.and_then(|value| value.cwd.clone()));
    let timeout_seconds = scenario
        .timeout_seconds
        .or_else(|| defaults.and_then(|value| value.timeout_seconds));
    let net_mode = scenario
        .net_mode
        .clone()
        .or_else(|| defaults.and_then(|value| value.net_mode.clone()));
    let no_sandbox = scenario
        .no_sandbox
        .or_else(|| defaults.and_then(|value| value.no_sandbox));
    let no_strace = scenario
        .no_strace
        .or_else(|| defaults.and_then(|value| value.no_strace));
    let snippet_max_lines = scenario
        .snippet_max_lines
        .or_else(|| defaults.and_then(|value| value.snippet_max_lines))
        .unwrap_or(DEFAULT_SNIPPET_MAX_LINES);
    let snippet_max_bytes = scenario
        .snippet_max_bytes
        .or_else(|| defaults.and_then(|value| value.snippet_max_bytes))
        .unwrap_or(DEFAULT_SNIPPET_MAX_BYTES);

    let scenario_digest = scenario_digest(&ScenarioDigestArgs {
        scenario,
        env: &env,
        seed: seed.as_ref(),
        seed_dir: seed_dir.as_deref(),
        cwd: cwd.as_deref(),
        timeout_seconds,
        net_mode: net_mode.as_deref(),
        no_sandbox,
        no_strace,
        snippet_max_lines,
        snippet_max_bytes,
    })?;

    Ok(ScenarioRunConfig {
        env,
        seed,
        seed_dir,
        cwd,
        timeout_seconds,
        net_mode,
        no_sandbox,
        no_strace,
        snippet_max_lines,
        snippet_max_bytes,
        scenario_digest,
    })
}

struct ScenarioDigestArgs<'a> {
    scenario: &'a ScenarioSpec,
    env: &'a BTreeMap<String, String>,
    seed: Option<&'a super::ScenarioSeedSpec>,
    seed_dir: Option<&'a str>,
    cwd: Option<&'a str>,
    timeout_seconds: Option<f64>,
    net_mode: Option<&'a str>,
    no_sandbox: Option<bool>,
    no_strace: Option<bool>,
    snippet_max_lines: usize,
    snippet_max_bytes: usize,
}

fn scenario_digest(args: &ScenarioDigestArgs<'_>) -> Result<String> {
    let scenario = args.scenario;
    let env = args.env;
    let seed = args.seed;
    let seed_dir = args.seed_dir;
    let cwd = args.cwd;
    let timeout_seconds = args.timeout_seconds;
    let net_mode = args.net_mode;
    let no_sandbox = args.no_sandbox;
    let no_strace = args.no_strace;
    let snippet_max_lines = args.snippet_max_lines;
    let snippet_max_bytes = args.snippet_max_bytes;
    let seed = if let Some(seed) = seed {
        let mut entries: Vec<ScenarioSeedEntryDigest> = seed
            .entries
            .iter()
            .map(|entry| {
                let path = normalize_seed_path(&entry.path)
                    .with_context(|| format!("seed entry path {:?}", entry.path))?;
                let target = match entry.target.as_ref() {
                    Some(target) => Some(
                        normalize_seed_path(target)
                            .with_context(|| format!("seed entry target {:?}", target))?,
                    ),
                    None => None,
                };
                Ok(ScenarioSeedEntryDigest {
                    path,
                    kind: entry.kind,
                    contents: match entry.kind {
                        super::SeedEntryKind::File => {
                            Some(entry.contents.clone().unwrap_or_default())
                        }
                        _ => entry.contents.clone(),
                    },
                    target,
                    mode: entry.mode,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        Some(ScenarioSeedSpecDigest { entries })
    } else {
        None
    };

    let payload = ScenarioDigestInput {
        argv: scenario.argv.clone(),
        expect: scenario.expect.clone(),
        seed_dir: seed_dir.map(|value| value.to_string()),
        seed,
        cwd: cwd.map(|value| value.to_string()),
        timeout_seconds,
        net_mode: net_mode.map(|value| value.to_string()),
        no_sandbox,
        no_strace,
        snippet_max_lines,
        snippet_max_bytes,
        env: env.clone(),
    };
    let bytes = serde_json::to_vec(&payload).context("serialize scenario digest input")?;
    Ok(sha256_hex(&bytes))
}

fn merge_env(
    defaults: &BTreeMap<String, String>,
    overrides: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut merged = defaults.clone();
    for (key, value) in overrides {
        merged.insert(key.clone(), value.clone());
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenarios::{
        plan_stub, ScenarioDefaults, ScenarioKind, VerificationPlan, VerificationTargetKind,
    };
    use std::collections::BTreeMap;

    fn base_expect() -> ScenarioExpect {
        ScenarioExpect {
            exit_code: Some(0),
            exit_signal: None,
            stdout_contains_all: Vec::new(),
            stdout_contains_any: Vec::new(),
            stdout_regex_all: Vec::new(),
            stdout_regex_any: Vec::new(),
            stderr_contains_all: Vec::new(),
            stderr_contains_any: Vec::new(),
            stderr_regex_all: Vec::new(),
            stderr_regex_any: Vec::new(),
        }
    }

    fn base_scenario() -> ScenarioSpec {
        ScenarioSpec {
            id: "scenario".to_string(),
            kind: ScenarioKind::Behavior,
            publish: false,
            argv: vec!["--help".to_string()],
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
            coverage_tier: None,
            baseline_scenario_id: None,
            assertions: Vec::new(),
            covers: Vec::new(),
            coverage_ignore: true,
            expect: base_expect(),
        }
    }

    fn plan_with(scenarios: Vec<ScenarioSpec>, defaults: Option<ScenarioDefaults>) -> ScenarioPlan {
        ScenarioPlan {
            schema_version: super::super::SCENARIO_PLAN_SCHEMA_VERSION,
            binary: None,
            default_env: BTreeMap::new(),
            defaults,
            coverage: None,
            verification: VerificationPlan::default(),
            scenarios,
        }
    }

    #[test]
    fn scenario_digest_stable_and_sensitive_to_env() {
        let scenario = base_scenario();
        let plan = plan_with(vec![scenario.clone()], None);
        let first = effective_scenario_config(&plan, &scenario).unwrap();
        let second = effective_scenario_config(&plan, &scenario).unwrap();
        assert_eq!(first.scenario_digest, second.scenario_digest);

        let mut scenario_changed = scenario;
        scenario_changed
            .env
            .insert("NO_COLOR".to_string(), "0".to_string());
        let changed = effective_scenario_config(&plan, &scenario_changed).unwrap();
        assert_ne!(first.scenario_digest, changed.scenario_digest);
    }

    #[test]
    fn defaults_merge_and_env_precedence() {
        let mut default_env = BTreeMap::new();
        default_env.insert("LANG".to_string(), "C".to_string());
        let mut defaults_env = BTreeMap::new();
        defaults_env.insert("LANG".to_string(), "C.UTF-8".to_string());
        let defaults = ScenarioDefaults {
            env: defaults_env,
            seed: None,
            seed_dir: Some("fixtures".to_string()),
            cwd: Some("work".to_string()),
            timeout_seconds: Some(3.0),
            net_mode: Some("off".to_string()),
            no_sandbox: Some(false),
            no_strace: Some(true),
            snippet_max_lines: Some(7),
            snippet_max_bytes: Some(77),
        };

        let mut scenario = base_scenario();
        scenario.timeout_seconds = Some(5.0);
        scenario.snippet_max_lines = Some(11);
        let mut plan = plan_with(vec![scenario.clone()], Some(defaults));
        plan.default_env = default_env;

        let config = effective_scenario_config(&plan, &scenario).unwrap();
        assert_eq!(config.timeout_seconds, Some(5.0));
        assert_eq!(config.net_mode.as_deref(), Some("off"));
        assert_eq!(config.no_sandbox, Some(false));
        assert_eq!(config.no_strace, Some(true));
        assert_eq!(config.snippet_max_lines, 11);
        assert_eq!(config.snippet_max_bytes, 77);
        assert_eq!(config.cwd.as_deref(), Some("work"));
        assert_eq!(config.seed_dir.as_deref(), Some("fixtures"));
        assert_eq!(config.env.get("LANG").map(String::as_str), Some("C.UTF-8"));

        scenario.env.insert("LANG".to_string(), "POSIX".to_string());
        let config_override = effective_scenario_config(&plan, &scenario).unwrap();
        assert_eq!(
            config_override.env.get("LANG").map(String::as_str),
            Some("POSIX")
        );
    }

    #[test]
    fn env_defaults_are_plan_owned() {
        let scenario = base_scenario();
        let mut plan = plan_with(vec![scenario.clone()], None);
        let config = effective_scenario_config(&plan, &scenario).unwrap();
        assert!(!config.env.contains_key("LC_ALL"));
        assert!(!config.env.contains_key("LANG"));

        plan.default_env
            .insert("LC_ALL".to_string(), "C".to_string());
        let config = effective_scenario_config(&plan, &scenario).unwrap();
        assert_eq!(config.env.get("LC_ALL").map(String::as_str), Some("C"));
    }

    fn assert_plan_stub_env(plan: &ScenarioPlan) {
        assert_eq!(
            plan.schema_version,
            super::super::SCENARIO_PLAN_SCHEMA_VERSION
        );
        assert_eq!(plan.binary.as_deref(), Some("tool"));
        let expected_env = [
            ("LC_ALL", "C"),
            ("LANG", "C"),
            ("TERM", "dumb"),
            ("NO_COLOR", "1"),
            ("PAGER", "cat"),
            ("GIT_PAGER", "cat"),
        ];
        for (key, value) in expected_env {
            assert_eq!(plan.default_env.get(key).map(String::as_str), Some(value));
        }
    }

    fn assert_plan_stub_defaults(plan: &ScenarioPlan) {
        let defaults = plan.defaults.as_ref().expect("defaults");
        assert_eq!(defaults.seed_dir.as_deref(), Some("fixtures/empty"));
        assert_eq!(defaults.cwd.as_deref(), Some("."));
        assert_eq!(defaults.timeout_seconds, Some(3.0));
        assert_eq!(defaults.net_mode.as_deref(), Some("off"));
        assert_eq!(defaults.no_sandbox, Some(false));
        assert_eq!(defaults.no_strace, Some(true));
        assert_eq!(defaults.snippet_max_lines, Some(12));
        assert_eq!(defaults.snippet_max_bytes, Some(1024));
        assert!(plan.verification.queue.is_empty());
        let policy = plan.verification.policy.as_ref().expect("policy");
        assert_eq!(policy.kinds, vec![VerificationTargetKind::Option]);
        assert_eq!(policy.max_new_runs_per_apply, 10);
    }

    fn assert_plan_stub_help_scenarios(plan: &ScenarioPlan) {
        let expected = [
            ("help--help", "--help"),
            ("help--usage", "--usage"),
            ("help--question", "-?"),
        ];
        let ids: Vec<&str> = plan
            .scenarios
            .iter()
            .map(|scenario| scenario.id.as_str())
            .collect();
        assert_eq!(ids, expected.iter().map(|(id, _)| *id).collect::<Vec<_>>());

        for (scenario, (expected_id, expected_arg)) in plan.scenarios.iter().zip(expected.iter()) {
            assert_eq!(scenario.id, *expected_id);
            assert_eq!(scenario.kind, ScenarioKind::Help);
            assert!(!scenario.publish);
            assert!(scenario.coverage_ignore);
            assert_eq!(scenario.argv, vec![(*expected_arg).to_string()]);
            assert!(scenario.timeout_seconds.is_none());
            assert!(scenario.net_mode.is_none());
            assert!(scenario.no_sandbox.is_none());
            assert!(scenario.no_strace.is_none());
            assert!(scenario.snippet_max_lines.is_none());
            assert!(scenario.snippet_max_bytes.is_none());
            assert_eq!(scenario.expect, ScenarioExpect::default());
        }
    }

    #[test]
    fn plan_stub_includes_multiple_help_scenarios() {
        let plan: ScenarioPlan = serde_json::from_str(&plan_stub(Some("tool"))).unwrap();
        assert_plan_stub_env(&plan);
        assert_plan_stub_defaults(&plan);
        assert_plan_stub_help_scenarios(&plan);
    }
}
