//! Data loading for the inspector TUI.
//!
//! Provides data models and loading functions for the three main tabs:
//! - Work: Unverified surface items from verification ledger
//! - Log: LM invocation history from lm_log.jsonl
//! - Browse: File tree of doc pack artifacts

use crate::enrich::{self, load_lm_log, DocPackPaths, LmLogEntry};
use crate::scenarios;
use crate::surface::SurfaceInventory;
use crate::workflow;
use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};

use super::format::preview_text;
use super::{Tab, WorkCategory, PREVIEW_LIMIT};

/// A work queue item representing an unverified surface item.
#[derive(Debug, Clone)]
pub(super) struct WorkItem {
    /// The surface item ID (e.g., "--verbose").
    pub(super) surface_id: String,
    /// Category: needs_scenario, needs_fix, or excluded.
    pub(super) category: WorkCategory,
    /// Reason code from verification ledger.
    pub(super) reason_code: String,
    /// Description from surface inventory.
    pub(super) description: Option<String>,
    /// Forms from surface inventory (e.g., "-v, --verbose").
    pub(super) forms: Vec<String>,
    /// Exit code from last run (if any).
    pub(super) exit_code: Option<i64>,
    /// Stderr preview from last run (if any).
    pub(super) stderr_preview: Option<String>,
    /// Suggested prereq (if any).
    pub(super) suggested_prereq: Option<String>,
    /// Scenario ID covering this item (if any).
    pub(super) scenario_id: Option<String>,
}

/// Work queue data grouped by category.
#[derive(Debug)]
pub(super) struct WorkQueue {
    /// Items needing a scenario.
    pub(super) needs_scenario: Vec<WorkItem>,
    /// Items needing a fix.
    pub(super) needs_fix: Vec<WorkItem>,
    /// Excluded items.
    pub(super) excluded: Vec<WorkItem>,
    /// Verified items.
    pub(super) verified: Vec<WorkItem>,
}

impl WorkQueue {
    /// Total count of all items.
    pub(super) fn total_count(&self) -> usize {
        self.needs_scenario.len() + self.needs_fix.len() + self.excluded.len() + self.verified.len()
    }

    /// Count of unverified items (work remaining).
    pub(super) fn unverified_count(&self) -> usize {
        self.needs_scenario.len() + self.needs_fix.len()
    }

    /// Get all items as a flat list with category headers.
    pub(super) fn flat_items(&self) -> Vec<(Option<WorkCategory>, Option<&WorkItem>)> {
        let mut items = Vec::new();

        if !self.needs_scenario.is_empty() {
            items.push((Some(WorkCategory::NeedsScenario), None));
            for item in &self.needs_scenario {
                items.push((None, Some(item)));
            }
        }

        if !self.needs_fix.is_empty() {
            items.push((Some(WorkCategory::NeedsFix), None));
            for item in &self.needs_fix {
                items.push((None, Some(item)));
            }
        }

        if !self.excluded.is_empty() {
            items.push((Some(WorkCategory::Excluded), None));
            for item in &self.excluded {
                items.push((None, Some(item)));
            }
        }

        if !self.verified.is_empty() {
            items.push((Some(WorkCategory::Verified), None));
            for item in &self.verified {
                items.push((None, Some(item)));
            }
        }

        items
    }
}

/// A file/directory entry in the browse tree.
#[derive(Debug, Clone)]
pub(super) struct BrowseEntry {
    /// Relative path from doc pack root.
    pub(super) rel_path: String,
    /// Absolute path.
    pub(super) path: PathBuf,
    /// Is this a directory?
    pub(super) is_dir: bool,
    /// Indentation level in tree.
    pub(super) depth: usize,
}

/// All inspector data.
#[derive(Debug)]
pub(super) struct InspectData {
    /// Work queue items grouped by category.
    pub(super) work: WorkQueue,
    /// LM invocation log entries (newest first).
    pub(super) log: Vec<LmLogEntry>,
    /// Browse tree entries.
    pub(super) browse: Vec<BrowseEntry>,
    /// Man page path (if exists).
    pub(super) man_page_path: Option<PathBuf>,
    /// Entry point scope (e.g., "config" for `git config`).
    pub(super) scope: Option<String>,
}

