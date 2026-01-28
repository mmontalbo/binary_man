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
        'scenarios': 'STRUCT(id VARCHAR, coverage_ignore BOOLEAN, covers VARCHAR[], argv VARCHAR[], coverage_tier VARCHAR)[]'
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
      ) as coverage_tier
    from plan,
      unnest(plan.scenarios) as t(s)
  ),
  semantics as (
    select
      verification
    from read_json(
      'enrich/semantics.json',
      columns={
        'verification': 'STRUCT(accepted STRUCT(exit_code BIGINT, exit_signal BIGINT, stdout_contains_all VARCHAR[], stdout_contains_any VARCHAR[], stdout_regex_all VARCHAR[], stdout_regex_any VARCHAR[], stderr_contains_all VARCHAR[], stderr_contains_any VARCHAR[], stderr_regex_all VARCHAR[], stderr_regex_any VARCHAR[])[], rejected STRUCT(exit_code BIGINT, exit_signal BIGINT, stdout_contains_all VARCHAR[], stdout_contains_any VARCHAR[], stdout_regex_all VARCHAR[], stdout_regex_any VARCHAR[], stderr_contains_all VARCHAR[], stderr_contains_any VARCHAR[], stderr_regex_all VARCHAR[], stderr_regex_any VARCHAR[])[])'
      }
    )
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
      e.scenario_path,
      case when e.scenario_id is null then false else true end as has_evidence,
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
    from plan_scenarios p
    left join latest_evidence e
      on e.scenario_id = p.scenario_id
  ),
  covers_raw as (
    select
      scenario_id,
      scenario_path,
      has_evidence,
      acceptance_outcome,
      coverage_tier,
      argv,
      trim(cover) as cover_raw
    from scenario_eval,
      unnest(coalesce(covers, [])) as t(cover)
    where coalesce(coverage_ignore, false) = false
  ),
  covers_norm as (
    select
      scenario_id,
      scenario_path,
      has_evidence,
      acceptance_outcome,
      coverage_tier,
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
            and c.acceptance_outcome = 'accepted'
        ) then 'verified'
        when exists (
          select 1
          from covers_norm c
          where c.surface_id = s.surface_id
            and c.coverage_tier = 'behavior'
            and c.acceptance_outcome = 'rejected'
        ) then 'rejected'
        when exists (
          select 1
          from covers_norm c
          where c.surface_id = s.surface_id
            and c.coverage_tier = 'behavior'
            and c.has_evidence = true
        ) then 'inconclusive'
        else 'recognized'
      end as status
    from surface s
  ),
  accepted_scenario_rollup as (
    select
      surface_id,
      to_json(list_sort(list_distinct(list(scenario_id)))) as scenario_ids
    from covers_norm
    where coverage_tier <> 'rejection'
    group by surface_id
  ),
  accepted_path_rollup as (
    select
      surface_id,
      to_json(list_sort(list_distinct(list(scenario_path)))) as scenario_paths
    from covers_norm
    where scenario_path is not null and scenario_path <> ''
      and coverage_tier <> 'rejection'
    group by surface_id
  ),
  behavior_scenario_rollup as (
    select
      surface_id,
      to_json(list_sort(list_distinct(list(scenario_id)))) as scenario_ids
    from covers_norm
    where coverage_tier = 'behavior'
    group by surface_id
  ),
  behavior_path_rollup as (
    select
      surface_id,
      to_json(list_sort(list_distinct(list(scenario_path)))) as scenario_paths
    from covers_norm
    where scenario_path is not null and scenario_path <> ''
      and coverage_tier = 'behavior'
    group by surface_id
  )
select
  surface.surface_id,
  accepted_status.status as status,
  behavior_status.status as behavior_status,
  coalesce(accepted_scenario_rollup.scenario_ids, to_json([])) as scenario_ids,
  coalesce(accepted_path_rollup.scenario_paths, to_json([])) as scenario_paths,
  coalesce(behavior_scenario_rollup.scenario_ids, to_json([])) as behavior_scenario_ids,
  coalesce(behavior_path_rollup.scenario_paths, to_json([])) as behavior_scenario_paths
from surface
left join accepted_status using (surface_id)
left join behavior_status using (surface_id)
left join accepted_scenario_rollup using (surface_id)
left join accepted_path_rollup using (surface_id)
left join behavior_scenario_rollup using (surface_id)
left join behavior_path_rollup using (surface_id)
order by surface.surface_id;
