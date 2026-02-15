# Milestones

This document tracks the static-first roadmap for generating man pages from
`binary_lens` packs. Dynamic execution is optional and used for scenario-backed
validation, coverage tracking, and (eventually) a structured "enrichment loop"
that supports iterative static + dynamic passes from portable doc packs.

Current focus: M23 — Prompt Consolidation.

## M23 — Prompt Consolidation (done)

Goal: Move LM prompt text from Rust code into markdown files for clearer
ownership, easier review, and reduced binary-specific coupling.

Motivation:
- LM prompts are split between `prompts/*.md` (300 lines) and embedded strings
  in `lm_client.rs` (~400 lines across 3 functions). Code review is harder when
  prompt text is interleaved with string formatting logic.
- Prompts contain binary-specific examples (ls, git) that should be generic.
- Prompt files are easier to diff, review, and version control than Rust strings.
- M22 established baselines to detect regressions from prompt changes.

Non-goal (deferred to M24):
- DuckDB optimization (separate concern, measure need first)
- Runtime template loading (compile-time embedding is sufficient)

---

## Deliverables

### Deliverable 1: Extract Embedded Prompts to Files

Move prompts from `lm_client.rs` to `prompts/` directory:

```
prompts/
├── enrich_agent_prompt.md     # Existing (for Claude Code agent)
├── behavior.md                # NEW: from build_behavior_prompt()
└── prereq_inference.md        # NEW: from build_prereq_prompt()
```

Template structure with separate files for reason-specific content:

```
prompts/
├── enrich_agent_prompt.md        # Existing (for Claude Code agent)
├── behavior_base.md              # Main structure + response format
├── behavior_reason_no_scenario.md
├── behavior_reason_outputs_equal.md
├── behavior_reason_assertion_failed.md
└── prereq_inference.md
```

Rust concatenates the appropriate pieces:
```rust
const BEHAVIOR_BASE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"), "/prompts/behavior_base.md"
));
const REASON_NO_SCENARIO: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"), "/prompts/behavior_reason_no_scenario.md"
));
// ... other reason constants

fn build_behavior_prompt(summary: &StatusSummary, payload: &Payload) -> String {
    let reason_section = match payload.reason_code.as_deref() {
        Some("no_scenario") => REASON_NO_SCENARIO,
        Some("outputs_equal") => REASON_OUTPUTS_EQUAL,
        Some("assertion_failed") => REASON_ASSERTION_FAILED,
        _ => "",
    };

    let binary_name = summary.binary_name.as_deref().unwrap_or("<binary>");
    let targets = format_targets(&payload.target_ids);
    let context = format_scaffold_context(payload); // value hints, guidance

    format!("{}\n\n{}\n\n## Target Options\n{}\n\n{}",
            BEHAVIOR_BASE.replace("{binary_name}", binary_name),
            reason_section,
            targets,
            context)
}
```

This avoids complex parsing — just string concatenation and simple `replace()`.

**Note**: `build_retry_prompt()` stays in Rust — it wraps `build_behavior_prompt()`
with error context, no separate template needed.

### Deliverable 2: Generalize and Reduce Prompts

Edit all prompt files to remove binary-specific content and redundancy:

**Generalization**:
| Current | Replacement |
|---------|-------------|
| `ls --all` | `--option` |
| `--color always\|never\|auto` | `--mode value1\|value2` |
| `.hidden`, `visible.txt` | `file1.txt`, `file2.txt` |
| git config references | (removed) |

**Redundancy reduction**:
1. Consolidate seed format docs (currently in 3 places → 1 canonical section)
2. Compress JSON examples to single-line
3. Remove "IMPORTANT:", "NOTE:" emphasis markers

**Before** (~15 lines):
```markdown
**Scenario with seed fixtures:**
```json
{
  "kind": "add_behavior_scenario",
  "argv": ["--all"],
  "seed": {
    "files": {".hidden": "secret", "visible.txt": "hello"},
    "dirs": ["subdir"]
  }
}
```
**IMPORTANT seed format:**
- `files`: Object mapping filename to content
- `dirs`: Array of directory names
NOTE: `files` is an OBJECT, not an array!
```

**After** (~3 lines):
```markdown
Scenario with seed:
`{"kind": "add_behavior_scenario", "argv": ["--opt"], "seed": {"files": {"f.txt": "x"}, "dirs": ["d"]}}`
```

---

## Files to Modify

| File | Changes |
|------|---------|
| `prompts/behavior_base.md` | NEW: main structure + response format |
| `prompts/behavior_reason_no_scenario.md` | NEW: no_scenario guidance |
| `prompts/behavior_reason_outputs_equal.md` | NEW: outputs_equal guidance |
| `prompts/behavior_reason_assertion_failed.md` | NEW: assertion_failed guidance |
| `prompts/prereq_inference.md` | NEW: prereq classification prompt |
| `prompts/enrich_agent_prompt.md` | Generalize examples, reduce redundancy |
| `src/workflow/lm_client.rs` | Use include_str!, format!() concatenation |

---

## Pre-work Measurement

Before starting, capture baseline metrics:
```bash
# Total lines in prompt-building functions (approximate)
grep -n "^fn build_" src/workflow/lm_client.rs  # note line numbers
# Then count lines between each function start and its closing brace

# Total prompt file lines
wc -l prompts/*.md

# Binary-specific references
grep -rn "ls \|--all\|\.hidden\|git config" prompts/ src/workflow/lm_client.rs
```

## Acceptance Criteria

| Criterion | Validation |
|-----------|------------|
| Prompts in files | All prompt text in `prompts/`, Rust only does selection + interpolation |
| Generalized | `grep -rE "\bls\b|--all|\.hidden|git config" prompts/` returns no matches |
| Reduced | Total `prompts/*.md` lines < (pre-work prompts + embedded) |
| Compiles | `cargo build` succeeds |
| Baselines maintained | ls: ≤ 12 cycles, git-config: ≤ 8 cycles |
| Mock tests pass | `cargo test --release` green |
| Real LM validation | Manual: `BMAN_LM_COMMAND="claude -p" cargo test --release` |

---

## Out of Scope (Deferred)

- DuckDB crate integration (M24 if measured need)
- Query result caching
- Token counting / cost tracking
- Runtime template loading (compile-time embedding is sufficient)
- Prompt A/B testing framework

## M22 — Testing Infrastructure (done)

Goal: Enable both fast deterministic testing (mock LM) and real LM regression
testing to catch orchestration bugs and prompt/model regressions.

Motivation:
- M21 uncovered an integration bug (prereq exclusions vs stuck detection) that
  slipped through because there's no test covering the full orchestration flow.
- Real LM calls take 5-10s each and are non-deterministic, making E2E tests slow
  and flaky. Most bugs are in orchestration logic, not LM response parsing.
- We already capture prompts/responses in `lm_log/` — these can be replay fixtures.
- Prompt changes or model updates could regress LM behavior; need regression tests.

Two LM backends, same tests:

| Backend | Purpose | Speed | When to Run |
|---------|---------|-------|-------------|
| **Mock** | Test orchestration logic | Fast (<5s) | Every commit |
| **Real LM** | Test LM produces good results | Slow (minutes) | Nightly/release |

Design principles:
- **LM-agnostic tests**: Test code doesn't know about mock vs real.
- **Backend injection**: Harness resolves LM command from env or mock responses.
- **Adaptive assertions**: Exact for mock (deterministic), loose for real LM.
- **Sequential mock**: Stateful script returns responses in order.
- **Parallel-safe**: Each test gets isolated state via temp directory.

---

## Deliverables

### Deliverable 1: Mock LM Script

A simple shell script replaces the real LM command:

```bash
#!/bin/bash
# tests/mock-lm.sh - Stateful mock that returns responses sequentially
FIXTURE_DIR="$1"
STATE_FILE="${BMAN_MOCK_STATE_DIR:-.}/.mock_cycle"
CYCLE=$(cat "$STATE_FILE" 2>/dev/null || echo 1)

# Check for injected failure
ERROR_FILE="$FIXTURE_DIR/responses/$(printf '%03d' $CYCLE)_error.txt"
if [[ -f "$ERROR_FILE" ]]; then
    cat "$ERROR_FILE" >&2
    echo $((CYCLE + 1)) > "$STATE_FILE"
    exit 1
fi

# Return response and advance cycle
RESPONSE="$FIXTURE_DIR/responses/$(printf '%03d' $CYCLE).txt"
if [[ -f "$RESPONSE" ]]; then
    cat "$RESPONSE"
    echo $((CYCLE + 1)) > "$STATE_FILE"
else
    echo "mock-lm: no response for cycle $CYCLE" >&2
    exit 1
fi
```

Usage:
```bash
BMAN_LM_COMMAND="./tests/mock-lm.sh tests/fixtures/git-config" bman git config
```

Key design decisions:
- **No prompt parsing**: Responses returned in fixed order regardless of prompt content
- **State via file**: Each invocation reads/increments cycle counter
- **Parallel isolation**: `BMAN_MOCK_STATE_DIR` env var points to test's temp directory
- **Failure injection**: `003_error.txt` simulates LM failure at cycle 3
- **No Rust code**: ~20 lines of bash instead of new subcommand

### Deliverable 2: Test Fixture Format

Fixtures are directories with numbered response files:

```
tests/fixtures/git-config/
├── fixture.json              # Metadata + expected outcomes
├── lm_log.jsonl              # For documentation/debugging (not used by mock)
└── responses/
    ├── 001.txt               # Cycle 1: prereq_inference response
    ├── 002.txt               # Cycle 2: behavior response
    ├── 003.txt               # Cycle 3: behavior response
    ├── 004.txt               # Cycle 4: behavior response
    └── 005.txt               # Cycle 5: behavior response
```

`fixture.json` schema:
```json
{
  "fixture_version": 1,
  "binary": "git",
  "context": ["config"],
  "timeout_secs": 300
}
```

Fixture directory structure:
```
tests/fixtures/git-config/
├── fixture.json
└── responses/
    ├── 001.txt
    ├── 002.txt
    └── ...
```

### Deliverable 3: Test Harness

Test harness in `tests/common.rs` (shared by all integration tests):

