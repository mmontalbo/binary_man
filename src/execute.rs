//! Execute the grid: states × invocations → observations.

use crate::parse::{Script, StdinSource, Run};
use crate::sandbox::{self, Sandbox};
use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Resource usage from a single execution.
#[derive(Debug, Clone, Default)]
pub struct ResourceUsage {
    pub wall_time_ms: u64,
}

/// Observation from a single execution.
#[derive(Debug, Clone)]
pub struct Observation {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub fs_changes: Vec<FsChange>,
    pub resources: ResourceUsage,
    pub trace_reads: Vec<String>,
    pub trace_failed: Vec<String>,
    pub trace_execs: Vec<String>,
    pub trace_net: Vec<String>,
    pub trace_signals: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FsChange {
    Created { path: String, size: u64 },
    Deleted { path: String },
    Modified { path: String, detail: String },
}

/// The full grid result.
#[derive(Debug)]
pub struct GridResult {
    pub cells: HashMap<(String, usize), Observation>,
    pub setup_failures: HashMap<String, String>,
    pub context_count: usize,
}

/// Snapshot entry: size, mode, and content hash for change detection.
#[derive(Clone)]
struct FileInfo {
    size: u64,
    mode: u32,
    mtime: u64, // seconds since epoch
}

type FsSnapshot = HashMap<String, FileInfo>;

fn snapshot_fs(work_dir: &Path) -> FsSnapshot {
    let mut snap = HashMap::new();
    if let Ok(entries) = walk_dir(work_dir, work_dir) {
        for (rel_path, info) in entries {
            snap.insert(rel_path, info);
        }
    }
    snap
}

fn walk_dir(base: &Path, dir: &Path) -> Result<Vec<(String, FileInfo)>> {
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let rel = path.strip_prefix(base)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();

        if path.is_dir() && !path.is_symlink() {
            let mode = get_mode(&path);
            entries.push((rel.clone(), FileInfo { size: 0, mode, mtime: 0 }));
            if let Ok(sub) = walk_dir(base, &path) {
                entries.extend(sub);
            }
        } else {
            let meta = path.metadata();
            let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            let mode = get_mode(&path);
            let mtime = meta.as_ref().ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            entries.push((rel, FileInfo { size, mode, mtime }));
        }
    }
    Ok(entries)
}

fn get_mode(path: &Path) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata().map(|m| m.permissions().mode()).unwrap_or(0)
    }
    #[cfg(not(unix))]
    { 0 }
}


fn diff_snapshots(before: &FsSnapshot, after: &FsSnapshot) -> Vec<FsChange> {
    let mut changes = Vec::new();

    for (path, after_info) in after {
        if !before.contains_key(path) {
            changes.push(FsChange::Created {
                path: path.clone(),
                size: after_info.size,
            });
        }
    }

    for path in before.keys() {
        if !after.contains_key(path) {
            changes.push(FsChange::Deleted {
                path: path.clone(),
            });
        }
    }

    for (path, before_info) in before {
        if let Some(after_info) = after.get(path) {
            let mut diffs = Vec::new();
            if before_info.size != after_info.size {
                diffs.push(format!("size: {} -> {}", before_info.size, after_info.size));
            }
            if before_info.mode != after_info.mode {
                diffs.push(format!("mode: {:o} -> {:o}", before_info.mode, after_info.mode));
            }
            if before_info.mtime != after_info.mtime {
                diffs.push("mtime changed".to_string());
            }
            if !diffs.is_empty() {
                changes.push(FsChange::Modified {
                    path: path.clone(),
                    detail: diffs.join(", "),
                });
            }
        }
    }

    changes.sort_by(|a, b| {
        let pa = match a {
            FsChange::Created { path, .. }
            | FsChange::Deleted { path }
            | FsChange::Modified { path, .. } => path,
        };
        let pb = match b {
            FsChange::Created { path, .. }
            | FsChange::Deleted { path }
            | FsChange::Modified { path, .. } => path,
        };
        pa.cmp(pb)
    });

    changes
}

