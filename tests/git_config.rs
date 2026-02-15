//! Integration test for `git config` subcommand documentation.
//!
//! This test verifies that bman can make progress on behavior verification
//! for `git config` options. Includes M21 regression check: prereq-excluded
//! items (like --edit) must not appear as stuck.

mod common;

use common::TestFixture;

#[test]
fn test_git_config_verification_progress() {
    let fixture = TestFixture::load("git-config").expect("Failed to load git-config fixture");

    // Skip if git binary not available
    if fixture.skip_if_binary_missing() {
        return;
    }

    // Check if using real LM or mock
    let using_real_lm = std::env::var("BMAN_LM_COMMAND").is_ok();

    // Skip if no LM backend available
    if !fixture.has_mock_responses() && !using_real_lm {
        eprintln!("Skipping: no LM backend (add responses/ or set BMAN_LM_COMMAND)");
        return;
    }

    let result = fixture.run().expect("bman run failed");

    if using_real_lm {
        // With real LM, expect completion
        assert_eq!(
            result.decision, "complete",
            "With real LM, expected decision 'complete', got '{}'",
            result.decision
        );
        assert!(
            !result.is_stuck,
            "With real LM, enrichment should not be stuck"
        );

        // All surface items must be accounted for
        assert!(
            result.behavior_unverified_count == 0,
            "All items should be verified or excluded, {} remain",
            result.behavior_unverified_count
        );

        // M21 regression: prereq-excluded items must not be stuck
        // --edit requires interactive TTY and should be excluded via prereqs
        if result.excluded_items.contains(&"--edit".to_string()) {
            assert!(
                !result.is_stuck,
                "--edit is excluded but workflow is stuck (M21 regression)"
            );
        }
    } else {
        // With mock, verify significant progress was made
        assert!(
            result.behavior_verified_count > 0,
            "Mock test should verify at least some behaviors, got {}",
            result.behavior_verified_count
        );

        // Verify infrastructure works: combined counts are reasonable
        let total_processed = result.behavior_verified_count
            + result.behavior_unverified_count
            + result.excluded_count;
        assert!(
            total_processed > 20,
            "Should process most surface items, got {} total",
            total_processed
        );
    }
}