```rust
// tests/common.rs
use std::process::{Command, Stdio};
use std::path::PathBuf;
use std::env;
use serde::Deserialize;
use tempfile::tempdir;

pub struct TestFixture {
    pub name: String,
    pub fixture_dir: PathBuf,
    pub binary: String,
    pub context: Vec<String>,
}

pub struct TestResult {
    pub decision: String,           // "complete" | "incomplete" | "blocked"
    pub behavior_verified_count: u32,
    pub excluded_count: u32,
    pub surface_count: u32,
    pub is_stuck: bool,
    pub excluded_items: Vec<String>,
    pub stuck_items: Vec<String>,
}

impl TestFixture {
    pub fn load(name: &str) -> Result<Self> {
        // Load from tests/fixtures/{name}/fixture.json
    }

    pub fn run(&self) -> TestResult {
        let temp_dir = tempdir().expect("Failed to create temp dir");
        let doc_pack = temp_dir.path().join("doc-pack");

        // Set mock state dir for parallel isolation
        env::set_var("BMAN_MOCK_STATE_DIR", temp_dir.path());

        // Resolve LM command
        let lm_cmd = self.resolve_lm_command();
        env::set_var("BMAN_LM_COMMAND", &lm_cmd);

        // Run bman with binary and context
        let mut args = vec![
            "--doc-pack".to_string(),
            doc_pack.display().to_string(),
        ];
        args.push(self.binary.clone());
        args.extend(self.context.clone());

        let output = Command::new("cargo")
            .args(["run", "--release", "--"])
            .args(&args)
            .output()
            .expect("Failed to run bman");

        if !output.status.success() {
            panic!("bman failed: {}", String::from_utf8_lossy(&output.stderr));
        }

        // Get status
        let status_output = Command::new("cargo")
            .args(["run", "--release", "--", "status", "--json"])
            .arg("--doc-pack")
            .arg(&doc_pack)
            .output()
            .expect("Failed to get status");

        serde_json::from_slice(&status_output.stdout)
            .expect("Failed to parse status JSON")
    }

    fn resolve_lm_command(&self) -> String {
        if let Ok(cmd) = env::var("BMAN_LM_COMMAND") {
            return cmd;  // Real LM
        }
        if self.has_mock_responses() {
            // Use absolute path for fixture dir
            let abs_fixture = self.fixture_dir.canonicalize()
                .expect("Fixture dir must exist");
            return format!("./tests/mock-lm.sh {}", abs_fixture.display());
        }
        panic!("No LM: set BMAN_LM_COMMAND or add responses/");
    }

    fn has_mock_responses(&self) -> bool {
        self.fixture_dir.join("responses").exists()
    }

    pub fn skip_if_binary_missing(&self) -> bool {
        let missing = Command::new(&self.binary)
            .arg("--help")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_err();
        if missing {
            eprintln!("Skipping: {} not available", self.binary);
        }
        missing
    }
}

// Fixture JSON deserialization
#[derive(Deserialize)]
struct FixtureConfig {
    fixture_version: u32,
    binary: String,
    #[serde(default)]
    context: Vec<String>,
    #[serde(default = "default_timeout")]
    timeout_secs: u64,
}

fn default_timeout() -> u64 { 300 }

// TestResult parsed from `bman status --json`
#[derive(Deserialize)]
pub struct TestResult {
    pub decision: String,
    pub behavior_verified_count: u32,
    pub excluded_count: u32,
    pub surface_count: u32,
    pub is_stuck: bool,
    #[serde(default)]
    pub excluded_items: Vec<String>,
    #[serde(default)]
    pub stuck_items: Vec<String>,
}
```

Test isolation:
- Each test gets fresh temp directory for doc pack AND mock state
- Fixtures are read-only, shared across parallel tests
- `BMAN_MOCK_STATE_DIR` prevents state file conflicts

### Deliverable 4: Initial Test Suite

Two comprehensive tests covering real binary surface areas:

**Test 1: `ls`** (~84 options)
**Test 2: `git config`** (~34 options)

Both tests verify the same invariants:
- `decision == "complete"`
- `is_stuck == false`
- All surface items accounted for: `verified + excluded == surface_count`

```rust
// tests/ls.rs
mod common;
use common::TestFixture;

#[test]
fn test_ls() {
    let fixture = TestFixture::load("ls").unwrap();
    if fixture.skip_if_binary_missing() { return; }

    let result = fixture.run();

    assert_eq!(result.decision, "complete");
    assert!(!result.is_stuck);
    assert_eq!(
        result.behavior_verified_count + result.excluded_count,
        result.surface_count,
        "All surface items must be verified or excluded"
    );
}

// tests/git_config.rs
mod common;
use common::TestFixture;

#[test]
fn test_git_config() {
    let fixture = TestFixture::load("git-config").unwrap();
    if fixture.skip_if_binary_missing() { return; }

    let result = fixture.run();

    // Core assertions
    assert_eq!(result.decision, "complete");
    assert!(!result.is_stuck);
    assert_eq!(
        result.behavior_verified_count + result.excluded_count,
        result.surface_count
    );

    // M21 regression: prereq-excluded items must not be stuck
    assert!(result.excluded_items.contains(&"--edit".into()));
    assert!(!result.stuck_items.contains(&"--edit".into()));
}
```

Known exclusions (verified via prereq inference, not hardcoded):
- `ls`: `-w` (terminal width), `-Z` (SELinux)
- `git config`: `--edit`, `-e` (interactive editor)

### Deliverable 5: Fixture Creation Workflow

Creating a new fixture from successful E2E:

```bash
# 1. Run real E2E with verbose logging
BMAN_LM_COMMAND="claude -p --model haiku" \
  bman --verbose --doc-pack /tmp/bman-fixture-git-config git config

# 2. Create fixture directory
mkdir -p tests/fixtures/git-config/responses

# 3. Copy response files (for mock mode)
for f in /tmp/bman-fixture-git-config/enrich/lm_log/cycle_*_response.txt; do
  n=$(echo "$f" | grep -oP 'cycle_\K\d+')
  cp "$f" "tests/fixtures/git-config/responses/$(printf '%03d' $n).txt"
done

# 4. Create fixture.json
cat > tests/fixtures/git-config/fixture.json << 'EOF'
{
  "fixture_version": 1,
  "binary": "git",
  "context": ["config"],
  "timeout_secs": 300
}
EOF
```

Running tests:
```bash
# With mock backend (fast, uses responses/)
cargo test

# With real LM backend (slow, uses BMAN_LM_COMMAND)
BMAN_LM_COMMAND="claude -p --model haiku" cargo test

# Same tests, different backend - no code changes needed
```

---

### Files to Create/Modify

| File | Description |
|------|-------------|
| `tests/mock-lm.sh` | Stateful mock script (~20 lines bash) |
| `tests/common.rs` | TestFixture, TestResult, LM backend resolution |
| `tests/ls.rs` | `test_ls` |
| `tests/git_config.rs` | `test_git_config` (includes M21 regression check) |
| `tests/fixtures/ls/fixture.json` | Fixture metadata |
| `tests/fixtures/ls/responses/*.txt` | Mock LM responses |
| `tests/fixtures/git-config/fixture.json` | Fixture metadata |
| `tests/fixtures/git-config/responses/*.txt` | Mock LM responses |

### Acceptance Criteria

| Criterion | Validation |
|-----------|------------|
| `ls` test passes | `decision=complete`, all ~84 options accounted for |
| `git config` test passes | `decision=complete`, all ~34 options accounted for |
| M21 regression covered | `git config` test asserts excluded items not stuck |
| Tests are LM-agnostic | Same test works with mock or real LM backend |
| Mock backend fast | < 5s with mock responses |
| Real LM backend works | `BMAN_LM_COMMAND=... cargo test` reaches complete |
| Parallel isolation | Multiple tests can run concurrently |
| Performance baselines | Tests assert LM cycles and scenario counts within limits |

### Delivered (beyond original scope)

- **Performance regression testing**: `fixture.json` includes `baseline.max_lm_cycles` and
  `baseline.max_scenarios`; tests assert metrics stay within bounds.

### Out of Scope (Deferred)

- Phase timing/profiling (M23 if needed)
- Token counting
- User-facing fixture commands
- Property-based testing / fuzzing
- Fixture auto-generation tooling

## M21 — TUI Redesign with LM Transparency (done)

Goal: Rebuild the `bman inspect` TUI to match current workflow concepts and
provide full visibility into LM interactions, enabling users to understand why
enrichment isn't complete and what the LM attempted.

Motivation:
- The existing TUI was built during M11 (Side Quest) and hasn't been updated
  since. It reflects outdated concepts (Intent/Evidence/Outputs/History tabs)
  that don't map to the current workflow.
- Users running `bman <binary>` have no visibility into what the LM is doing.
  The enrichment process is a black box until it succeeds or fails.
- M20 added prereqs as a first-class concept, but the TUI doesn't surface them.
- Debugging verification failures requires manually reading JSON files in
  `inventory/scenarios/`. The TUI should make this navigable.
- The current TUI shows file existence (present/missing) rather than answering
  the user's actual question: "what needs work and why?"

Design principles:
- **One question per view**: Each tab answers a single question clearly.
  Work = "what needs attention?", Log = "what did the LM do?", Browse = "what
  exists?"
- **Full-screen views**: No split panes. Each view takes the whole screen.
  Detail views are modal (Enter to open, Esc to close).
- **LM transparency**: Every LM invocation is logged with prompt preview,
  items processed, outcome, and optional full prompt/response storage.
- **Actionable output**: Every view shows something useful to copy (the next
  command to run, a scenario ID, an error message).
- **Minimal state**: Current tab + selection index + optional detail view.
  No complex navigation stacks.

### LM Log Storage

New artifact: `enrich/lm_log.jsonl` (append-only, durable)

```jsonl
{"ts":1707900000,"cycle":1,"kind":"prereq_inference","duration_ms":4200,"items_count":34,"outcome":"success","summary":"10 definitions, 34 mappings, 3 excluded"}
{"ts":1707900060,"cycle":2,"kind":"behavior","duration_ms":3100,"items":["--blob","--comment","--url"],"outcome":"partial","succeeded":2,"failed":1,"error":"--url: parse error at line 15"}
```

Optional full content (enabled by `--verbose` or config):

```
enrich/lm_log/
├── cycle_001_prereq_prompt.txt
├── cycle_001_prereq_response.txt
├── cycle_002_behavior_prompt.txt
└── cycle_002_behavior_response.txt
```

### Tab Structure

| Tab | Question | Content |
|-----|----------|---------|
| Work | "What needs attention?" | Items grouped by status: needs_scenario, needs_fix, excluded |
| Log | "What did the LM do?" | Chronological LM invocations with outcomes |
| Browse | "What exists?" | File tree of doc pack artifacts |

### Work Tab

Shows only items needing action, grouped by what's wrong:

- **NEEDS SCENARIO**: Items with no scenario (never run)
- **NEEDS FIX**: Items that ran but failed (outputs_equal, assertion_failed, etc.)
- **EXCLUDED**: Items excluded via prereqs (interactive, network, privilege)

Each item shows: surface_id, reason code, last run time, exit code.

