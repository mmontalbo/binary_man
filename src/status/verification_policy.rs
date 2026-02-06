use crate::scenarios;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VerificationTier {
    Accepted,
    Behavior,
}

impl VerificationTier {
    pub(crate) fn from_config(raw: Option<&str>) -> Self {
        if raw == Some("behavior") {
            Self::Behavior
        } else {
            Self::Accepted
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Behavior => "behavior",
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Accepted => "existence",
            Self::Behavior => "behavior",
        }
    }

    pub(crate) fn is_behavior(self) -> bool {
        matches!(self, Self::Behavior)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VerificationStatus {
    Verified,
    Excluded,
    Other,
}

impl VerificationStatus {
    pub(crate) fn from_code(raw: &str) -> Self {
        match raw {
            "verified" => Self::Verified,
            "excluded" => Self::Excluded,
            _ => Self::Other,
        }
    }

    pub(crate) fn from_entry(
        entry: Option<&scenarios::VerificationEntry>,
        tier: VerificationTier,
    ) -> Self {
        let raw = entry
            .map(|item| {
                if tier.is_behavior() {
                    item.behavior_status.as_str()
                } else {
                    item.status.as_str()
                }
            })
            .unwrap_or("unknown");
        Self::from_code(raw)
    }

    pub(crate) fn counts_as_unverified(self) -> bool {
        !matches!(self, Self::Verified | Self::Excluded)
    }

    pub(crate) fn requires_follow_up(self) -> bool {
        matches!(self, Self::Other)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BehaviorReasonKind {
    MissingValueExamples,
    MissingBehaviorScenario,
    ScenarioFailed,
    MissingAssertions,
    AssertionSeedPathNotSeeded,
    SeedSignatureMismatch,
    SeedMismatch,
    MissingDeltaAssertion,
    MissingSemanticPredicate,
    OutputsEqual,
    AssertionFailed,
    Unknown,
    Other,
}

impl BehaviorReasonKind {
    pub(crate) fn from_code(raw: Option<&str>) -> Self {
        match raw.unwrap_or("unknown") {
            "missing_value_examples" => Self::MissingValueExamples,
            "missing_behavior_scenario" => Self::MissingBehaviorScenario,
            "scenario_failed" => Self::ScenarioFailed,
            "missing_assertions" => Self::MissingAssertions,
            "assertion_seed_path_not_seeded" => Self::AssertionSeedPathNotSeeded,
            "seed_signature_mismatch" => Self::SeedSignatureMismatch,
            "seed_mismatch" => Self::SeedMismatch,
            "missing_delta_assertion" => Self::MissingDeltaAssertion,
            "missing_semantic_predicate" => Self::MissingSemanticPredicate,
            "outputs_equal" => Self::OutputsEqual,
            "assertion_failed" => Self::AssertionFailed,
            "unknown" => Self::Unknown,
            _ => Self::Other,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeltaOutcomeKind {
    DeltaSeen,
    OutputsEqual,
    ScenarioFailed,
    MissingValueExamples,
    Unknown,
    Other,
}

impl DeltaOutcomeKind {
    pub(crate) fn from_code(raw: Option<&str>) -> Self {
        match raw.unwrap_or("unknown") {
            "delta_seen" => Self::DeltaSeen,
            "outputs_equal" => Self::OutputsEqual,
            "scenario_failed" => Self::ScenarioFailed,
            "missing_value_examples" => Self::MissingValueExamples,
            "unknown" => Self::Unknown,
            _ => Self::Other,
        }
    }
}
