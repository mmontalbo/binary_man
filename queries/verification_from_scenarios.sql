-- Derive per-surface verification status from scenario evidence + plan claims.
with
  surface as (
    select
      item.id as surface_id
    from read_json_auto('inventory/surface.json') as inv,
      unnest(inv.items) as t(item)
    where item.kind in ('option', 'command', 'subcommand')
  ),
  plan as (
    select * from read_json(
      'scenarios/plan.json',
      columns={
        'defaults': 'STRUCT(seed STRUCT(entries STRUCT(path VARCHAR, kind VARCHAR, contents VARCHAR, target VARCHAR, mode BIGINT)[]))',
        'scenarios': 'STRUCT(id VARCHAR, coverage_ignore BOOLEAN, covers VARCHAR[], argv VARCHAR[], coverage_tier VARCHAR, baseline_scenario_id VARCHAR, assertions STRUCT(kind VARCHAR, path VARCHAR)[], seed STRUCT(entries STRUCT(path VARCHAR, kind VARCHAR, contents VARCHAR, target VARCHAR, mode BIGINT)[]), expect STRUCT(exit_code BIGINT, exit_signal BIGINT, stdout_contains_all VARCHAR[], stdout_contains_any VARCHAR[], stdout_regex_all VARCHAR[], stdout_regex_any VARCHAR[], stderr_contains_all VARCHAR[], stderr_contains_any VARCHAR[], stderr_regex_all VARCHAR[], stderr_regex_any VARCHAR[]) )[]'
      }
    )
  ),
  plan_scenarios as (
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
      coalesce(s.seed, plan.defaults.seed) as seed,
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
        'behavior_assertions': 'STRUCT(strip_ansi BOOLEAN, trim_whitespace BOOLEAN, collapse_internal_whitespace BOOLEAN)'
      }
    )
  ),
  behavior_semantics as (
    select
      coalesce(semantics.behavior_assertions.strip_ansi, true) as strip_ansi,
      coalesce(semantics.behavior_assertions.trim_whitespace, true) as trim_whitespace,
      coalesce(semantics.behavior_assertions.collapse_internal_whitespace, false) as collapse_internal_whitespace
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
      generated_at_epoch_ms,
      regexp_extract(replace(filename, '\\', '/'), '(inventory/scenarios/.*)', 1) as scenario_path
    from read_json_auto('inventory/scenarios/*.json', filename = true)
  ),
  latest_evidence as (
    select *
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
      scenario_id,
      argv,
      regexp_extract(scenario_id, '^auto_verify::(option|subcommand)::(.*)$', 1) as surface_kind,
      regexp_extract(scenario_id, '^auto_verify::(option|subcommand)::(.*)$', 2) as surface_id
    from latest_evidence
    where scenario_id like 'auto_verify::%'
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
      cast(null as STRUCT(entries STRUCT(path VARCHAR, kind VARCHAR, contents VARCHAR, target VARCHAR, mode BIGINT)[])) as seed,
      false as expect_has_output_predicate
    from auto_scenarios_raw
    where surface_kind is not null
      and surface_id is not null
      and surface_id <> ''
  ),
  combined_scenarios as (
    select * from plan_scenarios
    union all
    select * from auto_scenarios
  ),
  behavior_context as (
    select
      s.scenario_id,
      s.baseline_scenario_id,
      coalesce(var.last_pass, false) as variant_last_pass,
      case
        when s.baseline_scenario_id is null then false
        else coalesce(base.last_pass, false)
      end as baseline_last_pass,
      case
        when s.baseline_scenario_id is null then false
        when coalesce(var_seed.seed_signature, '[]') = coalesce(base_seed.seed_signature, '[]')
          then true
        else false
      end as seed_signature_match,
      var.normalized_stdout as variant_stdout,
      base.normalized_stdout as baseline_stdout,
      case
        when var.normalized_stdout is null then false
        when base.normalized_stdout is null then false
        when var.normalized_stdout = base.normalized_stdout then true
        else false
      end as outputs_equal
    from combined_scenarios s
    left join normalized_evidence var
      on var.scenario_id = s.scenario_id
    left join normalized_evidence base
      on base.scenario_id = s.baseline_scenario_id
    left join scenario_seed_signature var_seed
      on var_seed.scenario_id = s.scenario_id
    left join scenario_seed_signature base_seed
      on base_seed.scenario_id = s.baseline_scenario_id
  ),
  behavior_assertions_raw as (
    select
      s.scenario_id,
      s.baseline_scenario_id,
      a.kind as assertion_kind,
      a.path as assertion_path
    from combined_scenarios s,
      unnest(coalesce(s.assertions, [])) as t(a)
    where s.coverage_tier = 'behavior'
  ),
  behavior_assertion_detail as (
    select
      b.scenario_id,
      b.baseline_scenario_id,
      b.assertion_kind,
      b.assertion_path,
      ctx.variant_last_pass,
      ctx.baseline_last_pass,
      ctx.seed_signature_match,
      ctx.variant_stdout,
      ctx.baseline_stdout,
      ctx.outputs_equal,
      case
        when b.assertion_kind in (
          'baseline_stdout_not_contains_seed_path',
          'baseline_stdout_contains_seed_path',
          'variant_stdout_contains_seed_path',
          'variant_stdout_not_contains_seed_path'
        )
          and b.assertion_path is not null
          and b.assertion_path <> ''
          and coalesce(ctx.seed_signature_match, false) = true
          and exists (
            select 1
            from scenario_seed_paths sp
            where sp.scenario_id = b.scenario_id
              and sp.seed_path = b.assertion_path
          )
          and exists (
            select 1
            from scenario_seed_paths sp
            where sp.scenario_id = b.baseline_scenario_id
              and sp.seed_path = b.assertion_path
          )
          then 1
        else 0
      end as seeded_assertion,
      case
        when b.assertion_kind = 'baseline_stdout_not_contains_seed_path' then
          case
            when coalesce(ctx.variant_last_pass, false) = false then 0
            when coalesce(ctx.baseline_last_pass, false) = false then 0
            when b.assertion_path is null or b.assertion_path = '' then 0
            when coalesce(ctx.seed_signature_match, false) = false then 0
            when not exists (
              select 1
              from scenario_seed_paths sp
              where sp.scenario_id = b.scenario_id
                and sp.seed_path = b.assertion_path
            ) then 0
            when not exists (
              select 1
              from scenario_seed_paths sp
              where sp.scenario_id = b.baseline_scenario_id
                and sp.seed_path = b.assertion_path
            ) then 0
            when ctx.baseline_stdout is null then 0
            when position(b.assertion_path in ctx.baseline_stdout) = 0 then 1
            else 0
          end
        when b.assertion_kind = 'baseline_stdout_contains_seed_path' then
          case
            when coalesce(ctx.variant_last_pass, false) = false then 0
            when coalesce(ctx.baseline_last_pass, false) = false then 0
            when b.assertion_path is null or b.assertion_path = '' then 0
            when coalesce(ctx.seed_signature_match, false) = false then 0
            when not exists (
              select 1
              from scenario_seed_paths sp
              where sp.scenario_id = b.scenario_id
                and sp.seed_path = b.assertion_path
            ) then 0
            when not exists (
              select 1
              from scenario_seed_paths sp
              where sp.scenario_id = b.baseline_scenario_id
                and sp.seed_path = b.assertion_path
            ) then 0
            when ctx.baseline_stdout is null then 0
            when position(b.assertion_path in ctx.baseline_stdout) > 0 then 1
            else 0
          end
        when b.assertion_kind = 'variant_stdout_contains_seed_path' then
          case
            when coalesce(ctx.variant_last_pass, false) = false then 0
            when coalesce(ctx.baseline_last_pass, false) = false then 0
            when b.assertion_path is null or b.assertion_path = '' then 0
            when coalesce(ctx.seed_signature_match, false) = false then 0
            when not exists (
              select 1
              from scenario_seed_paths sp
              where sp.scenario_id = b.scenario_id
                and sp.seed_path = b.assertion_path
            ) then 0
            when not exists (
              select 1
              from scenario_seed_paths sp
              where sp.scenario_id = b.baseline_scenario_id
                and sp.seed_path = b.assertion_path
            ) then 0
            when ctx.variant_stdout is null then 0
            when position(b.assertion_path in ctx.variant_stdout) > 0 then 1
            else 0
          end
        when b.assertion_kind = 'variant_stdout_not_contains_seed_path' then
          case
            when coalesce(ctx.variant_last_pass, false) = false then 0
            when coalesce(ctx.baseline_last_pass, false) = false then 0
            when b.assertion_path is null or b.assertion_path = '' then 0
            when coalesce(ctx.seed_signature_match, false) = false then 0
            when not exists (
              select 1
              from scenario_seed_paths sp
              where sp.scenario_id = b.scenario_id
                and sp.seed_path = b.assertion_path
            ) then 0
            when not exists (
              select 1
              from scenario_seed_paths sp
              where sp.scenario_id = b.baseline_scenario_id
                and sp.seed_path = b.assertion_path
            ) then 0
            when ctx.variant_stdout is null then 0
            when position(b.assertion_path in ctx.variant_stdout) = 0 then 1
            else 0
          end
        when b.assertion_kind = 'variant_stdout_differs_from_baseline' then
          case
            when coalesce(ctx.variant_last_pass, false) = false then 0
            when coalesce(ctx.baseline_last_pass, false) = false then 0
            when coalesce(ctx.seed_signature_match, false) = false then 0
            when ctx.variant_stdout is null then 0
            when ctx.baseline_stdout is null then 0
            when ctx.variant_stdout <> ctx.baseline_stdout then 1
            else 0
          end
        else 0
      end as assertion_pass
    from behavior_assertions_raw b
    left join behavior_context ctx
      on ctx.scenario_id = b.scenario_id
  ),
  behavior_delta_pair as (
    select
      scenario_id,
      assertion_path,
      max(
        case
          when assertion_kind = 'baseline_stdout_not_contains_seed_path' then 1
          else 0
        end
      ) as has_baseline_not,
      max(
        case
          when assertion_kind = 'baseline_stdout_not_contains_seed_path'
            and assertion_pass = 1 then 1
          else 0
        end
      ) as baseline_not_pass,
      max(
        case
          when assertion_kind = 'baseline_stdout_contains_seed_path' then 1
          else 0
        end
      ) as has_baseline_contains,
      max(
        case
          when assertion_kind = 'baseline_stdout_contains_seed_path'
            and assertion_pass = 1 then 1
          else 0
        end
      ) as baseline_contains_pass,
      max(
        case
          when assertion_kind = 'variant_stdout_contains_seed_path' then 1
          else 0
        end
      ) as has_variant_contains,
      max(
        case
          when assertion_kind = 'variant_stdout_contains_seed_path'
            and assertion_pass = 1 then 1
          else 0
        end
      ) as variant_contains_pass,
      max(
        case
          when assertion_kind = 'variant_stdout_not_contains_seed_path' then 1
          else 0
        end
      ) as has_variant_not,
      max(
        case
          when assertion_kind = 'variant_stdout_not_contains_seed_path'
            and assertion_pass = 1 then 1
          else 0
        end
      ) as variant_not_pass
    from behavior_assertion_detail
    where assertion_kind in (
      'baseline_stdout_not_contains_seed_path',
      'baseline_stdout_contains_seed_path',
      'variant_stdout_contains_seed_path',
      'variant_stdout_not_contains_seed_path'
    )
    group by scenario_id, assertion_path
  ),
  behavior_delta_pair_summary as (
    select
      scenario_id,
      max(
        case
          when (has_baseline_not = 1 and has_variant_contains = 1)
            or (has_baseline_contains = 1 and has_variant_not = 1) then 1
          else 0
        end
      ) as delta_pair_present,
      max(
        case
          when (baseline_not_pass = 1 and variant_contains_pass = 1)
            or (baseline_contains_pass = 1 and variant_not_pass = 1) then 1
          else 0
        end
      ) as delta_pair_pass
    from behavior_delta_pair
    group by scenario_id
  ),
  behavior_assertion_eval as (
    select
      scenario_id,
      count(*) as assertion_count,
      sum(case when seeded_assertion = 1 then 1 else 0 end) as seeded_assertion_count,
      min(case when assertion_pass = 1 then 1 else 0 end) as all_pass_int,
      max(
        case
          when assertion_kind = 'variant_stdout_differs_from_baseline' then 1
          else 0
        end
      ) as diff_assertion_present,
      max(
        case
          when assertion_kind = 'variant_stdout_differs_from_baseline'
            and assertion_pass = 1 then 1
          else 0
        end
      ) as diff_assertion_pass
    from behavior_assertion_detail
    group by scenario_id
  ),
  behavior_eval as (
    select
      s.scenario_id,
      coalesce(a.assertion_count, 0) as assertion_count,
      coalesce(a.seeded_assertion_count, 0) as seeded_assertion_count,
      case
        when coalesce(a.assertion_count, 0) = 0 then false
        when coalesce(a.all_pass_int, 0) = 1 then true
        else false
      end as assertions_pass,
      case
        when coalesce(a.diff_assertion_present, 0) = 1 then true
        else false
      end as diff_assertion_present,
      case
        when coalesce(a.diff_assertion_pass, 0) = 1 then true
        else false
      end as diff_assertion_pass,
      case
        when coalesce(dp.delta_pair_present, 0) = 1 then true
        else false
      end as delta_pair_present,
      case
        when coalesce(dp.delta_pair_pass, 0) = 1 then true
        else false
      end as delta_pair_pass,
      case
        when coalesce(a.diff_assertion_pass, 0) = 1
          or coalesce(dp.delta_pair_pass, 0) = 1 then true
        else false
      end as delta_proof_pass,
      case
        when coalesce(a.diff_assertion_present, 0) = 1
          or coalesce(dp.delta_pair_present, 0) = 1 then true
        else false
      end as delta_assertion_present,
      coalesce(s.expect_has_output_predicate, false) as expect_has_output_predicate,
      case
        when coalesce(s.expect_has_output_predicate, false) = true
          or coalesce(dp.delta_pair_present, 0) = 1 then true
        else false
      end as semantic_predicate_present,
      case
        when coalesce(s.expect_has_output_predicate, false) = true
          or coalesce(dp.delta_pair_pass, 0) = 1 then true
        else false
      end as semantic_predicate_pass,
      coalesce(ctx.seed_signature_match, false) as seed_signature_match,
      coalesce(ctx.variant_last_pass, false) as variant_last_pass,
      coalesce(ctx.baseline_last_pass, false) as baseline_last_pass,
      coalesce(ctx.outputs_equal, false) as outputs_equal
    from combined_scenarios s
    left join behavior_assertion_eval a
      on a.scenario_id = s.scenario_id
    left join behavior_delta_pair_summary dp
      on dp.scenario_id = s.scenario_id
    left join behavior_context ctx
      on ctx.scenario_id = s.scenario_id
  ),
  rule_eval as (
    select
      e.scenario_id,
      e.scenario_path,
      r.rule_kind,
      case
        when e.scenario_id is null then false
        when coalesce(e.timed_out, false) then false
        when r.exit_code is not null
          and e.exit_code is distinct from r.exit_code then false
        when r.exit_signal is not null
          and e.exit_signal is distinct from r.exit_signal then false
        when not (
          r.stdout_contains_all is null
          or array_length(r.stdout_contains_all) = 0
          or not exists (
            select 1
            from unnest(r.stdout_contains_all) as t(needle)
            where position(needle in coalesce(e.stdout, '')) = 0
          )
        ) then false
        when not (
          r.stdout_contains_any is null
          or array_length(r.stdout_contains_any) = 0
          or exists (
            select 1
            from unnest(r.stdout_contains_any) as t(needle)
            where position(needle in coalesce(e.stdout, '')) > 0
          )
        ) then false
        when not (
          r.stdout_regex_all is null
          or array_length(r.stdout_regex_all) = 0
          or not exists (
            select 1
            from unnest(r.stdout_regex_all) as t(pattern)
            where not regexp_matches(coalesce(e.stdout, ''), pattern)
          )
        ) then false
        when not (
          r.stdout_regex_any is null
          or array_length(r.stdout_regex_any) = 0
          or exists (
            select 1
            from unnest(r.stdout_regex_any) as t(pattern)
            where regexp_matches(coalesce(e.stdout, ''), pattern)
          )
        ) then false
        when not (
          r.stderr_contains_all is null
          or array_length(r.stderr_contains_all) = 0
          or not exists (
            select 1
            from unnest(r.stderr_contains_all) as t(needle)
            where position(needle in coalesce(e.stderr, '')) = 0
          )
        ) then false
        when not (
          r.stderr_contains_any is null
          or array_length(r.stderr_contains_any) = 0
          or exists (
            select 1
            from unnest(r.stderr_contains_any) as t(needle)
            where position(needle in coalesce(e.stderr, '')) > 0
          )
        ) then false
        when not (
          r.stderr_regex_all is null
          or array_length(r.stderr_regex_all) = 0
          or not exists (
            select 1
            from unnest(r.stderr_regex_all) as t(pattern)
            where not regexp_matches(coalesce(e.stderr, ''), pattern)
          )
        ) then false
        when not (
          r.stderr_regex_any is null
          or array_length(r.stderr_regex_any) = 0
          or exists (
            select 1
            from unnest(r.stderr_regex_any) as t(pattern)
            where regexp_matches(coalesce(e.stderr, ''), pattern)
          )
        ) then false
        else true
      end as matches
    from latest_evidence e
    join verification_rules r on true
  ),
  scenario_eval as (
    select
      p.scenario_id,
      p.coverage_ignore,
      p.covers,
      p.argv,
      p.coverage_tier,
      coalesce(e.evidence_paths, list_value(e.scenario_path)) as scenario_paths,
      coalesce(b.assertion_count, 0) as assertion_count,
      coalesce(b.seeded_assertion_count, 0) as seeded_assertion_count,
      coalesce(b.assertions_pass, false) as assertions_pass,
      coalesce(b.delta_assertion_present, false) as delta_assertion_present,
      coalesce(b.delta_proof_pass, false) as delta_proof_pass,
      coalesce(b.expect_has_output_predicate, false) as expect_has_output_predicate,
      coalesce(b.semantic_predicate_present, false) as semantic_predicate_present,
      coalesce(b.semantic_predicate_pass, false) as semantic_predicate_pass,
      coalesce(b.outputs_equal, false) as outputs_equal,
      coalesce(b.seed_signature_match, false) as seed_signature_match,
      coalesce(b.variant_last_pass, false) as variant_last_pass,
      coalesce(b.baseline_last_pass, false) as baseline_last_pass,
      case when e.scenario_id is null then false else true end as has_evidence,
      e.last_pass as last_pass,
      case
        when e.scenario_id is null then 'unknown'
        when coalesce(e.timed_out, false) then 'inconclusive'
        when exists (
          select 1
          from rule_eval r
          where r.scenario_id = p.scenario_id
            and r.rule_kind = 'rejected'
            and r.matches = true
        ) then 'rejected'
        when exists (
          select 1
          from rule_eval r
          where r.scenario_id = p.scenario_id
            and r.rule_kind = 'accepted'
            and r.matches = true
        ) then 'accepted'
        else 'inconclusive'
      end as acceptance_outcome
    from combined_scenarios p
    left join normalized_evidence e
      on e.scenario_id = p.scenario_id
    left join behavior_eval b
      on b.scenario_id = p.scenario_id
  ),
  covers_raw as (
    select
      scenario_id,
      scenario_paths,
      has_evidence,
      acceptance_outcome,
      coverage_tier,
      assertion_count,
      seeded_assertion_count,
      assertions_pass,
      delta_assertion_present,
      delta_proof_pass,
      expect_has_output_predicate,
      semantic_predicate_present,
      semantic_predicate_pass,
      outputs_equal,
      seed_signature_match,
      variant_last_pass,
      baseline_last_pass,
      last_pass,
      argv,
      trim(cover) as cover_raw
    from scenario_eval,
      unnest(coalesce(covers, [])) as t(cover)
    where coalesce(coverage_ignore, false) = false
  ),
  covers_norm as (
    select
      scenario_id,
      scenario_paths,
      has_evidence,
      acceptance_outcome,
      coverage_tier,
      assertion_count,
      seeded_assertion_count,
      assertions_pass,
      delta_assertion_present,
      delta_proof_pass,
      expect_has_output_predicate,
      semantic_predicate_present,
      semantic_predicate_pass,
      outputs_equal,
      seed_signature_match,
      variant_last_pass,
      baseline_last_pass,
      last_pass,
      cover_raw as surface_id
    from covers_raw
    where cover_raw is not null
      and cover_raw <> ''
      and exists (
        select 1
        from unnest(coalesce(argv, [])) as t(token)
        where case
          when starts_with(cover_raw, '--') then token = cover_raw
            or starts_with(token, cover_raw || '=')
          when starts_with(cover_raw, '-') then token = cover_raw
            or starts_with(token, cover_raw)
          else token = cover_raw
        end
      )
  ),
  accepted_status as (
    select
      s.surface_id,
      case
        when not exists (
          select 1
          from covers_norm c
          where c.surface_id = s.surface_id
            and c.coverage_tier <> 'rejection'
        ) then 'unknown'
        when exists (
          select 1
          from covers_norm c
          where c.surface_id = s.surface_id
            and c.coverage_tier <> 'rejection'
            and c.acceptance_outcome = 'accepted'
        ) then 'verified'
        when exists (
          select 1
          from covers_norm c
          where c.surface_id = s.surface_id
            and c.coverage_tier <> 'rejection'
            and c.acceptance_outcome = 'rejected'
        ) then 'rejected'
        when exists (
          select 1
          from covers_norm c
          where c.surface_id = s.surface_id
            and c.coverage_tier <> 'rejection'
            and c.has_evidence = true
        ) then 'inconclusive'
        else 'recognized'
      end as status
    from surface s
  ),
  behavior_status as (
    select
      s.surface_id,
      case
        when not exists (
          select 1
          from covers_norm c
          where c.surface_id = s.surface_id
            and c.coverage_tier = 'behavior'
        ) then 'unknown'
        when exists (
          select 1
          from covers_norm c
          where c.surface_id = s.surface_id
            and c.coverage_tier = 'behavior'
            and c.variant_last_pass = true
            and c.baseline_last_pass = true
            and c.seed_signature_match = true
            and c.seeded_assertion_count > 0
            and c.assertions_pass = true
            and c.delta_proof_pass = true
            and c.semantic_predicate_pass = true
        ) then 'verified'
        when exists (
          select 1
          from covers_norm c
          where c.surface_id = s.surface_id
            and c.coverage_tier = 'behavior'
            and c.has_evidence = true
        ) then 'rejected'
        else 'recognized'
      end as status
    from surface s
  ),
  behavior_reason as (
    select
      s.surface_id,
      case
        when not exists (
          select 1
          from covers_norm c
          where c.surface_id = s.surface_id
            and c.coverage_tier = 'behavior'
        ) then 'missing_behavior_scenario'
        when not exists (
          select 1
          from covers_norm c
          where c.surface_id = s.surface_id
            and c.coverage_tier = 'behavior'
            and c.has_evidence = true
        ) then 'scenario_failed'
        when exists (
          select 1
          from covers_norm c
          where c.surface_id = s.surface_id
            and c.coverage_tier = 'behavior'
            and c.has_evidence = true
            and (
              c.variant_last_pass = false
              or c.baseline_last_pass = false
            )
        ) then 'scenario_failed'
        when not exists (
          select 1
          from covers_norm c
          where c.surface_id = s.surface_id
            and c.coverage_tier = 'behavior'
            and c.assertion_count > 0
        ) then 'missing_assertions'
        when not exists (
          select 1
          from covers_norm c
          where c.surface_id = s.surface_id
            and c.coverage_tier = 'behavior'
            and c.seeded_assertion_count > 0
        ) then 'seed_mismatch'
        when not exists (
          select 1
          from covers_norm c
          where c.surface_id = s.surface_id
            and c.coverage_tier = 'behavior'
            and c.delta_assertion_present = true
        ) then 'missing_delta_assertion'
        when not exists (
          select 1
          from covers_norm c
          where c.surface_id = s.surface_id
            and c.coverage_tier = 'behavior'
            and c.semantic_predicate_present = true
        ) then 'missing_semantic_predicate'
        when exists (
          select 1
          from covers_norm c
          where c.surface_id = s.surface_id
            and c.coverage_tier = 'behavior'
            and c.delta_assertion_present = true
            and c.outputs_equal = true
        ) then 'outputs_equal'
        when exists (
          select 1
          from covers_norm c
          where c.surface_id = s.surface_id
            and c.coverage_tier = 'behavior'
            and (c.assertions_pass = false or c.delta_proof_pass = false)
        ) then 'assertion_failed'
        else null
      end as behavior_unverified_reason_code
    from surface s
  ),
  accepted_scenario_rollup as (
    select
      surface_id,
      list_sort(list_distinct(list(scenario_id))) as scenario_ids
    from covers_norm
    where coverage_tier <> 'rejection'
    group by surface_id
  ),
  accepted_path_rollup as (
    select
      surface_id,
      list_sort(list_distinct(list(path))) as scenario_paths
    from covers_norm,
      unnest(coalesce(scenario_paths, [])) as t(path)
    where path is not null and path <> ''
      and coverage_tier <> 'rejection'
    group by surface_id
  ),
  behavior_scenario_rollup as (
    select
      surface_id,
      list_sort(list_distinct(list(scenario_id))) as scenario_ids
    from covers_norm
    where coverage_tier = 'behavior'
    group by surface_id
  ),
  behavior_assertion_scenario_rollup as (
    select
      surface_id,
      list_sort(list_distinct(list(scenario_id))) as scenario_ids
    from covers_norm
    where coverage_tier = 'behavior'
      and seeded_assertion_count > 0
    group by surface_id
  ),
  behavior_path_rollup as (
    select
      surface_id,
      list_sort(list_distinct(list(path))) as scenario_paths
    from covers_norm,
      unnest(coalesce(scenario_paths, [])) as t(path)
    where path is not null and path <> ''
      and coverage_tier = 'behavior'
    group by surface_id
  )