Selecting an item and pressing Enter shows detail view with:
- Description and forms from surface.json
- Prereqs with seed preview
- Status explanation (why it's in this category)
- Last run evidence (argv, exit, stdout/stderr preview)

### Log Tab

Shows LM invocation history, newest first:

- Cycle number, timestamp, kind (prereq_inference, behavior, behavior_retry)
- Items processed, outcome (success/partial/failed), duration
- Error preview if failed

Selecting a cycle and pressing Enter shows:
- Full item list
- Outcome per item (succeeded/failed with reason)
- Prompt preview (first ~500 chars)
- Links to view full prompt/response (if stored)

### Browse Tab

Simple file tree of doc pack:
- enrich/ (config, semantics, prereqs, lm_log)
- inventory/ (surface, scenarios)
- scenarios/ (plan)
- man/ (rendered output)

Selecting a file and pressing Enter opens in pager. Press `o` to open in editor.

### Keybindings

```
Navigation:
  Tab         Next tab
  Shift+Tab   Previous tab
  j/k ↑/↓     Move selection
  Enter       View detail / expand
  Esc         Back / close detail
  /           Search (in Browse tab)

Actions:
  o           Open in $EDITOR
  c           Copy (command, path, or error)
  r           Refresh
  m           Open man page (if exists)

General:
  q           Quit
  ?           Help
```

### Deliverables

1. **LM log storage**: Add `lm_log.jsonl` append during LM invocations. Add
   optional full prompt/response storage in `lm_log/` directory.

2. **Work tab**: Rewrite data loading to group surface items by verification
   status. Add detail view with prereqs, evidence preview.

3. **Log tab**: New view showing LM invocation history from `lm_log.jsonl`.
   Detail view shows per-item outcomes and prompt/response access.

4. **Browse tab**: Simplified file tree replacing old Intent/Outputs/History
   tabs. No artifact existence checks—just show what's there.

5. **Remove old tabs**: Delete Intent, Evidence, Outputs, History tab code.
   Delete associated data loading logic.

6. **Header simplification**: Keep status line (decision, next action) but
   remove gate labels (lock/plan fresh/stale). Those belong in `status --json`.

### Files to Modify

| File | Changes |
|------|---------|
| `src/inspect.rs` | New Tab enum (Work, Log, Browse), remove old types |
| `src/inspect/data.rs` | Rewrite for Work/Log/Browse data models |
| `src/inspect/app/view.rs` | Rewrite all draw_* methods for new tabs |
| `src/inspect/app/state.rs` | Simplify state, add detail view mode |
| `src/inspect/app/actions.rs` | Update for new navigation model |
| `src/workflow/lm_client.rs` | Add LM log appending |
| `src/workflow/apply/prereq_inference.rs` | Add LM log for prereq cycles |
| `src/enrich/paths.rs` | Add `lm_log_path()`, `lm_log_dir()` |
| `src/enrich/lm_log.rs` | NEW: LmLogEntry type, append/load functions |

### Acceptance Criteria

| Criterion | Validation |
|-----------|------------|
| Work tab shows unverified items | Run `bman git config`, open inspect, see --blob etc. in NEEDS FIX |
| Work detail shows prereqs | Select item with prereq, see seed preview |
| Log tab shows LM cycles | After enrichment, see prereq_inference and behavior cycles |
| Log detail shows outcomes | Select cycle, see which items succeeded/failed |
| Full prompt viewable | With --verbose, press `p` in log detail to see prompt |
| Browse shows prereqs.json | New M20 artifact appears in tree |
| Copy works | Press `c` on Work tab, paste shows `bman apply --doc-pack ...` |
| Old tabs removed | No Intent/Evidence/Outputs/History in codebase |

### Out of Scope

- In-TUI editing (remains read-only)
- Running apply from within TUI
- Real-time updates during enrichment (manual refresh only)
- Syntax highlighting for JSON/prompt content
- Search within file content (only file name search in Browse)

## M20 — LM-Driven Prereq and Fixture Generation (done)

Goal: Enable LMs to define prerequisite requirements from documentation and
synthesize appropriate fixtures, with tool support for suggestion and validation
but not automation of the creative/semantic work.

Motivation:
- M19 achieved ~74% behavior verification for `git config` (25/34 options).
- Remaining options fail due to environment requirements (repo context, config
  keys, interactive editors) that the LM doesn't address.
- The tool surfaces `auto_verify_exit_code` and `auto_verify_stderr` but the LM
  needs structured guidance to act on this evidence.
- Fixture generation must generalize beyond git to any CLI tool.

Design principles:
- **Documentation-first**: LM infers prereqs from help text and man pages, not
  just from error messages. Errors confirm/refine, not discover.
- **LM-owned semantics**: Prereqs are LM-authored definitions encoding binary-
  specific knowledge. Tool provides structure, not vocabulary.
- **No automation loops**: Tool suggests prereqs from stderr patterns; LM
  decides whether to act. Tool doesn't automatically retry with fixtures.

Delivered:
- **Prereq schema in semantics.json**: Extended schema (v6) with `prereqs` map
  (category, description, seed, exclude_from_auto_verify) and `prereq_suggestions`
  array for stderr pattern matching.
- **Prereq inference pipeline**: After surface discovery, LM analyzes surface
  item descriptions to classify prereqs (filesystem, interactive, network, etc.)
  and generate appropriate seed fixtures. Results cached in `enrich/prereqs.json`.
- **Auto-verify integration**: Auto-verification reads inferred prereqs to skip
  excluded items (interactive, network, privilege) and use prereq seeds as
  fixtures for remaining scenarios.
- **Scope-aware inference**: Prereq inference respects `--context` scoping so
  `bman git config` only infers prereqs for config options, not all git surfaces.
- **FlatSeed format**: Simplified seed format (dirs, files, symlinks, executables)
  for LM responses, converted to canonical ScenarioSeedSpec internally.
- **Prereq suggestions in decisions**: `suggested_prereq` field appears in
  decisions output when auto-verify stderr matches prereq_suggestions patterns.
- **Agent prompt guidance**: Updated `enrich/agent_prompt.md` with prereq
  workflow documentation (discovery → define → annotate → author).

Known limitations (deferred):
- User override via `surface.overlays.json` prereq_override field is wired in
  schema but not read by auto-verify (can be added when needed).
- No automated E2E regression test (manual E2E validated).

### Prereq Model

**Categories** (fixed set for consistency):
- `filesystem` — needs files/directories to exist
- `config` — needs configuration values
- `state` — needs prior state (history, packages, etc.)
- `interactive` — requires user input
- `network` — requires network access
- `privilege` — requires elevated permissions

**Prereq definitions** (LM-authored in `enrich/semantics.json`):
```json
{
  "prereqs": {
    "git_repository": {
      "category": "filesystem",
      "description": "Minimal git repository structure",
      "seed": {
        "entries": [
          { "path": ".git/HEAD", "content": "ref: refs/heads/main\n" },
          { "path": ".git/config", "content": "[core]\nbare = false\n" }
        ]
      },
      "auto_verify": "retry_with_seed"
    },
    "interactive_editor": {
      "category": "interactive",
      "description": "Opens external editor for input",
      "auto_verify": "exclude",
      "exclude_reason": "Cannot simulate editor interaction in sandbox"
    }
  },
  "prereq_suggestions": [
    { "stderr_contains": "not in a git directory", "suggest": "git_repository" },
    { "stderr_contains": "not a git repository", "suggest": "git_repository" }
  ]
}
```

**Surface prereqs** (LM-authored in `inventory/surface.overlays.json`):
```json
{
  "overlays": [
    { "id": "--edit", "prereqs": ["interactive_editor"] },
    { "id": "--get", "prereqs": ["git_repository"] }
  ]
}
```

**Scenario prereqs**: Scenarios inherit prereqs from surface overlays or specify
explicitly. Seed can use prereq template or override with inline seed.

### LM Workflow

1. **Documentation analysis**: LM reads help/man, identifies environmental
   requirements, creates prereq definitions in semantics.json.

2. **Surface annotation**: LM updates surface overlays to tag options with
   prereqs based on documentation understanding.

3. **Auto-verify runs**: Tool runs scenarios, records stderr. If stderr matches
   `prereq_suggestions`, adds `suggested_prereq` to decisions output.

4. **Scenario authoring**: LM uses prereq definitions to build seeds for
   scenarios that verify behavior.

### Deliverables

1. **Prereq schema in semantics.json**: Category, description, seed template,
   auto_verify policy (retry_with_seed | exclude | default).

2. **Surface prereqs in overlays**: Per-option prereq annotations as source of
   truth for what options need.

3. **Prereq suggestions via stderr patterns**: LM-authored patterns in
   `semantics.json.prereq_suggestions`. Tool matches failures, adds
   `suggested_prereq` to decisions output. Suggestion only, no automatic action.

4. **Scenario prereq inheritance**: Scenarios can specify prereq explicitly or
   inherit from surface overlay. Inline seed overrides prereq's seed template.

5. **Agent prompt: prereq workflow**: Document the discovery→define→annotate→
   author flow with examples.

### Acceptance Criteria

| Item | Outcome |
|------|---------|
| LM defines prereqs from documentation | Prereq definitions in semantics.json |
| LM annotates surface items with prereqs | Surface overlays updated |
| Auto-verify failure suggests prereq | Decisions shows `suggested_prereq` |
| LM authors scenario with prereq seed | Scenario verifies successfully |
| Interactive options excluded via prereq | `interactive_editor` with exclude policy |
| Pattern generalizes beyond git | Documented approach for other CLIs |

### Out of Scope

- Automatic retry with prereq seeds (tool suggests, LM acts)
- Complex seed composition/merging (scenarios override completely or use as-is)
- Cross-binary prereq libraries (each pack defines its own)
- Complex stateful fixtures (commit history, installed packages)

## M19 — Pack-Owned Verification Semantics (done)

Goal: Remove hardcoded option/subcommand verification semantics from the tool.
Make surface kinds, verification strategies, and auto-scenario generation fully
pack-owned so LMs can handle arbitrary CLI structures (including multi-level
subcommand hierarchies like `git config --global`).

Motivation:
- M18 validated `ls` (single command, flat options). Testing `git config`
  revealed that the tool bakes in assumptions about what "option" and
  "subcommand" mean and how to verify each.
- The tool currently has hardcoded `VerificationTargetKind::Option` and
  `VerificationTargetKind::Subcommand` with different verification behaviors.
- Behavior tier only targets options (hardcoded). Subcommands can only reach
  existence verification.
- No parent-child relationship support: `git config --global` cannot be modeled
  as an option belonging to the `config` subcommand.

Delivered:
- **Surface schema v3**: Added `parent_id` and `context_argv` fields to surface
  items, enabling hierarchical option→subcommand relationships.
- **Pack-owned auto-scenario generation**: `queries/auto_scenarios.sql` now
  constructs verification argv; Rust executes but doesn't interpret.
- **Kind derivation**: Removed stored `kind` field; derived from context_argv
  (entry points = subcommands) and id prefix (dash = option).
- **Auto-verify evidence exposure**: Added `auto_verify_exit_code` and
  `auto_verify_stderr` to decisions output, enabling LMs to diagnose failures.
- **Recursive subcommand discovery**: Help scenarios for discovered subcommands
  enable multi-level surface discovery (`git config` options from `git config --help`).
- **Simplified seed handling**: Removed `seed_dir` in favor of inline seed specs
  via `binary_lens run_seed_spec`.
- **CLI cleanup**: Removed `merge_behavior_edit` command; simplified interface.
- **git config validation**: Achieved ~74% behavior verification (25/34 options);
  remaining options blocked on fixture requirements (repo context, interactive).
- **ls backward compatibility**: Confirmed working with all changes.

Known limitations (deferred to M20):
- Options requiring git repo context (e.g., `--edit`) fail auto-verification
  with "not in a git directory" but LM doesn't synthesize fixtures from this.
- Interactive options timeout but aren't recognized as "verified via timeout".

## M18 — End-to-End LM Agent Validation (done)

Goal: Validate that small LM agents can drive behavior verification to completion
using the simplified M17 workflow, and identify remaining friction points.

Motivation:
- M17 simplified the tool surface for LM agents, but we hadn't validated that
  small models (Haiku-class) can actually complete behavior verification loops.
- Real agent runs exposed edge cases and workflow gaps not visible from manual
  testing.

Delivered:
- **Agent harness**: `apply --max-cycles N --lm CMD` runs LM in a loop, capturing
  decisions, responses, and outcomes. Unified `bman <binary>` command runs full
  enrichment loop (init → apply → render) like `man` displays pages.
- **Baseline agent prompt**: Built-in system/user prompts in `lm_client.rs` with
  FlatSeed format for simplified scenario authoring.
- **E2E validation**: Fresh `ls` pack reaches 92-96% behavior verification
  (77-81 of 84 options) in 15-17 minutes without manual intervention.
- **Progress tracking**: `verification_progress.json` detects stuck loops via
  retry counts and delta signatures; auto-escalates after `BEHAVIOR_RERUN_CAP`.
- **Failure modes categorized**: `no_scenario`, `outputs_equal`, `assertion_failed`,
  `assertion_gap` with fix hints and evidence paths.
- **Performance tuning**: `BEHAVIOR_BATCH_LIMIT` tuned to 15 for speed/quality
  balance.

Known limitations (not LM failures):
- `-Z` (SELinux context): Requires SELinux-enabled system to produce observable
  output difference.
- `-w` (output width): Terminal width detection doesn't differ in sandbox
  environment.

Out of scope (deferred):
- Fully automated CI integration.
- Multi-agent orchestration or parallel verification.
- `git` multi-command CLI validation (infrastructure ready, not exercised).

## M17 — Behavior Authoring Ergonomics Simplification (done)

Goal: Make behavior verification tractable for small LM agents by reducing
decision complexity and providing structured workflow interfaces.

Delivered:
- **Fewer decisions per unverified item**: Reason codes consolidated from 14+ to
  4 (`no_scenario`, `scenario_error`, `assertion_failed`, `outputs_equal`). Each
  code maps to a single remediation path.
- **Simpler exclusion authoring**: Exclusions require only `delta_variant_path`
  evidence—no workaround history tracking.
- **Deterministic retry behavior**: Stuck scenarios surface via threshold counts;
  the tool decides when to recommend exclusion.
- **Closed-loop LM workflow**: `bman status --decisions` emits a focused work
  queue; `bman apply --lm-response <file>` validates and applies actions. This
  enables `status → LM inference → apply` without manual JSON editing.
- **Merge-style scenario editing**: `edit_strategy: "merge_behavior_scenarios"`
  allows scoped upserts instead of full-file replacement.
- **Auto-included baseline scaffolding**: No separate "add baseline" step.
- **Slim status output**: Default `status --json` is actionability-first;
  `--full` retains triage detail.

### Historical notes (pre-simplification M17 draft)

Goal: Expand behavior verification beyond no-arg flags by making value-taking
options mechanically testable. This milestone focuses on **forms completeness**
and **value readiness gating** so small LMs can make deterministic progress on a
bounded behavior suite without guessing argv/value tokens.

Motivation:
- M16’s baseline+assertion model works well for toggles, but many real options
  take values. Without pack-owned examples, behavior runs often fail before they
  can produce a meaningful baseline→variant delta.
- Help syntax often encodes value shapes (`--color[=WHEN]`, `--hide=PATTERN`),
  but we currently under-preserve those raw forms and don’t reliably surface
  “what argv should I try” as a mechanical prerequisite.

Design constraints (non-negotiable for this milestone):
- JSON-only structured artifacts in the doc pack (JSONL permitted for history).
- No scores/percent truth: behavior verification remains pass/fail over explicit
  assertions with evidence refs + explicit reason codes for unmet status.
- Keep semantics out of Rust: interpretation lives in pack SQL and pack-owned
  JSON (semantics + overlays), not hardcoded strings in code.
- Evidence remains append-only; `apply` remains transactional.
- Safety-first execution remains enforced (bounded timeouts/outputs + sandboxing,
  network off unless explicitly enabled by the plan).
- Usage + surface discovery stay help-only: behavior scenarios must not change man
  rendering inputs or surface growth.
- Treat short/long forms as distinct targets for now (no alias linking).

Deliverables:
1) **Surface forms completeness (help-only, evidence-linked)**
- For each discovered surface item `(kind,id)`, `inventory/surface.json` records
  `forms[]` as all distinct raw help forms that canonicalize to that `id`
  (within the lens’s option/subcommand classification domain).
