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
            and c.seed_signature_match
            and c.seeded_assertion_count > 0
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
        when not exists (
          select 1
          from covers_norm c
          where c.surface_id = s.surface_id
            and c.coverage_tier = 'behavior'
        ) then case
          when s.value_arity = 'required'
            and coalesce(array_length(s.value_examples), 0) = 0
          then 'missing_value_examples'
          else 'missing_behavior_scenario'
        end
        when exists (
          select 1
          from required_value_misuse r
          where r.surface_id = s.surface_id
        ) then 'required_value_missing'
        when not exists (
          select 1
          from covers_norm c
          where c.surface_id = s.surface_id
            and c.coverage_tier = 'behavior'
            and c.has_evidence
        ) then 'scenario_failed'
        when exists (
          select 1
          from covers_norm c
          where c.surface_id = s.surface_id
            and c.coverage_tier = 'behavior'
            and c.has_evidence
            and (
              not c.variant_last_pass
              or not c.baseline_last_pass
            )
        ) then 'scenario_failed'
        when not exists (
          select 1
          from covers_norm c
          where c.surface_id = s.surface_id
            and c.coverage_tier = 'behavior'
            and c.assertion_count > 0
        ) then 'missing_assertions'
        when exists (
          select 1
          from covers_norm c
          where c.surface_id = s.surface_id
            and c.coverage_tier = 'behavior'
            and c.seed_path_missing
        ) then 'assertion_seed_path_not_seeded'
        when exists (
          select 1
          from covers_norm c
          where c.surface_id = s.surface_id
            and c.coverage_tier = 'behavior'
            and c.seed_path_assertion_count > 0
            and not c.seed_path_missing
            and not c.seed_signature_match
        ) then 'seed_signature_mismatch'
        when not exists (
          select 1
          from covers_norm c
          where c.surface_id = s.surface_id
            and c.coverage_tier = 'behavior'
            and c.seed_path_assertion_count > 0
        ) then 'seed_mismatch'
        when not exists (
          select 1
          from covers_norm c
          where c.surface_id = s.surface_id
            and c.coverage_tier = 'behavior'
            and c.delta_assertion_present
        ) then 'missing_delta_assertion'
        when not exists (
          select 1
          from covers_norm c
          where c.surface_id = s.surface_id
            and c.coverage_tier = 'behavior'
            and c.semantic_predicate_present
        ) then 'missing_semantic_predicate'
        when exists (
          select 1
          from covers_norm c
          where c.surface_id = s.surface_id
            and c.coverage_tier = 'behavior'
            and c.delta_assertion_present
            and c.outputs_equal
        ) then 'outputs_equal'
        when exists (
          select 1
          from covers_norm c
          where c.surface_id = s.surface_id
            and c.coverage_tier = 'behavior'
            and (not c.assertions_pass or not c.delta_proof_pass)
        ) then 'assertion_failed'
        else null
      end as behavior_unverified_reason_code
    from surface s
  ),
  behavior_reason_detail_candidates as (
    select
      c.surface_id,
      'scenario_failed' as reason_code,
      c.scenario_id,
      cast(null as VARCHAR) as assertion_kind,
      cast(null as VARCHAR) as assertion_seed_path,
      cast(null as VARCHAR) as assertion_token,
      10 as priority
    from covers_norm c
    where c.coverage_tier = 'behavior'
      and (
        not c.has_evidence
        or not c.variant_last_pass
        or not c.baseline_last_pass
      )
    union all
    select
      r.surface_id,
      'required_value_missing' as reason_code,
      r.scenario_id,
      cast(null as VARCHAR) as assertion_kind,
      cast(null as VARCHAR) as assertion_seed_path,
      cast(null as VARCHAR) as assertion_token,
      1 as priority
    from required_value_misuse r
    union all
    select
      c.surface_id,
      'missing_assertions' as reason_code,
      c.scenario_id,
      cast(null as VARCHAR) as assertion_kind,
      cast(null as VARCHAR) as assertion_seed_path,
      cast(null as VARCHAR) as assertion_token,
      10 as priority
    from covers_norm c
    where c.coverage_tier = 'behavior'
      and c.assertion_count = 0
    union all
    select
      c.surface_id,
      'assertion_seed_path_not_seeded' as reason_code,
      d.scenario_id,
      d.assertion_kind,
      d.seed_path as assertion_seed_path,
      d.match_token as assertion_token,
      1 as priority
    from behavior_assertion_detail d
    join covers_norm c
      on c.scenario_id = d.scenario_id
    where c.coverage_tier = 'behavior'
      and d.seed_path_missing = 1
    union all
    select
      c.surface_id,
      'seed_signature_mismatch' as reason_code,
      d.scenario_id,
      d.assertion_kind,
      d.seed_path as assertion_seed_path,
      d.match_token as assertion_token,
      1 as priority
    from behavior_assertion_detail d
    join covers_norm c
      on c.scenario_id = d.scenario_id
    where c.coverage_tier = 'behavior'
      and d.uses_seed_path_assertion
      and not d.seed_signature_match
    union all
    select
      c.surface_id,
      'seed_mismatch' as reason_code,
      c.scenario_id,
      cast(null as VARCHAR) as assertion_kind,
      cast(null as VARCHAR) as assertion_seed_path,
      cast(null as VARCHAR) as assertion_token,
      10 as priority
    from covers_norm c
    where c.coverage_tier = 'behavior'
      and c.seed_path_assertion_count = 0
    union all
    select
      c.surface_id,
      'missing_delta_assertion' as reason_code,
      c.scenario_id,
      cast(null as VARCHAR) as assertion_kind,
      cast(null as VARCHAR) as assertion_seed_path,
      cast(null as VARCHAR) as assertion_token,
      10 as priority
    from covers_norm c
    where c.coverage_tier = 'behavior'
      and not c.delta_assertion_present
    union all
    select
      c.surface_id,
      'missing_semantic_predicate' as reason_code,
      c.scenario_id,
      cast(null as VARCHAR) as assertion_kind,
      cast(null as VARCHAR) as assertion_seed_path,
      cast(null as VARCHAR) as assertion_token,
      10 as priority
    from covers_norm c
    where c.coverage_tier = 'behavior'
      and not c.semantic_predicate_present
    union all
    select
      c.surface_id,
      'outputs_equal' as reason_code,
      c.scenario_id,
      cast(null as VARCHAR) as assertion_kind,
      cast(null as VARCHAR) as assertion_seed_path,
      cast(null as VARCHAR) as assertion_token,
      10 as priority
    from covers_norm c
    where c.coverage_tier = 'behavior'
      and c.delta_assertion_present
      and c.outputs_equal
    union all
    select
      c.surface_id,
      'assertion_failed' as reason_code,
      d.scenario_id,
      d.assertion_kind,
      d.seed_path as assertion_seed_path,
      d.match_token as assertion_token,
      1 as priority
    from behavior_assertion_detail d
    join covers_norm c
      on c.scenario_id = d.scenario_id
    where c.coverage_tier = 'behavior'
      and d.assertion_ready = 1
      and d.assertion_pass = 0
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
