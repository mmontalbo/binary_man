//! Execute the grid: states × invocations → observations.

use crate::parse::{Script, StdinSource, Test};
use crate::sandbox;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::Path;
use std::process::{Command, Stdio};

/// Observation from a single execution.
#[derive(Debug, Clone)]
pub struct Observation {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub fs_changes: Vec<FsChange>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsChange {
    Created { path: String, size: u64 },
    Deleted { path: String },
    Modified { path: String, old_size: u64, new_size: u64 },
}

/// The full grid result.
#[derive(Debug)]
pub struct GridResult {
    pub cells: HashMap<(String, usize), Observation>,
    pub setup_failures: HashMap<String, String>,
    pub context_count: usize,
    pub test_count: usize,
}

/// Snapshot of the sandbox filesystem: path → size.
type FsSnapshot = HashMap<String, u64>;

fn snapshot_fs(work_dir: &Path) -> FsSnapshot {
    let mut snap = HashMap::new();
    if let Ok(entries) = walk_dir(work_dir, work_dir) {
        for (rel_path, size) in entries {
            snap.insert(rel_path, size);
        }
    }
    snap
}

fn walk_dir(base: &Path, dir: &Path) -> Result<Vec<(String, u64)>> {
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let rel = path.strip_prefix(base)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();

        if path.is_dir() && !path.is_symlink() {
            entries.push((rel.clone(), 0));
            if let Ok(sub) = walk_dir(base, &path) {
                entries.extend(sub);
            }
        } else {
            let size = path.metadata().map(|m| m.len()).unwrap_or(0);
            entries.push((rel, size));
        }
    }
    Ok(entries)
}

fn diff_snapshots(before: &FsSnapshot, after: &FsSnapshot) -> Vec<FsChange> {
    let mut changes = Vec::new();

    // Created: in after but not in before
    for (path, &size) in after {
        if !before.contains_key(path) {
            changes.push(FsChange::Created {
                path: path.clone(),
                size,
            });
        }
    }

    // Deleted: in before but not in after
    for path in before.keys() {
        if !after.contains_key(path) {
            changes.push(FsChange::Deleted {
                path: path.clone(),
            });
        }
    }

    // Modified: in both but different size
    for (path, &old_size) in before {
        if let Some(&new_size) = after.get(path) {
            if old_size != new_size {
                changes.push(FsChange::Modified {
                    path: path.clone(),
                    old_size,
                    new_size,
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
pub fn run_grid(binary: &str, script: &Script) -> Result<GridResult> {
    let mut cells: HashMap<(String, usize), Observation> = HashMap::new();
    let mut setup_failures: HashMap<String, String> = HashMap::new();

    for ctx in &script.contexts {
        let sandbox_dir = tempfile::Builder::new()
            .prefix("probe_")
            .tempdir()
            .context("create sandbox")?;
        let work_dir = sandbox_dir.path();

        match sandbox::apply_setup(work_dir, binary, &ctx.commands) {
            Ok(()) => {}
            Err(e) => {
                setup_failures.insert(ctx.name.clone(), format!("{}", e));
                continue;
            }
        }

        for (ti, test) in script.tests.iter().enumerate() {
            if let Some(ref scoped) = test.in_contexts {
                if !scoped.contains(&ctx.name) {
                    continue;
                }
            }

            let obs = run_invocation(binary, test, work_dir)?;
            cells.insert((ctx.name.clone(), ti), obs);
        }
    }

    Ok(GridResult {
        cells,
        setup_failures,
        context_count: script.contexts.len(),
        test_count: script.tests.len(),
    })
}

fn run_invocation(
    binary: &str,
    test: &Test,
    work_dir: &Path,
) -> Result<Observation> {
    // Snapshot before
    let before = snapshot_fs(work_dir);

    let mut cmd = Command::new(binary);
    cmd.args(&test.args);
    cmd.current_dir(work_dir);

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

    cmd.env_clear();
    cmd.env("PATH", std::env::var("PATH").unwrap_or_default());
    cmd.env("HOME", work_dir);
    cmd.env("LANG", "C");
    cmd.env("LC_ALL", "C");

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
