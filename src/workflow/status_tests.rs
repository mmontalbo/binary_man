use super::slim_status_for_actionability;
use crate::enrich;

#[test]
fn slim_status_drops_rich_behavior_diagnostics() {
    let mut summary = enrich::StatusSummary {
        schema_version: 1,
        generated_at_epoch_ms: 0,
        binary_name: Some("bin".to_string()),
        lock: enrich::LockStatus {
            present: true,
            stale: false,
            inputs_hash: None,
        },
        plan: enrich::PlanStatus {
            present: true,
            stale: false,
            inputs_hash: None,
            lock_inputs_hash: None,
        },
        requirements: vec![enrich::RequirementStatus {
            id: enrich::RequirementId::Verification,
            status: enrich::RequirementState::Unmet,
            reason: "verification behavior incomplete".to_string(),
            verification_tier: Some("behavior".to_string()),
            accepted_verified_count: Some(0),
            unverified_ids: Vec::new(),
            accepted_unverified_count: Some(1),
            behavior_verified_count: Some(0),
            behavior_unverified_count: Some(1),
            verification: Some(enrich::VerificationTriageSummary {
                triaged_unverified_count: 1,
                triaged_unverified_preview: vec!["--color".to_string()],
                remaining_by_kind: Vec::new(),
                excluded: Vec::new(),
                excluded_count: None,
                behavior_excluded_count: 0,
                behavior_excluded_preview: Vec::new(),
                behavior_excluded_reasons: Vec::new(),
                behavior_unverified_reasons: Vec::new(),
                behavior_unverified_preview: vec![enrich::BehaviorUnverifiedPreview {
                    surface_id: "--color".to_string(),
                    reason_code: "assertion_failed".to_string(),
                    auto_verify_exit_code: None,
                    auto_verify_stderr: None,
                }],
                behavior_unverified_diagnostics: vec![enrich::BehaviorUnverifiedDiagnostic {
                    surface_id: "--color".to_string(),
                    reason_code: "assertion_failed".to_string(),
                    fix_hint: "fix assertion failure".to_string(),
                    scenario_id: Some("verify_color".to_string()),
                    assertion_kind: Some("variant_stdout_has_line".to_string()),
                    assertion_seed_path: Some("work/file.txt".to_string()),
                    assertion_token: Some("file.txt".to_string()),
                }],
                behavior_warnings: Vec::new(),
                stub_blockers_preview: Vec::new(),
            }),
            evidence: Vec::new(),
            blockers: Vec::new(),
        }],
        missing_artifacts: Vec::new(),
        blockers: Vec::new(),
        scenario_failures: Vec::new(),
        lens_summary: Vec::new(),
        decision: enrich::Decision::Incomplete,
        decision_reason: None,
        focus: None,
        next_action: enrich::NextAction::Command {
            command: "bman apply --doc-pack .".to_string(),
            reason: "verification pending".to_string(),
            hint: None,
            payload: None,
        },
        warnings: Vec::new(),
        man_warnings: Vec::new(),
        force_used: false,
    };

    slim_status_for_actionability(&mut summary);

    let verification = summary.requirements[0]
        .verification
        .as_ref()
        .expect("verification summary");
    assert!(verification.behavior_unverified_diagnostics.is_empty());
    assert_eq!(verification.behavior_unverified_preview.len(), 1);
}
