use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

use crate::{display, runner, summary, GitInfo};

#[derive(Deserialize)]
struct MatrixConfig {
    name: String,
    #[serde(default)]
    defaults: Defaults,
    #[serde(default)]
    parallel_groups: bool,
    groups: Vec<Group>,
}

#[derive(Deserialize)]
struct Defaults {
    #[serde(default = "default_runs")]
    runs: usize,
    #[serde(default = "default_max_cycles")]
    max_cycles: u32,
    #[serde(default = "default_timeout")]
    timeout: u64,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            runs: default_runs(),
            max_cycles: default_max_cycles(),
            timeout: default_timeout(),
        }
    }
}

fn default_runs() -> usize {
    1
}
fn default_max_cycles() -> u32 {
    80
}
fn default_timeout() -> u64 {
    1800
}

#[derive(Deserialize)]
struct Group {
    lms: Vec<String>,
    binaries: Vec<Vec<String>>,
    runs: Option<usize>,
    max_cycles: Option<u32>,
    timeout: Option<u64>,
}

struct Cell {
    key: String,
    lm: String,
    binary: String,
    entry_point: Vec<String>,
    pack_name: String,
    runs: usize,
    max_cycles: u32,
    timeout: u64,
    group_idx: usize,
    idx: usize,
}

/// Results from running one group's cells.
struct GroupResult {
    completed: usize,
    skipped: usize,
    failures: Vec<String>,
}

pub fn run(config_path: &Path, bman_bin: &str, git: &GitInfo) -> Result<()> {
    let raw = std::fs::read_to_string(config_path)
        .with_context(|| format!("read {}", config_path.display()))?;
    let config: MatrixConfig =
        toml::from_str(&raw).with_context(|| format!("parse {}", config_path.display()))?;

    let cells = expand_cells(&config);
    let total = cells.len();
    let corpus_dir = PathBuf::from("tools/eval_data").join(&config.name);

    if config.parallel_groups && config.groups.len() > 1 {
        eprintln!(
            "Matrix: {} cells in {} groups (parallel) from {}",
            total,
            config.groups.len(),
            config_path.display()
        );
        run_groups_parallel(&config, &cells, total, &corpus_dir, bman_bin, git)
    } else {
        eprintln!("Matrix: {} cells from {}", total, config_path.display());
        run_groups_sequential(&config, &cells, total, &corpus_dir, bman_bin, git)
    }
}

fn run_groups_sequential(
    config: &MatrixConfig,
    cells: &[Cell],
    total: usize,
    corpus_dir: &Path,
    bman_bin: &str,
    git: &GitInfo,
) -> Result<()> {
    let cells_dir = corpus_dir.join("cells");
    let mut completed = 0usize;
    let mut skipped = 0usize;
    let mut failures: Vec<String> = Vec::new();

    for (i, cell) in cells.iter().enumerate() {
        let r = run_cell(cell, i, total, &cells_dir, bman_bin, git, &config.name)?;
        completed += r.completed;
        skipped += r.skipped;
        failures.extend(r.failures);
    }

    write_corpus_meta(corpus_dir, &config.name, total, completed, skipped, &failures, git)?;
    print_summary(total, completed, skipped, &failures, corpus_dir);
    Ok(())
}