impl InspectData {
    fn load(
        doc_pack_root: &Path,
        summary: &enrich::StatusSummary,
        show_all: &[bool; 3],
    ) -> Result<Self> {
        let paths = DocPackPaths::new(doc_pack_root.to_path_buf());

        // Load surface inventory for descriptions and scope
        let surface_path = paths.surface_path();
        let surface: Option<SurfaceInventory> = if surface_path.is_file() {
            let content = fs::read_to_string(&surface_path)?;
            serde_json::from_str(&content).ok()
        } else {
            None
        };

        // Derive scope from prereqs.json + surface items' context_argv
        let scope = derive_scope(&paths, &surface);

        // Load work queue from verification ledger
        let work = build_work_queue(
            &paths,
            summary,
            surface.as_ref(),
            show_all[Tab::Work.index()],
        )?;

        // Load LM log
        let mut log = load_lm_log(&paths).unwrap_or_default();
        log.reverse(); // Newest first
        if !show_all[Tab::Log.index()] {
            log.truncate(PREVIEW_LIMIT);
        }

        // Build browse tree
        let browse = build_browse_tree(&paths)?;

        // Find man page
        let man_page_path = resolve_man_page_path(&paths, summary.binary_name.as_deref());

        Ok(Self {
            work,
            log,
            browse,
            man_page_path,
            scope,
        })
    }
}

/// Derive scope from prereqs.json surface_map keys mapped to surface items.
///
/// The prereqs.json reflects what was actually processed in a scoped run,
/// so we use its surface_map keys to find the common context_argv.
fn derive_scope(paths: &DocPackPaths, surface: &Option<SurfaceInventory>) -> Option<String> {
    let surface = surface.as_ref()?;
    if surface.items.is_empty() {
        return None;
    }

    // Try to load prereqs.json to get the scoped surface IDs
    let prereqs_path = paths.prereqs_path();
    let scoped_ids: Option<std::collections::HashSet<String>> = if prereqs_path.is_file() {
        fs::read_to_string(&prereqs_path)
            .ok()
            .and_then(|content| serde_json::from_str::<serde_json::Value>(&content).ok())
            .and_then(|v| v.get("surface_map").cloned())
            .and_then(|sm| sm.as_object().map(|o| o.keys().cloned().collect()))
    } else {
        None
    };

    // Filter surface items to only those in the scoped set (if available)
    let items_to_check: Vec<&crate::surface::SurfaceItem> = if let Some(ids) = &scoped_ids {
        surface
            .items
            .iter()
            .filter(|item| ids.contains(&item.id))
            .collect()
    } else {
        surface.items.iter().collect()
    };

    if items_to_check.is_empty() {
        return None;
    }

    // Find common context_argv prefix
    let first_context = items_to_check.first()?.context_argv.as_slice();
    if first_context.is_empty() {
        return None;
    }

    // Check if all items share this context_argv
    let all_same = items_to_check
        .iter()
        .all(|item| item.context_argv == first_context);

    if all_same {
        Some(first_context.join(" "))
    } else {
        // Mixed contexts - find common prefix
        let mut common: Vec<&str> = first_context.iter().map(|s| s.as_str()).collect();
        for item in &items_to_check {
            let ctx: Vec<&str> = item.context_argv.iter().map(|s| s.as_str()).collect();
            let mut new_common = Vec::new();
            for (a, b) in common.iter().zip(ctx.iter()) {
                if a == b {
                    new_common.push(*a);
                } else {
                    break;
                }
            }
            common = new_common;
            if common.is_empty() {
                break;
            }
        }
        if common.is_empty() {
            None
        } else {
            Some(common.join(" "))
        }
    }
}

/// Load state for the inspector.
pub(super) fn load_state(
    doc_pack_root: &Path,
    show_all: &[bool; 3],
) -> Result<(enrich::StatusSummary, InspectData)> {
    let computation =
        workflow::status_summary_for_doc_pack(doc_pack_root.to_path_buf(), false, false)?;
    let summary = computation.summary;
    let data = InspectData::load(doc_pack_root, &summary, show_all)?;
    Ok((summary, data))
}

