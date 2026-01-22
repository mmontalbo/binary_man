//! Static man page generator entrypoint.

mod pack;
mod render;

use anyhow::{Context, Result};
use clap::Parser;
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_OUT_DIR: &str = "out";
const DEFAULT_PACKS_DIR: &str = "packs";

/// CLI arguments for the static man page generator.
#[derive(Parser, Debug)]
#[command(
    name = "bman",
    version,
    about = "Generate a man page from a binary_lens pack"
)]
struct Args {
    /// Binary name or path to inspect
    binary: String,

    /// Output directory root
    #[arg(long, value_name = "DIR", default_value = DEFAULT_OUT_DIR)]
    out_dir: PathBuf,

    /// Use an existing binary.lens pack (pack root or parent directory)
    #[arg(long, value_name = "DIR")]
    pack: Option<PathBuf>,

    /// Force regeneration of the pack when using the default pack location
    #[arg(long)]
    refresh_pack: bool,

    /// Emit a verbose transcript of the workflow
    #[arg(long)]
    verbose: bool,
}

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
    tool_name: String,
    tool_version: String,
    tool_revision: Option<String>,
    usage_lens_source_path: String,
    usage_lens_template_path: String,
    usage_lens_rendered_path: String,
    usage_evidence_path: String,
    usage_evidence_row_count: usize,
    warnings: Vec<String>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    run(args)
}

fn run(args: Args) -> Result<()> {
    let pack_root = if let Some(pack_path) = args.pack.as_ref() {
        pack::resolve_pack_root(pack_path)?
    } else {
        let binary_label = slugify_binary(&args.binary);
        let pack_dir = args.out_dir.join(DEFAULT_PACKS_DIR).join(binary_label);
        let pack_root = pack_dir.join("binary.lens");
        if pack_root.is_dir() && !args.refresh_pack {
            pack_root
        } else {
            if args.verbose {
                eprintln!("generating pack in {}", pack_dir.display());
            }
            pack::generate_pack(&args.binary, &pack_dir)?
        }
    };
    let pack_root = pack_root
        .canonicalize()
        .context("resolve pack root")?;

    let context = pack::load_pack_context(&pack_root)?;
    if args.verbose {
        eprintln!(
            "pack {} binary={} sha256={}",
            pack_root.display(),
            context.manifest.binary_name,
            context.manifest.binary_hashes.sha256
        );
        for warning in &context.warnings {
            eprintln!("warning: {warning}");
        }
    }

    let man_page = render::render_man_page(&context);

    let output_dir = write_outputs(
        &args.out_dir,
        &context,
        &pack_root,
        &man_page,
    )?;

    if args.verbose {
        eprintln!("wrote outputs to {}", output_dir.display());
    }

    Ok(())
}

fn slugify_binary(input: &str) -> String {
    let candidate = Path::new(input)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(input);
    let mut slug = String::new();
    for ch in candidate.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            slug.push(ch);
        } else {
            slug.push('_');
        }
    }
    if slug.is_empty() {
        "binary".to_string()
    } else {
        slug
    }
}

fn write_outputs(
    out_dir: &Path,
    context: &pack::PackContext,
    pack_root: &Path,
    man_page: &str,
) -> Result<PathBuf> {
    let man_dir = out_dir.join("man").join(&context.manifest.binary_name);
    fs::create_dir_all(&man_dir).context("create man output dir")?;

    let man_path = man_dir.join(format!("{}.1", context.manifest.binary_name));
    fs::write(&man_path, man_page.as_bytes()).context("write man page")?;

    let help_path = man_dir.join("help.txt");
    fs::write(&help_path, context.help_text.as_bytes()).context("write help text")?;

    let usage_evidence_path = man_dir.join("usage_evidence.json");
    fs::write(&usage_evidence_path, &context.usage_lens.raw_json)
        .context("write usage evidence")?;

    let usage_lens_template_path = man_dir.join("usage_lens.template.sql");
    fs::write(
        &usage_lens_template_path,
        context.usage_lens.template_sql.as_bytes(),
    )
    .context("write usage lens template")?;

    let usage_lens_rendered_path = man_dir.join("usage_lens.sql");
    fs::write(
        &usage_lens_rendered_path,
        context.usage_lens.rendered_sql.as_bytes(),
    )
    .context("write usage lens sql")?;

    let generated_at_epoch_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("compute timestamp")?
        .as_millis();

    let meta = Meta {
        schema_version: 1,
        generated_at_epoch_ms,
        binary_name: context.manifest.binary_name.clone(),
        binary_path: context.manifest.binary_path.clone(),
        binary_sha256: context.manifest.binary_hashes.sha256.clone(),
        binary_md5: context.manifest.binary_hashes.md5.clone(),
        pack_root: pack_root.display().to_string(),
        pack_manifest: pack_root.join("manifest.json").display().to_string(),
        binary_lens_version: context.manifest.binary_lens_version.clone(),
        pack_format_version: context.manifest.format_version.clone(),
        tool_name: context.manifest.tool.name.clone(),
        tool_version: context.manifest.tool.version.clone(),
        tool_revision: context.manifest.tool.revision.clone(),
        usage_lens_source_path: context.usage_lens.template_path.display().to_string(),
        usage_lens_template_path: usage_lens_template_path.display().to_string(),
        usage_lens_rendered_path: usage_lens_rendered_path.display().to_string(),
        usage_evidence_path: usage_evidence_path.display().to_string(),
        usage_evidence_row_count: context.usage_lens.rows.len(),
        warnings: context.warnings.clone(),
    };

    let meta_path = man_dir.join("meta.json");
    let meta_text = serde_json::to_string_pretty(&meta).context("serialize meta")?;
    fs::write(&meta_path, meta_text.as_bytes()).context("write meta")?;

    Ok(man_dir)
}
