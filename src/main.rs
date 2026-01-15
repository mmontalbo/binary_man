//! Iterative invocation runner entrypoint.

mod binary;
mod contract;
mod evidence;
mod fixture;
mod hashing;
mod invocation;
mod lm;
mod limits;
mod paths;
mod runner;
mod scenario;
mod transcript;

use anyhow::{Context, Result};
use clap::Parser;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::binary::{hash_binary, resolve_binary, resolve_binary_input, BinaryTarget};
use crate::contract::{env_contract, EnvContract};
use crate::evidence::{
    create_evidence_dir, write_meta, ArtifactsMeta, BinaryMeta, ErrorReport, FixtureMeta, Meta,
    Outcome, ResultMeta, SandboxMeta, TOOL_VERSION,
};
use crate::fixture::{fixture_root, load_fixture_catalog, prepare_fixture, validate_fixture};
use crate::hashing::sha256_hex;
use crate::lm::{capture_help, load_lm_command, load_text, run_lm};
use crate::invocation::{
    build_invocation_prompt, invocation_schema_path, parse_invocation_response,
    scenario_for_invocation, summarize_output, validate_invocation, InvocationEnvelope,
    InvocationFeedback, InvocationStatus, MAX_ITERATION_ROUNDS, INVOCATION_FIXTURE_ID,
};
use crate::runner::{run_direct, run_sandboxed};
use crate::scenario::{validate_scenario, Scenario};
use crate::transcript::Transcript;

const DEFAULT_OUT_DIR: &str = "out";
const FIXTURES_DIR: &str = "fixtures";

/// CLI arguments for the iterative runner.
#[derive(Parser, Debug)]
#[command(
    name = "bman",
    version,
    about = "Iteratively invoke a binary in a sandbox"
)]
struct Args {
    /// Binary name or path to inspect
    binary: String,

    /// Output directory root (evidence written under <dir>/evidence)
    #[arg(long, value_name = "DIR", default_value = DEFAULT_OUT_DIR)]
    out_dir: PathBuf,

    /// Validate the scenario JSON without executing
    #[arg(long)]
    dry_run: bool,

    /// Run without bwrap (dev/debug only)
    #[arg(long)]
    direct: bool,

    /// Emit a verbose transcript of the workflow
    #[arg(long)]
    verbose: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    run(args)
}

fn run(args: Args) -> Result<()> {
    run_iterate(args)
}

