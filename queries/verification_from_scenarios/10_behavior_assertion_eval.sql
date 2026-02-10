  behavior_context as (
    select
      s.scenario_id,
      s.baseline_scenario_id,
      coalesce(var.last_pass, false) as variant_last_pass,
      s.baseline_scenario_id is not null
        and coalesce(base.last_pass, false) as baseline_last_pass,
      s.baseline_scenario_id is not null
        and coalesce(var_seed.seed_signature, '[]') = coalesce(base_seed.seed_signature, '[]')
        as seed_signature_match,
      var.normalized_stdout as variant_stdout,
      base.normalized_stdout as baseline_stdout,
      var.normalized_stdout is not null
        and base.normalized_stdout is not null
        and var.normalized_stdout = base.normalized_stdout as outputs_equal
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
      a.seed_path as seed_path,
      coalesce(a.token, a.seed_path) as match_token,
      a.run as run_target,
      coalesce(a.exact_line, false) as exact_line,
      a.kind in ('stdout_contains', 'stdout_lacks') as uses_seed_path_assertion,
      case
        when a.kind = 'stdout_contains' and a.run = 'baseline' then 'baseline_contains'
        when a.kind = 'stdout_contains' and a.run = 'variant' then 'variant_contains'
        when a.kind = 'stdout_lacks' and a.run = 'baseline' then 'baseline_lacks'
        when a.kind = 'stdout_lacks' and a.run = 'variant' then 'variant_lacks'
        when a.kind = 'outputs_differ' then 'outputs_differ'
        else a.kind
      end as normalized_kind
    from combined_scenarios s,
      unnest(coalesce(s.assertions, []::STRUCT(
        kind VARCHAR,
        seed_path VARCHAR,
        token VARCHAR,
        run VARCHAR,
        exact_line BOOLEAN
      )[])) as t(a)
    where s.coverage_tier = 'behavior'
  ),
  behavior_assertion_detail as (
    select
      b.scenario_id,
      b.baseline_scenario_id,
      b.assertion_kind,
      b.normalized_kind,
      b.seed_path,
      b.match_token,
      b.exact_line,
      b.uses_seed_path_assertion,
      b.variant_last_pass,
      b.baseline_last_pass,
      b.seed_signature_match,
      b.variant_stdout,
      b.baseline_stdout,
      b.outputs_equal,
      b.seed_path_in_variant,
      b.seed_path_in_baseline,
      b.seed_path_missing,
      b.seeded_assertion,
      b.assertion_ready,
      case
        -- Baseline contains (substring or exact line)
        when b.normalized_kind = 'baseline_contains' then
          case
            when b.assertion_ready = 0 then 0
            when b.baseline_stdout is null then 0
            when b.exact_line and list_contains(
              str_split(b.baseline_stdout, chr(10)),
              b.match_token
            ) then 1
            when not b.exact_line and position(b.match_token in b.baseline_stdout) > 0 then 1
            else 0
          end
        -- Baseline lacks (substring or exact line)
        when b.normalized_kind = 'baseline_lacks' then
          case
            when b.assertion_ready = 0 then 0
            when b.baseline_stdout is null then 0
            when b.exact_line and not list_contains(
              str_split(b.baseline_stdout, chr(10)),
              b.match_token
            ) then 1
            when not b.exact_line and position(b.match_token in b.baseline_stdout) = 0 then 1
            else 0
          end
        -- Variant contains (substring or exact line)
        when b.normalized_kind = 'variant_contains' then
          case
            when b.assertion_ready = 0 then 0
            when b.variant_stdout is null then 0
            when b.exact_line and list_contains(
              str_split(b.variant_stdout, chr(10)),
              b.match_token
            ) then 1
            when not b.exact_line and position(b.match_token in b.variant_stdout) > 0 then 1
            else 0
          end
        -- Variant lacks (substring or exact line)
        when b.normalized_kind = 'variant_lacks' then
          case
            when b.assertion_ready = 0 then 0
            when b.variant_stdout is null then 0
            when b.exact_line and not list_contains(
              str_split(b.variant_stdout, chr(10)),
              b.match_token
            ) then 1
            when not b.exact_line and position(b.match_token in b.variant_stdout) = 0 then 1
            else 0
          end
        -- Outputs differ
        when b.normalized_kind = 'outputs_differ' then
          case
            when b.assertion_ready = 0 then 0
            when b.variant_stdout is null then 0
            when b.baseline_stdout is null then 0
            when b.variant_stdout <> b.baseline_stdout then 1
            else 0
          end
        else 0
      end as assertion_pass
    from (
      select
        b.scenario_id,
        b.baseline_scenario_id,
        b.assertion_kind,
        b.normalized_kind,
        b.seed_path,
        b.match_token,
        b.exact_line,
        b.uses_seed_path_assertion,
        ctx.variant_last_pass,
        ctx.baseline_last_pass,
        ctx.seed_signature_match,
        ctx.variant_stdout,
        ctx.baseline_stdout,
        ctx.outputs_equal,
        case
          when b.uses_seed_path_assertion
            and b.seed_path is not null
            and b.seed_path <> ''
            and exists (
              select 1
              from scenario_seed_paths sp
              where sp.scenario_id = b.scenario_id
                and sp.seed_path = b.seed_path
            )
            then 1
          else 0
        end as seed_path_in_variant,
        case
          when b.uses_seed_path_assertion
            and b.seed_path is not null
            and b.seed_path <> ''
            and exists (
              select 1
              from scenario_seed_paths sp
              where sp.scenario_id = b.baseline_scenario_id
                and sp.seed_path = b.seed_path
            )
            then 1
          else 0
        end as seed_path_in_baseline,
        case
          when b.uses_seed_path_assertion
            and b.seed_path is not null
            and b.seed_path <> ''
            and (
              not exists (
                select 1
                from scenario_seed_paths sp
                where sp.scenario_id = b.scenario_id
                  and sp.seed_path = b.seed_path
              )
              or not exists (
                select 1
                from scenario_seed_paths sp
                where sp.scenario_id = b.baseline_scenario_id
                  and sp.seed_path = b.seed_path
              )
            )
            then 1
          else 0
        end as seed_path_missing,
        case
          when b.uses_seed_path_assertion
            and b.seed_path is not null
            and b.seed_path <> ''
            and coalesce(ctx.seed_signature_match, false)
            and exists (
              select 1
              from scenario_seed_paths sp
              where sp.scenario_id = b.scenario_id
                and sp.seed_path = b.seed_path
            )
            and exists (
              select 1
              from scenario_seed_paths sp
              where sp.scenario_id = b.baseline_scenario_id
                and sp.seed_path = b.seed_path
            )
            then 1
          else 0
        end as seeded_assertion,
        case
          when not coalesce(ctx.variant_last_pass, false) then 0
          when not coalesce(ctx.baseline_last_pass, false) then 0
          when b.uses_seed_path_assertion
            and (b.seed_path is null or b.seed_path = '') then 0
          -- Only require seed_signature_match for assertions that use seed paths
          when b.uses_seed_path_assertion
            and not coalesce(ctx.seed_signature_match, false) then 0
          when b.uses_seed_path_assertion
            and not exists (
              select 1
              from scenario_seed_paths sp
              where sp.scenario_id = b.scenario_id
                and sp.seed_path = b.seed_path
            ) then 0
          when b.uses_seed_path_assertion
            and not exists (
              select 1
              from scenario_seed_paths sp
              where sp.scenario_id = b.baseline_scenario_id
                and sp.seed_path = b.seed_path
            ) then 0
          else 1
        end as assertion_ready
      from behavior_assertions_raw b
      left join behavior_context ctx
        on ctx.scenario_id = b.scenario_id
    ) b
  ),
  behavior_delta_pair as (
    select
      scenario_id,
      seed_path,
      -- Using normalized_kind which maps both legacy and new formats
      max(case when normalized_kind = 'baseline_lacks' then 1 else 0 end) as has_baseline_not,
      max(case when normalized_kind = 'baseline_lacks' and assertion_pass = 1 then 1 else 0 end) as baseline_not_pass,
      max(case when normalized_kind = 'baseline_contains' then 1 else 0 end) as has_baseline_contains,
      max(case when normalized_kind = 'baseline_contains' and assertion_pass = 1 then 1 else 0 end) as baseline_contains_pass,
      max(case when normalized_kind = 'variant_contains' then 1 else 0 end) as has_variant_contains,
      max(case when normalized_kind = 'variant_contains' and assertion_pass = 1 then 1 else 0 end) as variant_contains_pass,
      max(case when normalized_kind = 'variant_lacks' then 1 else 0 end) as has_variant_not,
      max(case when normalized_kind = 'variant_lacks' and assertion_pass = 1 then 1 else 0 end) as variant_not_pass
    from behavior_assertion_detail
    where uses_seed_path_assertion = true
    group by scenario_id, seed_path
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
      sum(
        case
          when uses_seed_path_assertion = true
            and seed_path is not null
            and seed_path <> '' then 1
          else 0
        end
      ) as seed_path_assertion_count,
      sum(case when seed_path_missing = 1 then 1 else 0 end) as seed_path_missing_count,
      min(case when assertion_pass = 1 then 1 else 0 end) as all_pass_int,
      -- Using normalized_kind to match both legacy and new format
      max(case when normalized_kind = 'outputs_differ' then 1 else 0 end) as diff_assertion_present,
      max(case when normalized_kind = 'outputs_differ' and assertion_pass = 1 then 1 else 0 end) as diff_assertion_pass
    from behavior_assertion_detail
    group by scenario_id
  ),
  behavior_eval as (
    select
      s.scenario_id,
      coalesce(a.assertion_count, 0) as assertion_count,
      coalesce(a.seeded_assertion_count, 0) as seeded_assertion_count,
      coalesce(a.seed_path_assertion_count, 0) as seed_path_assertion_count,
      coalesce(a.seed_path_missing_count, 0) as seed_path_missing_count,
      coalesce(a.seed_path_missing_count, 0) > 0 as seed_path_missing,
      (
        coalesce(a.assertion_count, 0) > 0
        and coalesce(a.all_pass_int, 0) = 1
      ) as assertions_pass,
      coalesce(a.diff_assertion_present, 0) = 1 as diff_assertion_present,
      coalesce(a.diff_assertion_pass, 0) = 1 as diff_assertion_pass,
      coalesce(dp.delta_pair_present, 0) = 1 as delta_pair_present,
      coalesce(dp.delta_pair_pass, 0) = 1 as delta_pair_pass,
      (
        coalesce(a.diff_assertion_pass, 0) = 1
        or coalesce(dp.delta_pair_pass, 0) = 1
      ) as delta_proof_pass,
      (
        coalesce(a.diff_assertion_present, 0) = 1
        or coalesce(dp.delta_pair_present, 0) = 1
      ) as delta_assertion_present,
      coalesce(s.expect_has_output_predicate, false) as expect_has_output_predicate,
      (
        coalesce(s.expect_has_output_predicate, false)
        or coalesce(a.diff_assertion_present, 0) = 1
        or coalesce(dp.delta_pair_present, 0) = 1
      ) as semantic_predicate_present,
      (
        coalesce(s.expect_has_output_predicate, false)
        or coalesce(a.diff_assertion_pass, 0) = 1
        or coalesce(dp.delta_pair_pass, 0) = 1
      ) as semantic_predicate_pass,
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
      coalesce(b.seed_path_assertion_count, 0) as seed_path_assertion_count,
      coalesce(b.seed_path_missing_count, 0) as seed_path_missing_count,
      coalesce(b.seed_path_missing, false) as seed_path_missing,
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
      e.scenario_id is not null as has_evidence,
      e.last_pass as last_pass,
      case
        when e.scenario_id is null then 'unknown'
        when coalesce(e.timed_out, false) then 'inconclusive'
        when exists (
          select 1
          from rule_eval r
          where r.scenario_id = p.scenario_id
            and r.rule_kind = 'rejected'
            and r.matches
        ) then 'rejected'
        when exists (
          select 1
          from rule_eval r
          where r.scenario_id = p.scenario_id
            and r.rule_kind = 'accepted'
            and r.matches
        ) then 'accepted'
        else 'inconclusive'
      end as acceptance_outcome
    from combined_scenarios p
    left join normalized_evidence e
      on e.scenario_id = p.scenario_id
    left join behavior_eval b
      on b.scenario_id = p.scenario_id
  ),