select
  surface.surface_id,
  accepted_status.status as status,
  behavior_status.status as behavior_status,
  to_json(coalesce(accepted_scenario_rollup.scenario_ids, []::VARCHAR[])) as scenario_ids,
  to_json(coalesce(accepted_path_rollup.scenario_paths, []::VARCHAR[])) as scenario_paths,
  to_json(coalesce(behavior_scenario_rollup.scenario_ids, []::VARCHAR[])) as behavior_scenario_ids,
  to_json(coalesce(behavior_assertion_scenario_rollup.scenario_ids, []::VARCHAR[])) as behavior_assertion_scenario_ids,
  to_json(coalesce(behavior_path_rollup.scenario_paths, []::VARCHAR[])) as behavior_scenario_paths,
  case
    when behavior_status.status = 'verified' then null
    else behavior_reason.behavior_unverified_reason_code
  end as behavior_unverified_reason_code
from surface
left join accepted_status using (surface_id)
left join behavior_status using (surface_id)
left join behavior_reason using (surface_id)
left join accepted_scenario_rollup using (surface_id)
left join accepted_path_rollup using (surface_id)
left join behavior_scenario_rollup using (surface_id)
left join behavior_assertion_scenario_rollup using (surface_id)
left join behavior_path_rollup using (surface_id)
order by surface.surface_id;
