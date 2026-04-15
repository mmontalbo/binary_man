use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Top-level experiment directory.
#[derive(Debug)]
pub struct Experiment {
    pub name: String,
    pub cells: Vec<Cell>,
}

/// A single cell (lm × binary) within an experiment.
#[derive(Debug)]
pub struct Cell {
    pub key: String,
    pub path: PathBuf,
    pub summary: Option<CellSummary>,
}

#[derive(Debug, Deserialize)]
pub struct CellSummary {
    pub total_surfaces: usize,
    pub mean_verified: f64,
    pub mean_cycles: f64,
    pub mean_elapsed: f64,
}

/// Loaded state for a specific cell run.
#[derive(Debug)]
pub struct CellState {
    pub surfaces: Vec<Surface>,
    pub lm_log_dir: PathBuf,
}

/// A surface extracted from state.json.
#[derive(Debug, Clone)]
pub struct Surface {
    pub id: String,
    pub description: String,
    pub status: String,
    pub probes: Vec<ProbeEvent>,
    pub attempts: Vec<AttemptEvent>,
    pub characterization: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ProbeEvent {
    pub cycle: u32,
    pub outputs_differ: bool,
    pub setup_failed: bool,
    pub stdout_preview: Option<String>,
    pub control_stdout_preview: Option<String>,
    pub raw: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct AttemptEvent {
    pub cycle: u32,
    pub outcome: String,
    pub raw: serde_json::Value,
}

/// Prompt text for a cycle (response data loaded separately via CycleAnalysisMap).
#[derive(Debug)]
pub struct Transcript {
    pub cycle: u32,
    pub prompt: String,
}

/// Characterization prompt + response pair (pre-verification).
#[derive(Debug)]
pub struct CharacterizationLog {
    pub chunk: u32,
    pub prompt: String,
    pub response: String,
}

/// Discover experiments under eval_data/.
pub fn discover_experiments(base: &Path) -> Result<Vec<Experiment>> {
    let mut experiments = Vec::new();
    let entries = std::fs::read_dir(base).context("read eval_data")?;

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        // Experiment has a cells/ subdirectory (matrix format)
        let cells_dir = path.join("cells");
        let cells = if cells_dir.is_dir() {
            discover_cells(&cells_dir)?
        } else {
            // Legacy format: commit hash dirs directly
            discover_legacy_cells(&path)?
        };

        if !cells.is_empty() {
            experiments.push(Experiment { name, cells });
        }
    }

    experiments.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(experiments)
}

fn discover_cells(cells_dir: &Path) -> Result<Vec<Cell>> {
    let mut cells = Vec::new();
    for entry in std::fs::read_dir(cells_dir)?.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let key = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        let summary = load_summary(&path);
        cells.push(Cell { key, path, summary });
    }
    cells.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(cells)
}

fn discover_legacy_cells(exp_dir: &Path) -> Result<Vec<Cell>> {
    let mut cells = Vec::new();
    for entry in std::fs::read_dir(exp_dir)?.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if path.join("summary.json").exists() || path.join("state.json").exists() {
            let key = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let summary = load_summary(&path);
            cells.push(Cell { key, path, summary });
        }
    }
    cells.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(cells)
}

fn load_summary(cell_dir: &Path) -> Option<CellSummary> {
    let path = cell_dir.join("summary.json");
    let raw = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Load cell state from run_0_state.json (or state.json for single-run packs).
pub fn load_cell_state(cell: &Cell) -> Result<CellState> {
    let state_path = if cell.path.join("run_0_state.json").exists() {
        cell.path.join("run_0_state.json")
    } else {
        cell.path.join("state.json")
    };
    let raw = std::fs::read_to_string(&state_path)
        .with_context(|| format!("read {}", state_path.display()))?;
    let state: serde_json::Value = serde_json::from_str(&raw)?;

    let mut surfaces = Vec::new();

    if let Some(entries) = state["entries"].as_array() {
        for entry in entries {
            surfaces.push(parse_surface(entry));
        }
    }

    surfaces.sort_by(|a, b| status_order(&a.status).cmp(&status_order(&b.status)));

    let lm_log_dir = if cell.path.join("run_0_lm_log").is_dir() {
        cell.path.join("run_0_lm_log")
    } else {
        cell.path.join("lm_log")
    };

    Ok(CellState {
        surfaces,
        lm_log_dir,
    })
}

fn status_order(status: &str) -> u8 {
    match status {
        "Verified" => 0,
        "Pending" => 1,
        _ => 2, // Excluded
    }
}

fn parse_surface(entry: &serde_json::Value) -> Surface {
    let id = entry["id"].as_str().unwrap_or("?").to_string();
    let description = entry["description"].as_str().unwrap_or("").to_string();

    let status = if let Some(kind) = entry["status"]["kind"].as_str() {
        kind.to_string()
    } else if let Some(s) = entry["status"].as_str() {
        s.to_string()
    } else {
        "Unknown".to_string()
    };

    let characterization = entry["characterization"]["trigger"]
        .as_str()
        .map(String::from);

    let probes = entry["probes"]
        .as_array()
        .map(|arr| arr.iter().map(parse_probe).collect())
        .unwrap_or_default();

    let attempts = entry["attempts"]
        .as_array()
        .map(|arr| arr.iter().map(parse_attempt).collect())
        .unwrap_or_default();

    Surface {
        id,
        description,
        status,
        probes,
        attempts,
        characterization,
    }
}

fn parse_probe(probe: &serde_json::Value) -> ProbeEvent {
    ProbeEvent {
        cycle: probe["cycle"].as_u64().unwrap_or(0) as u32,
        outputs_differ: probe["outputs_differ"].as_bool().unwrap_or(false),
        setup_failed: probe["setup_failed"].as_bool().unwrap_or(false),
        stdout_preview: probe["stdout_preview"].as_str().map(String::from),
        control_stdout_preview: probe["control_stdout_preview"].as_str().map(String::from),
        raw: probe.clone(),
    }
}

fn parse_attempt(attempt: &serde_json::Value) -> AttemptEvent {
    AttemptEvent {
        cycle: attempt["cycle"].as_u64().unwrap_or(0) as u32,
        outcome: attempt["outcome"]["kind"]
            .as_str()
            .unwrap_or("Unknown")
            .to_string(),
        raw: attempt.clone(),
    }
}

/// Load cycle prompts for a cell (responses loaded separately via load_cycle_data).
pub fn load_transcripts(lm_log_dir: &Path) -> Result<Vec<Transcript>> {
    if !lm_log_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut transcripts: Vec<Transcript> = Vec::new();

    for entry in std::fs::read_dir(lm_log_dir)?.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with("_prompt.md") {
            if let Some(cycle) = parse_lm_log_filename(&name) {
                let prompt = std::fs::read_to_string(entry.path()).unwrap_or_default();
                transcripts.push(Transcript { cycle, prompt });
            }
        }
    }

    transcripts.sort_by_key(|t| t.cycle);
    Ok(transcripts)
}

