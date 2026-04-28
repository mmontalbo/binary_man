use anyhow::{Context, Result};
use std::collections::HashMap;
use std::io::BufRead;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::summary::{LmStats, ProgressStats, RunOutcome, SurfaceOutcome};

/// Execute one full end-to-end bman run, return outcome.
pub fn run_single(
    bman_bin: &str,
    binary: &str,
    entry_point: &[String],
    max_cycles: u32,
    timeout: u64,
    run_idx: usize,
    lm: &str,
) -> Result<RunOutcome> {
    let label = format!("{}:{}", lm.split(':').next_back().unwrap_or(lm), binary);
    let tmpdir =
        tempfile::tempdir().with_context(|| format!("create tmpdir for run {}", run_idx))?;

    let mut cmd_args = vec![
        "--doc-pack".to_string(),
        tmpdir.path().to_string_lossy().to_string(),
        "--max-cycles".to_string(),
        max_cycles.to_string(),
        "--lm".to_string(),
        lm.to_string(),
        binary.to_string(),
    ];
    cmd_args.extend(entry_point.iter().cloned());

    let start = Instant::now();
    let result = run_with_timeout(bman_bin, &cmd_args, timeout, run_idx, &label)?;
    let elapsed = start.elapsed().as_secs_f64();

    // Read final state (even on crash/timeout for partial results)
    let state_path = tmpdir.path().join("state.json");
    let (surfaces, cycles, state_json) = if state_path.exists() {
        let raw = std::fs::read_to_string(&state_path)?;
        match serde_json::from_str::<serde_json::Value>(&raw) {
            Ok(state) => (
                extract_surface_outcomes(&state),
                state["cycle"].as_u64().unwrap_or(0) as u32,
                Some(raw),
            ),
            Err(_) => (HashMap::new(), 0, Some(raw)),
        }
    } else {
        (HashMap::new(), 0, None)
    };

    // Preserve lm_log files (prompts + responses) before the tmpdir is dropped.
    let lm_log_files = collect_lm_log_files(&tmpdir.path().join("lm_log"));

    let lm_stats = LmStats::from_stderr(&result.stderr);
    let progress_stats = ProgressStats::from_stderr(&result.stderr);

    Ok(RunOutcome {
        run_index: run_idx,
        elapsed_seconds: (elapsed * 10.0).round() / 10.0,
        cycles,
        timed_out: result.timed_out,
        crashed: result.crashed,
        surfaces,
        stderr: result.stderr,
        state_json,
        lm_log_files,
        lm_stats,
        progress_stats,
    })
}

/// Read all files from an lm_log directory as (filename, bytes) pairs.
/// Returns an empty Vec if the directory doesn't exist or can't be read.
fn collect_lm_log_files(lm_log_dir: &std::path::Path) -> Vec<(String, Vec<u8>)> {
    let entries = match std::fs::read_dir(lm_log_dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let mut files = Vec::new();
    for entry in entries.flatten() {
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if let Ok(bytes) = std::fs::read(entry.path()) {
            files.push((name, bytes));
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files
}

/// Run multiple trials in parallel using scoped threads.
pub fn run_parallel(
    bman_bin: &str,
    binary: &str,
    entry_point: &[String],
    max_cycles: u32,
    timeout: u64,
    num_runs: usize,
    lm: &str,
) -> Result<Vec<RunOutcome>> {
    let results: Vec<Result<RunOutcome>> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..num_runs)
            .map(|i| {
                s.spawn(move || {
                    let r = run_single(
                        bman_bin,
                        binary,
                        entry_point,
                        max_cycles,
                        timeout,
                        i,
                        lm,
                    )?;
                    let label = format!("{}:{}", lm.split(':').next_back().unwrap_or(lm), binary);
                    crate::display::print_run_progress(&r, i, num_runs, &label);
                    Ok(r)
                })
            })
            .collect();

        handles
            .into_iter()
            .map(|h| h.join().expect("worker thread panicked"))
            .collect()
    });

    let mut outcomes: Vec<RunOutcome> = Vec::with_capacity(num_runs);
    for r in results {
        outcomes.push(r?);
    }
    outcomes.sort_by_key(|r| r.run_index);
    Ok(outcomes)
}

/// Extract per-surface outcomes from a completed run's state JSON.
fn extract_surface_outcomes(state: &serde_json::Value) -> HashMap<String, SurfaceOutcome> {
    let mut results = HashMap::new();

    let entries = match state["entries"].as_array() {
        Some(e) => e,
        None => return results,
    };

    for entry in entries {
        let id = match entry["id"].as_str() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => continue,
        };

        let status_kind = entry["status"]["kind"]
            .as_str()
            .unwrap_or("Pending")
            .to_string();
        let verified = status_kind == "Verified";

        let attempts = entry["attempts"].as_array().map_or(0, Vec::len);
        let probes = entry["probes"].as_array().map_or(0, Vec::len);

        let first_verify_cycle =
            entry["attempts"]
                .as_array()
                .and_then(|attempts| {
                    attempts.iter().find_map(|a| {
                        if a["outcome"]["kind"].as_str() == Some("Verified") {
                            a["cycle"].as_u64().map(|c| c as u32)
                        } else {
                            None
                        }
                    })
                });

        results.insert(
            id,
            SurfaceOutcome {
                verified,
                status: status_kind,
                attempts,
                probes,
                first_verify_cycle,
            },
        );
    }

    results
}

/// Result of running a command with timeout.
struct RunResult {
    timed_out: bool,
    crashed: bool,
    stderr: String,
}

/// Run a command in its own process group with a timeout.
///
/// Captures stderr for diagnostics (rate limits, retries, errors).
/// Streams lines matching `PROGRESS:` to stderr with a run prefix.
/// Uses `setsid` + `killpg` to kill the entire process tree on timeout,
/// preventing orphaned LM plugin processes.
fn run_with_timeout(
    program: &str,
    args: &[String],
    timeout: u64,
    run_idx: usize,
    label: &str,
) -> Result<RunResult> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    // Put child in its own session so we can kill the entire process group.
    unsafe {
        cmd.pre_exec(|| {
            let _ = libc::setsid();
            Ok(())
        });
    }

    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawn {}", program))?;
    let pid = child.id() as libc::pid_t;

    // Read stderr line-by-line, streaming PROGRESS lines and accumulating the rest.
    let stderr_handle = child.stderr.take().expect("stderr was piped");
    let run_num = run_idx + 1;
    let label = label.to_string();
    let stderr_thread = std::thread::spawn(move || {
        let reader = std::io::BufReader::new(stderr_handle);
        let mut buf = String::new();
        for line in reader.lines() {
            match line {
                Ok(line) => {
                    if line.starts_with("PROGRESS:") {
                        eprintln!("  [{}|run {}] {}", label, run_num, line);
                    }
                    buf.push_str(&line);
                    buf.push('\n');
                }
                Err(_) => break,
            }
        }
        buf
    });

    let deadline = Instant::now() + Duration::from_secs(timeout);

    let (timed_out, crashed) = loop {
        match child.try_wait().context("check child status")? {
            Some(status) => break (false, !status.success()),
            None => {
                if Instant::now() >= deadline {
                    unsafe {
                        let _ = libc::killpg(pid, libc::SIGKILL);
                    }
                    let _ = child.wait();
                    break (true, false);
                }
                std::thread::sleep(Duration::from_millis(250));
            }
        }
    };

    let stderr = stderr_thread
        .join()
        .expect("stderr reader thread panicked");

    Ok(RunResult {
        timed_out,
        crashed,
        stderr,
    })
}
