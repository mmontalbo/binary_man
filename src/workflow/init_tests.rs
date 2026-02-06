use super::install_query_templates;
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

#[test]
fn install_query_templates_writes_verification_section_files() {
    let root = temp_doc_pack_root("bman-init-verification-sections");
    let paths = enrich::DocPackPaths::new(root.clone());

    install_query_templates(&paths, enrich::SCENARIO_USAGE_LENS_TEMPLATE_REL, false)
        .expect("install templates");

    let sections = [
        (
            enrich::VERIFICATION_FROM_SCENARIOS_SECTION_TEMPLATE_RELS[0],
            templates::VERIFICATION_FROM_SCENARIOS_00_INPUTS_NORMALIZATION_SQL,
        ),
        (
            enrich::VERIFICATION_FROM_SCENARIOS_SECTION_TEMPLATE_RELS[1],
            templates::VERIFICATION_FROM_SCENARIOS_10_BEHAVIOR_ASSERTION_EVAL_SQL,
        ),
        (
            enrich::VERIFICATION_FROM_SCENARIOS_SECTION_TEMPLATE_RELS[2],
            templates::VERIFICATION_FROM_SCENARIOS_20_COVERAGE_REASONING_SQL,
        ),
        (
            enrich::VERIFICATION_FROM_SCENARIOS_SECTION_TEMPLATE_RELS[3],
            templates::VERIFICATION_FROM_SCENARIOS_30_ROLLUPS_OUTPUT_SQL,
        ),
    ];
    for (rel_path, expected) in sections {
        let path = root.join(rel_path);
        assert!(
            path.is_file(),
            "expected template file at {}",
            path.display()
        );
        let contents = std::fs::read_to_string(&path).expect("read installed section template");
        assert_eq!(contents, expected);
    }

    let _ = std::fs::remove_dir_all(root);
}
