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
  ),
  delta_outcome as (
    select
      s.surface_id,
      coalesce(p.scenario_paths, []::VARCHAR[]) as delta_evidence_paths,
      case
        when s.value_arity = 'required'
          and coalesce(array_length(s.value_examples), 0) = 0 then 'missing_value_examples'
        when bs.status = 'verified' then 'delta_seen'
        when br.behavior_unverified_reason_code = 'outputs_equal' then 'outputs_equal'
        when br.behavior_unverified_reason_code = 'scenario_error' then 'scenario_error'
        when br.behavior_unverified_reason_code = 'assertion_failed' then 'assertion_failed'
        when br.behavior_unverified_reason_code = 'no_scenario' then null
        else null
      end as delta_outcome
    from surface s
    left join behavior_status bs using (surface_id)
    left join behavior_reason br using (surface_id)
    left join behavior_path_rollup p using (surface_id)
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
  end as behavior_unverified_reason_code,
  case
    when behavior_status.status = 'verified' then null
    else behavior_reason_detail.scenario_id
  end as behavior_unverified_scenario_id,
  case
    when behavior_status.status = 'verified' then null
    else behavior_reason_detail.assertion_kind
  end as behavior_unverified_assertion_kind,
  case
    when behavior_status.status = 'verified' then null
    else behavior_reason_detail.assertion_seed_path
  end as behavior_unverified_assertion_seed_path,
  case
    when behavior_status.status = 'verified' then null
    else behavior_reason_detail.assertion_token
  end as behavior_unverified_assertion_token,
  delta_outcome.delta_outcome as delta_outcome,
  to_json(coalesce(delta_outcome.delta_evidence_paths, []::VARCHAR[])) as delta_evidence_paths,
  to_json(
    coalesce(behavior_confounded_rollup.scenario_ids, []::VARCHAR[])
  ) as behavior_confounded_scenario_ids,
  to_json(
    coalesce(behavior_confounded_rollup.extra_surface_ids, []::VARCHAR[])
  ) as behavior_confounded_extra_surface_ids,
  auto_verify_evidence.auto_verify_exit_code as auto_verify_exit_code,
  auto_verify_evidence.auto_verify_stderr as auto_verify_stderr
from surface
left join accepted_status using (surface_id)
left join behavior_status using (surface_id)
left join behavior_reason using (surface_id)
left join behavior_reason_detail
  on behavior_reason_detail.surface_id = surface.surface_id
  and behavior_reason_detail.reason_code = behavior_reason.behavior_unverified_reason_code
left join accepted_scenario_rollup using (surface_id)
left join accepted_path_rollup using (surface_id)
left join behavior_scenario_rollup using (surface_id)
left join behavior_assertion_scenario_rollup using (surface_id)
left join behavior_path_rollup using (surface_id)
left join delta_outcome using (surface_id)
left join behavior_confounded_rollup using (surface_id)
left join auto_verify_evidence using (surface_id)
order by surface.surface_id;
