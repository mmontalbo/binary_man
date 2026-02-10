use super::SurfaceBehaviorExclusion;
use anyhow::{anyhow, Context, Result};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn validate_behavior_exclusions(
    exclusions: &[SurfaceBehaviorExclusion],
    option_ids: &BTreeSet<String>,
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
        BehaviorExclusion, BehaviorExclusionEvidence, BehaviorExclusionReasonCode,
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
    fn validates_exclusion_with_delta_variant_path() {
        let surface_id = "--color";
        let exclusions = vec![exclusion_with_evidence(
            surface_id,
            BehaviorExclusionEvidence {
                delta_variant_path: Some("inventory/scenarios/after.json".to_string()),
                delta_ids: Vec::new(),
            },
        )];

        let mapped = validate_behavior_exclusions(&exclusions, &option_ids(surface_id))
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
                },
            ),
            exclusion_with_evidence(
                surface_id,
                BehaviorExclusionEvidence {
                    delta_variant_path: Some("inventory/scenarios/second.json".to_string()),
                    delta_ids: Vec::new(),
                },
            ),
        ];

        let err = validate_behavior_exclusions(&exclusions, &option_ids(surface_id))
            .expect_err("duplicates must fail");

        assert!(err
            .to_string()
            .contains("duplicate behavior_exclusion entries for surface_id --color"));
    }
}