/// Execute iterative single-invocation rounds and emit evidence per run.
fn run_iterate(args: Args) -> Result<()> {
    let env = env_contract();
    let repo_root = std::env::current_dir().context("resolve repo root")?;
    let mut transcript = Transcript::new(args.verbose);
    transcript.note(format!(
        "start binary_input={} dry_run={} direct={} iterate=true",
        args.binary, args.dry_run, args.direct
    ));

    let target_binary = match resolve_binary_input(&args.binary) {
        Ok(target) => target,
        Err(err) => {
            transcript.note(format!("resolve_target failed: {err}"));
            let evidence_dir = record_early_failure(
                &args.out_dir,
                &env,
                "binary_target_invalid",
                "target binary invalid".to_string(),
                vec![err.to_string()],
                None,
            )?;
            transcript.note(format!("evidence_dir {}", evidence_dir.display()));
            return Ok(());
        }
    };
    transcript.note(format!(
        "resolve_target exec_path={} resolved_path={}",
        target_binary.exec_path.display(),
        target_binary.resolved_path.display()
    ));

    let help_capture = match capture_help(&target_binary.exec_path) {
        Ok(capture) => capture,
        Err(err) => {
            transcript.note(format!("capture_help failed: {err}"));
            let evidence_dir = record_early_failure(
                &args.out_dir,
                &env,
                "help_failed",
                "failed to capture help text".to_string(),
                vec![err.to_string()],
                None,
            )?;
            transcript.note(format!("evidence_dir {}", evidence_dir.display()));
            return Ok(());
        }
    };
    transcript.note(format!(
        "capture_help flag={} source={} bytes={}",
        help_capture.flag,
        help_capture.source,
        help_capture.bytes.len()
    ));

    let schema_text = match load_text(&invocation_schema_path(&repo_root)) {
        Ok(text) => text,
        Err(err) => {
            transcript.note(format!("load invocation schema failed: {err}"));
            let evidence_dir = record_early_failure(
                &args.out_dir,
                &env,
                "schema_asset_missing",
                "failed to load invocation schema".to_string(),
                vec![err.to_string()],
                None,
            )?;
            transcript.note(format!("evidence_dir {}", evidence_dir.display()));
            return Ok(());
        }
    };
    transcript.note(format!(
        "load_assets invocation_schema_bytes={}",
        schema_text.len()
    ));

    let fixtures_root = repo_root.join(FIXTURES_DIR);
    let fixture_catalog = match load_fixture_catalog(&fixtures_root) {
        Ok(catalog) => catalog,
        Err(err) => {
            transcript.note(format!("load_fixture_catalog failed: {err}"));
            let evidence_dir = record_early_failure(
                &args.out_dir,
                &env,
                "fixture_catalog_invalid",
                "fixture catalog invalid".to_string(),
                vec![err.to_string()],
                None,
            )?;
            transcript.note(format!("evidence_dir {}", evidence_dir.display()));
            return Ok(());
        }
    };
    if !fixture_catalog.contains(INVOCATION_FIXTURE_ID) {
        transcript.note(format!("fixture_not_allowed id={}", INVOCATION_FIXTURE_ID));
        let evidence_dir = record_early_failure(
            &args.out_dir,
            &env,
            "fixture_not_allowed",
            "fixture id not in catalog".to_string(),
            vec![INVOCATION_FIXTURE_ID.to_string()],
            None,
        )?;
        transcript.note(format!("evidence_dir {}", evidence_dir.display()));
        return Ok(());
    }
    let fixture_dir = match fixture_root(&fixtures_root, INVOCATION_FIXTURE_ID) {
        Ok(path) => path,
        Err(err) => {
            transcript.note(format!("fixture_root failed: {err}"));
            let evidence_dir = record_early_failure(
                &args.out_dir,
                &env,
                "fixture_invalid",
                format!("fixture id invalid: {err}"),
                Vec::new(),
                None,
            )?;
            transcript.note(format!("evidence_dir {}", evidence_dir.display()));
            return Ok(());
        }
    };
    transcript.note(format!("fixture_root {}", fixture_dir.display()));

    let dry_run_fixture_hash = if args.dry_run {
        match validate_fixture(&fixture_dir) {
            Ok(hash) => {
                transcript.note(format!("fixture_hash {}", hash));
                Some(hash)
            }
            Err(err) => {
                transcript.note(format!("validate_fixture failed: {err}"));
                let evidence_dir = record_early_failure(
                    &args.out_dir,
                    &env,
                    "fixture_invalid",
                    "fixture validation failed".to_string(),
                    vec![err.to_string()],
                    None,
                )?;
                transcript.note(format!("evidence_dir {}", evidence_dir.display()));
                return Ok(());
            }
        }
    } else {
        None
    };

    let help_text = String::from_utf8_lossy(&help_capture.bytes).into_owned();

    let lm_command = match load_lm_command() {
        Ok(command) => command,
        Err(err) => {
            transcript.note(format!("load LM command failed: {err}"));
            let evidence_dir = record_early_failure(
                &args.out_dir,
                &env,
                "lm_command_invalid",
                "failed to load LM command".to_string(),
                vec![err.to_string()],
                None,
            )?;
            transcript.note(format!("evidence_dir {}", evidence_dir.display()));
            return Ok(());
        }
    };
    transcript.note(format!(
        "run_lm program={} args={}",
        lm_command
            .argv
            .first()
            .map(|value| value.as_str())
            .unwrap_or("<unknown>"),
        lm_command.argv.len().saturating_sub(1)
    ));

    let mut history: Vec<InvocationFeedback> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let help_args = vec![help_capture.flag.to_string()];
    let (stdout_bytes, stderr_bytes) = if help_capture.source == "stdout" {
        (help_capture.bytes.len() as u64, 0)
    } else {
        (0, help_capture.bytes.len() as u64)
    };
    history.push(InvocationFeedback {
        args: help_args.clone(),
        status: InvocationStatus::Executed,
        exit_code: None,
        timed_out: false,
        stdout_bytes,
        stderr_bytes,
        stdout_snippet: None,
        stderr_snippet: None,
        note: Some(format!(
            "help captured via {} ({})",
            help_capture.flag, help_capture.source
        )),
    });
    seen.insert(invocation_key(&help_args));

    let mut sequence: u64 = 0;
    for round_index in 0..MAX_ITERATION_ROUNDS {
        let prompt = build_invocation_prompt(
            &target_binary.exec_path,
            &help_text,
            &schema_text,
            &history,
        );

        transcript.note(format!("iterate_round {}", round_index + 1));
        transcript.note(format!("build_invocation_prompt bytes={}", prompt.len()));
        transcript.block("lm.prompt", &prompt);

        let response_bytes = match run_lm(&prompt, &schema_text, &lm_command) {
            Ok(bytes) => bytes,
            Err(err) => {
                transcript.note(format!("run_lm failed: {err}"));
                let evidence_dir = record_early_failure(
                    &args.out_dir,
                    &env,
                    "lm_failed",
                    "failed to obtain LM response".to_string(),
                    vec![err.to_string()],
                    Some(&prompt),
                )?;
                transcript.note(format!("evidence_dir {}", evidence_dir.display()));
                return Ok(());
            }
        };
        transcript.note(format!("lm_response bytes={}", response_bytes.len()));
        let response_text = String::from_utf8_lossy(&response_bytes);
        transcript.block("lm.response", &response_text);

        let InvocationEnvelope {
            invocation,
            invocation_bytes,
        } = match parse_invocation_response(&response_bytes) {
            Ok(envelope) => envelope,
            Err(details) => {
                transcript.note(format!(
                    "parse_invocation failed: {}",
                    details.join("; ")
                ));
                let invocation_hash = sha256_hex(&response_bytes);
                let evidence_dir =
                    create_evidence_dir(&args.out_dir, Some(&invocation_hash), Some("lm_invalid"))?;
                transcript.note(format!("evidence_dir {}", evidence_dir.display()));
                if let Err(err) = write_invocation_provenance(
                    &evidence_dir,
                    &prompt,
                    &response_bytes,
                    None,
                    None,
                ) {
                    fail_schema(
                        &evidence_dir,
                        &env,
                        Some(&invocation_hash),
                        None,
                        "lm_io_failed",
                        "failed to write LM provenance".to_string(),
                        vec![err.to_string()],
                    )?;
                    return Ok(());
                }
                fail_schema(
                    &evidence_dir,
                    &env,
                    Some(&invocation_hash),
                    None,
                    "schema_invalid",
                    "invocation JSON failed to parse".to_string(),
                    details,
                )?;
                return Ok(());
            }
        };

        let invocation_text = String::from_utf8_lossy(&invocation_bytes);
        transcript.block("invocation.json", &invocation_text);

        if invocation.args.is_empty() {
            transcript.note("empty args; stop");
            break;
        }

        let mut errors = validate_invocation(&invocation).unwrap_or_default();
        let key = invocation_key(&invocation.args);
        if seen.contains(&key) {
            errors.push("invocation already tested".to_string());
        }

        if !errors.is_empty() {
            transcript.note(format!(
                "validate_invocation failed: {}",
                errors.join("; ")
            ));
            let invocation_hash = sha256_hex(&invocation_bytes);
            let evidence_dir = create_evidence_dir(
                &args.out_dir,
                Some(&invocation_hash),
                Some("invoke_invalid"),
            )?;
            if let Err(err) = write_invocation_provenance(
                &evidence_dir,
                &prompt,
                &response_bytes,
                Some(&invocation_bytes),
                None,
            ) {
                fail_schema(
                    &evidence_dir,
                    &env,
                    Some(&invocation_hash),
                    None,
                    "lm_io_failed",
                    "failed to write LM provenance".to_string(),
                    vec![err.to_string()],
                )?;
                return Ok(());
            }

            let feedback = InvocationFeedback {
                args: invocation.args.clone(),
                status: InvocationStatus::Rejected,
                exit_code: None,
                timed_out: false,
                stdout_bytes: 0,
                stderr_bytes: 0,
                stdout_snippet: None,
                stderr_snippet: None,
                note: Some(errors.join("; ")),
            };
            write_invocation_result(&evidence_dir, &feedback)?;
            fail_schema(
                &evidence_dir,
                &env,
                Some(&invocation_hash),
                None,
                "invocation_invalid",
                "invocation validation failed".to_string(),
                errors,
            )?;

            history.push(feedback);
            seen.insert(key);
            continue;
        }

        seen.insert(key);
        sequence += 1;
        let scenario = scenario_for_invocation(&invocation, &target_binary.exec_path, sequence);
        let scenario_bytes =
            serde_json::to_vec_pretty(&scenario).context("serialize scenario")?;
        let scenario_text = String::from_utf8_lossy(&scenario_bytes);
        transcript.block("scenario.json", &scenario_text);

        let scenario_hash = sha256_hex(&scenario_bytes);
        let evidence_dir = create_evidence_dir(
            &args.out_dir,
            Some(&scenario_hash),
            Some(&scenario.scenario_id),
        )?;
        if let Err(err) = write_invocation_provenance(
            &evidence_dir,
            &prompt,
            &response_bytes,
            Some(&invocation_bytes),
            Some(&scenario_bytes),
        ) {
            fail_schema(
                &evidence_dir,
                &env,
                Some(&scenario_hash),
                Some(&scenario.scenario_id),
                "lm_io_failed",
                "failed to write LM provenance".to_string(),
                vec![err.to_string()],
            )?;
            return Ok(());
        }

        if let Some(errors) = validate_scenario(&scenario) {
            transcript.note(format!(
                "validate_scenario failed: {}",
                errors.join("; ")
            ));
            fail_schema(
                &evidence_dir,
                &env,
                Some(&scenario_hash),
                Some(&scenario.scenario_id),
                "schema_invalid",
                "scenario validation failed".to_string(),
                errors,
            )?;
            return Ok(());
        }

        let binary_validation = match validate_binary(
            &args,
            &env,
            &evidence_dir,
            &scenario_hash,
            &scenario,
            &target_binary,
            &mut transcript,
        )? {
            Some(validation) => validation,
            None => return Ok(()),
        };
        let BinaryValidation {
            exec_binary,
            resolved_binary,
            binary_hash,
        } = binary_validation;

        if args.dry_run {
            let fixture_hash = match dry_run_fixture_hash.as_ref() {
                Some(hash) => hash.clone(),
                None => {
                    return Err(anyhow::anyhow!(
                        "fixture hash missing for dry-run invocation"
                    ))
                }
            };
            let feedback = InvocationFeedback {
                args: invocation.args.clone(),
                status: InvocationStatus::Skipped,
                exit_code: None,
                timed_out: false,
                stdout_bytes: 0,
                stderr_bytes: 0,
                stdout_snippet: None,
                stderr_snippet: None,
                note: Some("dry-run".to_string()),
            };
            write_invocation_result(&evidence_dir, &feedback)?;

            let meta = Meta {
                tool_version: TOOL_VERSION.to_string(),
                scenario_sha256: Some(scenario_hash),
                scenario_id: Some(scenario.scenario_id.clone()),
                binary: Some(BinaryMeta {
                    path: scenario.binary.path.clone(),
                    sha256: Some(binary_hash),
                }),
                fixture: Some(FixtureMeta {
                    id: scenario.fixture.id.clone(),
                    sha256: Some(fixture_hash),
                }),
                env: env.clone(),
                limits: Some(scenario.limits),
                outcome: Outcome::Exited,
                error: None,
                result: None,
                artifacts: None,
                sandbox: None,
            };

            write_meta(&evidence_dir, meta)?;
            transcript.note(format!("evidence_dir {}", evidence_dir.display()));
            println!("evidence: {}", evidence_dir.display());

            history.push(feedback);
            continue;
        }

        let prepared_fixture = match prepare_fixture(&fixture_dir) {
            Ok(prepared) => prepared,
            Err(err) => {
                transcript.note(format!("prepare_fixture failed: {}", err.message));
                let outcome = if err.is_missing {
                    Outcome::FixtureMissing
                } else {
                    Outcome::FixtureInvalid
                };
                write_meta(
                    &evidence_dir,
                    Meta {
                        tool_version: TOOL_VERSION.to_string(),
                        scenario_sha256: Some(scenario_hash),
                        scenario_id: Some(scenario.scenario_id.clone()),
                        binary: Some(BinaryMeta {
                            path: scenario.binary.path.clone(),
                            sha256: Some(binary_hash.clone()),
                        }),
                        fixture: Some(FixtureMeta {
                            id: scenario.fixture.id.clone(),
                            sha256: None,
                        }),
                        env: env.clone(),
                        limits: Some(scenario.limits),
                        outcome,
                        error: Some(ErrorReport {
                            code: match outcome {
                                Outcome::FixtureMissing => "fixture_missing".to_string(),
                                _ => "fixture_invalid".to_string(),
                            },
                            message: err.message,
                            details: err.details,
                        }),
                        result: None,
                        artifacts: None,
                        sandbox: None,
                    },
                )?;
                return Ok(());
            }
        };
        transcript.note(format!(
            "prepare_fixture hash={}",
            prepared_fixture.fixture_hash
        ));

        let run_result = if args.direct {
            run_direct(
                &exec_binary,
                &scenario.args,
                &prepared_fixture.fixture_root,
                scenario.limits,
            )
        } else {
            run_sandboxed(
                &exec_binary,
                &resolved_binary,
                &scenario.args,
                &prepared_fixture.fixture_root,
                scenario.limits,
            )
        };

        let run_result = match run_result {
            Ok(result) => result,
            Err(err) => {
                transcript.note(format!("run_failed: {err}"));
                write_meta(
                    &evidence_dir,
                    Meta {
                        tool_version: TOOL_VERSION.to_string(),
                        scenario_sha256: Some(scenario_hash),
                        scenario_id: Some(scenario.scenario_id.clone()),
                        binary: Some(BinaryMeta {
                            path: scenario.binary.path.clone(),
                            sha256: Some(binary_hash.clone()),
                        }),
                        fixture: Some(FixtureMeta {
                            id: scenario.fixture.id.clone(),
                            sha256: Some(prepared_fixture.fixture_hash.clone()),
                        }),
                        env: env.clone(),
                        limits: Some(scenario.limits),
                        outcome: Outcome::SandboxFailed,
                        error: Some(error_report("sandbox_failed", &err)),
                        result: None,
                        artifacts: None,
                        sandbox: Some(SandboxMeta {
                            mode: if args.direct {
                                "direct".to_string()
                            } else {
                                "bwrap".to_string()
                            },
                        }),
                    },
                )?;
                return Ok(());
            }
        };
        transcript.note(format!(
            "run_result exit_code={:?} timed_out={} wall_time_ms={} mode={}",
            run_result.exit_code,
            run_result.timed_out,
            run_result.wall_time_ms,
            if args.direct { "direct" } else { "bwrap" }
        ));
        if scenario.artifacts.capture_stdout {
            let stdout_text = String::from_utf8_lossy(&run_result.stdout);
            transcript.block("scenario.stdout", &stdout_text);
        }
        if scenario.artifacts.capture_stderr {
            let stderr_text = String::from_utf8_lossy(&run_result.stderr);
            transcript.block("scenario.stderr", &stderr_text);
        }

        let stdout_hash = sha256_hex(&run_result.stdout);
        let stderr_hash = sha256_hex(&run_result.stderr);
        if scenario.artifacts.capture_stdout {
            fs::write(evidence_dir.join("stdout.txt"), &run_result.stdout)
                .context("write stdout.txt")?;
        }
        if scenario.artifacts.capture_stderr {
            fs::write(evidence_dir.join("stderr.txt"), &run_result.stderr)
                .context("write stderr.txt")?;
        }

        let (stdout_bytes, stdout_snippet) = summarize_output(&run_result.stdout);
        let (stderr_bytes, stderr_snippet) = summarize_output(&run_result.stderr);
        let feedback = InvocationFeedback {
            args: invocation.args.clone(),
            status: InvocationStatus::Executed,
            exit_code: run_result.exit_code,
            timed_out: run_result.timed_out,
            stdout_bytes,
            stderr_bytes,
            stdout_snippet,
            stderr_snippet,
            note: None,
        };
        write_invocation_result(&evidence_dir, &feedback)?;
        history.push(feedback);

        let meta_outcome = if run_result.timed_out {
            Outcome::TimedOut
        } else {
            Outcome::Exited
        };

        let meta = Meta {
            tool_version: TOOL_VERSION.to_string(),
            scenario_sha256: Some(scenario_hash),
            scenario_id: Some(scenario.scenario_id.clone()),
            binary: Some(BinaryMeta {
                path: scenario.binary.path.clone(),
                sha256: Some(binary_hash),
            }),
            fixture: Some(FixtureMeta {
                id: scenario.fixture.id.clone(),
                sha256: Some(prepared_fixture.fixture_hash),
            }),
            env: env.clone(),
            limits: Some(scenario.limits),
            outcome: meta_outcome,
            error: None,
            result: Some(ResultMeta {
                exit_code: run_result.exit_code,
                timed_out: run_result.timed_out,
                wall_time_ms: run_result.wall_time_ms,
            }),
            artifacts: Some(ArtifactsMeta {
                stdout_sha256: stdout_hash,
                stderr_sha256: stderr_hash,
                stdout_bytes: run_result.stdout.len() as u64,
                stderr_bytes: run_result.stderr.len() as u64,
            }),
            sandbox: Some(SandboxMeta {
                mode: if args.direct {
                    "direct".to_string()
                } else {
                    "bwrap".to_string()
                },
            }),
        };

        write_meta(&evidence_dir, meta)?;
        transcript.note(format!("evidence_dir {}", evidence_dir.display()));
        println!("evidence: {}", evidence_dir.display());
    }

    Ok(())
}