fn run_groups_parallel(
    config: &MatrixConfig,
    cells: &[Cell],
    total: usize,
    corpus_dir: &Path,
    bman_bin: &str,
    git: &GitInfo,
) -> Result<()> {
    let cells_dir = corpus_dir.join("cells");
    let num_groups = config.groups.len();

    // Partition cells by group index.
    let mut groups: Vec<Vec<&Cell>> = vec![Vec::new(); num_groups];
    for cell in cells {
        groups[cell.group_idx].push(cell);
    }

    let results: Vec<Result<GroupResult>> = std::thread::scope(|s| {
        let handles: Vec<_> = groups
            .into_iter()
            .map(|group_cells| {
                let cells_dir = &cells_dir;
                let name = &config.name;
                s.spawn(move || {
                    let mut gr = GroupResult {
                        completed: 0,
                        skipped: 0,
                        failures: Vec::new(),
                    };
                    for cell in group_cells {
                        let r = run_cell(cell, cell.idx, total, cells_dir, bman_bin, git, name)?;
                        gr.completed += r.completed;
                        gr.skipped += r.skipped;
                        gr.failures.extend(r.failures);
                    }
                    Ok(gr)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("group thread panicked"))
            .collect()
    });

    let mut completed = 0usize;
    let mut skipped = 0usize;
    let mut failures: Vec<String> = Vec::new();
    for r in results {
        let gr = r?;
        completed += gr.completed;
        skipped += gr.skipped;
        failures.extend(gr.failures);
    }

    write_corpus_meta(corpus_dir, &config.name, total, completed, skipped, &failures, git)?;
    print_summary(total, completed, skipped, &failures, corpus_dir);
    Ok(())
}

/// Run a single cell: skip if exists, otherwise execute trials and save.
fn run_cell(
    cell: &Cell,
    idx: usize,
    total: usize,
    cells_dir: &Path,
    bman_bin: &str,
    git: &GitInfo,
    corpus_name: &str,
) -> Result<GroupResult> {
    let cell_dir = cells_dir.join(&cell.key);
    let mut gr = GroupResult {
        completed: 0,
        skipped: 0,
        failures: Vec::new(),
    };

    if cell_dir.join("summary.json").exists() {
        eprintln!(
            "[{}/{}] {} on {}: skipped (exists)",
            idx + 1,
            total,
            cell.lm,
            cell.pack_name
        );
        gr.skipped = 1;
        gr.completed = 1;
        return Ok(gr);
    }

    eprintln!(
        "[{}/{}] {} on {}: running {} trial(s)...",
        idx + 1,
        total,
        cell.lm,
        cell.pack_name,
        cell.runs
    );

    std::fs::create_dir_all(&cell_dir)
        .with_context(|| format!("create {}", cell_dir.display()))?;

    let label = format!(
        "{}:{}",
        cell.lm.split(':').next_back().unwrap_or(&cell.lm),
        cell.pack_name
    );

    // Load any previously completed runs (from a killed experiment).
    let mut runs = Vec::new();
    for i in 0..cell.runs {
        let run_path = cell_dir.join(format!("run_{}.json", i));
        if run_path.exists() {
            if let Ok(raw) = std::fs::read_to_string(&run_path) {
                if let Ok(r) = serde_json::from_str::<summary::RunOutcome>(&raw) {
                    eprintln!("[{}] Run {}/{}... resumed from disk", label, i + 1, cell.runs);
                    runs.push(r);
                    continue;
                }
            }
        }
        break; // Runs must be contiguous from 0
    }
    let start_from = runs.len();

    for run_idx in start_from..cell.runs {
        match runner::run_single(
            bman_bin,
            &cell.binary,
            &cell.entry_point,
            cell.max_cycles,
            cell.timeout,
            run_idx,
            &cell.lm,
        ) {
            Ok(r) => {
                display::print_run_progress(&r, run_idx, cell.runs, &label);
                // Save run immediately so it survives a kill
                crate::save_single_run(&cell_dir, run_idx, &r)?;
                runs.push(r);
            }
            Err(e) => {
                eprintln!("  run {} failed: {}", run_idx, e);
                gr.failures
                    .push(format!("{}: run {} - {}", cell.key, run_idx, e));
            }
        }
    }

    if runs.is_empty() {
        gr.failures
            .push(format!("{}: all runs failed", cell.key));
        return Ok(gr);
    }

    let current = summary::build(&runs);
    let meta = serde_json::json!({
        "commit": git.commit,
        "subject": git.subject,
        "dirty": git.dirty,
        "runs": cell.runs,
        "max_cycles": cell.max_cycles,
        "timeout": cell.timeout,
        "lm": cell.lm,
        "corpus": corpus_name,
        "cell_key": cell.key,
    });
    crate::save_eval_data(&cell_dir, &runs, &current, &meta)?;
    gr.completed = 1;

    eprintln!(
        "[{}] -> {:.0}/{} verified, {:.0} cycles, {:.0}s",
        label, current.mean_verified, current.total_surfaces, current.mean_cycles, current.mean_elapsed
    );

    Ok(gr)
}

fn write_corpus_meta(
    corpus_dir: &Path,
    name: &str,
    total: usize,
    completed: usize,
    skipped: usize,
    failures: &[String],
    git: &GitInfo,
) -> Result<()> {
    let corpus_meta = serde_json::json!({
        "name": name,
        "total_cells": total,
        "completed": completed,
        "skipped": skipped,
        "failures": failures,
        "git": { "commit": git.commit, "subject": git.subject, "dirty": git.dirty },
    });
    std::fs::create_dir_all(corpus_dir)?;
    std::fs::write(
        corpus_dir.join("corpus_meta.json"),
        serde_json::to_string_pretty(&corpus_meta)?,
    )?;
    Ok(())
}

fn print_summary(total: usize, completed: usize, skipped: usize, failures: &[String], corpus_dir: &Path) {
    eprintln!(
        "\nCorpus complete: {}/{} cells ({} skipped)",
        completed, total, skipped
    );
    if !failures.is_empty() {
        eprintln!("Failures:");
        for f in failures {
            eprintln!("  - {}", f);
        }
    }
    eprintln!("Output: {}", corpus_dir.display());
}

fn expand_cells(config: &MatrixConfig) -> Vec<Cell> {
    let mut cells = Vec::new();
    for (group_idx, group) in config.groups.iter().enumerate() {
        let runs = group.runs.unwrap_or(config.defaults.runs);
        let max_cycles = group.max_cycles.unwrap_or(config.defaults.max_cycles);
        let timeout = group.timeout.unwrap_or(config.defaults.timeout);

        for lm in &group.lms {
            for bin_args in &group.binaries {
                let binary = bin_args[0].clone();
                let entry_point: Vec<String> = bin_args[1..].to_vec();
                let pack_name = crate::pack_name(&binary, &entry_point);
                let key = format!("{}__{}", slug(lm), slug(&pack_name));

                let idx = cells.len();
                cells.push(Cell {
                    key,
                    lm: lm.clone(),
                    binary,
                    entry_point,
                    pack_name,
                    runs,
                    max_cycles,
                    timeout,
                    group_idx,
                    idx,
                });
            }
        }
    }
    cells
}

fn slug(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            ':' | '/' | ' ' => '-',
            c if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' => c,
            _ => '-',
        })
        .collect()
}
