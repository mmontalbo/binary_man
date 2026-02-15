use super::super::selectors::surface_kind_for_id;
use super::super::LedgerEntries;
use crate::enrich;
use crate::scenarios;
use serde_json::{Map, Value};
use std::cmp::Ordering;

pub(crate) fn surface_overlays_requires_argv_stub_batch(
    paths: &enrich::DocPackPaths,
    surface: &crate::surface::SurfaceInventory,
    target_ids: &[String],
) -> String {
    surface_overlays_stub_batch(
        paths,
        surface,
        target_ids,
        ensure_overlay_requires_argv_placeholder,
    )
}

pub(crate) fn surface_overlays_behavior_exclusion_stub_batch(
    paths: &enrich::DocPackPaths,
    surface: &crate::surface::SurfaceInventory,
    target_ids: &[String],
    ledger_entries: &LedgerEntries,
) -> String {
    surface_overlays_stub_batch(paths, surface, target_ids, |obj, kind, id| {
        ensure_overlay_behavior_exclusion(obj, kind, id, ledger_entries);
    })
}

fn surface_overlays_stub_batch<F>(
    paths: &enrich::DocPackPaths,
    surface: &crate::surface::SurfaceInventory,
    target_ids: &[String],
    update_overlay: F,
) -> String
where
    F: Fn(&mut Map<String, Value>, &str, &str),
{
    let ids = normalized_ids(target_ids);
    if let Some(serialized) = edit_existing_overlays(paths, |obj| {
        apply_stub_updates(obj, surface, &ids, &update_overlay);
    }) {
        return serialized;
    }
    build_new_overlays(|obj| apply_stub_updates(obj, surface, &ids, &update_overlay))
}

fn apply_stub_updates<F>(
    obj: &mut Map<String, Value>,
    surface: &crate::surface::SurfaceInventory,
    ids: &[String],
    update_overlay: &F,
) where
    F: Fn(&mut Map<String, Value>, &str, &str),
{
    for surface_id in ids {
        let kind = surface_kind_for_id(surface, surface_id, "option");
        ensure_overlay_entry(obj, &kind, surface_id);
        update_overlay(obj, &kind, surface_id);
    }
    sort_overlay_entries(obj);
}

