//! Static man page generator entrypoint.

mod pack;
mod render;
mod scenarios;

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use serde::Serialize;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_OUT_DIR: &str = "out";
const DEFAULT_PACKS_DIR: &str = "packs";
const DEFAULT_LENS_FLAKE: &str = "../binary_lens#binary_lens";

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

    /// Output directory root (ignored when --doc-pack is set)
    #[arg(long, value_name = "DIR", default_value = DEFAULT_OUT_DIR)]
    out_dir: PathBuf,

    /// Doc pack root to co-locate pack, scenarios, fixtures, and outputs
    #[arg(long, value_name = "DIR")]
    doc_pack: Option<PathBuf>,

    /// Use an existing binary.lens pack (pack root or parent directory)
    #[arg(long, value_name = "DIR")]
    pack: Option<PathBuf>,

    /// Force regeneration of the pack when using the default pack location
    #[arg(long)]
    refresh_pack: bool,

    /// Emit a verbose transcript of the workflow
    #[arg(long)]
    verbose: bool,

    /// Run scenario catalog and generate a validated EXAMPLES section
    #[arg(long)]
    run_scenarios: bool,

    /// Override scenarios catalog path (defaults to scenarios/<binary>.json)
    #[arg(long, value_name = "FILE")]
    scenarios: Option<PathBuf>,

    /// Nix flake reference for binary_lens
    #[arg(long, value_name = "REF", default_value = DEFAULT_LENS_FLAKE)]
    lens_flake: String,
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

fn main() -> Result<()> {
    let args = Args::parse();
    run(args)
}