- Canonicalization keeps mapping common patterns to stable ids:
  - `--color[=WHEN]` → `--color` (forms retain bracketed form)
  - `--hide=PATTERN` and `--hide PATTERN` → `--hide`
  - `-w COLS` → `-w`

2) **Value readiness gating (overlay-first for value options)**
- When `verification_tier: "behavior"` requires a surface id whose
  `invocation.value_arity` is `required`, status must recommend editing
  `inventory/surface.overlays.json` overlays to add `invocation.value_examples[]`
  before suggesting any behavior scenario for that id.
- Overlays remain pack-owned hints only (no behavior semantics): supported fields
  stay limited to:
  - `invocation.value_examples[]` (safe concrete argv tokens)
  - `invocation.requires_argv[]` (explicit extra argv tokens needed for meaning)

3) **Behavior suite expansion stays bounded and finishable**
- Behavior tier requires all option surface ids (minus explicit exclusions).
- Expand behavior coverage iteratively, focusing on value-taking options with
  seed-grounded add/remove assertions and clear baseline→variant deltas, while
  explicitly deferring known hard classes (tty/locale/time/format-width/
  numeric-format options) via exclusions.

4) **Deterministic next_action order remains stable**
- For behavior tier, status recommends next actions in this order:
  1) run `apply` until existence auto-verification is complete
  2) add missing surface inventory entry (if applicable)
  3) add missing value_examples overlay for required-value options
  4) add/repair baseline scenario
  5) add per-id behavior scenario stub (baseline_scenario_id + assertions)
  6) resolve behavior reason codes (edit scenarios)
  7) run `apply` when scenarios exist but have not run
- Status/plan summarize “why unmet” with reason codes (counts + previews),
  including value-readiness reasons (e.g. missing_value_examples).

Historical functionality snapshot (before ergonomics simplification):
- Help-only surface discovery populates `inventory/surface.json` with `forms[]` and
  `invocation.*` (value arity/separator/placeholder) derived mechanically from
  `help--*` scenario evidence.
- Behavior tier is opt-in: set `enrich/config.json` `"verification_tier": "behavior"`.
- `apply` runs existence auto-verification first (accepted tier) and writes
  append-only evidence under `inventory/scenarios/auto_verify::option::*`, bounded
  per apply by `verification.policy.max_new_runs_per_apply`.
- After existence is complete, behavior verification is gated work: the pack must
  supply a baseline scenario and (typically) one per-option behavior scenario with
  assertions; `status --json` drives this one edit/run step at a time.

Historical example (before ergonomics simplification):
- E2E `ls` pack (Haiku) — behavior tier, exhaustive options
  - Setup:
    - `./target/debug/bman init --doc-pack /tmp/bman-haiku-ls-behavior-e2e --binary ls --force`
    - edit `/tmp/bman-haiku-ls-behavior-e2e/enrich/config.json` → `"verification_tier": "behavior"`
    - edit `/tmp/bman-haiku-ls-behavior-e2e/scenarios/plan.json` → `verification.policy.max_new_runs_per_apply: 200`
  - Loop (repeat until `decision: complete` or blocked):
    - `./target/debug/bman apply --doc-pack /tmp/bman-haiku-ls-behavior-e2e`
    - `./target/debug/bman status --doc-pack /tmp/bman-haiku-ls-behavior-e2e --json`
  - What `status --json` drove:
    - Existence completed mechanically first (`accepted_verified_count: 84`, `accepted_unverified_count: 0`), writing append-only evidence under
      `/tmp/bman-haiku-ls-behavior-e2e/inventory/scenarios/auto_verify::option::*`.
    - Behavior then gated on “all options minus exclusions” (`behavior_verified_count: 0`, `behavior_unverified_count: 78`, `excluded_count: 6`).
    - Next actions alternated between:
      - edit `/tmp/bman-haiku-ls-behavior-e2e/inventory/surface.overlays.json` to add `invocation.value_examples[]` overlays (required-value options), and
      - edit `/tmp/bman-haiku-ls-behavior-e2e/scenarios/plan.json` to add a baseline scenario (once) and then add one per-option behavior scenario (baseline+assertions),
      - then rerun `apply` to execute the newly-authored behavior scenario.
    - When exclusions were necessary, they were recorded as `verification.queue[]` entries (`intent: "exclude"`, non-empty `prereqs[]`, and a short `reason`).
    - End state (this run): `decision: "incomplete"` with `next_action.kind: "edit"` for `scenarios/plan.json` (“add behavior scenario for --dereference”); existence was complete but
      no option behaviors were yet verified.

Acceptance criteria:
- Fresh `ls` pack: existence verification still completes via the existing
  auto-verify policy (no authored per-option existence scenarios).
- Surface forms completeness preserves bracketed/equals/space value forms in
  `forms[]` for canonical ids (e.g. `--color`).
- With `verification_tier: "behavior"`, a bounded suite including several
  value-taking options reaches `decision: complete` when each target is behavior
  verified or explicitly excluded with objective prereqs/reason codes.
- Behavior runs do not affect usage/surface discovery or man usage extraction
  (help-only lenses remain the sole inputs to those).

Out of scope:
- Alias linking/deduping (`-a` vs `--all`) or semantic grouping of options.
- Auto-inference of value ranges, inter-option dependencies, or conflict graphs.
- Making behavior verification exhaustive for all `ls` options without explicit
  exclusions.

## M16 — Surface Definition v2 + Behavior Verification Suite (ls options) (done)

Goal: Make behavior verification finishable and mechanically meaningful by first
solidifying what “surface” means for `ls` options, then verifying a **representative
suite** of option behaviors using pack-owned, seed-grounded assertions evaluated in
SQL (no Rust semantics, no heuristic scoring).

Motivation:
- M14/M15 make existence/recognition verification cheap and mechanical, but do
  not confirm documented behavior.
- Behavior verification is only as good as the surface model: we need stable,
  canonical option IDs and minimal structural shape so scenarios can be authored
  mechanically and safely.
- Small LMs should spend effort interpreting help evidence into testable claims,
  not reverse-engineering tool heuristics or guessing invocation forms.

Design constraints (non-negotiable for this milestone):
- JSON-only structured artifacts in the doc pack (JSONL permitted for history).
- No scores/percent truth: verification is pass/fail over explicit assertions
  with evidence refs + explicit reason codes for unmet status.