fn normalized_ids(raw_ids: &[String]) -> Vec<String> {
    let mut ids = raw_ids
        .iter()
        .map(|id| id.trim())
        .filter(|id| !id.is_empty())
        .map(|id| id.to_string())
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

fn edit_existing_overlays<F>(paths: &enrich::DocPackPaths, update: F) -> Option<String>
where
    F: FnOnce(&mut Map<String, Value>),
{
    let overlays_path = paths.surface_overlays_path();
    if !overlays_path.is_file() {
        return None;
    }
    let bytes = std::fs::read(&overlays_path).ok()?;
    let mut value = serde_json::from_slice::<Value>(&bytes).ok()?;
    let obj = value.as_object_mut()?;
    update(obj);
    serde_json::to_string_pretty(&value).ok()
}

fn build_new_overlays<F>(update: F) -> String
where
    F: FnOnce(&mut Map<String, Value>),
{
    let mut value = serde_json::json!({
        "schema_version": 3,
        "items": [],
        "overlays": []
    });
    if let Some(obj) = value.as_object_mut() {
        update(obj);
    }
    serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
}

fn ensure_overlay_entry(obj: &mut Map<String, Value>, kind: &str, id: &str) {
    let overlays = array_field_mut(obj, "overlays");
    if overlays
        .iter()
        .any(|entry| overlay_matches(entry, kind, id))
    {
        return;
    }
    overlays.push(serde_json::json!({
        "kind": kind,
        "id": id,
        "invocation": {
            "value_examples": [],
            "requires_argv": []
        }
    }));
}

fn ensure_overlay_requires_argv_placeholder(obj: &mut Map<String, Value>, kind: &str, id: &str) {
    let Some(entry_obj) = overlay_entry_object_mut(obj, kind, id) else {
        return;
    };
    let invocation_value = entry_obj
        .entry("invocation".to_string())
        .or_insert_with(|| serde_json::json!({"value_examples": [], "requires_argv": []}));
    if !invocation_value.is_object() {
        *invocation_value = serde_json::json!({"value_examples": [], "requires_argv": []});
    }
    let Some(invocation_obj) = invocation_value.as_object_mut() else {
        return;
    };
    let requires_argv_value = invocation_obj
        .entry("requires_argv".to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    if !requires_argv_value.is_array() {
        *requires_argv_value = Value::Array(Vec::new());
    }
    let Some(requires_argv) = requires_argv_value.as_array_mut() else {
        return;
    };
    if requires_argv
        .iter()
        .any(|value| value.as_str().is_some_and(|token| !token.trim().is_empty()))
    {
        return;
    }
    requires_argv.push(Value::String("<required_argv>".to_string()));
}

fn ensure_overlay_behavior_exclusion(
    obj: &mut Map<String, Value>,
    kind: &str,
    id: &str,
    ledger_entries: &LedgerEntries,
) {
    let Some(entry) = ledger_entries.get(id) else {
        return;
    };
    let delta_variant_path = delta_variant_path_for_entry(entry)
        .unwrap_or_else(|| "inventory/scenarios/<delta_variant>.json".to_string());
    let Some(overlay_obj) = overlay_entry_object_mut(obj, kind, id) else {
        return;
    };
    if overlay_obj
        .get("behavior_exclusion")
        .and_then(|value| value.as_object())
        .is_some()
    {
        return;
    }
    overlay_obj.insert(
        "behavior_exclusion".to_string(),
        serde_json::json!({
            "reason_code": "fixture_gap",
            "note": "still outputs_equal after workarounds",
            "evidence": {
                "delta_variant_path": delta_variant_path
            }
        }),
    );
}

fn delta_variant_path_for_entry(entry: &scenarios::VerificationEntry) -> Option<String> {
    entry
        .delta_evidence_paths
        .iter()
        .find(|path| path.contains("variant"))
        .cloned()
        .or_else(|| entry.delta_evidence_paths.first().cloned())
}

fn overlay_entry_object_mut<'a>(
    obj: &'a mut Map<String, Value>,
    kind: &str,
    id: &str,
) -> Option<&'a mut Map<String, Value>> {
    let overlays = array_field_mut(obj, "overlays");
    overlays
        .iter_mut()
        .find(|entry| overlay_matches(entry, kind, id))
        .and_then(Value::as_object_mut)
}

fn overlay_matches(entry: &Value, kind: &str, id: &str) -> bool {
    entry
        .get("kind")
        .and_then(Value::as_str)
        .is_some_and(|value| value == kind)
        && entry
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|value| value == id)
}

fn array_field_mut<'a>(obj: &'a mut Map<String, Value>, key: &str) -> &'a mut Vec<Value> {
    let array_value = obj
        .entry(key.to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    if !array_value.is_array() {
        *array_value = Value::Array(Vec::new());
    }
    array_value
        .as_array_mut()
        .expect("array field must be array")
}

fn sort_overlay_entries(obj: &mut Map<String, Value>) {
    let overlays = array_field_mut(obj, "overlays");
    overlays.sort_by(|a, b| {
        let a_id = a.get("id").and_then(Value::as_str).unwrap_or("");
        let b_id = b.get("id").and_then(Value::as_str).unwrap_or("");
        let id_cmp = a_id.cmp(b_id);
        if id_cmp == Ordering::Equal {
            let a_kind = a.get("kind").and_then(Value::as_str).unwrap_or("");
            let b_kind = b.get("kind").and_then(Value::as_str).unwrap_or("");
            a_kind.cmp(b_kind)
        } else {
            id_cmp
        }
    });
}
