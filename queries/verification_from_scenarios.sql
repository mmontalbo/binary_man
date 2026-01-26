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
    select * from read_json_auto('scenarios/plan.json')
  ),
  plan_scenarios as (
    select
      s.id as scenario_id,
      s.coverage_ignore as coverage_ignore,
      s.scope as scope,
      s.covers as covers,
      coalesce(
        nullif(trim(both '\"' from cast(s.coverage_tier as varchar)), ''),
        'acceptance'
      ) as coverage_tier,
      s.expect.exit_code as expect_exit_code,
      s.expect.exit_signal as expect_exit_signal,
      s.expect.stdout_contains_all as stdout_contains_all,
      s.expect.stdout_contains_any as stdout_contains_any,
      s.expect.stdout_regex_all as stdout_regex_all,
      s.expect.stdout_regex_any as stdout_regex_any,
      s.expect.stderr_contains_all as stderr_contains_all,
      s.expect.stderr_contains_any as stderr_contains_any,
      s.expect.stderr_regex_all as stderr_regex_all,
      s.expect.stderr_regex_any as stderr_regex_any
    from plan,
      unnest(plan.scenarios) as t(s)
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
  scenario_eval as (
    select
      p.scenario_id,
      p.coverage_ignore,
      p.scope,
      p.covers,
      p.coverage_tier,
      e.scenario_path,
      e.argv,
      case when e.scenario_id is null then false else true end as has_evidence,
      case
        when e.scenario_id is null then false
        when coalesce(e.timed_out, false) then false
        when p.expect_exit_code is not null
          and e.exit_code is distinct from p.expect_exit_code then false
        when p.expect_exit_signal is not null
          and e.exit_signal is distinct from p.expect_exit_signal then false
        when not (
          p.stdout_contains_all is null
          or array_length(p.stdout_contains_all) = 0
          or not exists (
            select 1
            from unnest(p.stdout_contains_all) as t(needle)
            where position(needle in coalesce(e.stdout, '')) = 0
          )
        ) then false
        when not (
          p.stdout_contains_any is null
          or array_length(p.stdout_contains_any) = 0
          or exists (
            select 1
            from unnest(p.stdout_contains_any) as t(needle)
            where position(needle in coalesce(e.stdout, '')) > 0
          )
        ) then false
        when not (
          p.stdout_regex_all is null
          or array_length(p.stdout_regex_all) = 0
          or not exists (
            select 1
            from unnest(p.stdout_regex_all) as t(pattern)
            where not regexp_matches(coalesce(e.stdout, ''), pattern)
          )
        ) then false
        when not (
          p.stdout_regex_any is null
          or array_length(p.stdout_regex_any) = 0
          or exists (
            select 1
            from unnest(p.stdout_regex_any) as t(pattern)
            where regexp_matches(coalesce(e.stdout, ''), pattern)
          )
        ) then false
        when not (
          p.stderr_contains_all is null
          or array_length(p.stderr_contains_all) = 0
          or not exists (
            select 1
            from unnest(p.stderr_contains_all) as t(needle)
            where position(needle in coalesce(e.stderr, '')) = 0
          )
        ) then false
        when not (
          p.stderr_contains_any is null
          or array_length(p.stderr_contains_any) = 0
          or exists (
            select 1
            from unnest(p.stderr_contains_any) as t(needle)
            where position(needle in coalesce(e.stderr, '')) > 0
          )
        ) then false
        when not (
          p.stderr_regex_all is null
          or array_length(p.stderr_regex_all) = 0
          or not exists (
            select 1
            from unnest(p.stderr_regex_all) as t(pattern)
            where not regexp_matches(coalesce(e.stderr, ''), pattern)
          )
        ) then false
        when not (
          p.stderr_regex_any is null
          or array_length(p.stderr_regex_any) = 0
          or exists (
            select 1
            from unnest(p.stderr_regex_any) as t(pattern)
            where regexp_matches(coalesce(e.stderr, ''), pattern)
          )
        ) then false
        else true
      end as pass
    from plan_scenarios p
    left join latest_evidence e
      on e.scenario_id = p.scenario_id
  ),
  covers_raw as (
    select
      scenario_id,
      scenario_path,
      has_evidence,
      pass,
      scope,
      argv,
      coverage_tier,
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
      pass,
      scope,
      argv,
      coverage_tier,
      case
        when position('=' in cover_raw) > 0 then split_part(cover_raw, '=', 1)
        else cover_raw
      end as cover_id
    from covers_raw
    where cover_raw is not null
      and cover_raw <> ''
  ),
  covers_parsed as (
    select
      scenario_id,
      scenario_path,
      has_evidence,
      pass,
      scope,
      argv,
      coverage_tier,
      cover_id,
      case
        when position('.' in cover_id) > 0
          then regexp_extract(cover_id, '^(.*)\\.[^\\.]+$', 1)
        else null
      end as cover_scope,
      case
        when position('.' in cover_id) > 0
          then regexp_extract(cover_id, '([^\\.]+)$', 1)
        else cover_id
      end as cover_token
    from covers_norm
  ),
  covers_invoked as (
    select
      scenario_id,
      scenario_path,
      has_evidence,
      pass,
      scope,
      coverage_tier,
      cover_id,
      cover_scope,
      cover_token,
      case
        when argv is null then false
        when cover_token is null or cover_token = '' then false
        when left(cover_token, 2) = '--' then exists (
          select 1
          from unnest(list_slice(argv, 2, array_length(argv))) as t(token)
          where (
              token = cover_token
              or left(token, length(cover_token) + 1) = cover_token || '='
            )
        )
        when left(cover_token, 1) = '-' then exists (
          select 1
          from unnest(list_slice(argv, 2, array_length(argv))) as t(token)
          where (
              token = cover_token
              or left(token, length(cover_token)) = cover_token
              or (
                left(token, 1) = '-'
                and left(token, 2) <> '--'
                and length(token) > 2
                and position(substring(cover_token, 2) in substring(token, 2)) > 0
              )
            )
        )
        else exists (
          select 1
          from unnest(list_slice(argv, 2, array_length(argv))) as t(token)
          where token = cover_token
        )
      end as invoked,
      case
        when cover_scope is null or cover_scope = '' then true
        when scope is null then false
        else array_to_string(scope, '.') = cover_scope
      end as cover_scope_ok,
      case
        when argv is null then false
        when scope is null or array_length(scope) = 0 then true
        else not exists (
          select 1
          from range(1, array_length(scope) + 1) as t(i)
          where list_extract(argv, i + 1) is distinct from list_extract(scope, i)
        )
      end as argv_scope_ok
    from covers_parsed
  ),
  covers_scoped as (
    select
      scenario_id,
      scenario_path,
      has_evidence,
      pass,
      coverage_tier,
      case
        when scope is not null
          and coalesce(array_length(scope), 0) > 0
          and position('.' in cover_id) = 0
          then array_to_string(scope, '.') || '.' || cover_id
        else cover_id
      end as surface_id
    from covers_invoked
    where invoked = true
      and (
        case
          when cover_scope is not null and cover_scope <> '' then cover_scope_ok and argv_scope_ok
          when left(cover_token, 1) <> '-' and coalesce(array_length(scope), 0) > 0 then argv_scope_ok
          else true
        end
      )
  ),
  accepted_status as (
    select
      s.surface_id,
      case
        when not exists (
          select 1
          from covers_scoped c
          where c.surface_id = s.surface_id
            and c.coverage_tier <> 'rejection'
        ) then 'unknown'
        when exists (
          select 1
          from covers_scoped c
          where c.surface_id = s.surface_id
            and c.coverage_tier <> 'rejection'
            and c.pass = true
        ) then 'verified'
        when exists (
          select 1
          from covers_scoped c
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
          from covers_scoped c
          where c.surface_id = s.surface_id
            and c.coverage_tier = 'behavior'
        ) then 'unknown'
        when exists (
          select 1
          from covers_scoped c
          where c.surface_id = s.surface_id
            and c.coverage_tier = 'behavior'
            and c.pass = true
        ) then 'verified'
        when exists (
          select 1
          from covers_scoped c
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
    from covers_scoped
    where coverage_tier <> 'rejection'
    group by surface_id
  ),
  accepted_path_rollup as (
    select
      surface_id,
      to_json(list_sort(list_distinct(list(scenario_path)))) as scenario_paths
    from covers_scoped
    where scenario_path is not null and scenario_path <> ''
      and coverage_tier <> 'rejection'
    group by surface_id
  ),
  behavior_scenario_rollup as (
    select
      surface_id,
      to_json(list_sort(list_distinct(list(scenario_id)))) as scenario_ids
    from covers_scoped
    where coverage_tier = 'behavior'
    group by surface_id
  ),
  behavior_path_rollup as (
    select
      surface_id,
      to_json(list_sort(list_distinct(list(scenario_path)))) as scenario_paths
    from covers_scoped
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