fn record_early_failure(
    out_dir: &Path,
    env: &EnvContract,
    code: &str,
    message: String,
    details: Vec<String>,
    prompt: Option<&str>,
) -> Result<PathBuf> {
    let evidence_dir = create_evidence_dir(out_dir, None, Some(code))?;
    if let Some(prompt) = prompt {
        fs::write(evidence_dir.join("lm.prompt.txt"), prompt.as_bytes())
            .context("write lm.prompt.txt")?;
    }
    fail_schema(&evidence_dir, env, None, None, code, message, details)?;
    Ok(evidence_dir)
}

fn write_invocation_provenance(
    evidence_dir: &Path,
    prompt: &str,
    response: &[u8],
    invocation_bytes: Option<&[u8]>,
    scenario_bytes: Option<&[u8]>,
) -> Result<()> {
    fs::write(evidence_dir.join("lm.prompt.txt"), prompt.as_bytes())
        .context("write lm.prompt.txt")?;
    fs::write(evidence_dir.join("lm.response.json"), response)
        .context("write lm.response.json")?;
    if let Some(invocation_bytes) = invocation_bytes {
        fs::write(evidence_dir.join("invocation.json"), invocation_bytes)
            .context("write invocation.json")?;
    }
    if let Some(scenario_bytes) = scenario_bytes {
        fs::write(evidence_dir.join("scenario.json"), scenario_bytes)
            .context("write scenario.json")?;
    }
    Ok(())
}

