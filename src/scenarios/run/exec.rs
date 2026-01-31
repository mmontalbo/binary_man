use super::validate::validate_scenario;
use super::{ScenarioExecution, ScenarioRunContext};
use crate::enrich;
use crate::scenarios::{MAX_SCENARIO_EVIDENCE_BYTES, SCENARIO_EVIDENCE_SCHEMA_VERSION};
use crate::util::truncate_bytes;
use anyhow::{anyhow, Context, Result};
use std::path::Path;
use std::process::Command;

use crate::scenarios::evidence::{
    read_json, read_ref_bytes, resolve_new_run, stage_scenario_evidence, RunIndexEntry,
    RunManifest, ScenarioEvidence, ScenarioIndexEntry, ScenarioOutcome,
};

pub(super) fn build_failed_execution(
    context: &ScenarioRunContext<'_>,
    status: &std::process::ExitStatus,
) -> Result<ScenarioExecution> {
    let scenario = context.scenario;
    let run_config = context.run_config;
    let argv = scenario.argv.clone();
    let command_line = format_command_line(context.run_argv0, &argv);
    let failures = vec![format!(
        "binary_lens run failed with status {}",
        exit_status_string(status)
    )];
    let outcome = scenario.publish.then(|| ScenarioOutcome {
        scenario_id: scenario.id.clone(),
        publish: scenario.publish,
        argv,
        env: run_config.env.clone(),
        seed_dir: run_config.seed_dir.clone(),
        cwd: run_config.cwd.clone(),
        timeout_seconds: run_config.timeout_seconds,
        net_mode: run_config.net_mode.clone(),
        no_sandbox: run_config.no_sandbox,
        no_strace: run_config.no_strace,
        snippet_max_lines: run_config.snippet_max_lines,
        snippet_max_bytes: run_config.snippet_max_bytes,
        run_argv0: context.run_argv0.to_string(),
        expected: scenario.expect.clone(),
        run_id: None,
        manifest_ref: None,
        stdout_ref: None,
        stderr_ref: None,
        observed_exit_code: None,
        observed_exit_signal: None,
        observed_timed_out: false,
        pass: false,
        failures: failures.clone(),
        command_line,
        stdout_snippet: String::new(),
        stderr_snippet: String::new(),
    });
    let index_entry = ScenarioIndexEntry {
        scenario_id: scenario.id.clone(),
        scenario_digest: run_config.scenario_digest.clone(),
        last_run_epoch_ms: Some(enrich::now_epoch_ms()?),
        last_pass: Some(false),
        failures,
        evidence_paths: Vec::new(),
    };
    Ok(ScenarioExecution {
        outcome,
        index_entry,
    })
}

