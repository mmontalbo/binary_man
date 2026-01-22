use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const DEFAULT_LENS_FLAKE: &str = "../binary_lens#binary_lens";
const DEFAULT_USAGE_LENS: &str = "queries/ls_usage_evidence.sql";

#[derive(Deserialize)]
pub struct BinaryHashes {
    pub sha256: String,
    pub md5: Option<String>,
}

#[derive(Deserialize)]
pub struct ToolInfo {
    pub name: String,
    pub version: String,
    pub revision: Option<String>,
}

#[derive(Deserialize)]
pub struct PackManifest {
    pub binary_hashes: BinaryHashes,
    pub binary_lens_version: String,
    pub binary_name: String,
    pub binary_path: String,
    pub format_version: String,
    pub tool: ToolInfo,
}

#[derive(Deserialize)]
pub struct UsageEvidenceRow {
    pub status: String,
    pub basis: String,
    pub string_value: Option<String>,
}

pub struct UsageLensOutput {
    pub rows: Vec<UsageEvidenceRow>,
    pub raw_json: Vec<u8>,
    pub template_path: PathBuf,
    pub template_sql: String,
    pub rendered_sql: String,
}

pub struct PackContext {
    pub manifest: PackManifest,
    pub help_text: String,
    pub warnings: Vec<String>,
    pub usage_lens: UsageLensOutput,
}

struct HelpExtraction {
    text: String,
    warnings: Vec<String>,
}

struct LensQuery {
    template_path: PathBuf,
    template_sql: String,
    rendered_sql: String,
}

pub fn resolve_pack_root(path: &Path) -> Result<PathBuf> {
    let path = path
        .canonicalize()
        .with_context(|| format!("resolve pack path {}", path.display()))?;
    if path.is_dir() && path.file_name().and_then(|name| name.to_str()) == Some("binary.lens") {
        return Ok(path);
    }
    let candidate = path.join("binary.lens");
    if candidate.is_dir() {
        return Ok(candidate);
    }
    Err(anyhow!(
        "pack root not found; expected binary.lens at {}",
        path.display()
    ))
}

pub fn generate_pack(binary: &str, out_dir: &Path) -> Result<PathBuf> {
    fs::create_dir_all(out_dir).context("create pack output dir")?;

    let out_dir_str = out_dir
        .to_str()
        .ok_or_else(|| anyhow!("pack output path is not valid UTF-8"))?;

    let output = Command::new("nix")
        .args([
            "run",
            DEFAULT_LENS_FLAKE,
            "--",
            binary,
            "-o",
            out_dir_str,
        ])
        .output()
        .context("run binary_lens via nix")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("binary_lens failed: {}", stderr.trim()));
    }

    let pack_root = out_dir.join("binary.lens");
    if !pack_root.is_dir() {
        return Err(anyhow!(
            "binary_lens did not produce binary.lens under {}",
            out_dir.display()
        ));
    }

    Ok(pack_root)
}