fn write_invocation_result(evidence_dir: &Path, record: &InvocationFeedback) -> Result<()> {
    let json = serde_json::to_vec_pretty(record).context("serialize invocation.result.json")?;
    fs::write(evidence_dir.join("invocation.result.json"), json)
        .context("write invocation.result.json")?;
    Ok(())
}

fn invocation_key(args: &[String]) -> String {
    args.join("\u{0}")
}

struct BinaryValidation {
    exec_binary: PathBuf,
    resolved_binary: PathBuf,
    binary_hash: String,
}

fn fail_schema(
    evidence_dir: &Path,
    env: &EnvContract,
    scenario_hash: Option<&str>,
    scenario_id: Option<&str>,
    code: &str,
    message: String,
    details: Vec<String>,
) -> Result<()> {
    write_schema_invalid(
        evidence_dir,
        env,
        scenario_hash,
        scenario_id,
        code,
        message,
        details,
    )
}

fn write_schema_invalid(
    evidence_dir: &Path,
    env: &EnvContract,
    scenario_hash: Option<&str>,
    scenario_id: Option<&str>,
    code: &str,
    message: String,
    details: Vec<String>,
) -> Result<()> {
    write_meta(
        evidence_dir,
        Meta {
            tool_version: TOOL_VERSION.to_string(),
            scenario_sha256: scenario_hash.map(|value| value.to_string()),
            scenario_id: scenario_id.map(|value| value.to_string()),
            binary: None,
            fixture: None,
            env: env.clone(),
            limits: None,
            outcome: Outcome::SchemaInvalid,
            error: Some(ErrorReport {
                code: code.to_string(),
                message,
                details,
            }),
            result: None,
            artifacts: None,
            sandbox: None,
        },
    )
}

