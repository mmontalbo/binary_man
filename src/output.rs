use crate::enrich;
use crate::pack;
use crate::render;
use crate::scenarios;
use crate::staging::{write_staged_json, write_staged_text};
use crate::util::display_path;
use anyhow::{Context, Result};
use serde::Serialize;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Serialize)]
struct Meta {
    schema_version: u32,
    generated_at_epoch_ms: u128,
    binary_name: String,
    binary_path: String,
    binary_sha256: String,
    binary_md5: Option<String>,
    pack_root: String,
    pack_manifest: String,
    binary_lens_version: String,
    pack_format_version: String,
    inputs_hash: Option<String>,
    tool_name: String,
    tool_version: String,
    tool_revision: Option<String>,
    usage_lens_source_path: String,
    warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    render_summary: Option<render::RenderSummary>,
    examples: Option<ExamplesMeta>,
}

#[derive(Serialize)]
struct ExamplesMeta {
    examples_report_path: String,
    runs_index_path: String,
    run_ids: Vec<String>,
    scenario_count: usize,
    pass_count: usize,
    fail_count: usize,
}

pub fn write_outputs_staged(
    staging_root: &Path,
    doc_pack_root: &Path,
    context: &pack::PackContext,
    pack_root: &Path,
    inputs_hash: Option<&str>,
    man_page: Option<&str>,
    render_summary: Option<&render::RenderSummary>,
    examples_report: Option<&scenarios::ExamplesReport>,
) -> Result<()> {
    let paths = enrich::DocPackPaths::new(doc_pack_root.to_path_buf());
    if let Some(man_page) = man_page {
        let rel = format!("man/{}.1", context.manifest.binary_name);
        write_staged_text(staging_root, &rel, man_page)?;
    }

    if let Some(report) = examples_report {
        write_staged_json(staging_root, "man/examples_report.json", report)?;
    }

    let examples_meta = examples_report.map(|report| ExamplesMeta {
        examples_report_path: display_path(&paths.examples_report_path(), Some(doc_pack_root)),
        runs_index_path: display_path(&pack_root.join("runs/index.json"), Some(doc_pack_root)),
        run_ids: report.run_ids.clone(),
        scenario_count: report.scenario_count,
        pass_count: report.pass_count,
        fail_count: report.fail_count,
    });

    let generated_at_epoch_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("compute timestamp")?
        .as_millis();

    let meta = Meta {
        schema_version: 5,
        generated_at_epoch_ms,
        binary_name: context.manifest.binary_name.clone(),
        binary_path: context.manifest.binary_path.clone(),
        binary_sha256: context.manifest.binary_hashes.sha256.clone(),
        binary_md5: context.manifest.binary_hashes.md5.clone(),
        pack_root: display_path(pack_root, Some(doc_pack_root)),
        pack_manifest: display_path(&pack_root.join("manifest.json"), Some(doc_pack_root)),
        binary_lens_version: context.manifest.binary_lens_version.clone(),
        pack_format_version: context.manifest.format_version.clone(),
        inputs_hash: inputs_hash.map(|hash| hash.to_string()),
        tool_name: context.manifest.tool.name.clone(),
        tool_version: context.manifest.tool.version.clone(),
        tool_revision: context.manifest.tool.revision.clone(),
        usage_lens_source_path: display_path(
            &context.usage_lens.template_path,
            Some(doc_pack_root),
        ),
        warnings: context.warnings.clone(),
        render_summary: render_summary.cloned(),
        examples: examples_meta,
    };

    write_staged_json(staging_root, "man/meta.json", &meta)?;

    Ok(())
}