/// Per-cell timeout in seconds.
pub const CELL_TIMEOUT_SECS: u64 = 2;

/// Max concurrent cells (threads).
const MAX_THREADS: usize = 8;

/// Run the entire grid with per-cell parallelism.
///
/// Each cell (context × run) gets its own sandbox directory. Context setup is
/// replayed per cell. All cells run in parallel bounded by a thread pool.
pub fn run_grid(
    binary: &str,
    script: &Script,
    probe_dir: &Path,
    sandbox: &Sandbox,
) -> Result<GridResult> {
    // Enumerate all (context_index, run_index) pairs
    let mut work_items: Vec<(usize, usize)> = Vec::new();
    for (ci, ctx) in script.contexts.iter().enumerate() {
        for (ri, run) in script.runs.iter().enumerate() {
            if run_matches_context(run, ctx) {
                work_items.push((ci, ri));
            }
        }
    }

    let total = work_items.len();
    let completed = AtomicUsize::new(0);
    let grid_start = std::time::Instant::now();

    // Parallel execution with bounded threads
    let results: Vec<_> = std::thread::scope(|s| {
        // Chunk work items across threads
        let n_threads = MAX_THREADS.min(work_items.len()).max(1);
        let chunk_size = work_items.len().div_ceil(n_threads);
        let chunks: Vec<&[(usize, usize)]> = work_items.chunks(chunk_size).collect();

        let handles: Vec<_> = chunks.into_iter().map(|chunk| {
            let completed = &completed;
            s.spawn(move || {
                let mut results: Vec<(String, usize, Result<Observation, String>)> = Vec::new();
                for &(ci, ri) in chunk {
                    let ctx = &script.contexts[ci];
                    let run = &script.runs[ri];

                    // Per-cell sandbox
                    let sandbox_dir = match tempfile::Builder::new()
                        .prefix("bgrid_")
                        .tempdir() {
                        Ok(d) => d,
                        Err(e) => {
                            results.push((ctx.name.clone(), ri, Err(format!("create sandbox: {}", e))));
                            continue;
                        }
                    };
                    let work_dir = sandbox_dir.path();

                    // Replay context setup
                    let env_vars = match sandbox::apply_setup(work_dir, binary, &ctx.commands, probe_dir, sandbox) {
                        Ok(env) => env,
                        Err(e) => {
                            results.push((ctx.name.clone(), ri, Err(format!("{}", e))));
                            continue;
                        }
                    };

                    // Run
                    match run_invocation(binary, run, work_dir, &env_vars, sandbox) {
                        Ok(obs) => results.push((ctx.name.clone(), ri, Ok(obs))),
                        Err(e) => results.push((ctx.name.clone(), ri, Err(format!("{}", e)))),
                    }

                    let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
                    if done.is_multiple_of(200) || done == total {
                        eprint!("\r  {}/{} cells", done, total);
                    }
                }
                results
            })
        }).collect();

        handles.into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect()
    });

    let grid_elapsed = grid_start.elapsed();

    if total >= 200 {
        eprintln!(); // newline after progress counter
    }

    // Collect into GridResult
    let mut cells: HashMap<(String, usize), Observation> = HashMap::new();
    let mut setup_failures: HashMap<String, String> = HashMap::new();
    let mut timeout_count = 0usize;

    for (ctx_name, ri, result) in results {
        match result {
            Ok(obs) => {
                if obs.resources.wall_time_ms >= (CELL_TIMEOUT_SECS * 1000 - 100) {
                    timeout_count += 1;
                }
                cells.insert((ctx_name, ri), obs);
            }
            Err(e) => { setup_failures.entry(ctx_name).or_insert(e); }
        }
    }

    let cells_per_sec = if grid_elapsed.as_secs() > 0 {
        total as u64 / grid_elapsed.as_secs()
    } else {
        total as u64
    };
    eprintln!("  grid: {} cells in {:.1}s ({} cells/s, {} timeouts)",
        total, grid_elapsed.as_secs_f64(), cells_per_sec, timeout_count);

    Ok(GridResult {
        cells,
        setup_failures,
        context_count: script.contexts.len(),
    })
}