- Keep semantics out of Rust: parsing/interpretation lives in pack SQL and
  pack-owned JSON (semantics + overlays), not hardcoded strings in code.
- Evidence remains append-only; `apply` remains transactional.
- Safety-first execution remains enforced (bounded timeouts/outputs + sandboxing,
  network off unless explicitly enabled by the plan).
- Usage + surface discovery stay help-only: behavior scenarios must not change man
  rendering inputs or surface growth.
- Treat short/long forms as distinct targets for now (e.g. `-a` and `--all` are
  verified independently; no alias linking in this milestone).

Deliverables:
1) **Surface v2 is canonical and structurally useful (help-only, evidence-linked)**
- Extend `inventory/surface.json` to record (per item) the help-syntax forms and a
  minimal invocation shape derived mechanically from help evidence:
  - `forms[]` (raw forms seen in help; evidence-linked)
  - `invocation.value_arity`: `none|optional|required|unknown`
  - `invocation.value_separator`: `none|space|equals|either|unknown`
  - `invocation.value_placeholder` (e.g. `WHEN`, `PATTERN`, `SIZE`) when available
- Fix `queries/options_from_scenarios.sql` canonicalization so common patterns map
  to stable IDs:
  - `--color[=WHEN]` → `--color`
  - `--hide=PATTERN` and `--hide PATTERN` → `--hide`
  - `-w COLS` → `-w`
- Surface completeness gate: if a behavior target is queued but missing from
  `inventory/surface.json`, status must recommend fixing the surface lens or
  adding a surface overlay before suggesting behavior scenarios.

2) **Surface overlay v2 (pack-owned hints, no behavior semantics)**
- Extend `inventory/surface.overlays.json` from “seed items” to also support
  overlaying existing surface items keyed by `(kind,id)` (help-derived items are
  still the source of truth; overlays only add missing structure).
- Supported overlay fields (optional, evidence-linked if derived):
  - `invocation.value_examples[]` (safe concrete examples for required/optional
    values; prefer examples over “ranges”)
  - `invocation.requires_argv[]` (explicit additional argv tokens needed to make
    the option’s output meaningful, when discovered)
- Overlays must not be required for existence verification; they exist only to
  make behavior scenario authoring tractable and mechanical.

3) **Behavior scope becomes option-exhaustive (minus explicit exclusions)**
- With `verification_tier: "behavior"`, the behavior-required set is all option
  surface ids (minus explicit exclusions).
- Objective skips are expressed as `scenarios/plan.json.verification.queue[]`
  entries with `intent: "exclude"` that must include enum `prereqs[]` + `reason`,
  and are enumerated in `verification_ledger.json` as excluded targets.

4) **Behavior is defined by baseline+variant and seed-grounded assertions (SQL-evaluated)**
- Behavior scenarios are `kind: "behavior"` and must declare:
  - `baseline_scenario_id` (baseline run to compare against)
  - `assertions[]` (typed vocabulary evaluated in SQL, not Rust)
- Extend `enrich/semantics.json` to configure assertion normalization (ANSI
  stripping and basic whitespace normalization) and any pack defaults.
- Extend `queries/verification_from_scenarios.sql` to:
  - join baseline + variant evidence via `baseline_scenario_id`
  - normalize stdout/stderr using pack semantics
  - evaluate `assertions[]` deterministically
  - mark `behavior_status: "verified"` only when scenario runs pass and the
    assertions pass, with explicit `behavior_unverified_reason_code` otherwise
- Policy: diff-only checks may exist, but **must not** be sufficient on their own;
  at least one assertion must be anchored to seeded facts (e.g., a seeded file
  path) so expectations are grounded in pack-owned evidence.

5) **Deterministic next_action loop + reason-code visibility**
- When behavior verification is required and unmet, status chooses the next missing
  suite id in a stable order and emits exactly one next action: edit
  `scenarios/plan.json` (baseline stub first, then per-id behavior stub).
- Status/plan must summarize “why unmet” using reason codes (counts + previews),
  so “not feasible yet” is evidence-backed (missing baseline, missing scenario,
  missing semantic predicate, outputs equal, excluded with prereqs, etc.).

Acceptance criteria:
- Surface v2 canonicalization discovers previously-missed help forms (e.g.
  bracketed `--color[=WHEN]`-style options) as stable IDs with structural shape.
- Fresh `ls` pack: existence verification still completes via the existing
  auto-verify policy (no authored per-option existence scenarios).
- With `verification_tier: "behavior"`, `bman status --json` reaches `decision:
  complete` when every suite target is behavior-verified by passing assertion
  checks or explicitly excluded with objective prereqs/reason codes.
- Behavior runs do not affect usage/surface discovery or man usage extraction
  (help-only lenses remain the sole inputs to those).

Out of scope:
- Alias linking/deduping (`-a` vs `--all`) or semantic grouping of options.
- Auto-inference of value ranges, inter-option dependencies, or conflict graphs.
- Behavior verification for all `ls` options by default (requires new fixture
  primitives like timestamp control, SELinux support, and/or tty modeling).

## M15 — Batched Auto-Verification (Subcommand Existence) v1 (done)

Goal: Extend the M14 “apply until done” loop to **subcommands**, so multi-command
CLIs (e.g. `git`) can reach “existence verified” without authoring per-subcommand
scenarios.

Motivation:
- M14 verifies option existence, but command-centric tools may have few/no
  options and a large subcommand surface.
- We want the same mechanical, bounded loop for subcommands with pack-owned
  invocation and interpretation (evidence > scores).

Design constraints (non-negotiable for this milestone):
- JSON-only structured artifacts in the doc pack (JSONL permitted for history).
- Keep parsing semantics out of Rust: invocation shape + evidence interpretation
  stay pack-owned (semantics JSON + pack SQL), not hardcoded strings.
- Evidence remains append-only; `apply` remains transactional.
- Safety-first execution remains enforced (bounded timeouts/outputs + sandboxing,
  network off unless explicitly enabled by the plan).
- Usage + surface discovery stay help-only: verification runs must not change man
  rendering inputs or surface growth.

Deliverables:
1) **Verification policy becomes kind-driven (breaking change acceptable)**
- Replace `scenarios/plan.json.verification.policy.mode` with an ordered list
  `verification.policy.kinds`.
- v1 supported kinds: `option`, `subcommand`.
- Keep objective exclusions:
  - explicit exclusions by `surface_id` with enum `prereqs` + `reason`
  - explicit batch bound `max_new_runs_per_apply`

2) **Pack-owned subcommand invocation shape**
- Extend `enrich/semantics.json` with:
  - `verification.subcommand_existence_argv_prefix`
  - `verification.subcommand_existence_argv_suffix`
- Default suffix is `["--help"]`; packs override when they require `-h`, `-?`, or
  an alternate help affordance (e.g. `help <cmd>`-style CLIs).

3) **Batched auto-verification runs for subcommands**
- During `apply`, expand the policy into implicit verification runs over
  `inventory/surface.json` items with `kind: "subcommand"` (deterministic
  ordering, stable synthetic IDs).
- Run at most `max_new_runs_per_apply` new subcommands per `apply`; re-running
  `apply` continues where it left off using the existing skip/index mechanism.
- Evidence is recorded as scenario evidence under `inventory/scenarios/*.json`
  (append-only).

4) **Verification lens/ledger/status support**
- Extend `queries/verification_from_scenarios.sql` + `verification_ledger.json`
  generation to include subcommand targets, with accepted/rejected classification
  driven by `enrich/semantics.json` rules (no argv-token parsing assumptions in
  Rust).
- `bman plan` and `bman status --json` summarize remaining targets by kind and
  recommend `run: bman apply ...` until remaining reaches 0.
- When blocked (missing/invalid policy, unusable semantics), `status --json`
  recommends editing a single concrete target (`scenarios/plan.json` or
  `enrich/semantics.json`).

Acceptance criteria:
- Fresh `git` pack: enable `verification.policy.kinds` to include `subcommand`
  and rerun `apply` until verification is met (`verification_ledger.json`
  `unverified_count` becomes 0).
- Verification runs do not change `inventory/surface.json` or man usage
  extraction (help-only lenses remain the sole inputs to those).
- The loop stays bounded and mechanically resumable, with `scenarios/plan.json`
  remaining compact.

Out of scope:
- Multi-token/nested subcommands (space-separated command hierarchies).
- Subcommand behavior verification (side effects) or argument synthesis beyond
  help-style existence checks.
- Auto-inference of safe invocation shapes or exclusion prereqs.

## Side Quest — Read-only Doc-Pack Inspector TUI v1 (done)

Goal: Make doc packs easy to inspect and navigate without memorizing artifact
paths or workflow mechanics. The TUI is **read-only**: it helps users understand
state and jump into external tools (editor/man), but does not replace the
validate/plan/apply loop.

Motivation:
- Doc packs now have a stable mechanical workflow, but it is still hard to
  quickly answer: “what’s the next action?”, “what changed?”, and “where is the
  evidence?”.
- A simple inspector reduces cognitive overhead and makes it practical to work
  with larger, multi-command binaries (e.g. `git`).

Design constraints (non-negotiable for this side quest):
- Read-only: the inspector must not modify the doc pack.
- Portability: runnable from any CWD; no repo-root dependencies.
- Lock + plan freshness are global state:
  - Always visible as a sticky header/status bar.
  - Optionally exposed via a small “plan/lock details” popover (inputs hash,
    etc).
- Implementation: use `ratatui` (Rust) for a simple, keyboard-first TUI.
- Minimize duplicate logic: the inspector should render the same core state as
  `bman status --json` by calling the same internal evaluation code (not by
  re-implementing decisions in the UI).
- Accessibility:
  - Do not rely on color alone for meaning (always show textual labels).
  - If not running in a TTY, print a structured text summary instead of
    attempting to draw the TUI.

Deliverables:
- New command: `bman inspect --doc-pack <dir>` (read-only `ratatui` TUI).
- Sticky header/status bar always shows:
  - doc pack path, binary name
  - lock: `missing|stale|fresh`
  - plan: `missing|stale|fresh`
  - decision: `complete|incomplete|blocked`
  - next action: the single deterministic recommendation from `status --json`
- Primary views/tabs:
  - **Intent**: list edit targets (`scenarios/plan.json`, `enrich/semantics.json`,
    `enrich/config.json`, `binary_lens/export_plan.json`, `queries/*.sql`) with
    “open in $EDITOR”.
  - **Evidence**: scenario evidence inventory (`inventory/scenarios/*.json`) with
    bounded stdout/stderr previews and links back to scenario ids.
  - **Outputs**: surface + ledgers + rendered man outputs
    (`inventory/surface.json`, `verification_ledger.json`, `man/<bin>.1`,
    `man/meta.json`) including warnings.
  - **History/Audit**: transactional/audit artifacts (`enrich/report.json`,
    `enrich/history.jsonl`, last txn metadata).