fn validate_binary(
    args: &Args,
    env: &EnvContract,
    evidence_dir: &Path,
    scenario_hash: &str,
    scenario: &Scenario,
    target_binary: &BinaryTarget,
    transcript: &mut Transcript,
) -> Result<Option<BinaryValidation>> {
    // Preserve argv[0] semantics while hashing the resolved target.
    let exec_binary = PathBuf::from(&scenario.binary.path);
    if exec_binary != target_binary.exec_path {
        transcript.note(format!(
            "binary_mismatch exec_path={} scenario_path={}",
            target_binary.exec_path.display(),
            exec_binary.display()
        ));
        fail_schema(
            evidence_dir,
            env,
            Some(scenario_hash),
            Some(&scenario.scenario_id),
            "binary_mismatch",
            "scenario binary does not match target".to_string(),
            vec![
                format!("target: {}", target_binary.exec_path.display()),
                format!("scenario: {}", exec_binary.display()),
            ],
        )?;
        return Ok(None);
    }

    let resolved_binary = match resolve_binary(&exec_binary) {
        Ok(path) => path,
        Err(err) => {
            transcript.note(format!("resolve scenario binary failed: {err}"));
            let message = format!("binary path invalid: {err}");
            return record_binary_failure(
                args,
                env,
                evidence_dir,
                scenario_hash,
                scenario,
                "binary_invalid",
                message,
            );
        }
    };

    if resolved_binary != target_binary.resolved_path {
        transcript.note(format!(
            "binary_mismatch resolved_target={} resolved_scenario={}",
            target_binary.resolved_path.display(),
            resolved_binary.display()
        ));
        fail_schema(
            evidence_dir,
            env,
            Some(scenario_hash),
            Some(&scenario.scenario_id),
            "binary_mismatch",
            "scenario binary does not match target".to_string(),
            vec![
                format!("target: {}", target_binary.resolved_path.display()),
                format!("scenario: {}", resolved_binary.display()),
            ],
        )?;
        return Ok(None);
    }

    let binary_hash = match hash_binary(&resolved_binary) {
        Ok(hash) => hash,
        Err(err) => {
            transcript.note(format!("hash binary failed: {err}"));
            let message = format!("failed to hash binary: {err}");
            return record_binary_failure(
                args,
                env,
                evidence_dir,
                scenario_hash,
                scenario,
                "binary_invalid",
                message,
            );
        }
    };
    transcript.note(format!("binary_hash {}", binary_hash));

    Ok(Some(BinaryValidation {
        exec_binary,
        resolved_binary,
        binary_hash,
    }))
}

