#!/usr/bin/env bash
# Generate test fixture files matching actual scenario schema

FIXTURE_DIR="${1:-/tmp/sql-test-fixtures-$$}"
mkdir -p "$FIXTURE_DIR/inventory/scenarios"

# Create minimal surface.json (required by SQL)
cat > "$FIXTURE_DIR/inventory/surface.json" << 'EOF'
{"items": [{"id": "--test-baseline-error"}, {"id": "--test-both-ok"}]}
EOF

# Create behavior.json (required by SQL for normalization)
cat > "$FIXTURE_DIR/inventory/behavior.json" << 'EOF'
{"strip_ansi": false, "trim_whitespace": false, "collapse_internal_whitespace": false, "confounded_coverage_gate": false}
EOF

# Create semantics.json (required by SQL for verification rules)
cat > "$FIXTURE_DIR/inventory/semantics.json" << 'EOF'
{"verification_rules": []}
EOF

# Create scenario index (required by SQL)
cat > "$FIXTURE_DIR/inventory/scenarios/index.json" << 'EOF'
{
  "scenarios": [
    {"scenario_id": "verify_--test-baseline-error", "last_pass": true, "evidence_paths": ["inventory/scenarios/verify_--test-baseline-error-1234.json"]},
    {"scenario_id": "auto_verify_--test-baseline-error", "last_pass": true, "evidence_paths": ["inventory/scenarios/auto_verify_--test-baseline-error-1234.json"]},
    {"scenario_id": "verify_--test-both-ok", "last_pass": true, "evidence_paths": ["inventory/scenarios/verify_--test-both-ok-1234.json"]},
    {"scenario_id": "auto_verify_--test-both-ok", "last_pass": true, "evidence_paths": ["inventory/scenarios/auto_verify_--test-both-ok-1234.json"]}
  ]
}
EOF

# Fixture 1: baseline_error (baseline exits 129, variant exits 0)
# This should NOT be verified as delta_seen
cat > "$FIXTURE_DIR/inventory/scenarios/verify_--test-baseline-error-1234.json" << 'EOF'
{
  "schema_version": 3,
  "scenario_id": "verify_--test-baseline-error",
  "baseline_scenario_id": "auto_verify_--test-baseline-error",
  "exit_code": 0,
  "stdout": "",
  "stderr": "",
  "coverage_tier": "behavior",
  "covers": ["--test-baseline-error"],
  "argv": ["git", "diff", "--test-baseline-error"],
  "assertions": [{"kind": "outputs_differ"}]
}
EOF

# Baseline scenario for fixture 1 (exits with error)
cat > "$FIXTURE_DIR/inventory/scenarios/auto_verify_--test-baseline-error-1234.json" << 'EOF'
{
  "schema_version": 3,
  "scenario_id": "auto_verify_--test-baseline-error",
  "exit_code": 129,
  "stdout": "usage: git diff [options]...",
  "stderr": "",
  "coverage_tier": "auto_verify",
  "covers": ["--test-baseline-error"],
  "argv": ["git", "diff"]
}
EOF

# Fixture 2: both_succeed_differ (both exit 0, different stdout)
# This SHOULD be verified as delta_seen
cat > "$FIXTURE_DIR/inventory/scenarios/verify_--test-both-ok-1234.json" << 'EOF'
{
  "schema_version": 3,
  "scenario_id": "verify_--test-both-ok",
  "baseline_scenario_id": "auto_verify_--test-both-ok",
  "exit_code": 0,
  "stdout": "diff --git a/file.txt b/file.txt",
  "stderr": "",
  "coverage_tier": "behavior",
  "covers": ["--test-both-ok"],
  "argv": ["git", "diff", "--test-both-ok"],
  "assertions": [{"kind": "outputs_differ"}]
}
EOF

# Baseline scenario for fixture 2 (exits successfully with different output)
cat > "$FIXTURE_DIR/inventory/scenarios/auto_verify_--test-both-ok-1234.json" << 'EOF'
{
  "schema_version": 3,
  "scenario_id": "auto_verify_--test-both-ok",
  "exit_code": 0,
  "stdout": "",
  "stderr": "",
  "coverage_tier": "auto_verify",
  "covers": ["--test-both-ok"],
  "argv": ["git", "diff"]
}
EOF

# Create enrich/semantics.json (required by SQL for verification rules)
mkdir -p "$FIXTURE_DIR/enrich"
cat > "$FIXTURE_DIR/enrich/semantics.json" << 'EOF'
{
  "verification": {
    "accepted": [],
    "rejected": []
  },
  "behavior_assertions": {
    "strip_ansi": false,
    "trim_whitespace": false,
    "collapse_internal_whitespace": false,
    "confounded_coverage_gate": false
  }
}
EOF

# Create scenarios/plan.json (required by SQL)
mkdir -p "$FIXTURE_DIR/scenarios"
cat > "$FIXTURE_DIR/scenarios/plan.json" << 'EOF'
{
  "defaults": {},
  "scenarios": [
    {
      "id": "verify_--test-baseline-error",
      "coverage_ignore": false,
      "covers": ["--test-baseline-error"],
      "argv": ["git", "diff", "--test-baseline-error"],
      "coverage_tier": "behavior",
      "baseline_scenario_id": "auto_verify_--test-baseline-error",
      "assertions": [{"kind": "outputs_differ"}]
    },
    {
      "id": "auto_verify_--test-baseline-error",
      "coverage_ignore": false,
      "covers": ["--test-baseline-error"],
      "argv": ["git", "diff"],
      "coverage_tier": "auto_verify"
    },
    {
      "id": "verify_--test-both-ok",
      "coverage_ignore": false,
      "covers": ["--test-both-ok"],
      "argv": ["git", "diff", "--test-both-ok"],
      "coverage_tier": "behavior",
      "baseline_scenario_id": "auto_verify_--test-both-ok",
      "assertions": [{"kind": "outputs_differ"}]
    },
    {
      "id": "auto_verify_--test-both-ok",
      "coverage_ignore": false,
      "covers": ["--test-both-ok"],
      "argv": ["git", "diff"],
      "coverage_tier": "auto_verify"
    }
  ]
}
EOF

echo "$FIXTURE_DIR"