- Category coloring for scanability:
  - Intent / Evidence / Outputs / Audit are distinct colors; gates use
    traffic-light colors.
- Navigation/ergonomics:
  - `o`: open the selected file in `$EDITOR`.
  - `m`: open the rendered man page via `man -l` (when present).
  - `c`: copy the selected path or recommended command line.
  - `r`: refresh/reload artifacts (no file watcher required for v1).
  - Lists default to counts + previews; “show all” is explicit.

Non-goals:
- In-TUI editing of JSON or SQL.
- Embedding a pager for large text; rely on external editor/man pager.
- Running validate/plan/apply from inside the TUI (may be added later if needed).

Acceptance criteria:
- For fresh `ls` and `git` packs, `bman inspect` shows gate/decision state that
  matches `bman status --json` and provides a clear next action.
- Users can jump to the relevant file(s) in `$EDITOR` and open the generated man
  page (`man -l`) from the inspector.

## M14 — Batched Auto-Verification (Options Existence) (done)

Goal: Make “verify existence for all discovered options” cheap and mechanical,
without requiring an LM to author hundreds of near-identical scenarios. `apply`
runs verification in bounded batches and `status --json` recommends “run apply
again” until verification is met.

Motivation:
- We can discover option surface area from help evidence, but getting from
  “discovered” to “verified accepted/rejected” is still too much manual work for
  large CLIs.
- The tool should execute mechanics; the pack (and LM) should own meaning
  (invocation shape + evidence interpretation).

Design constraints (non-negotiable for this milestone):
- JSON-only structured artifacts in the doc pack (JSONL permitted for history).
- Keep parsing semantics out of Rust: verification meaning stays pack-owned
  (JSON + pack SQL), not hardcoded strings/patterns in code.
- Evidence remains append-only; `apply` remains transactional (no partial publish
  of ledgers/man outputs on failure).
- Safety-first defaults remain enforced (bounded timeouts/outputs + sandboxing,
  network off unless explicitly enabled by the plan).
- Scope: options only (defer command/subcommand verification).

Deliverables:
1) **Compact verification policy (no per-flag scenarios)**
- Extend `scenarios/plan.json` with a strict, schema-validated verification
  policy that can express:
  - “verify all discovered options”
  - explicit exclusions by `surface_id` with objective `prereqs` + `reason`
  - an explicit batch bound `max_new_runs_per_apply`
- `bman status --json` recommends editing `scenarios/plan.json` when the policy
  is missing/invalid.

2) **Batched auto-verification runs**
- During `apply`, expand the policy into implicit verification runs over
  `inventory/surface.json` (deterministic ordering, stable synthetic IDs).
- Run at most `max_new_runs_per_apply` new targets per `apply`; re-running
  `apply` continues where it left off using the existing skip/index mechanism.
- Evidence is still recorded as scenario evidence under
  `inventory/scenarios/*.json` (append-only).

3) **Pack-owned invocation shape**
- Add pack-owned semantics fields in `enrich/semantics.json` to control how an
  option existence verification argv is formed (e.g. prefix/suffix arrays).
- Rust must not hardcode “append `--help`” or other CLI-specific conventions in
  this milestone.

4) **Plan transparency**
- `bman plan` writes a verification plan summary into `enrich/plan.out.json`
  (counts + a small preview list), so the user/LM can see what `apply` will run
  before executing.

5) **Status loop becomes “apply until done”**
- When the policy is present and verification is unmet due to remaining targets,
  `status --json` recommends `run: bman apply ...` (not “author N stub
  scenarios”).
- When targets are excluded, status reports excluded counts + prereq tags (no
  heuristic scoring).

Acceptance criteria:
- Fresh `ls` pack: set policy to “verify all discovered options” and rerun
  `apply` until verification is met (ledger unverified_count becomes 0).
- Verification progress is bounded per apply and mechanically resumable.
- No explosion of authored scenarios in `scenarios/plan.json`; the plan remains
  compact and edits remain obvious from status.

Out of scope:
- Command/subcommand existence verification.
- Behavior verification (expected side effects / fixtures beyond existence).
- Auto-inference of exclusions or “smart” prioritization heuristics.

## M13 — Verification Triage + Verification By Default v1 (done)

Goal: Make verification the default gate for new packs, while keeping the loop
safe and mechanically navigable for small LMs by requiring **explicit,
pack-owned triage** of what is in-scope to verify and what evidence is needed.

Motivation:
- “Surface discovery” from help output is a claim inventory; we still need a
  reliable, evidence-linked path to confirm options/subcommands are accepted and
  (eventually) behave as documented.
- For small LMs, verifying everything is not realistic; the agent should first
  narrow the target set using objective properties (not subjective “easy/hard”
  labels), then incrementally execute scenarios to reduce the unverified set.

Design constraints (non-negotiable for this milestone):
- JSON-only structured artifacts in the doc pack (JSONL permitted for history).
- Keep parsing semantics out of Rust: verification meaning stays pack-owned
  (JSON + pack SQL), not hardcoded strings in code.
- Evidence remains append-only; `apply` remains transactional.
- Safety-first defaults remain enforced (bounded timeouts/outputs + sandboxing,
  network off unless explicitly enabled in scenarios).

Deliverables:
- Verification enabled by default (opt-out, not opt-in):
  - Fresh `bman init` writes `enrich/config.json` with verification required at a
    default tier (`accepted`), and documents a simple opt-out (edit config).
- Pack-owned verification exclusions (schema bump in `scenarios/plan.json`):
  - Add a `verification` section with a `queue` of explicit exclusions.
  - Each queue entry uses objective properties (no fuzzy labels), e.g.:
    - `surface_id`: the item being verified (matches `inventory/surface.json`).
    - `intent`: `exclude` (requires non-empty `prereqs[]` and a short `reason`).
    - `prereqs`: a small fixed enum list describing required setup, e.g.
      `needs_arg_value`, `needs_seed_fs`, `needs_repo`, `needs_network`,
      `needs_interactive`, `needs_privilege`.
  - `status --json` reports excluded targets and otherwise uses deterministic
    policy/ledger-driven next actions (not queue ordering) to recommend edits
    or commands.
- Pack-owned semantics for “accepted” verification:
  - Extend `enrich/semantics.json` with matchers/rules used to classify scenario
    outputs as accepted vs rejected vs inconclusive, so localization/format
    differences are handled by pack edits (not tool changes).
  - Update the pack verification lens (`queries/verification_from_scenarios.sql`)
    to consume those semantics rules and emit evidence-linked statuses without
    argv-token parsing assumptions.
- Clear status reporting (no scores):
  - `status --json` distinguishes:
    - discovered-but-not-triaged surface items
    - triaged-but-unverified targets (behavior)
    - excluded targets (with reasons)
  - Next actions always point at one concrete edit target
    (`scenarios/plan.json`, a specific scenario id, or `enrich/semantics.json`).

Non-goals:
- Exhaustive option/subcommand behavior testing.
- Automatic inference of safe invocations, argument values, or fixtures.
- Adding per-binary “unknown option” string parsing in Rust.

Acceptance criteria:
- Fresh `ls` and `git` packs start with verification required by default and
  produce a deterministic next action to begin triage (then scenarios), without
  any repo-root dependencies.
- A small LM can drive `decision: complete` by iterating only on pack-owned
  artifacts (`scenarios/plan.json`, scenarios, `enrich/semantics.json`) and
  following `status --json` next actions, producing evidence-linked accepted
  verification for the policy-derived surface targets.

## M12 — Pack-Owned Semantics v1 (done)

Goal: Remove “meaning” heuristics from Rust (hardcoded strings/patterns for help
parsing/rendering and surface discovery selection). Make semantics a **pack-owned,
schema-validated JSON artifact** that an LM can edit, while Rust enforces
mechanics (schemas, determinism, gating).

Motivation:
- We still have implicit semantics in code (e.g., help section heuristics in
  `src/render.rs`) that are English/formatting-biased and brittle under
  localization or atypical help layouts.
- We want the LM to own interpretation, not the tool.

Design constraints (non-negotiable for this milestone):
- JSON-only structured artifacts in the doc pack (JSONL permitted for history).
- Keep parsing semantics out of Rust: no new help/CLI parsers. Semantics must be
  driven by pack-owned artifacts.
- Evidence remains append-only; `apply` remains transactional.
- Portability: pack runs from any CWD; no repo-root dependencies.

Deliverables:
- New pack-owned semantics artifact:
  - `<doc-pack>/enrich/semantics.json` (strict schema; deny unknown fields),
    installed by `bman init`.
  - Describes how to interpret help/usage evidence for rendering, via rule sets
    (e.g., regex/prefix selectors for synopsis lines, exit-status headings,
    boilerplate line filters, optional see-also extraction).
- Renderer becomes semantics-driven:
  - Refactor `src/render.rs` to use `enrich/semantics.json` for extraction and
    filtering instead of hardcoded strings.
  - Keep rendering deterministic; when semantics yield no results, render still
    succeeds but status reports the missing semantics as unmet with an explicit
    next action.
- Pack-owned help affordances (bootstrap, not hardcoded):
  - Default `scenarios/plan.json` includes a small, safe set of help scenarios
    that cover common help affordances (e.g. `--help`, `--usage`, `-?`), so the LM
    can adjust based on evidence instead of relying on tool assumptions.
  - When no usable help output exists (stdout/stderr empty/only noise), `status
    --json` recommends editing `scenarios/plan.json` to add/adjust help scenarios.
- Pack-owned discovery lenses:
  - Surface discovery is driven by the pack-local SQL templates under
    `queries/` (no config selection).
  - `bman validate` includes those lens templates in `enrich/lock.json`
    inputs.
- Lean artifact policy:
  - Only write `coverage_ledger.json` / `verification_ledger.json` when required
    by `enrich/config.json.requirements` (avoid confusing extra artifacts for
    small LMs).
  - Rename coverage ledger vocabulary to be surface-agnostic (avoid `option_*`
    terms when items are subcommands/commands).
- Reduce remaining tool-owned semantics (help + execution):
  - Move usage-evidence “reliability” filtering out of Rust (e.g. basis/status
    selection) and into pack-owned lenses/config, so packs can adjust for
    nonstandard evidence layouts.
  - Move runner env defaults (e.g. `LC_ALL`, `TERM`, `PAGER`) out of Rust and
    into pack-owned `scenarios/plan.json` defaults so the LM can see and edit
    them directly.
  - Remove parsing conventions from Rust that encode CLI semantics (e.g. argv
    token heuristics) in favor of pack-owned structure and/or pack-owned SQL
    interpretation.
- Status diagnostics for small LMs:
  - Extend `status --json` to summarize which pack lenses/templates were used
    (used/empty/error + evidence refs) so the next edit target is mechanically
    obvious without additional prose.
- Workflow integration + gating:
  - `bman validate` validates `enrich/semantics.json` and includes it in
    `enrich/lock.json` inputs.
  - `status --json` recommends editing `enrich/semantics.json` when rendering is
    blocked/unmet due to insufficient semantics.