/// Build work queue from verification ledger.
fn build_work_queue(
    paths: &DocPackPaths,
    summary: &enrich::StatusSummary,
    surface: Option<&SurfaceInventory>,
    show_all: bool,
) -> Result<WorkQueue> {
    let mut needs_scenario = Vec::new();
    let mut needs_fix = Vec::new();
    let mut excluded = Vec::new();

    // Build surface lookup
    let surface_map: std::collections::HashMap<&str, &crate::surface::SurfaceItem> = surface
        .map(|s| {
            s.items
                .iter()
                .map(|item| (item.id.as_str(), item))
                .collect()
        })
        .unwrap_or_default();

    // Load verification ledger
    let binary_name = summary.binary_name.as_deref().unwrap_or("<binary>");
    let scenarios_path = paths.scenarios_plan_path();
    let template_path = paths
        .root()
        .join(enrich::VERIFICATION_FROM_SCENARIOS_TEMPLATE_REL);

    let ledger = if scenarios_path.is_file() && template_path.is_file() {
        let surface_inventory = surface.cloned().unwrap_or_else(|| SurfaceInventory {
            schema_version: 1,
            generated_at_epoch_ms: 0,
            binary_name: Some(binary_name.to_string()),
            inputs_hash: None,
            discovery: Vec::new(),
            items: Vec::new(),
            blockers: Vec::new(),
        });
        scenarios::build_verification_ledger(
            binary_name,
            &surface_inventory,
            paths.root(),
            &scenarios_path,
            &template_path,
            None,
            Some(paths.root()),
        )
        .ok()
    } else {
        None
    };

    let mut verified = Vec::new();

    if let Some(ledger) = ledger {
        for entry in &ledger.entries {
            let surface_item = surface_map.get(entry.surface_id.as_str());
            let reason_code = entry
                .behavior_unverified_reason_code
                .as_deref()
                .unwrap_or("")
                .to_string();

            let category = if entry.behavior_status == "verified" {
                WorkCategory::Verified
            } else if entry.behavior_status == "excluded" {
                WorkCategory::Excluded
            } else if reason_code == "no_scenario" {
                WorkCategory::NeedsScenario
            } else {
                WorkCategory::NeedsFix
            };

            let work_item = WorkItem {
                surface_id: entry.surface_id.clone(),
                category,
                reason_code,
                description: surface_item.and_then(|s| s.description.clone()),
                forms: surface_item.map(|s| s.forms.clone()).unwrap_or_default(),
                exit_code: entry.auto_verify_exit_code,
                stderr_preview: entry.auto_verify_stderr.as_ref().map(|s| preview_text(s)),
                suggested_prereq: None, // TODO: prereq lookup
                scenario_id: entry.behavior_unverified_scenario_id.clone(),
            };

            match category {
                WorkCategory::NeedsScenario => needs_scenario.push(work_item),
                WorkCategory::NeedsFix => needs_fix.push(work_item),
                WorkCategory::Excluded => excluded.push(work_item),
                WorkCategory::Verified => verified.push(work_item),
            }
        }
    }

    // Apply limits
    let limit = if show_all { usize::MAX } else { PREVIEW_LIMIT };
    needs_scenario.truncate(limit);
    needs_fix.truncate(limit);
    excluded.truncate(limit);
    verified.truncate(limit);

    Ok(WorkQueue {
        needs_scenario,
        needs_fix,
        excluded,
        verified,
    })
}

/// Build browse tree from doc pack structure.
fn build_browse_tree(paths: &DocPackPaths) -> Result<Vec<BrowseEntry>> {
    let mut entries = Vec::new();
    let root = paths.root();

    // Key directories to show (in order)
    let dirs = ["enrich", "inventory", "scenarios", "man", "queries"];

    for dir_name in dirs {
        let dir_path = root.join(dir_name);
        if dir_path.is_dir() {
            // Add the top-level directory
            entries.push(BrowseEntry {
                rel_path: dir_name.to_string(),
                path: dir_path.clone(),
                is_dir: true,
                depth: 0,
            });

            // Read directory contents - show all files
            if let Ok(read_dir) = fs::read_dir(&dir_path) {
                let mut children: Vec<_> = read_dir.filter_map(Result::ok).collect();
                children.sort_by_key(|e| e.file_name());

                for child in children {
                    let child_path = child.path();
                    let is_dir = child_path.is_dir();
                    let rel_path = child_path
                        .strip_prefix(root)
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|_| child_path.display().to_string());

                    entries.push(BrowseEntry {
                        rel_path,
                        path: child_path.clone(),
                        is_dir,
                        depth: 1,
                    });

                    // One level of nesting for subdirectories
                    if is_dir {
                        if let Ok(subdir) = fs::read_dir(&child_path) {
                            let mut subchildren: Vec<_> = subdir.filter_map(Result::ok).collect();
                            subchildren.sort_by_key(|e| e.file_name());

                            for subchild in subchildren {
                                let subpath = subchild.path();
                                let sub_rel = subpath
                                    .strip_prefix(root)
                                    .map(|p| p.display().to_string())
                                    .unwrap_or_else(|_| subpath.display().to_string());

                                entries.push(BrowseEntry {
                                    rel_path: sub_rel,
                                    path: subpath.clone(),
                                    is_dir: subpath.is_dir(),
                                    depth: 2,
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(entries)
}

/// Find man page path.
fn resolve_man_page_path(paths: &DocPackPaths, binary_name: Option<&str>) -> Option<PathBuf> {
    if let Some(name) = binary_name {
        let path = paths.man_page_path(name);
        if path.is_file() {
            return Some(path);
        }
    }
    let man_dir = paths.man_dir();
    let entries = fs::read_dir(&man_dir).ok()?;
    let mut man_pages: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.eq_ignore_ascii_case("1"))
                .unwrap_or(false)
        })
        .collect();
    man_pages.sort();
    if man_pages.len() == 1 {
        return Some(man_pages.remove(0));
    }
    None
}