fn record_binary_failure(
    args: &Args,
    env: &EnvContract,
    evidence_dir: &Path,
    scenario_hash: &str,
    scenario: &Scenario,
    code: &str,
    message: String,
) -> Result<Option<BinaryValidation>> {
    if args.dry_run {
        fail_schema(
            evidence_dir,
            env,
            Some(scenario_hash),
            Some(&scenario.scenario_id),
            code,
            message,
            Vec::new(),
        )?;
        return Ok(None);
    }
    write_binary_missing(evidence_dir, env, scenario_hash, scenario, message)?;
    Ok(None)
}

fn write_binary_missing(
    evidence_dir: &Path,
    env: &EnvContract,
    scenario_hash: &str,
    scenario: &Scenario,
    message: String,
) -> Result<()> {
    write_meta(
        evidence_dir,
        Meta {
            tool_version: TOOL_VERSION.to_string(),
            scenario_sha256: Some(scenario_hash.to_string()),
            scenario_id: Some(scenario.scenario_id.clone()),
            binary: Some(BinaryMeta {
                path: scenario.binary.path.clone(),
                sha256: None,
            }),
            fixture: None,
            env: env.clone(),
            limits: Some(scenario.limits),
            outcome: Outcome::BinaryMissing,
            error: Some(ErrorReport {
                code: "binary_missing".to_string(),
                message,
                details: Vec::new(),
            }),
            result: None,
            artifacts: None,
            sandbox: None,
        },
    )
}

fn error_report(code: &str, err: &anyhow::Error) -> ErrorReport {
    let details = err.chain().skip(1).map(|cause| cause.to_string()).collect();
    ErrorReport {
        code: code.to_string(),
        message: err.to_string(),
        details,
    }
}
