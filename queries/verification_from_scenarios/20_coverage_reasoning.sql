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
  -- Pre-aggregate covers_norm for accepted_status (avoids correlated EXISTS)
  covers_acceptance_agg as (
    select
      surface_id,
      max(case when coverage_tier <> 'rejection' then 1 else 0 end) as has_non_rejection,
      max(case when coverage_tier <> 'rejection' and acceptance_outcome = 'accepted' then 1 else 0 end) as has_accepted,
      max(case when coverage_tier <> 'rejection' and acceptance_outcome = 'rejected' then 1 else 0 end) as has_rejected,
      max(case when coverage_tier <> 'rejection' and has_evidence then 1 else 0 end) as has_evidence
    from covers_norm
    group by surface_id
  ),
  -- Pre-aggregate covers_norm for behavior_status and behavior_reason (avoids correlated EXISTS)
  covers_behavior_agg as (
    select
      surface_id,
      max(case when coverage_tier = 'behavior' then 1 else 0 end) as has_behavior,
      max(case when coverage_tier = 'behavior'
        and variant_last_pass
        and baseline_last_pass
        and (seeded_assertion_count = 0 or seed_signature_match)
        and (seeded_assertion_count > 0 or delta_assertion_present)
        and assertions_pass
        and delta_proof_pass
        and semantic_predicate_pass then 1 else 0 end) as is_verified,
      max(case when coverage_tier = 'behavior' and has_evidence then 1 else 0 end) as has_evidence,
      max(case when coverage_tier = 'behavior' and delta_assertion_present and outputs_equal then 1 else 0 end) as has_outputs_equal,
      max(case when coverage_tier = 'behavior'
        and has_evidence
        and variant_last_pass
        and baseline_last_pass
        and (not assertions_pass or not delta_proof_pass) then 1 else 0 end) as has_assertion_failed
    from covers_norm
    group by surface_id
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
        when coalesce(a.has_non_rejection, 0) = 0 then 'unknown'
        when a.has_accepted = 1 then 'verified'
        when a.has_rejected = 1 then 'rejected'
        when a.has_evidence = 1 then 'inconclusive'
        else 'recognized'
      end as status
    from surface s
    left join covers_acceptance_agg a using (surface_id)
  ),
  behavior_status as (
    select
      s.surface_id,
      case
        -- Deferred: auto_verify timed out (likely interactive/hanging command)
        when t.surface_id is not null then 'deferred'
        when coalesce(b.has_behavior, 0) = 0 then 'unknown'
        when b.is_verified = 1 then 'verified'
        when b.has_evidence = 1 then 'rejected'
        else 'recognized'
      end as status
    from surface s
    left join auto_verify_timeout t using (surface_id)
    left join covers_behavior_agg b using (surface_id)
  ),
  behavior_reason as (
    select
      s.surface_id,
      case
        -- auto_verify_timeout: auto_verify timed out (likely interactive/hanging)
        when t.surface_id is not null then 'auto_verify_timeout'
        -- no_scenario: no behavior scenario covers this surface
        when coalesce(b.has_behavior, 0) = 0 then 'no_scenario'
        -- outputs_equal: variant output same as baseline
        when b.has_outputs_equal = 1 then 'outputs_equal'
        -- assertion_failed: assertions ran but didn't pass
        when b.has_assertion_failed = 1 then 'assertion_failed'
        -- scenario_error: everything else (run failed, missing assertions, bad config)
        when b.has_behavior = 1 then 'scenario_error'
        else null
      end as behavior_unverified_reason_code
    from surface s
    left join auto_verify_timeout t using (surface_id)
    left join covers_behavior_agg b using (surface_id)
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
