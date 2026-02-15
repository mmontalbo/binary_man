# Pack Query Templates

SQL query templates installed into doc packs for lens-driven data extraction.
These run via DuckDB and produce JSON inventories from scenario evidence.

## `verification_from_scenarios.sql`

The main verification query, split into composable sections for maintainability
and performance optimization.

### Section Structure

| File | Purpose | Key CTEs |
|------|---------|----------|
| `00_inputs_normalization.sql` | Load and normalize inputs | `surface`, `plan_scenarios`, `scenario_evidence`, `normalized_evidence` |
| `10_behavior_assertion_eval.sql` | Evaluate behavior assertions | `behavior_context`, `behavior_assertions_raw`, `behavior_eval`, `scenario_eval` |
| `20_coverage_reasoning.sql` | Derive verification status per surface | `covers_norm` (MATERIALIZED), pre-aggregation CTEs, status CTEs |
| `30_rollups_output.sql` | Aggregate and produce final output | `covers_scenario_rollup`, `covers_path_rollup`, final SELECT |

### Performance Optimizations (M24)

The verification query includes several optimizations that reduced execution
time from ~4.8s to ~0.4s (12x improvement):

1. **MATERIALIZED covers_norm** (`20_coverage_reasoning.sql:30-33`)
   - `covers_norm` is referenced 8+ times across downstream CTEs
   - Without MATERIALIZED, DuckDB re-evaluates the entire CTE chain per reference
   - Impact: 1.2s → 0.4s (3x speedup)

2. **Pre-aggregation CTEs** (`20_coverage_reasoning.sql:73-108`)
   - Replaced correlated EXISTS subqueries with pre-aggregated JOINs
   - `covers_acceptance_agg` and `covers_behavior_agg` compute per-surface flags once
   - Impact: 4.8s → 1.7s (2.9x speedup)

3. **Consolidated rollups** (`30_rollups_output.sql:1-21`)
   - Merged 5 separate rollup CTEs into 2 using DuckDB's FILTER clause
   - `covers_scenario_rollup` computes 3 aggregations in one scan
   - `covers_path_rollup` computes 2 aggregations with UNNEST in one scan
   - Impact: 1.64s → 1.19s (27% speedup)

### Cache Layer

Results are cached in `inventory/verification_cache.json` keyed by hash of:
- All `inventory/scenarios/*.json` evidence files
- All `queries/verification_from_scenarios/*.sql` template files
- `inventory/surface.json` content

Cache hits return in ~4ms vs ~400ms for query execution.

## Other Queries

- `usage_from_scenarios.sql` - Extract usage/synopsis from help evidence
- `surface_from_scenarios.sql` - Extract surface items (options, subcommands)
- `options_from_scenarios.sql` - Legacy option extraction
- `subcommands_from_scenarios.sql` - Subcommand extraction
