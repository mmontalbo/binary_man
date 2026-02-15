-- Section 00: Inputs Normalization
-- Loads and normalizes all inputs: surface inventory, scenario plan, and evidence.
-- Key outputs: surface, plan_scenarios, normalized_evidence, combined_scenarios.
with
  surface as (
    select
      item.id as surface_id,
      lower(coalesce(item.invocation.value_arity, 'unknown')) as value_arity,
      coalesce(item.invocation.value_examples, []::VARCHAR[]) as value_examples,
      coalesce(item.context_argv, []::VARCHAR[]) as context_argv
    from read_json(
      'inventory/surface.json',
      columns={
        'items': 'STRUCT(id VARCHAR, context_argv VARCHAR[], invocation STRUCT(value_arity VARCHAR, value_examples VARCHAR[]))[]'
      }
    ) as inv,
      unnest(inv.items) as t(item)
    -- Include non-entry-point items (items whose id is NOT the last element of context_argv)
    where coalesce(item.id, '') <> ''
      and (
        len(coalesce(item.context_argv, []::VARCHAR[])) = 0
        or item.context_argv[-1] <> item.id
      )
  ),
  plan as (
    select * from read_json(
      'scenarios/plan.json',
      columns={
        'defaults': 'STRUCT(seed_dir VARCHAR, seed STRUCT(entries STRUCT(path VARCHAR, kind VARCHAR, contents VARCHAR, target VARCHAR, mode BIGINT)[]))',
        'scenarios': 'STRUCT(id VARCHAR, coverage_ignore BOOLEAN, covers VARCHAR[], argv VARCHAR[], coverage_tier VARCHAR, baseline_scenario_id VARCHAR, assertions STRUCT(kind VARCHAR, seed_path VARCHAR, token VARCHAR, run VARCHAR, exact_line BOOLEAN, path VARCHAR, pattern VARCHAR)[], seed_dir VARCHAR, seed STRUCT(entries STRUCT(path VARCHAR, kind VARCHAR, contents VARCHAR, target VARCHAR, mode BIGINT)[]), expect STRUCT(exit_code BIGINT, exit_signal BIGINT, stdout_contains_all VARCHAR[], stdout_contains_any VARCHAR[], stdout_regex_all VARCHAR[], stdout_regex_any VARCHAR[], stderr_contains_all VARCHAR[], stderr_contains_any VARCHAR[], stderr_regex_all VARCHAR[], stderr_regex_any VARCHAR[]) )[]'
      }
    )
  ),
  plan_scenarios_raw as (
    select
      s.id as scenario_id,
      s.coverage_ignore as coverage_ignore,
      s.covers as covers,
      s.argv as argv,
      coalesce(
        nullif(trim(both '\"' from cast(s.coverage_tier as varchar)), ''),
        'acceptance'
      ) as coverage_tier,
      s.baseline_scenario_id as baseline_scenario_id,
      s.assertions as assertions,
      s.seed_dir as seed_dir,
      plan.defaults.seed_dir as defaults_seed_dir,
      s.seed as seed,
      plan.defaults.seed as defaults_seed,
      case
        when s.expect is null then false
        when coalesce(array_length(s.expect.stdout_contains_all), 0) > 0 then true
        when coalesce(array_length(s.expect.stdout_contains_any), 0) > 0 then true
        when coalesce(array_length(s.expect.stdout_regex_all), 0) > 0 then true
        when coalesce(array_length(s.expect.stdout_regex_any), 0) > 0 then true
        when coalesce(array_length(s.expect.stderr_contains_all), 0) > 0 then true
        when coalesce(array_length(s.expect.stderr_contains_any), 0) > 0 then true
        when coalesce(array_length(s.expect.stderr_regex_all), 0) > 0 then true
        when coalesce(array_length(s.expect.stderr_regex_any), 0) > 0 then true
        else false
      end as expect_has_output_predicate
    from plan,
      unnest(plan.scenarios) as t(s)
  ),
  plan_scenarios as (
    select
      scenario_id,
      coverage_ignore,
      covers,
      argv,
      coverage_tier,
      baseline_scenario_id,
      assertions,
      case
        when seed is not null then seed
        when seed_dir is not null then cast(null as STRUCT(entries STRUCT(path VARCHAR, kind VARCHAR, contents VARCHAR, target VARCHAR, mode BIGINT)[]))
        else defaults_seed
      end as seed,
      case
        when seed is not null then null
        when seed_dir is not null then seed_dir
        when defaults_seed is not null then null
        else defaults_seed_dir
      end as seed_dir,
      expect_has_output_predicate
    from plan_scenarios_raw
  ),
  scenario_seed_paths as (
    select distinct
      s.scenario_id,
      e.path as seed_path
    from plan_scenarios s,
      unnest(coalesce(s.seed.entries, [])) as t(e)
    where e.path is not null
      and e.path <> ''
  ),
  scenario_seed_signature as (
    select
      s.scenario_id,
      to_json(
        list_sort(
          list(
            to_json(
              struct_pack(
                path := e.path,
                kind := e.kind,
                contents := case
                  when lower(e.kind) = 'file' then coalesce(e.contents, '')
                  else e.contents
                end,
                target := e.target,
                mode := e.mode
              )
            )
          )
        )
      ) as seed_signature
    from plan_scenarios s,
      unnest(coalesce(s.seed.entries, [])) as t(e)
    group by s.scenario_id
  ),
  semantics as (
    select
      verification,
      behavior_assertions
    from read_json(
      'enrich/semantics.json',
      columns={
        'verification': 'STRUCT(accepted STRUCT(exit_code BIGINT, exit_signal BIGINT, stdout_contains_all VARCHAR[], stdout_contains_any VARCHAR[], stdout_regex_all VARCHAR[], stdout_regex_any VARCHAR[], stderr_contains_all VARCHAR[], stderr_contains_any VARCHAR[], stderr_regex_all VARCHAR[], stderr_regex_any VARCHAR[])[], rejected STRUCT(exit_code BIGINT, exit_signal BIGINT, stdout_contains_all VARCHAR[], stdout_contains_any VARCHAR[], stdout_regex_all VARCHAR[], stdout_regex_any VARCHAR[], stderr_contains_all VARCHAR[], stderr_contains_any VARCHAR[], stderr_regex_all VARCHAR[], stderr_regex_any VARCHAR[])[])',
        'behavior_assertions': 'STRUCT(strip_ansi BOOLEAN, trim_whitespace BOOLEAN, collapse_internal_whitespace BOOLEAN, confounded_coverage_gate BOOLEAN)'
      }
    )
  ),
  behavior_semantics as (
    select
      coalesce(semantics.behavior_assertions.strip_ansi, true) as strip_ansi,
      coalesce(semantics.behavior_assertions.trim_whitespace, true) as trim_whitespace,
      coalesce(semantics.behavior_assertions.collapse_internal_whitespace, false) as collapse_internal_whitespace,
      coalesce(semantics.behavior_assertions.confounded_coverage_gate, false) as confounded_coverage_gate
    from semantics
  ),
  verification_rules as (
    select
      'accepted' as rule_kind,
      r.exit_code as exit_code,
      r.exit_signal as exit_signal,
      r.stdout_contains_all as stdout_contains_all,
      r.stdout_contains_any as stdout_contains_any,
      r.stdout_regex_all as stdout_regex_all,
      r.stdout_regex_any as stdout_regex_any,
      r.stderr_contains_all as stderr_contains_all,
      r.stderr_contains_any as stderr_contains_any,
      r.stderr_regex_all as stderr_regex_all,
      r.stderr_regex_any as stderr_regex_any
    from semantics,
      unnest(coalesce(semantics.verification.accepted, [])) as t(r)
    union all
    select
      'rejected' as rule_kind,
      r.exit_code as exit_code,
      r.exit_signal as exit_signal,
      r.stdout_contains_all as stdout_contains_all,
      r.stdout_contains_any as stdout_contains_any,
      r.stdout_regex_all as stdout_regex_all,
      r.stdout_regex_any as stdout_regex_any,
      r.stderr_contains_all as stderr_contains_all,
      r.stderr_contains_any as stderr_contains_any,
      r.stderr_regex_all as stderr_regex_all,
      r.stderr_regex_any as stderr_regex_any
    from semantics,
      unnest(coalesce(semantics.verification.rejected, [])) as t(r)
  ),
  scenario_evidence as (
    select
      scenario_id,
      argv,
      exit_code,
      exit_signal,
      timed_out,
      stdout,
      stderr,
      files_checked,
      generated_at_epoch_ms,
      regexp_extract(replace(filename, '\\', '/'), '(inventory/scenarios/.*)', 1) as scenario_path
    from read_json(
      'inventory/scenarios/*.json',
      columns={
        'scenario_id': 'VARCHAR',
        'argv': 'VARCHAR[]',
        'exit_code': 'BIGINT',
        'exit_signal': 'BIGINT',
        'timed_out': 'BOOLEAN',
        'stdout': 'VARCHAR',
        'stderr': 'VARCHAR',
        'files_checked': 'JSON',
        'generated_at_epoch_ms': 'UBIGINT'
      },
      filename = true
    )
  ),
  latest_evidence as (
    select
      scenario_id,
      argv,
      exit_code,
      exit_signal,
      timed_out,
      stdout,
      stderr,
      files_checked,
      generated_at_epoch_ms,
      scenario_path
    from (
      select
        *,
        row_number() over (
          partition by scenario_id
          order by generated_at_epoch_ms desc
        ) as rk
      from scenario_evidence
      where scenario_id is not null
    )
    where rk = 1
  ),
  scenario_index as (
    select
      s.scenario_id as scenario_id,
      s.last_pass as last_pass,
      s.evidence_paths as evidence_paths
    from read_json(
      'inventory/scenarios/index.json',
      columns={
        'scenarios': 'STRUCT(scenario_id VARCHAR, last_pass BOOLEAN, evidence_paths VARCHAR[])[]'
      }
    ) as idx,
      unnest(coalesce(idx.scenarios, [])) as t(s)
  ),
  normalized_evidence_base as (
    select
      e.scenario_id,
      e.argv,
      e.exit_code,
      e.exit_signal,
      e.timed_out,
      e.stdout,
      e.stderr,
      e.files_checked,
      e.generated_at_epoch_ms,
      e.scenario_path,
      idx.last_pass as last_pass,
      idx.evidence_paths as evidence_paths,
      b.strip_ansi as strip_ansi,
      b.trim_whitespace as trim_whitespace,
      b.collapse_internal_whitespace as collapse_internal_whitespace,
      coalesce(e.stdout, '') as stdout_raw
    from latest_evidence e
    left join scenario_index idx
      on idx.scenario_id = e.scenario_id
    cross join behavior_semantics b
  ),
  normalized_evidence_strip as (
    select
      *,
      case
        when strip_ansi then regexp_replace(
          stdout_raw,
          '\\x1b\\[[0-9;]*[A-Za-z]',
          '',
          'g'
        )
        else stdout_raw
      end as stdout_no_ansi
    from normalized_evidence_base
  ),
  normalized_evidence_trim as (
    select
      *,
      case
        when trim_whitespace then trim(stdout_no_ansi)
        else stdout_no_ansi
      end as stdout_trimmed
    from normalized_evidence_strip
  ),
  normalized_evidence as (
    select
      scenario_id,
      argv,
      exit_code,
      exit_signal,
      timed_out,
      stdout,
      stderr,
      files_checked,
      generated_at_epoch_ms,
      scenario_path,
      last_pass,
      evidence_paths,
      case
        when collapse_internal_whitespace then regexp_replace(
          stdout_trimmed,
          '\\s+',
          ' ',
          'g'
        )
        else stdout_trimmed
      end as normalized_stdout
    from normalized_evidence_trim
  ),
  auto_scenarios_raw as (
    select
      e.scenario_id,
      e.argv,
      -- New format: auto_verify::surface_id (no kind prefix)
      regexp_extract(e.scenario_id, '^auto_verify::(.+)$', 1) as surface_id,
      coalesce(e.timed_out, false) as timed_out
    from latest_evidence e
    where e.scenario_id like 'auto_verify::%'
  ),
  -- Track surfaces where auto_verify timed out (likely interactive/hanging)
  auto_verify_timeout as (
    select distinct surface_id
    from auto_scenarios_raw
    where timed_out
      and surface_id is not null
      and surface_id <> ''
  ),
  -- Extract auto_verify evidence (exit_code, stderr) for each surface
  auto_verify_evidence as (
    select
      a.surface_id,
      e.exit_code as auto_verify_exit_code,
      -- Truncate stderr to first 200 chars for preview
      case
        when length(coalesce(e.stderr, '')) > 200 then substr(e.stderr, 1, 200) || '...'
        else e.stderr
      end as auto_verify_stderr
    from auto_scenarios_raw a
    join latest_evidence e on e.scenario_id = a.scenario_id
    where a.surface_id is not null
      and a.surface_id <> ''
  ),
  auto_scenarios as (
    select
      scenario_id,
      false as coverage_ignore,
      list_value(surface_id) as covers,
      argv,
      'acceptance' as coverage_tier,
      null as baseline_scenario_id,
      null as assertions,
      cast(null as VARCHAR) as seed_dir,
      cast(null as STRUCT(entries STRUCT(path VARCHAR, kind VARCHAR, contents VARCHAR, target VARCHAR, mode BIGINT)[])) as seed,
      false as expect_has_output_predicate
    from auto_scenarios_raw
    where surface_id is not null
      and surface_id <> ''
  ),
  combined_scenarios as (
    select * from plan_scenarios
    union all
    select * from auto_scenarios
  ),
