use super::*;

#[test]
fn next_action_edit_deserialize_defaults_edit_strategy() {
    let value = serde_json::json!({
        "kind": "edit",
        "path": "scenarios/plan.json",
        "content": "{}",
        "reason": "replace"
    });
    let action: NextAction = serde_json::from_value(value).expect("deserialize next action");
    match action {
        NextAction::Edit { edit_strategy, .. } => {
            assert_eq!(edit_strategy, "replace_file");
        }
        _ => panic!("expected edit next action"),
    }
}

#[test]
fn normalize_next_action_fills_missing_edit_strategy() {
    let mut action = NextAction::Edit {
        path: "enrich/config.json".to_string(),
        content: "{}".to_string(),
        reason: "replace".to_string(),
        hint: None,
        edit_strategy: String::new(),
        payload: None,
    };
    normalize_next_action(&mut action);
    let serialized =
        serde_json::to_value(action).expect("serialize normalized next action as value");
    assert_eq!(
        serialized
            .get("edit_strategy")
            .and_then(serde_json::Value::as_str),
        Some("replace_file")
    );
}
