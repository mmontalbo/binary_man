//! Integration test for `ls` binary documentation.
//!
//! This test verifies that bman can make progress on behavior verification
//! for `ls` options. With a real LM, it should reach completion. With mock
//! responses, it verifies the infrastructure works and significant progress
//! is made.

mod common;

use common::TestFixture;

#[test]
fn test_ls_verification_progress() {
    let fixture = TestFixture::load("ls").expect("Failed to load ls fixture");

    // Skip if ls binary not available
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
    } else {
        // With mock, verify significant progress was made
        // (mock responses may not perfectly replay due to non-determinism)
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
            total_processed > 50,
            "Should process most surface items, got {} total",
            total_processed
        );
    }
}
