//! Execute the grid: states × invocations → observations.

use crate::parse::{Script, Run};
use crate::sandbox::{self, Sandbox};
use anyhow::Result;
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
const MAX_THREADS: usize = 16;

/// Run the entire grid with batched execution.
///
/// Groups cells by context. For each context, creates per-cell workspace
/// directories under a batch parent, generates a shell script that runs all
/// commands, and invokes bwrap once per context. Parallelized across contexts.
pub fn run_grid(
    binary: &str,
    script: &Script,
    probe_dir: &Path,
    sandbox: &Sandbox,
) -> Result<GridResult> {
    // Group run indices by context
    struct ContextBatch {
        ctx_index: usize,
        run_indices: Vec<usize>,
    }
    let mut batches: Vec<ContextBatch> = Vec::new();
    let mut total_cells = 0;
    for (ci, ctx) in script.contexts.iter().enumerate() {
        let runs: Vec<usize> = script.runs.iter().enumerate()
            .filter(|(_, run)| run_matches_context(run, ctx))
            .map(|(ri, _)| ri)
            .collect();
        total_cells += runs.len();
        if !runs.is_empty() {
            batches.push(ContextBatch { ctx_index: ci, run_indices: runs });
        }
    }

    let completed = AtomicUsize::new(0);
    let grid_start = std::time::Instant::now();

    // Process contexts in parallel
    let results: Vec<_> = std::thread::scope(|s| {
        let n_threads = MAX_THREADS.min(batches.len()).max(1);
        let chunk_size = batches.len().div_ceil(n_threads);
        let chunks: Vec<&[ContextBatch]> = batches.chunks(chunk_size).collect();

        let handles: Vec<_> = chunks.into_iter().map(|chunk| {
            let completed = &completed;
            s.spawn(move || {
                let mut results: Vec<(String, usize, Result<Observation, String>)> = Vec::new();

                for batch in chunk {
                    let ctx = &script.contexts[batch.ctx_index];

                    // Create batch directory with per-cell workspaces + output dir
                    let batch_dir = match tempfile::Builder::new()
                        .prefix("bgrid_batch_")
                        .tempdir() {
                        Ok(d) => d,
                        Err(e) => {
                            for &ri in &batch.run_indices {
                                results.push((ctx.name.clone(), ri, Err(format!("create batch dir: {}", e))));
                            }
                            continue;
                        }
                    };

                    let out_dir = batch_dir.path().join("out");
                    let _ = std::fs::create_dir(&out_dir);

                    // Create per-cell workspaces and take before-snapshots
                    let mut cell_data: Vec<(usize, FsSnapshot)> = Vec::new();
                    let mut env_vars = HashMap::new();
                    let mut setup_failed = false;

                    for (cell_idx, &ri) in batch.run_indices.iter().enumerate() {
                        let cell_dir = batch_dir.path().join(format!("c{}", cell_idx));
                        let _ = std::fs::create_dir(&cell_dir);

                        match sandbox::apply_setup(&cell_dir, binary, &ctx.commands, probe_dir, sandbox) {
                            Ok(env) => {
                                if cell_idx == 0 { env_vars = env; }
                            }
                            Err(e) => {
                                for &ri2 in &batch.run_indices {
                                    results.push((ctx.name.clone(), ri2, Err(format!("{}", e))));
                                }
                                setup_failed = true;
                                break;
                            }
                        }

                        let before = snapshot_fs(&cell_dir);
                        cell_data.push((ri, before));
                    }

                    if setup_failed { continue; }

                    // Generate shell script
                    let script_path = batch_dir.path().join("run.sh");
                    {
                        let mut script_content = String::new();

                        // Hoist Extract expressions to variables at the top of the
                        // batch script. All cells in a batch share the same context,
                        // so the expression result is constant — evaluate once.
                        let mut extract_vars: HashMap<String, String> = HashMap::new();
                        let mut extract_counter = 0usize;
                        for &ri in &batch.run_indices {
                            for arg in &script.runs[ri].args {
                                if let crate::parse::Arg::Extract(e) = arg {
                                    if !extract_vars.contains_key(e) {
                                        let var = format!("_E{}", extract_counter);
                                        extract_counter += 1;
                                        extract_vars.insert(e.clone(), var);
                                    }
                                }
                            }
                        }
                        if !extract_vars.is_empty() {
                            // Evaluate in the first cell's working directory
                            script_content.push_str("cd /batch/c0\n");
                            for (expr, var) in &extract_vars {
                                script_content.push_str(&format!("{}=$({})\n", var, expr));
                            }
                        }

                        for (cell_idx, &ri) in batch.run_indices.iter().enumerate() {
                            let run = &script.runs[ri];
                            let args_str: String = run.args.iter()
                                .map(|a| match a {
                                    crate::parse::Arg::Literal(s) => shell_escape(s),
                                    crate::parse::Arg::Extract(e) => {
                                        let var = &extract_vars[e];
                                        format!("\"${}\"", var)
                                    }
                                })
                                .collect::<Vec<_>>()
                                .join(" ");

                            // Handle stdin from context (not run)
                            let stdin_part = match &ctx.stdin {
                                Some(crate::parse::StdinSource::Lines(lines)) => {
                                    let content = lines.join("\n");
                                    format!("printf '{}' | ", content.replace('\'', "'\\''"))
                                }
                                Some(crate::parse::StdinSource::FromFile(path)) => {
                                    format!("cat /batch/c{}/{} | ", cell_idx, shell_escape(path))
                                }
                                None => String::new(),
                            };

                            // Set per-context env vars inline
                            let env_str: String = env_vars.iter()
                                .map(|(k, v)| format!("{}={}", k, shell_escape(v)))
                                .collect::<Vec<_>>()
                                .join(" ");
                            let env_prefix = if env_str.is_empty() { String::new() }
                                else { format!("{} ", env_str) };

                            script_content.push_str(&format!(
                                "cd /batch/c{} && {}timeout {} {}{}{}>/batch/out/{}.out 2>/batch/out/{}.err; echo $? >/batch/out/{}.rc\n",
                                cell_idx, stdin_part, CELL_TIMEOUT_SECS,
                                env_prefix, shell_escape(binary), if args_str.is_empty() { String::new() } else { format!(" {}", args_str) },
                                cell_idx, cell_idx, cell_idx,
                            ));
                        }
                        if let Err(e) = std::fs::write(&script_path, &script_content) {
                            for &ri in &batch.run_indices {
                                results.push((ctx.name.clone(), ri, Err(format!("write script: {}", e))));
                            }
                            continue;
                        }
                    }

                    // Run bwrap with the batch
                    let mut cmd = sandbox.batch_command(batch_dir.path(), "run.sh", &HashMap::new());
                    cmd.stdin(Stdio::null());
                    cmd.stdout(Stdio::null());
                    cmd.stderr(Stdio::null());

                    // Process group for overall timeout
                    #[cfg(unix)]
                    {
                        use std::os::unix::process::CommandExt;
                        unsafe { cmd.pre_exec(|| { libc::setpgid(0, 0); Ok(()) }); }
                    }

                    let batch_timeout = CELL_TIMEOUT_SECS * (batch.run_indices.len() as u64 + 1);
                    let child = cmd.spawn();
                    match child {
                        Ok(mut child) => {
                            let child_id = child.id();
                            let timer = std::thread::spawn(move || {
                                std::thread::sleep(std::time::Duration::from_secs(batch_timeout));
                                unsafe { libc::kill(-(child_id as i32), libc::SIGKILL); }
                            });
                            let _ = child.wait();
                            drop(timer);
                        }
                        Err(e) => {
                            for &ri in &batch.run_indices {
                                results.push((ctx.name.clone(), ri, Err(format!("spawn bwrap: {}", e))));
                            }
                            continue;
                        }
                    }

                    // Read results and build observations
                    for (cell_idx, (ri, before)) in cell_data.into_iter().enumerate() {
                        let stdout = std::fs::read_to_string(out_dir.join(format!("{}.out", cell_idx)))
                            .unwrap_or_default();
                        let stderr = std::fs::read_to_string(out_dir.join(format!("{}.err", cell_idx)))
                            .unwrap_or_default();
                        let exit_str = std::fs::read_to_string(out_dir.join(format!("{}.rc", cell_idx)))
                            .unwrap_or_default();
                        let exit_code: Option<i32> = exit_str.trim().parse().ok();

                        let cell_dir = batch_dir.path().join(format!("c{}", cell_idx));
                        let after = snapshot_fs(&cell_dir);
                        let fs_changes = diff_snapshots(&before, &after);

                        // Estimate wall time (we don't have per-command timing in batch mode)
                        let wall_time_ms = if exit_code == Some(137) || exit_code == Some(-1) {
                            CELL_TIMEOUT_SECS * 1000
                        } else {
                            0 // unknown in batch mode
                        };

                        results.push((ctx.name.clone(), ri, Ok(Observation {
                            stdout, stderr, exit_code, fs_changes,
                            resources: ResourceUsage { wall_time_ms },
                        })));
                    }

                    let done = completed.fetch_add(batch.run_indices.len(), Ordering::Relaxed)
                        + batch.run_indices.len();
                    if total_cells >= 200 {
                        eprint!("\r  {}/{} cells", done, total_cells);
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

    if total_cells >= 200 {
        eprintln!();
    }

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
        total_cells as u64 / grid_elapsed.as_secs()
    } else {
        total_cells as u64
    };
    eprintln!("  grid: {} cells in {:.1}s ({} cells/s, {} timeouts)",
        total_cells, grid_elapsed.as_secs_f64(), cells_per_sec, timeout_count);

    Ok(GridResult {
        cells,
        setup_failures,
        context_count: script.contexts.len(),
    })
}

/// Escape a string for shell use.
fn shell_escape(s: &str) -> String {
    if s.is_empty() { return "''".to_string(); }
    if s.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-/=".contains(&b)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
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
                let args_str = ref_args.iter().map(|a| a.display()).collect::<Vec<_>>().join(" ");
                eprintln!("warning: from {} has no matching standalone run (add `run {}` outside any from block)", args_str, args_str);
            }
        }
    }
}