fn run_invocation(
    binary: &str,
    test: &Run,
    work_dir: &Path,
    env_vars: &HashMap<String, String>,
    sandbox: &Sandbox,
) -> Result<Observation> {
    // Snapshot before
    let before = snapshot_fs(work_dir);

    // Create a separate trace dir if tracing is enabled
    let trace_dir = if sandbox.strace.is_some() {
        Some(tempfile::Builder::new().prefix("bgrid_trace_").tempdir()
            .context("create trace dir")?)
    } else {
        None
    };
    let trace_path = trace_dir.as_ref().map(|d| d.path());

    let str_args: Vec<&str> = test.args.iter().map(|s| s.as_str()).collect();
    let mut cmd = sandbox.command(binary, &str_args, work_dir, env_vars, trace_path);

    match &test.stdin {
        Some(StdinSource::Lines(_)) => {
            cmd.stdin(Stdio::piped());
        }
        Some(StdinSource::FromFile(path)) => {
            let full = work_dir.join(path);
            let file = std::fs::File::open(&full)
                .with_context(|| format!("open stdin file {}", path))?;
            cmd.stdin(Stdio::from(file));
        }
        None => {
            cmd.stdin(Stdio::null());
        }
    }

    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    // New process group so timeout can kill the entire tree
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe { cmd.pre_exec(|| { libc::setpgid(0, 0); Ok(()) }); }
    }

    let mut child = cmd.spawn()
        .with_context(|| format!("spawn {} {:?}", binary, test.args))?;

    if let Some(StdinSource::Lines(lines)) = &test.stdin {
        use std::io::Write;
        if let Some(mut stdin) = child.stdin.take() {
            let content = lines.join("\n") + "\n";
            let _ = stdin.write_all(content.as_bytes());
        }
    }

    let wall_start = std::time::Instant::now();

    // Per-cell timeout: kill the process group
    let child_id = child.id();
    let timer = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(CELL_TIMEOUT_SECS));
        unsafe { libc::kill(-(child_id as i32), libc::SIGKILL); }
    });

    let output = child.wait_with_output()
        .with_context(|| format!("wait for {} {:?}", binary, test.args))?;
    drop(timer);

    let wall_time = wall_start.elapsed();
    let resources = ResourceUsage {
        wall_time_ms: wall_time.as_millis() as u64,
    };

    // Snapshot after and diff
    let after = snapshot_fs(work_dir);
    let fs_changes = diff_snapshots(&before, &after);

    // Parse trace if available
    let trace = if let Some(ref td) = trace_dir {
        sandbox::parse_trace(td.path())
    } else {
        sandbox::TraceData {
            reads: Vec::new(), failed: Vec::new(),
            execs: Vec::new(), net: Vec::new(), signals: Vec::new(),
        }
    };

    Ok(Observation {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        exit_code: output.status.code(),
        fs_changes,
        resources,
        trace_reads: trace.reads,
        trace_failed: trace.failed,
        trace_execs: trace.execs,
        trace_net: trace.net,
        trace_signals: trace.signals,
    })
}

/// Group observations by identical output. Returns (context_names, representative_obs) groups.
pub fn collapse<'a>(
    obs_list: &[(&'a str, &'a Observation)],
) -> Vec<(Vec<&'a str>, &'a Observation)> {
    let mut groups: Vec<(Vec<&'a str>, &'a Observation)> = Vec::new();
    for (ctx, obs) in obs_list {
        let found = groups.iter_mut().find(|(_, existing)| {
            existing.stdout == obs.stdout
                && existing.stderr == obs.stderr
                && existing.exit_code == obs.exit_code
                && existing.fs_changes == obs.fs_changes
        });
        if let Some((names, _)) = found {
            names.push(ctx);
        } else {
            groups.push((vec![ctx], obs));
        }
    }
    groups
}

