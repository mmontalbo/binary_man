
use super::*;
use std::cell::RefCell;
use std::rc::Rc;

fn apply_args(refresh_pack: bool) -> ApplyArgs {
    ApplyArgs {
        doc_pack: std::path::PathBuf::from("/tmp/doc-pack"),
        refresh_pack,
        verbose: false,
        rerun_all: false,
        rerun_failed: false,
        lens_flake: "unused".to_string(),
    }
}

#[test]
fn refresh_pack_runs_before_validate_and_plan_derivation() {
    let args = apply_args(true);
    let lock_status = enrich::LockStatus {
        present: true,
        stale: false,
        inputs_hash: Some("stale".to_string()),
    };
    let plan_state = enrich::PlanStatus {
        present: true,
        stale: false,
        inputs_hash: Some("stale".to_string()),
        lock_inputs_hash: Some("stale".to_string()),
    };
    let call_order = Rc::new(RefCell::new(Vec::new()));
    let input_state = Rc::new(RefCell::new("pre_refresh".to_string()));
    let plan_input_state = Rc::new(RefCell::new(None::<String>));

    let preflight = run_apply_preflight(
        &args,
        &lock_status,
        &plan_state,
        {
            let call_order = Rc::clone(&call_order);
            let input_state = Rc::clone(&input_state);
            move || {
                call_order.borrow_mut().push("refresh");
                *input_state.borrow_mut() = "post_refresh".to_string();
                Ok(())
            }
        },
        {
            let call_order = Rc::clone(&call_order);
            let input_state = Rc::clone(&input_state);
            move || {
                call_order.borrow_mut().push("validate");
                assert_eq!(input_state.borrow().as_str(), "post_refresh");
                Ok(())
            }
        },
        {
            let call_order = Rc::clone(&call_order);
            let input_state = Rc::clone(&input_state);
            let plan_input_state = Rc::clone(&plan_input_state);
            move || {
                call_order.borrow_mut().push("plan");
                *plan_input_state.borrow_mut() = Some(input_state.borrow().clone());
                Ok(())
            }
        },
    )
    .expect("preflight should succeed");

    assert!(preflight.ran_validate);
    assert!(preflight.ran_plan);
    assert_eq!(
        call_order.borrow().as_slice(),
        &["refresh", "validate", "plan"]
    );
    assert_eq!(
        plan_input_state.borrow().as_deref(),
        Some("post_refresh"),
        "plan derivation must run against refreshed inputs"
    );
}
