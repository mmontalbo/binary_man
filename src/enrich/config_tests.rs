use super::{default_config, resolve_inputs};
use crate::enrich;
use crate::templates;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_doc_pack_root(name: &str) -> std::path::PathBuf {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("{name}-{}-{now}", std::process::id()));
    std::fs::create_dir_all(&root).expect("create temp root");
    root
}

fn write_file(path: &std::path::Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent directory");
    }
    std::fs::write(path, contents.as_bytes()).expect("write file");
}

fn write_required_inputs(root: &std::path::Path, config: &enrich::EnrichConfig) {
    write_file(
        &root.join(&config.usage_lens_template),
        templates::USAGE_FROM_SCENARIOS_SQL,
    );
    write_file(
        &root.join(enrich::SUBCOMMANDS_FROM_SCENARIOS_TEMPLATE_REL),
        templates::SUBCOMMANDS_FROM_SCENARIOS_SQL,
    );
    write_file(
        &root.join(enrich::OPTIONS_FROM_SCENARIOS_TEMPLATE_REL),
        templates::OPTIONS_FROM_SCENARIOS_SQL,
    );
    write_file(
        &root.join(enrich::VERIFICATION_FROM_SCENARIOS_TEMPLATE_REL),
        templates::VERIFICATION_FROM_SCENARIOS_SQL,
    );
    write_file(
        &root.join(enrich::VERIFICATION_FROM_SCENARIOS_SECTION_TEMPLATE_RELS[0]),
        templates::VERIFICATION_FROM_SCENARIOS_00_INPUTS_NORMALIZATION_SQL,
    );
    write_file(
        &root.join(enrich::VERIFICATION_FROM_SCENARIOS_SECTION_TEMPLATE_RELS[1]),
        templates::VERIFICATION_FROM_SCENARIOS_10_BEHAVIOR_ASSERTION_EVAL_SQL,
    );
    write_file(
        &root.join(enrich::VERIFICATION_FROM_SCENARIOS_SECTION_TEMPLATE_RELS[2]),
        templates::VERIFICATION_FROM_SCENARIOS_20_COVERAGE_REASONING_SQL,
    );
    write_file(
        &root.join(enrich::VERIFICATION_FROM_SCENARIOS_SECTION_TEMPLATE_RELS[3]),
        templates::VERIFICATION_FROM_SCENARIOS_30_ROLLUPS_OUTPUT_SQL,
    );
    write_file(
        &root.join("scenarios/plan.json"),
        &crate::scenarios::plan_stub(Some("bin")),
    );
    write_file(
        &root.join("binary_lens/export_plan.json"),
        templates::BINARY_LENS_EXPORT_PLAN_JSON,
    );
    write_file(&root.join("enrich/config.json"), "{}");
}

#[test]
fn resolve_inputs_and_lock_status_track_verification_section_templates() {
    let root = temp_doc_pack_root("bman-enrich-verification-sections-inputs");
    let config = default_config();
    write_required_inputs(&root, &config);

    let resolved = resolve_inputs(&config, &root).expect("resolve required inputs");
    for rel in enrich::VERIFICATION_FROM_SCENARIOS_SECTION_TEMPLATE_RELS {
        let expected = root.join(rel);
        assert!(
            resolved.iter().any(|path| path == &expected),
            "missing required input {}",
            expected.display()
        );
    }

    let lock = enrich::build_lock(&root, &config, Some("bin")).expect("build lock");
    for rel in enrich::VERIFICATION_FROM_SCENARIOS_SECTION_TEMPLATE_RELS {
        assert!(
            lock.inputs.iter().any(|path| path == rel),
            "lock inputs missing {rel}"
        );
    }
    assert!(
        !enrich::lock_status(&root, Some(&lock))
            .expect("compute lock status")
            .stale
    );

    write_file(
        &root.join(enrich::VERIFICATION_FROM_SCENARIOS_SECTION_TEMPLATE_RELS[2]),
        "-- modified for staleness test\n",
    );
    assert!(
        enrich::lock_status(&root, Some(&lock))
            .expect("compute stale lock status")
            .stale
    );

    let _ = std::fs::remove_dir_all(root);
}
