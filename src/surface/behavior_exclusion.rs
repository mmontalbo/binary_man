use super::SurfaceBehaviorExclusion;
use anyhow::{anyhow, Context, Result};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Default)]
pub(crate) struct BehaviorExclusionLedgerEntry {
    pub delta_outcome: Option<String>,
    pub delta_evidence_paths: Vec<String>,
}

pub(crate) fn validate_behavior_exclusions(
    exclusions: &[SurfaceBehaviorExclusion],
    option_ids: &BTreeSet<String>,
    ledger_entries: &BTreeMap<String, BehaviorExclusionLedgerEntry>,
    missing_entry_suffix: &str,
    missing_delta_outcome_suffix: &str,
) -> Result<BTreeMap<String, SurfaceBehaviorExclusion>> {
    let mut validated = BTreeMap::new();
    for exclusion in exclusions {
        let kind = exclusion.kind.trim();
        let surface_id = exclusion.surface_id.trim();
        if kind != "option" {
            return Err(anyhow!(
                "behavior_exclusion only supports option overlays (got kind={} for {})",
                kind,
                surface_id
            ));
        }
        if surface_id.is_empty() {
            return Err(anyhow!("behavior_exclusion surface_id must not be empty"));
        }
        exclusion
            .exclusion
            .validate_shape(surface_id)
            .with_context(|| format!("validate behavior_exclusion for {surface_id}"))?;
        if !option_ids.contains(surface_id) {
            return Err(anyhow!(
                "behavior_exclusion surface_id {} missing from inventory/surface.json options",
                surface_id
            ));
        }
        let Some(entry) = ledger_entries.get(surface_id) else {
            return Err(anyhow!(
                "behavior_exclusion surface_id {} {}",
                surface_id,
                missing_entry_suffix
            ));
        };
        let Some(delta_outcome) = entry.delta_outcome.as_deref() else {
            return Err(anyhow!(
                "behavior_exclusion surface_id {} {}",
                surface_id,
                missing_delta_outcome_suffix
            ));
        };
        match delta_outcome {
            "delta_seen" => {
                return Err(anyhow!(
                    "behavior_exclusion surface_id {} invalid: delta_outcome=delta_seen must be verified with assertions",
                    surface_id
                ));
            }
            "missing_value_examples" => {
                return Err(anyhow!(
                    "behavior_exclusion surface_id {} invalid: delta_outcome=missing_value_examples must be fixed with value_examples",
                    surface_id
                ));
            }
            "outputs_equal" => {
                if exclusion
                    .exclusion
                    .evidence
                    .attempted_workarounds
                    .is_empty()
                {
                    return Err(anyhow!(
                        "behavior_exclusion surface_id {} invalid: outputs_equal requires evidence.attempted_workarounds",
                        surface_id
                    ));
                }
                for workaround in &exclusion.exclusion.evidence.attempted_workarounds {
                    let matched = entry
                        .delta_evidence_paths
                        .iter()
                        .any(|path| path == &workaround.delta_variant_path_after);
                    if !matched {
                        return Err(anyhow!(
                            "behavior_exclusion surface_id {} invalid: attempted_workaround delta_variant_path_after {} not found in latest delta_evidence_paths",
                            surface_id,
                            workaround.delta_variant_path_after
                        ));
                    }
                }
            }
            _ => {}
        }
        if validated.contains_key(surface_id) {
            return Err(anyhow!(
                "duplicate behavior_exclusion entries for surface_id {}",
                surface_id
            ));
        }
        validated.insert(surface_id.to_string(), exclusion.clone());
    }
    Ok(validated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surface::overlays::{
        BehaviorExclusion, BehaviorExclusionAttemptedWorkaround, BehaviorExclusionEvidence,
        BehaviorExclusionReasonCode, BehaviorExclusionWorkaroundKind,
    };

    fn exclusion_with_evidence(
        surface_id: &str,
        evidence: BehaviorExclusionEvidence,
    ) -> SurfaceBehaviorExclusion {
        SurfaceBehaviorExclusion {
            kind: "option".to_string(),
            surface_id: surface_id.to_string(),
            exclusion: BehaviorExclusion {
                reason_code: BehaviorExclusionReasonCode::AssertionGap,
                note: None,
                evidence,
            },
        }
    }

    fn option_ids(surface_id: &str) -> BTreeSet<String> {
        [surface_id.to_string()].into_iter().collect()
    }

    #[test]
    fn validates_outputs_equal_with_matching_workaround_path() {
        let surface_id = "--color";
        let exclusions = vec![exclusion_with_evidence(
            surface_id,
            BehaviorExclusionEvidence {
                delta_variant_path: None,
                delta_ids: Vec::new(),
                attempted_workarounds: vec![BehaviorExclusionAttemptedWorkaround {
                    kind: BehaviorExclusionWorkaroundKind::AddedRequiresArgv,
                    ref_path: "scenarios/plan.json".to_string(),
                    delta_variant_path_after: "inventory/scenarios/after.json".to_string(),
                }],
            },
        )];
        let ledger_entries = BTreeMap::from([(
            surface_id.to_string(),
            BehaviorExclusionLedgerEntry {
                delta_outcome: Some("outputs_equal".to_string()),
                delta_evidence_paths: vec!["inventory/scenarios/after.json".to_string()],
            },
        )]);

        let mapped = validate_behavior_exclusions(
            &exclusions,
            &option_ids(surface_id),
            &ledger_entries,
            "missing from verification rows",
            "requires delta_outcome evidence",
        )
        .expect("valid exclusion");

        assert_eq!(mapped.len(), 1);
        assert!(mapped.contains_key(surface_id));
    }

    #[test]
    fn rejects_duplicate_entries() {
        let surface_id = "--color";
        let exclusions = vec![
            exclusion_with_evidence(
                surface_id,
                BehaviorExclusionEvidence {
                    delta_variant_path: Some("inventory/scenarios/first.json".to_string()),
                    delta_ids: Vec::new(),
                    attempted_workarounds: Vec::new(),
                },
            ),
            exclusion_with_evidence(
                surface_id,
                BehaviorExclusionEvidence {
                    delta_variant_path: Some("inventory/scenarios/second.json".to_string()),
                    delta_ids: Vec::new(),
                    attempted_workarounds: Vec::new(),
                },
            ),
        ];
        let ledger_entries = BTreeMap::from([(
            surface_id.to_string(),
            BehaviorExclusionLedgerEntry {
                delta_outcome: Some("not_applicable".to_string()),
                delta_evidence_paths: Vec::new(),
            },
        )]);

        let err = validate_behavior_exclusions(
            &exclusions,
            &option_ids(surface_id),
            &ledger_entries,
            "missing from verification rows",
            "requires delta_outcome evidence",
        )
        .expect_err("duplicates must fail");

        assert!(err
            .to_string()
            .contains("duplicate behavior_exclusion entries for surface_id --color"));
    }
}
