//! Integration test for `git config` subcommand documentation.
//!
//! Verifies correctness (behavior verification, M21 regression) and performance
//! (LM cycles, scenario count) against established baselines.

mod common;

use common::TestFixture;

#[test]
fn test_git_config_verification_progress() {
    let fixture = TestFixture::load("git-config").expect("Failed to load git-config fixture");

    if fixture.skip_if_binary_missing() {
        return;
    }

    let using_real_lm = std::env::var("BMAN_LM_COMMAND").is_ok();
    if !fixture.has_mock_responses() && !using_real_lm {
        eprintln!("Skipping: no LM backend (add responses/ or set BMAN_LM_COMMAND)");
        return;
    }

    let result = fixture.run().expect("bman run failed");

    // Log performance metrics
    eprintln!(
        "Performance: {} LM cycles, {} scenarios, {:.1}s",
        result.lm_cycles, result.scenarios_run, result.elapsed_secs
    );

    // Correctness assertions
    if using_real_lm {
        assert_eq!(
            result.decision, "complete",
            "Expected completion with real LM"
        );
        assert!(!result.is_stuck, "Should not be stuck with real LM");
        assert_eq!(
            result.behavior_unverified_count, 0,
            "All items should be verified or excluded"
        );

        // M21 regression: prereq-excluded items must not cause stuck state
        if result.excluded_items.contains(&"--edit".to_string()) {
            assert!(
                !result.is_stuck,
                "--edit excluded but workflow stuck (M21 regression)"
            );
        }
    } else {
        assert!(
            result.behavior_verified_count > 0,
            "Mock should verify some behaviors, got {}",
            result.behavior_verified_count
        );
        let total = result.behavior_verified_count
            + result.behavior_unverified_count
            + result.excluded_count;
        assert!(
            total > 20,
            "Should process most surface items, got {}",
            total
        );
    }

    // Performance regression check (applies to both mock and real LM)
    if let Some(baseline) = &fixture.config.baseline {
        result.assert_performance(baseline);
    }
}
