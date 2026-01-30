//! Rendering output staging for man pages and metadata.
//!
//! Outputs are staged to keep apply transactional and deterministic.
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

/// Inputs required to stage rendered outputs.
pub(crate) struct WriteOutputsArgs<'a> {
    pub(crate) staging_root: &'a Path,
    pub(crate) doc_pack_root: &'a Path,
    pub(crate) context: &'a pack::PackContext,
    pub(crate) pack_root: &'a Path,
    pub(crate) inputs_hash: Option<&'a str>,
    pub(crate) man_page: Option<&'a str>,
    pub(crate) render_summary: Option<&'a render::RenderSummary>,
    pub(crate) examples_report: Option<&'a scenarios::ExamplesReport>,
}

/// Stage render outputs and `man/meta.json` for transactional publish.
pub fn write_outputs_staged(args: &WriteOutputsArgs<'_>) -> Result<()> {
    let paths = enrich::DocPackPaths::new(args.doc_pack_root.to_path_buf());
    if let Some(man_page) = args.man_page {
        let rel = format!("man/{}.1", args.context.manifest.binary_name);
        write_staged_text(args.staging_root, &rel, man_page)?;
    }

    if let Some(report) = args.examples_report {
        write_staged_json(args.staging_root, "man/examples_report.json", report)?;
    }

    let examples_meta = args.examples_report.map(|report| ExamplesMeta {
        examples_report_path: display_path(&paths.examples_report_path(), Some(args.doc_pack_root)),
        runs_index_path: display_path(
            &args.pack_root.join("runs/index.json"),
            Some(args.doc_pack_root),
        ),
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
        binary_name: args.context.manifest.binary_name.clone(),
        binary_path: args.context.manifest.binary_path.clone(),
        binary_sha256: args.context.manifest.binary_hashes.sha256.clone(),
        binary_md5: args.context.manifest.binary_hashes.md5.clone(),
        pack_root: display_path(args.pack_root, Some(args.doc_pack_root)),
        pack_manifest: display_path(
            &args.pack_root.join("manifest.json"),
            Some(args.doc_pack_root),
        ),
        binary_lens_version: args.context.manifest.binary_lens_version.clone(),
        pack_format_version: args.context.manifest.format_version.clone(),
        inputs_hash: args.inputs_hash.map(|hash| hash.to_string()),
        tool_name: args.context.manifest.tool.name.clone(),
        tool_version: args.context.manifest.tool.version.clone(),
        tool_revision: args.context.manifest.tool.revision.clone(),
        usage_lens_source_path: display_path(
            &args.context.usage_lens.template_path,
            Some(args.doc_pack_root),
        ),
        warnings: args.context.warnings.clone(),
        render_summary: args.render_summary.cloned(),
        examples: examples_meta,
    };

    write_staged_json(args.staging_root, "man/meta.json", &meta)?;

    Ok(())
}
