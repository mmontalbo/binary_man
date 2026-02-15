//! Pack generation and pack-context helpers.
//!
//! External pack extraction is isolated here so the rest of the workflow
//! can remain deterministic and pack-owned.
use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Instant;

/// Hashes recorded for the analyzed binary.
#[derive(Deserialize, Clone)]
pub struct BinaryHashes {
    pub sha256: String,
    pub md5: Option<String>,
}

/// Tool metadata emitted by binary_lens.
#[derive(Deserialize, Clone)]
pub struct ToolInfo {
    pub name: String,
    pub version: String,
    pub revision: Option<String>,
}

/// Manifest describing the exported pack.
#[derive(Deserialize, Clone)]
pub struct PackManifest {
    pub binary_hashes: BinaryHashes,
    pub binary_lens_version: String,
    pub binary_name: String,
    pub binary_path: String,
    pub format_version: String,
    pub tool: ToolInfo,
}

/// Row emitted by the usage lens for help text extraction.
#[derive(Deserialize)]
pub struct UsageEvidenceRow {
    #[serde(default)]
    pub string_value: Option<String>,
}

/// Usage lens output plus the template that produced it.
pub struct UsageLensOutput {
    pub rows: Vec<UsageEvidenceRow>,
    pub template_path: PathBuf,
}

/// Parsed pack context used for rendering and status evaluation.
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

/// Generate a pack using binary_lens, optionally with a plan and an anchor pack.
pub fn generate_pack_with_plan(
    binary: &str,
    out_dir: &Path,
    lens_flake: &str,
    plan_path: Option<&Path>,
    from_pack: Option<&Path>,
) -> Result<PathBuf> {
    fs::create_dir_all(out_dir).context("create pack output dir")?;

    let out_dir_str = path_to_string(out_dir, "pack output")?;
    let plan_str = match plan_path {
        Some(path) => Some(path_to_string(path, "export plan")?),
        None => None,
    };
    let from_pack_str = match from_pack {
        Some(path) => Some(path_to_string(path, "from-pack")?),
        None => None,
    };

    let args = build_export_args(
        binary,
        &out_dir_str,
        plan_str.as_deref(),
        from_pack_str.as_deref(),
    );
    let output = run_binary_lens(lens_flake, &args)?;
    if !output.status.success() {
        return Err(anyhow!("binary_lens failed: {}", stderr_trim(&output)));
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

fn build_export_args(
    binary: &str,
    out_dir: &str,
    plan: Option<&str>,
    from_pack: Option<&str>,
) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(from_pack) = from_pack {
        args.push("--from-pack".to_string());
        args.push(from_pack.to_string());
        args.push("--in-place".to_string());
    } else {
        args.push(binary.to_string());
    }
    args.push("-o".to_string());
    args.push(out_dir.to_string());
    if let Some(plan) = plan {
        args.push("--plan".to_string());
        args.push(plan.to_string());
    }
    args
}

fn run_binary_lens(lens_flake: &str, args: &[String]) -> Result<Output> {
    Command::new("nix")
        .args(["run", lens_flake, "--"])
        .args(args)
        .output()
        .context("run binary_lens via nix")
}

fn path_to_string(path: &Path, label: &str) -> Result<String> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow!("{label} path is not valid UTF-8"))
}

fn stderr_trim(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).trim().to_string()
}

/// Load a pack context by running the usage lens at the provided template.
pub fn load_pack_context_with_template_at(
    pack_root: &Path,
    template_path: &Path,
    duckdb_cwd: &Path,
    scenarios_glob: Option<&str>,
) -> Result<PackContext> {
    let manifest = load_manifest(pack_root)?;
    let usage_lens = run_usage_lens_at(pack_root, template_path, duckdb_cwd, scenarios_glob)?;
    if usage_lens.rows.is_empty() {
        return Err(anyhow!("usage lens returned no rows"));
    }

    let help = extract_help_text_from_usage_evidence(&usage_lens.rows);
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

/// Load the pack manifest from disk.
pub fn load_manifest(pack_root: &Path) -> Result<PackManifest> {
    let manifest_path = pack_root.join("manifest.json");
    let manifest_bytes =
        fs::read(&manifest_path).with_context(|| format!("read {}", manifest_path.display()))?;
    let manifest: PackManifest =
        serde_json::from_slice(&manifest_bytes).context("parse pack manifest")?;
    Ok(manifest)
}

fn run_usage_lens_at(
    pack_root: &Path,
    template_path: &Path,
    duckdb_cwd: &Path,
    scenarios_glob: Option<&str>,
) -> Result<UsageLensOutput> {
    let rendered_sql = render_usage_lens(pack_root, template_path, scenarios_glob)?;
    let output = run_duckdb_query(&rendered_sql, duckdb_cwd)?;
    let rows: Vec<UsageEvidenceRow> =
        serde_json::from_slice(&output).context("parse usage evidence JSON output")?;

    Ok(UsageLensOutput {
        rows,
        template_path: template_path.to_path_buf(),
    })
}

fn render_usage_lens(
    pack_root: &Path,
    template_path: &Path,
    scenarios_glob: Option<&str>,
) -> Result<String> {
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
    if template_sql.contains("{{scenarios_glob}}") {
        let scenarios_glob = scenarios_glob.ok_or_else(|| {
            anyhow!(
                "usage lens template {} requires scenarios_glob",
                template_path.display()
            )
        })?;
        rendered_sql =
            rendered_sql.replace("{{scenarios_glob}}", &sql_quote_literal(scenarios_glob));
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
    let start = Instant::now();
    let output = Command::new("nix")
        .args(["run", "nixpkgs#duckdb", "--", "-json", "-c"])
        .arg(sql)
        .current_dir(cwd)
        .output()
        .context("run duckdb query")?;
    let elapsed_ms = start.elapsed().as_millis();

    tracing::info!(
        elapsed_ms,
        sql_bytes = sql.len(),
        stdout_bytes = output.stdout.len(),
        "duckdb query complete"
    );

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

fn extract_help_text_from_usage_evidence(rows: &[UsageEvidenceRow]) -> HelpExtraction {
    let mut warnings = Vec::new();
    if rows.is_empty() {
        warnings.push("usage evidence is empty".to_string());
        return HelpExtraction {
            text: String::new(),
            warnings,
        };
    }

    let mut text = String::new();

    for row in rows {
        let Some(value) = row.string_value.as_ref() else {
            continue;
        };
        text.push_str(value);
        if !text.ends_with('\n') {
            text.push('\n');
        }
    }

    if text.trim().is_empty() {
        warnings.push("usage evidence did not yield help text".to_string());
    }

    HelpExtraction { text, warnings }
}