- LM edit surface update:
  - Update `<doc-pack>/enrich/agent_prompt.md` to allow editing
    `enrich/semantics.json` (and only recommend editing `queries/**` when status
    explicitly points there).

Acceptance criteria:
- Fresh `ls` and `git` packs reach `decision: complete` without any tool-owned
  hardcoded `"Usage:"`-style assumptions.
- When help output is localized or atypically formatted, an LM can fix the man
  rendering loop by editing only pack-owned artifacts (starting with
  `enrich/semantics.json`), guided by `status --json`.
- When a binary’s help affordances differ (e.g. stderr-only usage, multiple help
  flags), the pack can be adapted by editing only pack-owned artifacts
  (`scenarios/plan.json` + pack SQL lenses), with `status --json` pointing at the
  smallest next action.

Out of scope:
- “Universal” help parsing or auto-learning semantics.
- Adding new binary-specific heuristics in Rust.

## M11.1 — Scenario Loop Rough-Edge Smoothing (done)

Goal: Keep “learn-by-executing scenarios” as the core agent job, but make the
loop cheaper and failures mechanically actionable (especially for small LMs).

Motivation:
- Scenario-based verification is the right direction, but can become slow and
  boilerplate-heavy as surface size grows.
- Small LMs should be able to progress mechanically from `status --json` without
  needing bespoke per-binary prompting or manual debugging.

Design constraints (non-negotiable for this milestone):
- JSON-only structured artifacts in the doc pack (JSONL permitted for history).
- Keep parsing semantics out of Rust: do not add help/CLI parsers; interpretation
  remains in pack-local SQL templates over scenario evidence.
- Evidence remains append-only; `apply` remains transactional.
- Safety defaults remain enforced (bounded timeouts/outputs + sandboxing).

Deliverables:
- Incremental scenario execution:
  - `apply` runs only new/changed/failed scenarios by default, keyed by a stable
    `scenario_digest` over the effective scenario + seed materialization inputs.
  - Provide explicit escape hatches: `--rerun-all` and `--rerun-failed`.
- Scenario plan `defaults` to reduce boilerplate:
  - Extend `scenarios/plan.json` to support a strict, schema-validated top-level
    `defaults` object (timeouts, net/sandbox/no_strace, snippet limits, cwd, env).
  - Evidence must record effective values so decisions remain reproducible.
- Runner environment normalization:
  - Apply safe, binary-agnostic env defaults (e.g. `LC_ALL=C`, `LANG=C`,
    `TERM=dumb`, `NO_COLOR=1`, `PAGER=cat`, `GIT_PAGER=cat`) unless overridden.
  - Record the final env used in scenario evidence.
- Status failure UX (deterministic next actions):
  - When a scenario fails, `status --json` includes a compact machine-readable
    failure summary and the evidence path(s), and recommends editing a single
    specific scenario ID.
- Two-tier verification (no scores):
  - Keep “accepted” (option/subcommand recognized) separate from “behavior”
    (seed + output/FS predicates), both evidence-linked in `verification_ledger.json`.
  - `enrich/config.json` can require either tier (default `accepted`).
- Pack-local agent prompt update:
  - Update `<doc-pack>/enrich/agent_prompt.md` to rely on incremental apply,
    plan defaults, and the accepted/behavior split; remove binary-specific argv hints.

Non-goals:
- Negative-testing framework or exhaustive combination testing.
- Auto-inference of option argument values or baked-in help parsing semantics.

Acceptance criteria:
- Editing one scenario and re-running `apply` re-executes only that scenario (and
  any required discovery), not the entire plan.
- A failing scenario yields a single deterministic next action plus evidence
  pointers sufficient for a small LM to proceed without extra narration.
- Haiku can reach “accepted” verification complete for `ls` using only
  `<doc-pack>/enrich/agent_prompt.md` + `status --json` loop, with stable iteration
  time due to incremental apply.

## M11 — Execution-Backed Verification v1 (done)

Goal: Move from “help-derived surface claims” to **execution-backed verification**
for surface IDs (starting with `ls`), using scenario evidence as the source of
truth. Keep decisions evidence-linked and avoid heuristic scoring.

Motivation:
- Help output is a claim, not evidence that an option/subcommand is accepted or
  behaves as documented.
- We want a simple LM to make progress mechanically by proposing scenarios (and
  inline seeds) without the tool baking in per-binary help/CLI parsing logic.

Design constraints (non-negotiable for this milestone):
- JSON-only structured artifacts in the doc pack (JSONL permitted for history).
- Scenarios remain the only execution concept; evidence is append-only.
- Keep parsing semantics out of Rust: interpretation lives in pack-local SQL
  templates over scenario evidence, not hardcoded parsers.
- Safety-first execution: bounded timeouts, bounded outputs, and sandboxing
  defaults remain enforced.

Deliverables:
- Scenario plan extensions (strict schema; schema bump):
  - Inline `seed` specification on scenarios so agents can define deterministic
    filesystem fixtures without authoring `fixtures/**` trees by hand. The tool
    materializes seeds into an isolated per-run directory.
- Pack-local verification lens:
  - Install/standardize `queries/verification_from_scenarios.sql` that produces a
    deterministic, evidence-linked verification status per surface ID using:
    `<doc-pack>/inventory/scenarios/*.json`, `<doc-pack>/inventory/surface.json`,
    and `<doc-pack>/scenarios/plan.json`.
  - Verified status must come from scenario outcomes (not plan-only `covers`
    claims). No confidence scores.
- Evidence-linked verification ledger:
  - Emit `verification_ledger.json` that:
    - enumerates per-surface status (`verified`, `recognized`, `unknown`, `inconclusive`)
      and an explicit unverified list
    - links each decision to concrete evidence refs (`inventory/scenarios/*.json`,
      `inventory/surface.json`, `scenarios/plan.json`)
- Mechanical gating and deterministic next actions:
  - When verification is enabled as a requirement, `status --json` drives the
    smallest next action to reduce unverified IDs (edit/add a single scenario,
    then `apply`).

Acceptance criteria:
- `ls`: starting from help-derived surface, agents can mechanically add acceptance
  scenarios (with inline seeds where needed) until every surface ID is
  `verified` or explicitly `blocked` with evidence-linked reasons.
- `git`: surface IDs like `commit.--amend` can be verified with explicit
  scenarios; multi-step behaviors may remain blocked until multi-step scenarios
  are supported.
- No scoring; all verification decisions and blockers cite concrete evidence.

## M10 — Scenario-Only Evidence + Coverage v1 (done)

Goal: Use a single concept — **scenarios** — for all execution-based evidence
(help/usage capture, surface discovery, examples, and optional coverage). Keep
decisions evidence-linked and avoid heuristic scoring.

Motivation:
- Reduce concepts and file formats a small LM must learn (scenarios only).
- Avoid baking help parsing semantics into the tool; keep parsing/editability in
  pack-local SQL templates.
- Make “coverage” mean “missing evidence items”, not a percent score.

Design constraints (non-negotiable for this milestone):
- JSON-only structured artifacts in the doc pack (JSONL permitted for history).
- Mechanical gating remains: edits don’t count until `apply` refreshes `lock.json`.
- Portability: everything runs from the doc pack, from any CWD.
- Keep it lean: do not add debug/provenance artifacts unless they’re needed as
  evidence inputs or hard requirements.

Deliverables:
- Scenario-only evidence:
  - Agent-edited: `<doc-pack>/scenarios/plan.json` (strict schema; includes help-style
    scenarios and behavior scenarios; includes optional `covers` claims).
  - Tool-written, append-only evidence: `<doc-pack>/inventory/scenarios/*.json`
    (normalized scenario results with bounded stdout/stderr).
- Lens-driven surface discovery from scenario evidence:
  - Install/standardize templates that read scenario evidence (not tool-parsed help):
    - `queries/usage_from_scenarios.sql`
    - `queries/subcommands_from_scenarios.sql`
    - `queries/options_from_scenarios.sql`
  - `inventory/surface.json` is derived from scenario evidence + optional seed and
    records discovery attempts and evidence refs; it blocks only when necessary
    (e.g., multi-command CLI detected but no subcommands extracted).
- Optional coverage gate (no scores):
  - Add an opt-in coverage requirement (not in `default_requirements`) that is met
    only when the uncovered surface ID list is empty (explicit list of missing items,
    evidence refs, and structured blockers/capability tags).
  - Coverage claims may be used as hints, but the tool must remain able to produce an
    uncovered list deterministically (no confidence scoring).

Acceptance criteria:
- Fresh `ls` and `git` packs can reach `decision=complete` for default requirements.
- When coverage is enabled, `status --json` drives the smallest next edit (scenario
  stubs or fixes) until uncovered is empty or blockers are explicit.
- Multi-command CLIs produce `.SH COMMANDS` or block with a single, concrete next action.
- Lock inputs include scenario plan + relevant lens templates so agents cannot
  “progress” by editing without re-validating.

Out of scope:
- Automatic scenario synthesis (LM-driven).
- A full interactive wizard/REPL UI.
- Perfect rollback of append-only evidence artifacts.

## M9 — Enrich v1 (JSON-only + Validate/Lock + Evidence-First Plan/Apply) (done)

Goal: Make doc-pack enrichment a **mechanically enforced** workflow with a
`init → apply` loop (apply auto-runs validate + plan), where all structured
artifacts are JSON (JSONL permitted for history) and decisions are driven by
evidence-linked requirements (not heuristic scores).

Motivation:
- Agents can currently edit files and “progress” without a disciplined loop.
- Percent/goal heuristics are useful as derived convenience, but not as truth.
- JSON-only structured config/state reduces ambiguity and enables strict validation.
- Doc packs must remain portable: runnable from any CWD with no repo-root deps.

Design constraints (non-negotiable for this milestone):
- JSON-only structured artifacts in the doc pack (JSONL permitted for history).
- Clean break from `bman enrich`: bootstrap with `init`, then iterate with `apply/status`
  (validate/plan remain optional debug steps).
- Edits “don’t count” until `apply` produces a fresh `lock.json`.
- Decisions are evidence-linked: every unmet requirement and blocker points to concrete artifacts.

Artifacts (doc pack):
- Agent-edited inputs (locked by `validate`):
  - `<doc-pack>/enrich/config.json` (desired state; strict schema; invalid rejected)
  - `<doc-pack>/scenarios/plan.json` (scenario plan; strict schema; agent-editable)
  - optional: `<doc-pack>/inventory/surface.overlays.json` (agent-provided surface overlays; stable IDs)
  - `<doc-pack>/queries/`, `<doc-pack>/binary.lens/views/queries/`, `<doc-pack>/scenarios/`, `<doc-pack>/fixtures/`
- Tool-written evidence (append-only / evidence-first):
  - `<doc-pack>/inventory/scenarios/*.json` (mechanical scenario outputs, captured as structured evidence)
  - `<doc-pack>/binary.lens/runs/index.json`, `<doc-pack>/binary.lens/runs/**` (scenario run evidence index + artifacts)
- Tool-written canonical inventory:
  - `<doc-pack>/inventory/surface.json` (canonical surface inventory; stable IDs + evidence refs)