pub(super) fn build_success_execution(
    pack_root: &Path,
    staging_root: Option<&Path>,
    context: &ScenarioRunContext<'_>,
    before: &[RunIndexEntry],
    after: &[RunIndexEntry],
    verbose: bool,
) -> Result<ScenarioExecution> {
    let scenario = context.scenario;
    let run_config = context.run_config;
    let (run_id, entry) = resolve_new_run(before, after)
        .with_context(|| format!("resolve new run for scenario {}", scenario.id))?;

    let RunIndexEntry {
        manifest_ref,
        stdout_ref,
        stderr_ref,
        ..
    } = entry;
    let manifest_ref = manifest_ref.unwrap_or_else(|| format!("runs/{run_id}/manifest.json"));
    let stdout_ref = stdout_ref.unwrap_or_else(|| format!("runs/{run_id}/stdout.txt"));
    let stderr_ref = stderr_ref.unwrap_or_else(|| format!("runs/{run_id}/stderr.txt"));

    let run_manifest: RunManifest = read_json(pack_root, &manifest_ref)
        .with_context(|| format!("read run manifest {manifest_ref}"))?;

    let stdout_bytes = read_ref_bytes(pack_root, &stdout_ref)
        .with_context(|| format!("read stdout {stdout_ref}"))?;
    let stderr_bytes = read_ref_bytes(pack_root, &stderr_ref)
        .with_context(|| format!("read stderr {stderr_ref}"))?;
    let stdout_text = String::from_utf8_lossy(&stdout_bytes);
    let stderr_text = String::from_utf8_lossy(&stderr_bytes);

    let observed_exit_code = run_manifest.result.exit_code;
    let observed_exit_signal = run_manifest.result.exit_signal;
    let observed_timed_out = run_manifest.result.timed_out;

    let mut evidence_paths = Vec::new();
    let mut evidence_epoch_ms = None;
    if let Some(staging_root) = staging_root {
        let mut argv_full = Vec::with_capacity(scenario.argv.len() + 1);
        argv_full.push(context.run_argv0.to_string());
        argv_full.extend(scenario.argv.iter().cloned());
        let generated_at_epoch_ms = enrich::now_epoch_ms()?;
        let is_auto = scenario
            .id
            .starts_with(crate::scenarios::AUTO_VERIFY_SCENARIO_PREFIX);
        let (stdout, stderr) = if is_auto {
            (
                bounded_snippet(
                    stdout_text.as_ref(),
                    run_config.snippet_max_lines,
                    run_config.snippet_max_bytes,
                ),
                bounded_snippet(
                    stderr_text.as_ref(),
                    run_config.snippet_max_lines,
                    run_config.snippet_max_bytes,
                ),
            )
        } else {
            (
                truncate_bytes(&stdout_bytes, MAX_SCENARIO_EVIDENCE_BYTES),
                truncate_bytes(&stderr_bytes, MAX_SCENARIO_EVIDENCE_BYTES),
            )
        };
        let evidence = ScenarioEvidence {
            schema_version: SCENARIO_EVIDENCE_SCHEMA_VERSION,
            generated_at_epoch_ms,
            scenario_id: scenario.id.clone(),
            argv: argv_full,
            env: run_config.env.clone(),
            seed_dir: context.run_seed_dir.map(|value| value.to_string()),
            cwd: run_config.cwd.clone(),
            timeout_seconds: run_config.timeout_seconds,
            net_mode: run_config.net_mode.clone(),
            no_sandbox: run_config.no_sandbox,
            no_strace: run_config.no_strace,
            snippet_max_lines: run_config.snippet_max_lines,
            snippet_max_bytes: run_config.snippet_max_bytes,
            exit_code: observed_exit_code,
            exit_signal: observed_exit_signal,
            timed_out: observed_timed_out,
            duration_ms: context.duration_ms,
            stdout,
            stderr,
        };
        let rel = stage_scenario_evidence(staging_root, &evidence)?;
        evidence_paths.push(rel);
        evidence_epoch_ms = Some(generated_at_epoch_ms);
    }

    let failures = validate_scenario(
        &scenario.expect,
        observed_exit_code,
        observed_exit_signal,
        observed_timed_out,
        stdout_text.as_ref(),
        stderr_text.as_ref(),
    );
    let pass = failures.is_empty();

    let command_line = format_command_line(context.run_argv0, &scenario.argv);
    let stdout_snippet = bounded_snippet(
        stdout_text.as_ref(),
        run_config.snippet_max_lines,
        run_config.snippet_max_bytes,
    );
    let stderr_snippet = bounded_snippet(
        stderr_text.as_ref(),
        run_config.snippet_max_lines,
        run_config.snippet_max_bytes,
    );

    if verbose && !pass {
        eprintln!("scenario {} failed: {}", scenario.id, failures.join("; "));
    }

    let outcome = scenario.publish.then(|| ScenarioOutcome {
        scenario_id: scenario.id.clone(),
        publish: scenario.publish,
        argv: scenario.argv.clone(),
        env: run_config.env.clone(),
        seed_dir: run_config.seed_dir.clone(),
        cwd: run_config.cwd.clone(),
        timeout_seconds: run_config.timeout_seconds,
        net_mode: run_config.net_mode.clone(),
        no_sandbox: run_config.no_sandbox,
        no_strace: run_config.no_strace,
        snippet_max_lines: run_config.snippet_max_lines,
        snippet_max_bytes: run_config.snippet_max_bytes,
        run_argv0: context.run_argv0.to_string(),
        expected: scenario.expect.clone(),
        run_id: Some(run_id),
        manifest_ref: Some(manifest_ref),
        stdout_ref: Some(stdout_ref),
        stderr_ref: Some(stderr_ref),
        observed_exit_code,
        observed_exit_signal,
        observed_timed_out,
        pass,
        failures: failures.clone(),
        command_line,
        stdout_snippet,
        stderr_snippet,
    });
    let index_entry = ScenarioIndexEntry {
        scenario_id: scenario.id.clone(),
        scenario_digest: run_config.scenario_digest.clone(),
        last_run_epoch_ms: evidence_epoch_ms,
        last_pass: Some(pass),
        failures,
        evidence_paths,
    };
    Ok(ScenarioExecution {
        outcome,
        index_entry,
    })
}

