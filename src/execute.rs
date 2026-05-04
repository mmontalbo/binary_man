//! Execute the grid: states × invocations → observations.

use crate::parse::{Script, StdinSource, Run};
use crate::sandbox::{self, Sandbox};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;

/// Observation from a single execution.
#[derive(Debug, Clone)]
pub struct Observation {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub fs_changes: Vec<FsChange>,
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
    content_hash: u64,
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
            entries.push((rel.clone(), FileInfo { size: 0, mode, content_hash: 0 }));
            if let Ok(sub) = walk_dir(base, &path) {
                entries.extend(sub);
            }
        } else {
            let meta = path.metadata();
            let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            let mode = get_mode(&path);
            let content_hash = hash_file(&path);
            entries.push((rel, FileInfo { size, mode, content_hash }));
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

fn hash_file(path: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    use std::collections::hash_map::DefaultHasher;
    let mut hasher = DefaultHasher::new();
    if let Ok(content) = std::fs::read(path) {
        content.hash(&mut hasher);
    }
    hasher.finish()
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
            if before_info.content_hash != after_info.content_hash && before_info.size == after_info.size {
                diffs.push("content changed".to_string());
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

/// Run the entire grid.
pub fn run_grid(
    binary: &str,
    script: &Script,
    probe_dir: &Path,
    sandbox: &Sandbox,
) -> Result<GridResult> {
    let mut cells: HashMap<(String, usize), Observation> = HashMap::new();
    let mut setup_failures: HashMap<String, String> = HashMap::new();

    for ctx in &script.contexts {
        let sandbox_dir = tempfile::Builder::new()
            .prefix("bman_")
            .tempdir()
            .context("create sandbox")?;
        let work_dir = sandbox_dir.path();

        let env_vars = match sandbox::apply_setup(work_dir, binary, &ctx.commands, probe_dir, sandbox) {
            Ok(env) => env,
            Err(e) => {
                setup_failures.insert(ctx.name.clone(), format!("{}", e));
                continue;
            }
        };

        for (ti, test) in script.runs.iter().enumerate() {
            if let Some(ref scoped) = test.in_contexts {
                let matches = scoped.iter().any(|s| {
                    *s == ctx.name
                    || ctx.name.starts_with(&format!("{} / ", s))
                    || ctx.extends.as_deref() == Some(s.as_str())
                });
                if !matches { continue; }
            }

            let obs = run_invocation(binary, test, work_dir, &env_vars, sandbox)?;
            cells.insert((ctx.name.clone(), ti), obs);
        }
    }

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

    let str_args: Vec<&str> = test.args.iter().map(|s| s.as_str()).collect();
    let mut cmd = sandbox.command(binary, &str_args, work_dir, env_vars);

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

    let mut child = cmd.spawn()
        .with_context(|| format!("spawn {} {:?}", binary, test.args))?;

    if let Some(StdinSource::Lines(lines)) = &test.stdin {
        use std::io::Write;
        if let Some(mut stdin) = child.stdin.take() {
            let content = lines.join("\n") + "\n";
            let _ = stdin.write_all(content.as_bytes());
        }
    }

    let output = child.wait_with_output()
        .with_context(|| format!("wait for {} {:?}", binary, test.args))?;

    // Snapshot after and diff
    let after = snapshot_fs(work_dir);
    let fs_changes = diff_snapshots(&before, &after);

    Ok(Observation {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        exit_code: output.status.code(),
        fs_changes,
    })
}
