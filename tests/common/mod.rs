//! Shared test infrastructure for integration tests.

use serde::Deserialize;
use std::env;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Instant;
use tempfile::TempDir;

/// Test fixture metadata loaded from fixture.json.
#[derive(Debug, Deserialize)]
pub struct FixtureConfig {
    pub binary: String,
    #[serde(default)]
    pub context: Vec<String>,
    /// Performance baselines (optional, for regression detection).
    #[serde(default)]
    pub baseline: Option<Baseline>,
}

/// Performance baseline for regression detection.
#[derive(Debug, Deserialize, Clone)]
pub struct Baseline {
    /// Maximum expected LM cycles to complete.
    pub max_lm_cycles: u32,
    /// Maximum expected scenarios to run.
    pub max_scenarios: u32,
}

/// Test fixture that can run bman with mock or real LM backend.
pub struct TestFixture {
    pub fixture_dir: PathBuf,
    pub config: FixtureConfig,
}

/// Result from running bman, parsed from `bman status --json` + metrics.
#[derive(Debug)]
pub struct TestResult {
    pub decision: String,
    pub behavior_verified_count: u32,
    pub behavior_unverified_count: u32,
    pub excluded_count: u32,
    pub is_stuck: bool,
    /// Excluded items preview for regression checks (used by git_config test).
    #[allow(dead_code)]
    pub excluded_items: Vec<String>,
    // Performance metrics
    pub lm_cycles: u32,
    pub scenarios_run: u32,
    pub elapsed_secs: f64,
}

impl TestResult {
    /// Check performance against baseline, panic if regression detected.
    pub fn assert_performance(&self, baseline: &Baseline) {
        assert!(
            self.lm_cycles <= baseline.max_lm_cycles,
            "LM cycle regression: {} cycles exceeds baseline of {}",
            self.lm_cycles,
            baseline.max_lm_cycles
        );
        assert!(
            self.scenarios_run <= baseline.max_scenarios,
            "Scenario count regression: {} scenarios exceeds baseline of {}",
            self.scenarios_run,
            baseline.max_scenarios
        );
    }
}

fn manifest_dir() -> PathBuf {
    PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into()))
}

#[allow(clippy::type_complexity)]
fn parse_status_json(
    json: &str,
) -> anyhow::Result<(String, u32, u32, u32, bool, Vec<String>, u32)> {
    let raw: serde_json::Value = serde_json::from_str(json)?;

    let verification = raw["requirements"]
        .as_array()
        .and_then(|arr| arr.iter().find(|r| r["id"] == "verification"));

    let (verified, unverified, excluded, excluded_items) = match verification {
        Some(req) => (
            req["behavior_verified_count"].as_u64().unwrap_or(0) as u32,
            req["behavior_unverified_count"].as_u64().unwrap_or(0) as u32,
            req["verification"]["excluded_count"].as_u64().unwrap_or(0) as u32,
            req["verification"]["behavior_excluded_preview"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
        ),
        None => (0, 0, 0, vec![]),
    };

    let is_stuck = raw["blockers"]
        .as_array()
        .map(|arr| {
            arr.iter().any(|b| {
                b["code"]
                    .as_str()
                    .map(|s| s.contains("stuck"))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false);

    // Count scenarios from plan requirements
    let scenarios_run = raw["plan"]["actions"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter(|a| a["kind"] == "scenario_runs")
                .filter_map(|a| a["scenario_count"].as_u64())
                .sum::<u64>() as u32
        })
        .unwrap_or(0);

    let decision = raw["decision"].as_str().unwrap_or("unknown").into();
    Ok((
        decision,
        verified,
        unverified,
        excluded,
        is_stuck,
        excluded_items,
        scenarios_run,
    ))
}

fn count_lm_cycles(doc_pack: &std::path::Path) -> u32 {
    let lm_log = doc_pack.join("enrich/lm_log.jsonl");
    if !lm_log.exists() {
        return 0;
    }
    std::fs::read_to_string(&lm_log)
        .map(|content| content.lines().count() as u32)
        .unwrap_or(0)
}

impl TestFixture {
    /// Load a fixture by name from tests/fixtures/{name}/.
    pub fn load(name: &str) -> anyhow::Result<Self> {
        let fixture_dir = manifest_dir().join("tests/fixtures").join(name);
        let config_path = fixture_dir.join("fixture.json");
        let config: FixtureConfig =
            serde_json::from_str(&std::fs::read_to_string(&config_path).map_err(|e| {
                anyhow::anyhow!("Failed to read {}: {}", config_path.display(), e)
            })?)?;
        Ok(Self {
            fixture_dir,
            config,
        })
    }

    /// Check if the binary is available; skip test if not.
    pub fn skip_if_binary_missing(&self) -> bool {
        let missing = Command::new(&self.config.binary)
            .arg("--help")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_err();
        if missing {
            eprintln!("Skipping: {} not available", self.config.binary);
        }
        missing
    }

    /// Check if mock responses exist in the fixture.
    pub fn has_mock_responses(&self) -> bool {
        self.fixture_dir.join("responses/001.txt").exists()
    }

    /// Run bman with this fixture and return the test result.
    pub fn run(&self) -> anyhow::Result<TestResult> {
        let temp_dir = TempDir::new()?;
        let doc_pack = temp_dir.path().join("doc-pack");

        // Set mock state dir for parallel test isolation
        env::set_var("BMAN_MOCK_STATE_DIR", temp_dir.path());

        // Resolve LM command: env var takes precedence, then mock script
        let lm_cmd = env::var("BMAN_LM_COMMAND").ok().or_else(|| {
            self.has_mock_responses().then(|| {
                let mock_script = manifest_dir().join("tests/mock-lm.sh");
                let abs_fixture = self.fixture_dir.canonicalize().expect("fixture dir exists");
                format!("{} {}", mock_script.display(), abs_fixture.display())
            })
        });
        let lm_cmd = lm_cmd.ok_or_else(|| {
            anyhow::anyhow!("No LM: set BMAN_LM_COMMAND or add responses/ to fixture")
        })?;
        env::set_var("BMAN_LM_COMMAND", &lm_cmd);

        let bman = manifest_dir().join("target/release/bman");
        if !bman.exists() {
            return Err(anyhow::anyhow!(
                "Release binary not found at {}. Run `cargo build --release` first.",
                bman.display()
            ));
        }

        // Run bman enrichment with timing
        let mut args = vec![
            "--doc-pack".into(),
            doc_pack.display().to_string(),
            "--max-cycles".into(),
            "50".into(),
            self.config.binary.clone(),
        ];
        args.extend(self.config.context.clone());

        let start = Instant::now();
        let output = Command::new(&bman).args(&args).output()?;
        let elapsed_secs = start.elapsed().as_secs_f64();

        if !output.status.success() {
            return Err(anyhow::anyhow!(
                "bman failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        // Count LM cycles from log
        let lm_cycles = count_lm_cycles(&doc_pack);

        // Get status JSON
        let status_output = Command::new(&bman)
            .args(["status", "--json", "--doc-pack"])
            .arg(&doc_pack)
            .output()?;

        let (decision, verified, unverified, excluded, is_stuck, excluded_items, scenarios_run) =
            parse_status_json(&String::from_utf8_lossy(&status_output.stdout))?;

        Ok(TestResult {
            decision,
            behavior_verified_count: verified,
            behavior_unverified_count: unverified,
            excluded_count: excluded,
            is_stuck,
            excluded_items,
            lm_cycles,
            scenarios_run,
            elapsed_secs,
        })
    }
}