pub(super) fn invoke_binary_lens_run(
    pack_root: &Path,
    run_root: &Path,
    lens_flake: &str,
    run_kv_args: &[String],
    scenario_argv: &[String],
    env_overrides: &std::collections::BTreeMap<String, String>,
) -> Result<std::process::ExitStatus> {
    let pack_root_str = pack_root
        .to_str()
        .ok_or_else(|| anyhow!("pack root path is not valid UTF-8"))?;

    let mut cmd = Command::new("nix");
    cmd.args(["run", lens_flake, "--"]);
    cmd.args(run_kv_args);
    cmd.arg(pack_root_str);
    cmd.args(scenario_argv);
    for (key, value) in env_overrides {
        cmd.env(key, value);
    }
    cmd.current_dir(run_root);
    let status = cmd.status().context("spawn nix run")?;
    Ok(status)
}

pub(super) fn build_run_kv_args(
    run_argv0: &str,
    run_seed_dir: Option<&str>,
    cwd: Option<&str>,
    timeout_seconds: Option<f64>,
    net_mode: Option<&str>,
    no_sandbox: Option<bool>,
    no_strace: Option<bool>,
) -> Result<Vec<String>> {
    let mut args = vec![String::from("run=1"), format!("run_argv0={run_argv0}")];

    if let Some(seed_dir) = run_seed_dir {
        args.push(format!("run_seed_dir={seed_dir}"));
    }
    if let Some(cwd) = cwd {
        args.push(format!("run_cwd={cwd}"));
    }
    if let Some(timeout_seconds) = timeout_seconds {
        args.push(format!("run_timeout_seconds={timeout_seconds}"));
    }
    if let Some(net_mode) = net_mode {
        args.push(format!("run_net={net_mode}"));
    }
    if let Some(no_sandbox) = no_sandbox {
        args.push(format!("run_no_sandbox={}", if no_sandbox { 1 } else { 0 }));
    }
    if let Some(no_strace) = no_strace {
        args.push(format!("run_no_strace={}", if no_strace { 1 } else { 0 }));
    }

    Ok(args)
}

fn bounded_snippet(text: &str, max_lines: usize, max_bytes: usize) -> String {
    let marker = "\n[... output truncated ...]\n";
    if max_lines == 0 || max_bytes == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut truncated = false;

    for (line_idx, chunk) in text.split_inclusive('\n').enumerate() {
        if line_idx >= max_lines {
            truncated = true;
            break;
        }
        if out.len() + chunk.len() > max_bytes {
            let remaining = max_bytes.saturating_sub(out.len());
            out.push_str(truncate_utf8(chunk, remaining));
            truncated = true;
            break;
        }
        out.push_str(chunk);
    }

    if !truncated && out.len() < text.len() {
        truncated = true;
    }

    if truncated {
        if max_bytes <= marker.len() {
            return truncate_utf8(marker, max_bytes).to_string();
        }
        let available = max_bytes - marker.len();
        if out.len() > available {
            out = truncate_utf8(&out, available).to_string();
        }
        out.push_str(marker);
    }

    out
}

fn truncate_utf8(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn exit_status_string(status: &std::process::ExitStatus) -> String {
    if let Some(code) = status.code() {
        format!("{code}")
    } else {
        "terminated by signal".to_string()
    }
}

fn format_command_line(binary_name: &str, argv: &[String]) -> String {
    let mut parts = Vec::with_capacity(argv.len() + 1);
    parts.push(shell_quote(binary_name));
    for arg in argv {
        parts.push(shell_quote(arg));
    }
    parts.join(" ")
}

fn shell_quote(arg: &str) -> String {
    if arg.is_empty() {
        return "''".to_string();
    }
    let safe = arg.chars().all(|ch| {
        matches!(
            ch,
            'a'..='z'
                | 'A'..='Z'
                | '0'..='9'
                | '_'
                | '-'
                | '.'
                | '/'
                | ':'
                | '@'
                | '+'
                | '='
        )
    });
    if safe {
        return arg.to_string();
    }
    let escaped = arg.replace('\'', "'\"'\"'");
    format!("'{escaped}'")
}
