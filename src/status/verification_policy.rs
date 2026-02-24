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
    /// Deferred: auto_verify timed out, likely interactive/hanging command
    Deferred,
    Other,
}

impl VerificationStatus {
    pub(crate) fn from_code(raw: &str) -> Self {
        match raw {
            "verified" => Self::Verified,
            "excluded" => Self::Excluded,
            "deferred" => Self::Deferred,
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
        !matches!(self, Self::Verified | Self::Excluded | Self::Deferred)
    }

    pub(crate) fn requires_follow_up(self) -> bool {
        matches!(self, Self::Other)
    }
}

/// Behavior verification status reasons (what state is the surface in).
///
/// These codes describe WHY a surface is unverified based on ledger evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BehaviorReasonKind {
    /// Initial scenario generation - surface exists but no behavior scenarios yet.
    InitialScenarios,
    /// No scenario exists for this surface.
    NoScenario,
    /// Scenario configuration is invalid.
    ScenarioError,
    /// Delta was seen but assertion failed.
    AssertionFailed,
    /// Scenario output equals baseline (no observable difference).
    OutputsEqual,
    /// auto_verify timed out (likely interactive/hanging command).
    AutoVerifyTimeout,
    /// Option requires a value but no examples are provided.
    MissingValueExamples,
    /// Delta was seen but no assertion exists to verify it.
    MissingDeltaAssertion,
    /// Post-execution judgment failed, needs retry with feedback.
    JudgmentRetry,
    /// A setup command failed before the main command ran.
    SetupFailed,
}

impl BehaviorReasonKind {
    pub(crate) fn as_code(self) -> &'static str {
        match self {
            Self::InitialScenarios => "initial_scenarios",
            Self::NoScenario => "no_scenario",
            Self::ScenarioError => "scenario_error",
            Self::AssertionFailed => "assertion_failed",
            Self::OutputsEqual => "outputs_equal",
            Self::AutoVerifyTimeout => "auto_verify_timeout",
            Self::MissingValueExamples => "missing_value_examples",
            Self::MissingDeltaAssertion => "missing_delta_assertion",
            Self::JudgmentRetry => "judgment_retry",
            Self::SetupFailed => "setup_failed",
        }
    }

    pub(crate) fn from_code(raw: Option<&str>) -> Self {
        match raw.unwrap_or("no_scenario") {
            "initial_scenarios" => Self::InitialScenarios,
            "no_scenario" => Self::NoScenario,
            "scenario_error" => Self::ScenarioError,
            "assertion_failed" => Self::AssertionFailed,
            "outputs_equal" => Self::OutputsEqual,
            "auto_verify_timeout" => Self::AutoVerifyTimeout,
            "missing_value_examples" => Self::MissingValueExamples,
            "missing_delta_assertion" => Self::MissingDeltaAssertion,
            "judgment_retry" => Self::JudgmentRetry,
            "setup_failed" => Self::SetupFailed,
            _ => Self::NoScenario,
        }
    }

    /// Returns the suggested action for this status reason.
    pub(crate) fn suggested_action(self) -> BehaviorAction {
        match self {
            Self::InitialScenarios | Self::NoScenario | Self::MissingValueExamples => {
                BehaviorAction::GenerateScenarios
            }
            Self::ScenarioError
            | Self::AssertionFailed
            | Self::MissingDeltaAssertion
            | Self::JudgmentRetry
            | Self::SetupFailed => BehaviorAction::FixScenario,
            Self::OutputsEqual => BehaviorAction::AddWorkaround,
            Self::AutoVerifyTimeout => BehaviorAction::Defer,
        }
    }

    /// Returns true if this reason indicates scenarios need to be created.
    #[allow(dead_code)]
    pub(crate) fn needs_scenario_generation(self) -> bool {
        matches!(self.suggested_action(), BehaviorAction::GenerateScenarios)
    }

    /// Returns true if this reason indicates existing scenarios need fixes.
    pub(crate) fn needs_scenario_fix(self) -> bool {
        matches!(self.suggested_action(), BehaviorAction::FixScenario)
    }
}

/// Behavior verification actions (what to do next).
///
/// These actions describe WHAT should be done to progress verification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BehaviorAction {
    /// Generate new scenarios (initial or missing).
    GenerateScenarios,
    /// Fix existing scenarios (assertions, seed, config).
    FixScenario,
    /// Add workaround (requires_argv) for outputs_equal.
    AddWorkaround,
    /// Rerun scenarios with updated config or feedback.
    Rerun,
    /// Exclude surface after exhausting options.
    Exclude,
    /// Run apply to execute scenarios.
    Apply,
    /// Defer verification (timeout, interactive command).
    Defer,
}

impl BehaviorAction {
    /// Convert action to string code (for serialization/logging).
    #[allow(dead_code)]
    pub(crate) fn as_code(self) -> &'static str {
        match self {
            Self::GenerateScenarios => "generate_scenarios",
            Self::FixScenario => "fix_scenario",
            Self::AddWorkaround => "add_workaround",
            Self::Rerun => "rerun",
            Self::Exclude => "exclude",
            Self::Apply => "apply",
            Self::Defer => "defer",
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
