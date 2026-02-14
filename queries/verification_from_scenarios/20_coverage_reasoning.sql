  covers_raw as (
    select
      scenario_id,
      scenario_paths,
      has_evidence,
      acceptance_outcome,
      coverage_tier,
      assertion_count,
      seeded_assertion_count,
      seed_path_assertion_count,
      seed_path_missing_count,
      seed_path_missing,
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
    where not coalesce(coverage_ignore, false)
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
      seed_path_assertion_count,
      seed_path_missing_count,
      seed_path_missing,
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
  required_value_misuse as (
    select
      c.surface_id,
      c.scenario_id
    from covers_norm c
    join surface s
      on s.surface_id = c.surface_id
    where c.coverage_tier = 'behavior'
      and s.value_arity = 'required'
      and (
        exists (
          select 1
          from unnest(coalesce(c.argv, []::VARCHAR[])) as t(token)
          where starts_with(token, c.surface_id || '=')
            and trim(substr(token, length(c.surface_id) + 2)) = ''
        )
        or (
          list_position(coalesce(c.argv, []::VARCHAR[]), c.surface_id) is not null
          and not exists (
            select 1
            from unnest(coalesce(c.argv, []::VARCHAR[])) as t(token)
            where starts_with(token, c.surface_id || '=')
              and trim(substr(token, length(c.surface_id) + 2)) <> ''
          )
          and (
            coalesce(array_length(c.argv), 0)
              = list_position(coalesce(c.argv, []::VARCHAR[]), c.surface_id)
            or coalesce(
              trim(
                list_extract(
                  c.argv,
                  list_position(coalesce(c.argv, []::VARCHAR[]), c.surface_id) + 1
                )
              ),
              ''
            ) = ''
            or starts_with(
              coalesce(
                list_extract(
                  c.argv,
                  list_position(coalesce(c.argv, []::VARCHAR[]), c.surface_id) + 1
                ),
                ''
              ),
              '-'
            )
          )
        )
      )
    group by c.surface_id, c.scenario_id
  ),
  behavior_confounded_cover as (
    select
      c.surface_id,
      c.scenario_id,
      extra.surface_id as extra_surface_id,
      semantics.confounded_coverage_gate as confounded_coverage_gate
    from covers_norm c
    join behavior_semantics semantics on true
    cross join unnest(coalesce(c.argv, []::VARCHAR[])) as t(token)
    join surface extra
      on extra.surface_id <> c.surface_id
    where c.coverage_tier = 'behavior'
      and case
        when starts_with(extra.surface_id, '--') then token = extra.surface_id
          or starts_with(token, extra.surface_id || '=')
        when starts_with(extra.surface_id, '-') then token = extra.surface_id
          or starts_with(token, extra.surface_id)
        else token = extra.surface_id
      end
  ),
  behavior_confounded_rollup as (
    select
      surface_id,
      list_sort(list_distinct(list(scenario_id))) as scenario_ids,
      list_sort(list_distinct(list(extra_surface_id))) as extra_surface_ids,
      max(case when confounded_coverage_gate then 1 else 0 end) = 1 as confounded_coverage_gate
    from behavior_confounded_cover
    group by surface_id
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
            and c.has_evidence
        ) then 'inconclusive'
        else 'recognized'
      end as status
    from surface s
  ),
  behavior_status as (
    select
      s.surface_id,
      case
        -- Deferred: auto_verify timed out (likely interactive/hanging command)
        when exists (
          select 1 from auto_verify_timeout t
          where t.surface_id = s.surface_id
        ) then 'deferred'
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
            and c.variant_last_pass
            and c.baseline_last_pass
            -- Only require seed_signature_match if using seeded assertions
            and (c.seeded_assertion_count = 0 or c.seed_signature_match)
            and (c.seeded_assertion_count > 0 or c.delta_assertion_present)
            and c.assertions_pass
            and c.delta_proof_pass
            and c.semantic_predicate_pass
        ) then 'verified'
        when exists (
          select 1
          from covers_norm c
          where c.surface_id = s.surface_id
            and c.coverage_tier = 'behavior'
            and c.has_evidence
        ) then 'rejected'
        else 'recognized'
      end as status
    from surface s
  ),
  behavior_reason as (
    select
      s.surface_id,
      case
        -- auto_verify_timeout: auto_verify timed out (likely interactive/hanging)
        when exists (
          select 1 from auto_verify_timeout t
          where t.surface_id = s.surface_id
        ) then 'auto_verify_timeout'
        -- no_scenario: no behavior scenario covers this surface
        when not exists (
          select 1
          from covers_norm c
          where c.surface_id = s.surface_id
            and c.coverage_tier = 'behavior'
        ) then 'no_scenario'
        -- outputs_equal: variant output same as baseline
        when exists (
          select 1
          from covers_norm c
          where c.surface_id = s.surface_id
            and c.coverage_tier = 'behavior'
            and c.delta_assertion_present
            and c.outputs_equal
        ) then 'outputs_equal'
        -- assertion_failed: assertions ran but didn't pass
        when exists (
          select 1
          from covers_norm c
          where c.surface_id = s.surface_id
            and c.coverage_tier = 'behavior'
            and c.has_evidence
            and c.variant_last_pass
            and c.baseline_last_pass
            and (not c.assertions_pass or not c.delta_proof_pass)
        ) then 'assertion_failed'
        -- scenario_error: everything else (run failed, missing assertions, bad config)
        when exists (
          select 1
          from covers_norm c
          where c.surface_id = s.surface_id
            and c.coverage_tier = 'behavior'
        ) then 'scenario_error'
        else null
      end as behavior_unverified_reason_code
    from surface s
  ),
  behavior_reason_detail_candidates as (
    -- scenario_error: any behavior scenario with issues
    select
      c.surface_id,
      'scenario_error' as reason_code,
      c.scenario_id,
      cast(null as VARCHAR) as assertion_kind,
      cast(null as VARCHAR) as assertion_seed_path,
      cast(null as VARCHAR) as assertion_token,
      10 as priority
    from covers_norm c
    where c.coverage_tier = 'behavior'
    union all
    -- outputs_equal
    select
      c.surface_id,
      'outputs_equal' as reason_code,
      c.scenario_id,
      cast(null as VARCHAR) as assertion_kind,
      cast(null as VARCHAR) as assertion_seed_path,
      cast(null as VARCHAR) as assertion_token,
      5 as priority
    from covers_norm c
    where c.coverage_tier = 'behavior'
      and c.delta_assertion_present
      and c.outputs_equal
    union all
    -- assertion_failed
    select
      c.surface_id,
      'assertion_failed' as reason_code,
      c.scenario_id,
      cast(null as VARCHAR) as assertion_kind,
      cast(null as VARCHAR) as assertion_seed_path,
      cast(null as VARCHAR) as assertion_token,
      1 as priority
    from covers_norm c
    where c.coverage_tier = 'behavior'
      and c.has_evidence
      and c.variant_last_pass
      and c.baseline_last_pass
      and (not c.assertions_pass or not c.delta_proof_pass)
  ),
  behavior_reason_detail as (
    select
      surface_id,
      reason_code,
      scenario_id,
      assertion_kind,
      assertion_seed_path,
      assertion_token
    from (
      select
        surface_id,
        reason_code,
        scenario_id,
        assertion_kind,
        assertion_seed_path,
        assertion_token,
        row_number() over (
          partition by surface_id, reason_code
          order by priority, scenario_id, assertion_kind, assertion_seed_path
        ) as rk
      from behavior_reason_detail_candidates
    )
    where rk = 1
  ),