pub fn load_pack_context(pack_root: &Path) -> Result<PackContext> {
    let manifest_path = pack_root.join("manifest.json");
    let manifest_bytes = fs::read(&manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    let manifest: PackManifest = serde_json::from_slice(&manifest_bytes)
        .context("parse pack manifest")?;

    let usage_lens = run_usage_lens(pack_root)?;
    if usage_lens.rows.is_empty() {
        return Err(anyhow!("usage lens returned no rows"));
    }

    let help = extract_help_text_from_usage_evidence(&usage_lens.rows, &manifest.binary_name);
    if help.text.trim().is_empty() {
        return Err(anyhow!("usage evidence produced empty help text"));
    }

    Ok(PackContext {
        manifest,
        help_text: help.text,
        warnings: help.warnings,
        usage_lens,
    })
}

fn run_usage_lens(pack_root: &Path) -> Result<UsageLensOutput> {
    let query = render_usage_lens(pack_root)?;
    let output = run_duckdb_query(&query.rendered_sql, pack_root)?;
    let rows: Vec<UsageEvidenceRow> = serde_json::from_slice(&output)
        .context("parse usage evidence JSON output")?;

    Ok(UsageLensOutput {
        rows,
        raw_json: output,
        template_path: query.template_path,
        template_sql: query.template_sql,
        rendered_sql: query.rendered_sql,
    })
}

fn render_usage_lens(pack_root: &Path) -> Result<LensQuery> {
    let lens_path = PathBuf::from(DEFAULT_USAGE_LENS);
    let lens_path = lens_path
        .canonicalize()
        .with_context(|| format!("resolve usage lens path {}", lens_path.display()))?;
    let template_sql = fs::read_to_string(&lens_path)
        .with_context(|| format!("read usage lens {}", lens_path.display()))?;

    let call_edges = facts_relative_path(pack_root, "call_edges.parquet")?;
    let callgraph_nodes = facts_relative_path(pack_root, "callgraph_nodes.parquet")?;
    let callsite_args = facts_relative_path(pack_root, "callsite_arg_observations.parquet")?;
    let callsites = facts_relative_path(pack_root, "callsites.parquet")?;
    let strings = facts_relative_path(pack_root, "strings.parquet")?;

    let mut rendered_sql = template_sql.clone();
    let replacements = [
        ("{{call_edges}}", call_edges),
        ("{{callgraph_nodes}}", callgraph_nodes),
        ("{{callsite_arg_observations}}", callsite_args),
        ("{{callsites}}", callsites),
        ("{{strings}}", strings),
    ];
    for (token, path) in replacements {
        rendered_sql = rendered_sql.replace(token, &sql_quote_literal(&path));
    }

    Ok(LensQuery {
        template_path: lens_path,
        template_sql,
        rendered_sql,
    })
}

fn facts_relative_path(pack_root: &Path, file_name: &str) -> Result<String> {
    let path = pack_root.join("facts").join(file_name);
    if !path.is_file() {
        return Err(anyhow!(
            "facts parquet not found at {}",
            path.display()
        ));
    }
    Ok(format!("facts/{}", file_name))
}

fn run_duckdb_query(sql: &str, cwd: &Path) -> Result<Vec<u8>> {
    let output = Command::new("nix")
        .args(["run", "nixpkgs#duckdb", "--", "-json", "-c"])
        .arg(sql)
        .current_dir(cwd)
        .output()
        .context("run duckdb query")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("duckdb failed: {}", stderr.trim()));
    }

    Ok(output.stdout)
}

fn sql_quote_literal(value: &str) -> String {
    value.replace('\'', "''")
}

fn extract_help_text_from_usage_evidence(
    rows: &[UsageEvidenceRow],
    binary_name: &str,
) -> HelpExtraction {
    let mut warnings = Vec::new();
    if rows.is_empty() {
        warnings.push("usage evidence is empty".to_string());
        return HelpExtraction {
            text: String::new(),
            warnings,
        };
    }

    let mut seen = HashSet::new();
    let mut text = String::new();

    for row in rows.iter().filter(|row| is_reliable_string(row)) {
        let value = match row.string_value.as_ref() {
            Some(value) => value,
            None => continue,
        };
        let cleaned = replace_program_name(value, binary_name);
        if seen.insert(cleaned.clone()) {
            text.push_str(&cleaned);
            if !text.ends_with('\n') {
                text.push('\n');
            }
        }
    }

    if text.trim().is_empty() {
        warnings.push("no resolved usage strings; falling back to unresolved".to_string());
        for row in rows {
            let value = match row.string_value.as_ref() {
                Some(value) => value,
                None => continue,
            };
            let cleaned = replace_program_name(value, binary_name);
            if seen.insert(cleaned.clone()) {
                text.push_str(&cleaned);
                if !text.ends_with('\n') {
                    text.push('\n');
                }
            }
        }
    }

    if text.trim().is_empty() {
        warnings.push("usage evidence did not yield help text".to_string());
    }

    HelpExtraction { text, warnings }
}

fn is_reliable_string(row: &UsageEvidenceRow) -> bool {
    row.status == "resolved" && (row.basis == "string_direct" || row.basis == "string_gettext")
}

fn replace_program_name(text: &str, binary_name: &str) -> String {
    text.replace("%s", binary_name)
}
