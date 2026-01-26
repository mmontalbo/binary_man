use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Deserialize, Clone)]
pub struct BinaryHashes {
    pub sha256: String,
    pub md5: Option<String>,
}

#[derive(Deserialize, Clone)]
pub struct ToolInfo {
    pub name: String,
    pub version: String,
    pub revision: Option<String>,
}

#[derive(Deserialize, Clone)]
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
    pub template_path: PathBuf,
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

pub fn generate_pack(binary: &str, out_dir: &Path, lens_flake: &str) -> Result<PathBuf> {
    fs::create_dir_all(out_dir).context("create pack output dir")?;

    let out_dir_str = out_dir
        .to_str()
        .ok_or_else(|| anyhow!("pack output path is not valid UTF-8"))?;

    let output = Command::new("nix")
        .args(["run", lens_flake, "--", binary, "-o", out_dir_str])
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

pub fn load_pack_context_with_template(
    pack_root: &Path,
    template_path: &Path,
) -> Result<PackContext> {
    let manifest = load_manifest(pack_root)?;
    let usage_lens = run_usage_lens(pack_root, template_path)?;
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

pub fn load_manifest(pack_root: &Path) -> Result<PackManifest> {
    let manifest_path = pack_root.join("manifest.json");
    let manifest_bytes =
        fs::read(&manifest_path).with_context(|| format!("read {}", manifest_path.display()))?;
    let manifest: PackManifest =
        serde_json::from_slice(&manifest_bytes).context("parse pack manifest")?;
    Ok(manifest)
}

fn run_usage_lens(pack_root: &Path, template_path: &Path) -> Result<UsageLensOutput> {
    let rendered_sql = render_usage_lens(pack_root, template_path)?;
    let output = run_duckdb_query(&rendered_sql, pack_root)?;
    let rows: Vec<UsageEvidenceRow> =
        serde_json::from_slice(&output).context("parse usage evidence JSON output")?;

    Ok(UsageLensOutput {
        rows,
        template_path: template_path.to_path_buf(),
    })
}

fn render_usage_lens(pack_root: &Path, template_path: &Path) -> Result<String> {
    let template_sql = fs::read_to_string(template_path)
        .with_context(|| format!("read usage lens template {}", template_path.display()))?;
    let mut rendered_sql = template_sql.clone();
    if template_sql.contains("{{call_edges}}") {
        let call_edges = facts_relative_path(pack_root, "call_edges.parquet")?;
        let callgraph_nodes = facts_relative_path(pack_root, "callgraph_nodes.parquet")?;
        let callsite_args = facts_relative_path(pack_root, "callsite_arg_observations.parquet")?;
        let callsites = facts_relative_path(pack_root, "callsites.parquet")?;
        let strings = facts_relative_path(pack_root, "strings.parquet")?;

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
    }

    let loader_path = pack_root
        .join("views")
        .join("queries")
        .join("load_tables.sql");
    if loader_path.is_file() {
        let loader_sql = fs::read_to_string(&loader_path)
            .with_context(|| format!("read usage lens loader {}", loader_path.display()))?;
        rendered_sql = format!("{loader_sql}\n\n{rendered_sql}");
    }

    Ok(rendered_sql)
}

fn facts_relative_path(pack_root: &Path, file_name: &str) -> Result<String> {
    let path = pack_root.join("facts").join(file_name);
    if !path.is_file() {
        return Err(anyhow!("facts parquet not found at {}", path.display()));
    }
    Ok(format!("facts/{}", file_name))
}

pub(crate) fn run_duckdb_query(sql: &str, cwd: &Path) -> Result<Vec<u8>> {
    let output = Command::new("nix")
        .args(["run", "nixpkgs#duckdb", "--", "-json", "-c"])
        .arg(sql)
        .current_dir(cwd)
        .output()
        .context("run duckdb query")?;

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr_trimmed = stderr.trim();
    let stderr_line = stderr_trimmed.lines().next().unwrap_or_default();
    if !output.status.success() {
        let detail = if stderr_line.is_empty() {
            format!("status {}", output.status)
        } else {
            stderr_line.to_string()
        };
        return Err(anyhow!("duckdb failed: {detail}"));
    }
    // DuckDB reports many query failures on stderr while still returning exit code 0.
    if !stderr_trimmed.is_empty()
        && (stderr_trimmed.contains("Error") || stderr_trimmed.contains("ERROR"))
    {
        let detail = if stderr_line.is_empty() {
            stderr_trimmed
        } else {
            stderr_line
        };
        return Err(anyhow!("duckdb failed: {detail}"));
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