fn parse_lm_log_filename(name: &str) -> Option<u32> {
    // Format: c{N}_prompt.md or c{N}_response.json or c{N}_response_raw.txt
    let rest = name.strip_prefix('c')?;
    let end = rest.find('_')?;
    rest[..end].parse().ok()
}

/// Raw response text per cycle for verbatim display.
pub type CycleResponseMap = HashMap<u32, String>;

/// Load raw response text for each cycle.
/// Prefers _response.json, falls back to _response_raw.txt (stripping markdown fences).
pub fn load_cycle_responses(lm_log_dir: &Path) -> CycleResponseMap {
    let mut responses = CycleResponseMap::new();
    let dir = match std::fs::read_dir(lm_log_dir) {
        Ok(d) => d,
        Err(_) => return responses,
    };

    for entry in dir.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let cycle = match parse_lm_log_filename(&name) {
            Some(c) => c,
            None => continue,
        };

        let text = if name.ends_with("_response.json") {
            std::fs::read_to_string(entry.path()).ok()
        } else if name.ends_with("_response_raw.txt") {
            let parsed_path = entry.path().with_file_name(format!("c{}_response.json", cycle));
            if parsed_path.exists() {
                continue;
            }
            std::fs::read_to_string(entry.path())
                .ok()
                .and_then(|raw| strip_markdown_fences(&raw))
        } else {
            continue;
        };

        if let Some(text) = text {
            responses.insert(cycle, text);
        }
    }

    responses
}

/// Load characterization logs (char_N_prompt.md + char_N_response.txt).
pub fn load_characterization_logs(lm_log_dir: &Path) -> Vec<CharacterizationLog> {
    let mut logs = Vec::new();
    let dir = match std::fs::read_dir(lm_log_dir) {
        Ok(d) => d,
        Err(_) => return logs,
    };

    let mut chunks: HashMap<u32, (Option<PathBuf>, Option<PathBuf>)> = HashMap::new();

    for entry in dir.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if let Some(rest) = name.strip_prefix("char_") {
            if let Some(num_str) = rest.split('_').next() {
                if let Ok(chunk) = num_str.parse::<u32>() {
                    let e = chunks.entry(chunk).or_insert((None, None));
                    if name.ends_with("_prompt.md") {
                        e.0 = Some(entry.path());
                    } else if name.ends_with("_response.txt") {
                        e.1 = Some(entry.path());
                    }
                }
            }
        }
    }

    for (chunk, (prompt_path, response_path)) in &chunks {
        let prompt = prompt_path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_default();
        let response = response_path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_default();
        if !prompt.is_empty() || !response.is_empty() {
            logs.push(CharacterizationLog {
                chunk: *chunk,
                prompt,
                response,
            });
        }
    }

    logs.sort_by_key(|l| l.chunk);
    logs
}

/// Find the characterization log chunk that contains a specific surface.
pub fn find_char_log_for_surface<'a>(logs: &'a [CharacterizationLog], surface_id: &str) -> Option<&'a CharacterizationLog> {
    let needle = format!("### {}\n", surface_id);
    logs.iter().find(|log| log.prompt.contains(&needle))
}

/// Strip markdown code fences (```json ... ```) to get the inner JSON.
fn strip_markdown_fences(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let body = trimmed.strip_prefix("```json").or_else(|| trimmed.strip_prefix("```"))?;
    let body = body.strip_suffix("```")?;
    Some(body.trim().to_string())
}