fn run(args: Args) -> Result<()> {
    if args.doc_pack.is_some() && args.pack.is_some() {
        return Err(anyhow!("--doc-pack cannot be combined with --pack"));
    }
    if args.doc_pack.is_some() && args.out_dir != PathBuf::from(DEFAULT_OUT_DIR) && args.verbose {
        eprintln!("warning: --out-dir is ignored when --doc-pack is set");
    }

    let lens_flake = resolve_flake_ref(&args.lens_flake)?;

    let doc_pack_root = if let Some(doc_pack) = args.doc_pack.as_ref() {
        fs::create_dir_all(doc_pack).context("create doc pack root")?;
        Some(
            doc_pack
                .canonicalize()
                .with_context(|| format!("resolve doc pack root {}", doc_pack.display()))?,
        )
    } else {
        None
    };

    let pack_root = if let Some(doc_pack_root) = doc_pack_root.as_ref() {
        let pack_root = doc_pack_root.join("binary.lens");
        if pack_root.is_dir() && !args.refresh_pack {
            pack_root
        } else {
            if args.verbose {
                eprintln!("generating pack in {}", doc_pack_root.display());
            }
            pack::generate_pack(&args.binary, doc_pack_root, &lens_flake)?
        }
    } else if let Some(pack_path) = args.pack.as_ref() {
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
            pack::generate_pack(&args.binary, &pack_dir, &lens_flake)?
        }
    };
    let pack_root = pack_root.canonicalize().context("resolve pack root")?;

    let manifest = pack::load_manifest(&pack_root)?;
    let default_scenarios_path = if let Some(doc_pack_root) = doc_pack_root.as_ref() {
        doc_pack_root
            .join("scenarios")
            .join(format!("{}.json", manifest.binary_name))
    } else {
        scenarios::default_scenarios_path(&manifest.binary_name)
    };

    let scenarios_path = if let Some(path) = args.scenarios.as_ref() {
        if let Some(doc_pack_root) = doc_pack_root.as_ref() {
            if has_parent_components(path) {
                return Err(anyhow!(
                    "scenarios path must not include '..' when using --doc-pack"
                ));
            }
            if path.is_absolute() {
                path.clone()
            } else {
                doc_pack_root.join(path)
            }
        } else {
            path.clone()
        }
    } else {
        default_scenarios_path
    };

    if let Some(doc_pack_root) = doc_pack_root.as_ref() {
        if !scenarios_path.starts_with(doc_pack_root) {
            return Err(anyhow!(
                "scenarios path {} must live under doc pack {}",
                scenarios_path.display(),
                doc_pack_root.display()
            ));
        }
        ensure_doc_pack_inputs(
            doc_pack_root,
            &manifest.binary_name,
            &scenarios_path,
            args.run_scenarios,
            args.verbose,
        )?;
    }

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

    let run_root = if let Some(doc_pack_root) = doc_pack_root.as_ref() {
        doc_pack_root.clone()
    } else {
        scenarios_root(&scenarios_path)
    };
    let display_root = doc_pack_root.as_deref();

    let examples_report = if args.run_scenarios {
        let run_root = run_root
            .canonicalize()
            .with_context(|| format!("resolve scenarios root {}", run_root.display()))?;
        Some(scenarios::run_scenarios(
            &pack_root,
            &run_root,
            &context.manifest.binary_name,
            &scenarios_path,
            &lens_flake,
            display_root,
            args.verbose,
        )?)
    } else {
        None
    };

    let man_page = render::render_man_page(&context, examples_report.as_ref());

    let man_dir = if let Some(doc_pack_root) = doc_pack_root.as_ref() {
        doc_pack_root.join("man")
    } else {
        args.out_dir.join("man").join(&context.manifest.binary_name)
    };
    let output_dir = write_outputs(
        &man_dir,
        &context,
        &pack_root,
        &man_page,
        examples_report.as_ref(),
        display_root,
    )?;

    let coverage_path = if let Some(doc_pack_root) = doc_pack_root.as_ref() {
        doc_pack_root.join("coverage_ledger.json")
    } else {
        man_dir.join("coverage_ledger.json")
    };
    if scenarios_path.is_file() {
        let ledger = scenarios::build_coverage_ledger(
            &context.manifest.binary_name,
            &context.help_text,
            &scenarios_path,
            examples_report.as_ref(),
            display_root,
        )?;
        scenarios::write_coverage_ledger(&ledger, &coverage_path)?;
        if args.verbose {
            eprintln!("wrote coverage ledger to {}", coverage_path.display());
        }
    } else if args.verbose {
        eprintln!(
            "warning: scenarios catalog {} not found; skipping coverage ledger",
            scenarios_path.display()
        );
    }

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
    man_dir: &Path,
    context: &pack::PackContext,
    pack_root: &Path,
    man_page: &str,
    examples_report: Option<&scenarios::ExamplesReport>,
    display_root: Option<&Path>,
) -> Result<PathBuf> {
    fs::create_dir_all(man_dir).context("create man output dir")?;

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

    if let Some(report) = examples_report {
        let report_path = man_dir.join("examples_report.json");
        let report_text =
            serde_json::to_string_pretty(report).context("serialize examples report")?;
        fs::write(&report_path, report_text.as_bytes()).context("write examples report")?;
    }

    let examples_meta = examples_report.map(|report| ExamplesMeta {
        examples_report_path: display_path(&man_dir.join("examples_report.json"), display_root),
        runs_index_path: display_path(&pack_root.join("runs/index.json"), display_root),
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
        schema_version: 2,
        generated_at_epoch_ms,
        binary_name: context.manifest.binary_name.clone(),
        binary_path: context.manifest.binary_path.clone(),
        binary_sha256: context.manifest.binary_hashes.sha256.clone(),
        binary_md5: context.manifest.binary_hashes.md5.clone(),
        pack_root: display_path(pack_root, display_root),
        pack_manifest: display_path(&pack_root.join("manifest.json"), display_root),
        binary_lens_version: context.manifest.binary_lens_version.clone(),
        pack_format_version: context.manifest.format_version.clone(),
        tool_name: context.manifest.tool.name.clone(),
        tool_version: context.manifest.tool.version.clone(),
        tool_revision: context.manifest.tool.revision.clone(),
        usage_lens_source_path: display_path(&context.usage_lens.template_path, display_root),
        usage_lens_template_path: display_path(&usage_lens_template_path, display_root),
        usage_lens_rendered_path: display_path(&usage_lens_rendered_path, display_root),
        usage_evidence_path: display_path(&usage_evidence_path, display_root),
        usage_evidence_row_count: context.usage_lens.rows.len(),
        warnings: context.warnings.clone(),
        examples: examples_meta,
    };

    let meta_path = man_dir.join("meta.json");
    let meta_text = serde_json::to_string_pretty(&meta).context("serialize meta")?;
    fs::write(&meta_path, meta_text.as_bytes()).context("write meta")?;

    Ok(man_dir.to_path_buf())
}

fn scenarios_root(scenarios_path: &Path) -> PathBuf {
    let parent = scenarios_path.parent().unwrap_or_else(|| Path::new("."));
    if parent.file_name().and_then(|name| name.to_str()) == Some("scenarios") {
        if let Some(root) = parent.parent() {
            return root.to_path_buf();
        }
    }
    parent.to_path_buf()
}

fn has_parent_components(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
}

fn display_path(path: &Path, base: Option<&Path>) -> String {
    if let Some(base) = base {
        if let Ok(relative) = path.strip_prefix(base) {
            return relative.display().to_string();
        }
    }
    path.display().to_string()
}

fn resolve_flake_ref(input: &str) -> Result<String> {
    let (path_part, attr_part) = match input.split_once('#') {
        Some((path_part, attr_part)) => (path_part, Some(attr_part)),
        None => (input, None),
    };

    if path_part.is_empty() {
        return Ok(input.to_string());
    }

    let path = Path::new(path_part);
    let should_resolve = path_part.starts_with('.') || path.is_absolute() || path.exists();
    if !should_resolve {
        return Ok(input.to_string());
    }

    let resolved = path
        .canonicalize()
        .with_context(|| format!("resolve lens flake path {}", path.display()))?;
    let resolved_str = resolved
        .to_str()
        .ok_or_else(|| anyhow!("lens flake path is not valid UTF-8"))?;

    Ok(match attr_part {
        Some(attr) if !attr.is_empty() => format!("{resolved_str}#{attr}"),
        _ => resolved_str.to_string(),
    })
}

fn ensure_doc_pack_inputs(
    doc_pack_root: &Path,
    binary_name: &str,
    scenarios_path: &Path,
    require_inputs: bool,
    verbose: bool,
) -> Result<()> {
    fs::create_dir_all(doc_pack_root.join("queries")).context("create doc pack queries dir")?;
    let query_rel = PathBuf::from("queries").join(format!("{binary_name}_usage_evidence.sql"));
    let query_target = doc_pack_root.join(&query_rel);
    if !query_target.is_file() {
        let source = find_source_path(&query_rel).ok_or_else(|| {
            anyhow!(
                "usage lens template {} not found in doc pack or repo roots",
                query_rel.display()
            )
        })?;
        if verbose {
            eprintln!(
                "copying usage lens template from {} to {}",
                source.display(),
                query_target.display()
            );
        }
        if let Some(parent) = query_target.parent() {
            fs::create_dir_all(parent).context("create doc pack queries dir")?;
        }
        fs::copy(&source, &query_target).context("copy usage lens template")?;
    }

    fs::create_dir_all(doc_pack_root.join("scenarios")).context("create doc pack scenarios dir")?;
    fs::create_dir_all(doc_pack_root.join("fixtures")).context("create doc pack fixtures dir")?;

    if !scenarios_path.is_file() {
        let default_rel = PathBuf::from("scenarios").join(format!("{binary_name}.json"));
        if let Some(source) = find_source_path(&default_rel) {
            if verbose {
                eprintln!(
                    "copying scenarios from {} to {}",
                    source.display(),
                    scenarios_path.display()
                );
            }
            if let Some(parent) = scenarios_path.parent() {
                fs::create_dir_all(parent).context("create doc pack scenarios dir")?;
            }
            fs::copy(&source, scenarios_path).context("copy scenarios catalog")?;
        } else if require_inputs {
            return Err(anyhow!(
                "scenarios catalog not found at {} and no default catalog found",
                scenarios_path.display()
            ));
        } else {
            if verbose {
                eprintln!(
                    "warning: scenarios catalog {} not found; skipping fixture seeding",
                    scenarios_path.display()
                );
            }
            return Ok(());
        }
    }

    let catalog = scenarios::load_catalog(scenarios_path)?;
    for scenario in catalog.scenarios {
        let seed_dir = match scenario.seed_dir.as_deref() {
            Some(seed_dir) => seed_dir,
            None => continue,
        };
        let seed_path = Path::new(seed_dir);
        if seed_path.is_absolute() || has_parent_components(seed_path) {
            return Err(anyhow!(
                "seed_dir must be a relative path without '..' for doc packs (got {seed_dir:?})"
            ));
        }
        let target = doc_pack_root.join(seed_path);
        if target.exists() {
            continue;
        }
        let source = find_source_path(seed_path).ok_or_else(|| {
            anyhow!(
                "fixture {} not found in doc pack or repo roots",
                seed_path.display()
            )
        })?;
        if verbose {
            eprintln!(
                "copying fixture from {} to {}",
                source.display(),
                target.display()
            );
        }
        copy_dir_recursive(&source, &target)?;
    }

    Ok(())
}

fn find_source_path(relative: &Path) -> Option<PathBuf> {
    for root in source_roots() {
        let candidate = root.join(relative);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

fn source_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let manifest_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    roots.push(manifest_root);
    if let Ok(cwd) = env::current_dir() {
        if !roots.iter().any(|root| root == &cwd) {
            roots.push(cwd);
        }
    }
    roots
}

fn copy_dir_recursive(source: &Path, destination: &Path) -> Result<()> {
    let metadata =
        fs::symlink_metadata(source).with_context(|| format!("inspect {}", source.display()))?;
    let file_type = metadata.file_type();

    if file_type.is_symlink() {
        return copy_symlink(source, destination);
    }
    if file_type.is_dir() {
        fs::create_dir_all(destination)
            .with_context(|| format!("create {}", destination.display()))?;
        for entry in fs::read_dir(source).with_context(|| format!("read {}", source.display()))? {
            let entry = entry.context("read directory entry")?;
            let src_path = entry.path();
            let dst_path = destination.join(entry.file_name());
            copy_dir_recursive(&src_path, &dst_path)?;
        }
        return Ok(());
    }
    if file_type.is_file() {
        if destination.exists() {
            return Ok(());
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        fs::copy(source, destination).with_context(|| {
            format!(
                "copy {} to {}",
                source.display(),
                destination.display()
            )
        })?;
        return Ok(());
    }

    Err(anyhow!(
        "unsupported fixture entry {}",
        source.display()
    ))
}

#[cfg(unix)]
fn copy_symlink(source: &Path, destination: &Path) -> Result<()> {
    if destination.exists() {
        return Ok(());
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let target = fs::read_link(source)
        .with_context(|| format!("read symlink {}", source.display()))?;
    std::os::unix::fs::symlink(&target, destination).with_context(|| {
        format!(
            "symlink {} -> {}",
            destination.display(),
            target.display()
        )
    })?;
    Ok(())
}

#[cfg(not(unix))]
fn copy_symlink(_source: &Path, _destination: &Path) -> Result<()> {
    Err(anyhow!("symlink copy is not supported on this platform"))
}