/// Compute line-level diff between two observations.
pub fn compute_diff(reference: &Observation, option: &Observation) -> Vec<String> {
    let mut lines = Vec::new();

    let ref_lines: HashSet<&str> = reference.stdout.lines().collect();
    let opt_lines: HashSet<&str> = option.stdout.lines().collect();
    let ref_vec: Vec<&str> = reference.stdout.lines().collect();
    let opt_vec: Vec<&str> = option.stdout.lines().collect();

    let only_in_ref: Vec<&&str> = ref_vec.iter().filter(|l| !opt_lines.contains(**l)).collect();
    let only_in_opt: Vec<&&str> = opt_vec.iter().filter(|l| !ref_lines.contains(**l)).collect();
    let shared: Vec<&&str> = ref_vec.iter().filter(|l| opt_lines.contains(**l)).collect();

    if ref_vec == opt_vec {
        // stdout identical
    } else if only_in_opt.is_empty() && only_in_ref.is_empty() {
        lines.push("stdout: same lines, different order".into());
    } else {
        if !only_in_opt.is_empty() {
            let preview: Vec<&str> = only_in_opt.iter().take(5).map(|l| **l).collect();
            lines.push(format!("{} only in this: {}", only_in_opt.len(), preview.join(", ")));
        }
        if !only_in_ref.is_empty() {
            let preview: Vec<&str> = only_in_ref.iter().take(5).map(|l| **l).collect();
            lines.push(format!("{} only in ref: {}", only_in_ref.len(), preview.join(", ")));
        }
        if !shared.is_empty() {
            lines.push(format!("{} shared", shared.len()));
        }
    }

    if reference.exit_code != option.exit_code {
        lines.push(format!("exit: {} → {}",
            reference.exit_code.unwrap_or(-1),
            option.exit_code.unwrap_or(-1)));
    }

    if reference.stderr != option.stderr {
        if reference.stderr.is_empty() && !option.stderr.is_empty() {
            lines.push(format!("stderr added: {}", option.stderr.trim()));
        } else if !reference.stderr.is_empty() && option.stderr.is_empty() {
            lines.push("stderr removed".into());
        } else {
            lines.push("stderr changed".into());
        }
    }

    let ref_fs: HashSet<&FsChange> = reference.fs_changes.iter().collect();
    let opt_fs: HashSet<&FsChange> = option.fs_changes.iter().collect();
    for c in option.fs_changes.iter().filter(|c| !ref_fs.contains(c)) {
        lines.push(format!("fs additional: {:?}", c));
    }
    for c in reference.fs_changes.iter().filter(|c| !opt_fs.contains(c)) {
        lines.push(format!("fs missing: {:?}", c));
    }

    lines
}

/// Count cells in the execution grid (contexts × applicable runs).
pub fn count_cells(script: &Script) -> usize {
    let mut count = 0;
    for run in &script.runs {
        for ctx in &script.contexts {
            if run_matches_context(run, ctx) {
                count += 1;
            }
        }
    }
    count
}

/// Check if a run should execute in a given context.
pub fn run_matches_context(run: &Run, ctx: &crate::parse::NamedContext) -> bool {
    if let Some(ref scoped) = run.in_contexts {
        scoped.iter().any(|s| {
            *s == ctx.name
            || ctx.name.starts_with(&format!("{} / ", s))
            || ctx.extends.as_deref() == Some(s.as_str())
        })
    } else {
        true
    }
}

/// Validate that from-references have matching standalone runs.
pub fn validate_from_references(script: &Script) {
    for run in &script.runs {
        if let Some(ref ref_args) = run.diff_from {
            let has_match = script.runs.iter().any(|r| r.args == *ref_args && r.diff_from.is_none());
            if !has_match {
                let args_str = ref_args.iter().map(|a| format!("\"{}\"", a)).collect::<Vec<_>>().join(" ");
                eprintln!("warning: from {} has no matching standalone run (add `run {}` outside any from block)", args_str, args_str);
            }
        }
    }
}