- Tool-written workflow/state:
  - `<doc-pack>/enrich/lock.json` (authoritative input snapshot: selected inputs + hashes/snapshot id)
  - `<doc-pack>/enrich/history.jsonl` (authoritative event log; JSONL)
  - `<doc-pack>/enrich/plan.out.json` (derived plan; must match `lock.json`)
  - `<doc-pack>/enrich/report.json` (derived report; evidence-linked)
  - temporary: `<doc-pack>/enrich/txns/<txn_id>/**` (staging + backups for atomic apply; cleaned on success)
- Derived outputs (not authoritative for decisions):
  - `<doc-pack>/man/**` (rendered man page artifacts)
  - `<doc-pack>/coverage_ledger.json` (derived convenience view; never a progress gate)

Commands (clean break):
- `bman init --doc-pack <dir> [--binary <bin>]` writes a schema-valid starter `<doc-pack>/enrich/config.json` (and generates the pack if missing; `--binary` is required when creating a new pack).
- `bman validate --doc-pack <dir>` validates inputs and writes `<doc-pack>/enrich/lock.json` (optional).
- `bman plan --doc-pack <dir>` writes `<doc-pack>/enrich/plan.out.json` (optional).
- `bman apply --doc-pack <dir>` applies transactionally (auto-runs validate/plan) and writes `<doc-pack>/enrich/report.json`.
- `bman status --doc-pack <dir> [--json]` reports issues and the deterministic next action (stable machine-readable contract in `--json` mode).
- `bman enrich` is removed; use `init/apply/status` (validate/plan optional).

Mechanical gating:
- `apply` refreshes `lock.json` and `plan.out.json` when missing or stale.
- `status --json` always emits a machine-readable next action (even when lock is missing/stale).
- `apply` still executes from a deterministic plan snapshot tied to the current lock.

Surface discovery (first-class, no “confidence”):
- Goal: produce a canonical `<doc-pack>/inventory/surface.json` with stable item IDs and evidence refs (even when runtime help is missing/stripped).
- Tool collects help/usage evidence mechanically into `<doc-pack>/inventory/scenarios/*.json` and run artifacts under `<doc-pack>/binary.lens/runs/**`.
- Do not treat derived man artifacts as canonical help evidence; only accept scenario/run outputs as help/usage evidence inputs.
- `surface.json` records the discovery attempts taken (as stable event codes) and the evidence artifacts each attempt produced/consumed.
- Every discovered item includes evidence refs (paths + hashes, and run IDs where applicable).
- Subcommand discovery is driven by a pack-local SQL template (`queries/subcommands_from_scenarios.sql`) so parsing remains editable.
- When discovery is underconstrained, emit explicit blocker codes plus an evidence-linked “next unlock” action.
- V1 simplification: treat options/commands/subcommands as `surface.json` item kinds (no separate `options.json`, `commands.json`, …).

Evidence > scores:
- Requirements are predicates over canonical inventory IDs (`inventory/surface.json`) and canonical evidence indices (`inventory/scenarios/*.json`, `binary.lens/runs/index.json`).
- `man/examples_report.json` (only when publishable examples exist) and `coverage_ledger.json` may exist as derived views, but are never authoritative for gating decisions.
- Reports enumerate unmet requirements, missing evidence, and blockers as structured codes/tags with evidence refs.
- Metrics may be emitted only as derived summaries, never as authoritative decision inputs.

LLM UX helpers (tool-owned edits, not hand-crafted JSON):
- Provide scaffolding and formatting helpers so agents can follow `next_action` without manual multi-file JSON surgery.

Acceptance criteria:
- Starting from a moved doc pack (arbitrary CWD), an agent can iterate:
  `apply` until requirements are met or blocked, without modifying anything outside the doc pack.
- Starting from a doc pack with missing/stripped help output, the tool can still:
  - produce a surface inventory mechanically, or
  - fail with explicit blocker codes and an evidence-linked smallest “next unlock” action (scenario/fixture/manual seed).
- `status --json` always returns exactly one deterministic `next_action` that is either a single command to run or a single tool-owned edit to apply.
- `apply` is transactional: failures do not partially update state/output artifacts.
- `report.json` is evidence-linked (scenario IDs, run IDs, artifact paths) and records blockers as stable tags/codes.
- All structured config/state/report/lock/plan artifacts are JSON (JSONL permitted for history).

Out of scope:
- Fully interactive wizard/REPL UI.
- Automatic scenario synthesis (LM-driven).
- Full Terraform-style drift detection / predictive diffs over dynamic runs.
- Perfect rollback of append-only run artifacts (rollback operates on committed pointers/txns).

## M8 — Broad Dynamic Validation + Coverage Ledger (ls) (deferred; folded into M9)

Goal: Expand dynamic scenario execution so the generated `ls(1)` man page can be
backed by **real, sandboxed binary behavior** for as much of the option surface
as is practical, while explicitly tracking what remains blocked.

Motivation:
- `ls` has many options whose behavior is only meaningful with deterministic filesystem fixtures.
- Many outputs are inherently volatile (timestamps, uid/gid names, PTY-dependent behavior).

Deliverables:
- Maintain an `ls` doc pack that includes:
  - a scenario catalog with explicit coverage metadata (`coverage_tier`, `covers_options`, `coverage_ignore`)
  - deterministic fixture-backed behavior scenarios (`seed_dir` + `cwd`)
  - a coverage ledger that classifies option IDs as accepted/rejected/unknown and tracks behavior coverage separately
  - explicit “blockers” for behavior scenarios expressed as capability tags (timestamps, uid/gid mapping, PTY, etc.)
- Codify “listed-but-rejected” options (surface-area inventory vs runtime acceptance).
- Keep `.SH EXAMPLES` curation independent of raw coverage expansion (publish only high-value scenarios).

Acceptance criteria:
- For the current extracted option inventory, every option ID is classified as accepted/rejected/unknown, and unknowns are explained.
- Behavior coverage is non-trivial (fixture-backed examples exist) and remaining gaps are explicitly blocked with capability tags.

Out of scope:
- Making every option a published man page example.
- Capability unlock work beyond recording blockers (e.g., timestamps/ownership control, PTY capture).

## M7 — Portable Doc Packs (done)

Goal: Make per-binary documentation artifacts **portable and self-contained** so
scenario catalogs, fixtures, and usage lens templates live with the binary’s
documentation pack (not in the `binary_man` repo).

Deliverables:
- Define a doc-pack directory layout (per binary) that co-locates:
  - `binary.lens/` pack
  - scenario catalog(s)
  - fixture trees
- usage lens templates (`queries/*.sql`)
  - generated man page + `examples_report.json` (when publishable examples exist) + `meta.json`
- Make scenario fixture paths resolve relative to the doc pack (or the scenario file), not the process working directory.
- Add a `--lens-flake <ref>` override for pack generation and scenario runs.

Acceptance criteria:
- A doc pack containing `ls` (pack + scenarios + fixtures + pack-local queries) can be moved to an arbitrary directory and rerun successfully.
- No repo-root `scenarios/`, `fixtures/`, or `queries/` directories are required to reproduce scenario runs once the doc pack exists.

Out of scope:
- Packaging/distribution format (e.g., `.zip`) beyond a stable on-disk layout.

## M6 — Scenario-Backed EXAMPLES (done)

Goal: Populate the man page’s `EXAMPLES` section with **outputs from real runs**
to validate that documented invocations behave as described, using the runtime
scenario capture feature in `binary_lens` packs.

Deliverables:
- Scenario catalog (per binary) with explicit expectations:
  - argv, env overrides, timeout, and output excerpt policy
  - expected exit code and minimal stdout/stderr matchers (regex/substring)
- Runner that executes scenarios and appends them to an existing pack’s `runs/`
  overlay via `binary_lens run=1 <pack_root> ...` (no re-export).
- Validation report artifact (JSON) mapping scenario IDs → `runs/<run_id>/` refs
  + pass/fail status + observed exit code.
- Man page renderer emits `.SH EXAMPLES` from passing scenarios marked
  `"publish": true`:
  - show the exact command line as run
  - include a bounded stdout/stderr snippet and note non-zero exit status
- Provenance: extend `meta.json` schema to reference the runs index and the
  examples/validation report.
- Docs: document the workflow for (re)running scenarios and regenerating the man
  page.

Acceptance criteria (`ls` guinea pig):
- Running the examples workflow produces ≥3 captured runs in
  `<doc-pack>/binary.lens/runs/` (e.g., `--help`, `--version`, invalid option),
  and `<doc-pack>/man/ls.1` includes a corresponding `.SH EXAMPLES` section.
- Scenario results are reproducible under a controlled env (e.g., `LC_ALL=C`)
  and output is kept bounded via truncation rules.

Out of scope:
- Automatic scenario synthesis (LM-driven) from static analysis.
- Deep semantic assertions beyond exit status + lightweight output checks.
- Cross-platform sandbox parity; Linux-first is acceptable.

## M5 — Comprehensive `ls(1)` Man Page (done)

Goal: Generate a comprehensive, plausible `ls(1)` man page from a fresh
`binary_lens` pack, using deterministic rendering over lens output. Dynamic
validation is deferred to a later milestone (implemented for `EXAMPLES` in M6).

Deliverables:
- Fresh `binary_lens` pack under `<doc-pack>/binary.lens/`.
- `<doc-pack>/man/ls.1` rendered from the pack + lens output.
- Provenance artifact (`meta.json`).

Out of scope:
- Dynamic execution or sandbox validation.
- Scenario runners or inference loops.

## M4 — Provenance Bundle (done)

Goal: Make outputs auditable.

Deliverables:
- Store prompt, response, help text, and a metadata JSON pointing back to the pack.

Note: Prompt/response artifacts are deprecated in favor of the lens outputs in M5.

## M3 — LM Man Page Pipeline (done)

Goal: Produce a plausible, comprehensive man page from static evidence.

Deliverables:
- Assemble a prompt from pack metadata + extracted help text.
- Invoke the configured LM CLI (Claude default).
- Emit `ls.1` plus prompt/response provenance.

Note: LM synthesis is deprecated in favor of deterministic rendering in M5.

## M2 — Evidence Extraction (done)

Goal: Pull help/usage strings from static pack facts for LM context.

Deliverables:
- Query `facts/strings.parquet` via DuckDB to extract `ls` usage/help text.
- Preserve extracted help text as a first-class artifact.

Note: Raw-string extraction artifacts are deprecated in favor of the
lens-based evidence trail in M5.

## M1 — Pack Ingest (done)

Goal: Treat `binary_lens` packs as the canonical input artifact.

Deliverables:
- Accept a pack root (`binary.lens/`) or generate one via `nix run ../binary_lens#binary_lens`.
- Read pack manifest for binary identity and tool provenance.

## M0 — Static Reset (done)

Goal: Strip the project back to static analysis + LM-assisted documentation.

Deliverables:
- Remove sandboxed runner/scenario machinery.
- Keep only pack ingestion, evidence extraction, and man page generation.
