use super::super::data::{ArtifactEntry, EvidenceCounts, EvidenceEntry};
use super::super::format::{gate_label, next_action_summary, truncate_text};
use super::super::{EvidenceFilter, Tab};
use super::App;
use crate::enrich;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Tabs, Wrap};
use ratatui::Frame;
use std::cmp::min;

impl App {
    pub(in crate::inspect) fn draw(&mut self, frame: &mut Frame) {
        let area = frame.size();
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),
                Constraint::Length(1),
                Constraint::Min(2),
                Constraint::Length(1),
            ])
            .split(area);

        self.draw_header(frame, layout[0]);
        self.draw_tabs(frame, layout[1]);
        self.draw_main(frame, layout[2]);
        self.draw_footer(frame, layout[3]);

        if self.show_help {
            self.draw_help(frame);
        }
    }

    fn draw_header(&self, frame: &mut Frame, area: Rect) {
        let binary = self
            .summary
            .binary_name
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        let lock_label = gate_label(self.summary.lock.present, self.summary.lock.stale);
        let plan_label = gate_label(self.summary.plan.present, self.summary.plan.stale);
        let decision_label = self.summary.decision.as_str();
        let next_action = next_action_summary(&self.summary.next_action);
        let next_action = truncate_text(&next_action, area.width.saturating_sub(12) as usize);

        let reserved = "Doc pack: ".len() + " | Binary: ".len() + binary.len();
        let max_doc_pack_width = (area.width as usize).saturating_sub(reserved);
        let doc_pack_path = truncate_text(
            &self.doc_pack_root.display().to_string(),
            max_doc_pack_width,
        );

        let line1 = Line::from(vec![
            Span::raw("Doc pack: "),
            Span::styled(doc_pack_path, Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" | Binary: "),
            Span::styled(binary, Style::default().add_modifier(Modifier::BOLD)),
        ]);

        let line2 = Line::from(vec![
            Span::raw("Lock: "),
            Span::styled(
                lock_label,
                gate_style(self.summary.lock.present, self.summary.lock.stale),
            ),
            Span::raw(" | Plan: "),
            Span::styled(
                plan_label,
                gate_style(self.summary.plan.present, self.summary.plan.stale),
            ),
            Span::raw(" | Decision: "),
            Span::styled(decision_label, decision_style(&self.summary.decision)),
            Span::raw(" | Next: "),
            Span::raw(next_action),
        ]);

        let paragraph = Paragraph::new(vec![line1, line2]).wrap(Wrap { trim: true });
        frame.render_widget(paragraph, area);
    }

    fn draw_tabs(&self, frame: &mut Frame, area: Rect) {
        let titles = Tab::ALL.iter().map(|tab| {
            Span::styled(
                tab.label(),
                Style::default()
                    .fg(tab_color(*tab))
                    .add_modifier(Modifier::BOLD),
            )
        });
        let tabs = Tabs::new(titles)
            .select(self.tab.index())
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        frame.render_widget(tabs, area);
    }

    fn draw_main(&mut self, frame: &mut Frame, area: Rect) {
        match self.tab {
            Tab::Intent => self.draw_intent(frame, area),
            Tab::Evidence => self.draw_evidence(frame, area),
            Tab::Outputs => self.draw_outputs(frame, area),
            Tab::History => self.draw_history(frame, area),
        }
    }

    fn draw_intent(&mut self, frame: &mut Frame, area: Rect) {
        let total = self.data.intent.len();
        let visible = self.visible_items_len(self.tab);
        let title = list_title("Intent", total, visible, self.show_all[self.tab.index()]);
        let items = self
            .data
            .intent
            .iter()
            .take(visible)
            .map(artifact_list_item)
            .collect::<Vec<_>>();
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(title))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        let mut state = ListState::default();
        state.select(Some(self.selection[self.tab.index()]));
        frame.render_stateful_widget(list, area, &mut state);
    }

    fn draw_evidence(&mut self, frame: &mut Frame, area: Rect) {
        let counts = self.data.evidence.counts;
        let total = self.data.evidence.total_count;
        let visible = self.visible_items_len(self.tab);
        let title = list_title("Evidence", total, visible, self.show_all[self.tab.index()]);

        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(2)])
            .split(area);

        let filter_line = evidence_filter_line(counts, self.evidence_filter);
        let filter_paragraph = Paragraph::new(vec![filter_line]).wrap(Wrap { trim: true });
        frame.render_widget(filter_paragraph, layout[0]);

        let items = self
            .data
            .evidence
            .entries
            .iter()
            .take(visible)
            .map(evidence_list_item)
            .collect::<Vec<_>>();
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(title))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        let mut state = ListState::default();
        state.select(Some(self.selection[self.tab.index()]));
        frame.render_stateful_widget(list, layout[1], &mut state);
    }

    fn draw_outputs(&mut self, frame: &mut Frame, area: Rect) {
        let verification_lines = self.verification_lines();
        let verification_height = verification_lines.len().saturating_add(2) as u16;
        let has_warnings = !self.data.man_warnings.is_empty();
        let mut constraints = vec![Constraint::Length(verification_height)];
        if has_warnings {
            constraints.push(Constraint::Length(min(
                self.data.man_warnings.len() as u16 + 1,
                4,
            )));
        }
        constraints.push(Constraint::Min(2));
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(area);

        let verification = Paragraph::new(verification_lines)
            .block(Block::default().borders(Borders::ALL).title("Verification"))
            .wrap(Wrap { trim: true });
        frame.render_widget(verification, layout[0]);

        let mut offset = 1;
        if has_warnings {
            let warnings = self
                .data
                .man_warnings
                .iter()
                .take(3)
                .map(|warning| Line::from(format!("warning: {warning}")))
                .collect::<Vec<_>>();
            let paragraph = Paragraph::new(warnings)
                .block(Block::default().borders(Borders::ALL).title("Man warnings"));
            frame.render_widget(paragraph, layout[offset]);
            offset += 1;
        }

        let total = self.data.outputs.len();
        let visible = self.visible_items_len(self.tab);
        let title = list_title("Outputs", total, visible, self.show_all[self.tab.index()]);
        let items = self
            .data
            .outputs
            .iter()
            .take(visible)
            .map(artifact_list_item)
            .collect::<Vec<_>>();
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(title))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        let mut state = ListState::default();
        state.select(Some(self.selection[self.tab.index()]));
        frame.render_stateful_widget(list, layout[offset], &mut state);
    }

    fn verification_lines(&self) -> Vec<Line<'static>> {
        let Some(req) = self
            .summary
            .requirements
            .iter()
            .find(|req| req.id == enrich::RequirementId::Verification)
        else {
            return vec![Line::from("Verification: not required")];
        };

        let mut lines = Vec::new();
        let status = req.status.as_str();
        let tier = req.verification_tier.as_deref().unwrap_or("accepted");
        let tier_label = if tier == "behavior" {
            "behavior"
        } else {
            "existence"
        };
        lines.push(Line::from(format!(
            "Required tier: {tier} ({tier_label}) | Status: {status}"
        )));
        if let (Some(verified), Some(unverified)) =
            (req.accepted_verified_count, req.accepted_unverified_count)
        {
            let total = verified.saturating_add(unverified);
            lines.push(Line::from(format!(
                "Existence (accepted): {verified}/{total}"
            )));
        }
        if let (Some(verified), Some(unverified)) =
            (req.behavior_verified_count, req.behavior_unverified_count)
        {
            let total = verified.saturating_add(unverified);
            lines.push(Line::from(format!("Behavior: {verified}/{total}")));
        }
        if let Some(summary) = req.verification.as_ref() {
            if !summary.remaining_by_kind.is_empty() {
                let total: usize = summary
                    .remaining_by_kind
                    .iter()
                    .map(|group| group.target_count)
                    .sum();
                let remaining: usize = summary
                    .remaining_by_kind
                    .iter()
                    .map(|group| group.remaining_count)
                    .sum();
                lines.push(Line::from(format!(
                    "Remaining ({tier_label}): {remaining}/{total}"
                )));
                for group in &summary.remaining_by_kind {
                    if req.status == enrich::RequirementState::Met {
                        let group_verified =
                            group.target_count.saturating_sub(group.remaining_count);
                        lines.push(Line::from(format!(
                            "{}: {group_verified}/{}",
                            group.kind, group.target_count
                        )));
                    } else {
                        let preview = preview_list(&group.remaining_preview);
                        lines.push(Line::from(format!(
                            "{}: remaining {} ({preview})",
                            group.kind, group.remaining_count
                        )));
                    }
                }
                if summary.behavior_excluded_count > 0 {
                    let preview = preview_list(&summary.behavior_excluded_preview);
                    lines.push(Line::from(format!(
                        "Excluded (behavior): {} ({preview})",
                        summary.behavior_excluded_count
                    )));
                }
            } else if req.status == enrich::RequirementState::Met {
                lines.push(Line::from("Verification: met".to_string()));
            } else {
                let remaining = summary.triaged_unverified_count;
                let preview = preview_list(&summary.triaged_unverified_preview);
                lines.push(Line::from(format!("Remaining ({tier_label}): {remaining}")));
                if remaining > 0 && !preview.is_empty() {
                    lines.push(Line::from(format!("Remaining: {preview}")));
                }
                if summary.behavior_excluded_count > 0 {
                    let excluded_preview = preview_list(&summary.behavior_excluded_preview);
                    lines.push(Line::from(format!(
                        "Excluded (behavior): {} ({excluded_preview})",
                        summary.behavior_excluded_count
                    )));
                }
            }
            if !summary.stub_blockers_preview.is_empty() {
                lines.push(Line::from(format!(
                    "Stub blockers preview: {}",
                    summary.stub_blockers_preview.len()
                )));
                for blocker in &summary.stub_blockers_preview {
                    let placeholder = blocker
                        .surface
                        .value_placeholder
                        .as_deref()
                        .unwrap_or("none");
                    lines.push(Line::from(format!(
                        "{} [{}]",
                        blocker.surface_id, blocker.reason_code
                    )));
                    lines.push(Line::from(format!(
                        "shape: kind={} forms={} arity={} sep={} placeholder={}",
                        blocker.surface.kind,
                        preview_list(&blocker.surface.forms),
                        blocker.surface.value_arity,
                        blocker.surface.value_separator,
                        placeholder
                    )));
                    lines.push(Line::from(format!(
                        "invocation: requires_argv={} value_examples={}",
                        preview_list(&blocker.surface.requires_argv),
                        preview_list(&blocker.surface.value_examples_preview)
                    )));
                    lines.push(Line::from(format!(
                        "delta: outcome={} paths={}",
                        blocker.delta.delta_outcome.as_deref().unwrap_or("unknown"),
                        preview_list(&blocker.delta.delta_evidence_paths)
                    )));
                    let evidence_paths = blocker
                        .evidence
                        .iter()
                        .map(|entry| entry.path.clone())
                        .take(2)
                        .collect::<Vec<_>>();
                    if !evidence_paths.is_empty() {
                        lines.push(Line::from(format!(
                            "evidence: {}",
                            evidence_paths.join(", ")
                        )));
                    }
                }
            }
        }

        if let Some(policy) = self.data.verification_policy.as_ref() {
            let kinds = if policy.kinds.is_empty() {
                "none".to_string()
            } else {
                policy.kinds.join(", ")
            };
            lines.push(Line::from(format!(
                "Batch: {} | Kinds: {} | Queue excludes: {}",
                policy.max_new_runs_per_apply, kinds, policy.excludes_count
            )));
        }

        let paths = ["scenarios/plan.json", "enrich/semantics.json"];
        lines.push(Line::from(format!("Open: {}", paths.join(" | "))));

        lines
    }

    fn draw_history(&mut self, frame: &mut Frame, area: Rect) {
        let mut sections = Vec::new();
        if self.data.last_history.is_some() || self.data.last_txn_id.is_some() {
            sections.push(Constraint::Length(4));
        }
        sections.push(Constraint::Min(2));
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints(sections)
            .split(area);

        if self.data.last_history.is_some() || self.data.last_txn_id.is_some() {
            let mut lines = Vec::new();
            if let Some(entry) = self.data.last_history.as_ref() {
                lines.push(Line::from(format!(
                    "last history: step={} success={} force_used={}",
                    entry.step, entry.success, entry.force_used
                )));
            }
            if let Some(txn_id) = self.data.last_txn_id.as_ref() {
                lines.push(Line::from(format!("last txn: {txn_id}")));
            }
            let paragraph = Paragraph::new(lines)
                .block(Block::default().borders(Borders::ALL).title("Last run"));
            frame.render_widget(paragraph, layout[0]);
        }

        let total = self.data.history.len();
        let visible = self.visible_items_len(self.tab);
        let title = list_title(
            "History/Audit",
            total,
            visible,
            self.show_all[self.tab.index()],
        );
        let items = self
            .data
            .history
            .iter()
            .take(visible)
            .map(artifact_list_item)
            .collect::<Vec<_>>();
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(title))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        let mut state = ListState::default();
        state.select(Some(self.selection[self.tab.index()]));
        let list_area = if layout.len() == 1 {
            layout[0]
        } else {
            layout[1]
        };
        frame.render_stateful_widget(list, list_area, &mut state);
    }

    fn draw_footer(&self, frame: &mut Frame, area: Rect) {
        let message = self.message.clone().unwrap_or_else(|| {
            "q quit | tab switch | enter view | o edit | m man | c copy | r refresh | f filter | ? help"
                .to_string()
        });
        let message = truncate_text(&message, area.width as usize);
        let paragraph =
            Paragraph::new(message).style(Style::default().add_modifier(Modifier::REVERSED));
        frame.render_widget(paragraph, area);
    }

    fn draw_help(&self, frame: &mut Frame) {
        let area = centered_rect(70, 70, frame.size());
        let lines = vec![
            Line::from("Keys:"),
            Line::from("  q / Esc: quit"),
            Line::from("  Tab: next tab"),
            Line::from("  Shift+Tab: previous tab"),
            Line::from("  Up/Down: move selection"),
            Line::from("  Enter: open in pager"),
            Line::from("  o: open in editor"),
            Line::from("  m: open man page"),
            Line::from("  c: copy next action command or selected path"),
            Line::from("  r: refresh"),
            Line::from("  a: toggle show all"),
            Line::from("  f: cycle evidence filter"),
            Line::from("  ?: toggle help"),
            Line::from(""),
            Line::from("Non-goals:"),
            Line::from("  no in-TUI editing, no validate/plan/apply, no embedded pager"),
        ];
        let paragraph = Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title("Help"))
            .wrap(Wrap { trim: true });
        frame.render_widget(Clear, area);
        frame.render_widget(paragraph, area);
    }
}

fn list_title(label: &str, total: usize, visible: usize, show_all: bool) -> String {
    if show_all || total <= visible {
        format!("{label} ({total})")
    } else {
        format!("{label} (showing {visible} of {total}, press 'a' to show all)")
    }
}

fn evidence_filter_line(counts: EvidenceCounts, selected: EvidenceFilter) -> Line<'static> {
    let mut spans = vec![Span::raw("Filter: ")];
    for (idx, filter) in EvidenceFilter::DISPLAY.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::raw(" "));
        }
        spans.push(evidence_filter_span(*filter, counts, selected));
    }
    spans.push(Span::raw(" | f to cycle"));
    Line::from(spans)
}

fn evidence_filter_span(
    filter: EvidenceFilter,
    counts: EvidenceCounts,
    selected: EvidenceFilter,
) -> Span<'static> {
    let count = counts.count_for(filter);
    let marker = if filter == selected { "*" } else { "" };
    let label = format!("[{} {count}]{marker}", filter.label());
    let style = if filter == selected {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    Span::styled(label, style)
}

fn preview_list(preview: &[String]) -> String {
    if preview.is_empty() {
        "none".to_string()
    } else {
        preview.join(", ")
    }
}

fn artifact_list_item(entry: &ArtifactEntry) -> ListItem<'static> {
    let status = if entry.exists { "present" } else { "missing" };
    let text = format!("{} ({status})", entry.rel_path);
    ListItem::new(Line::from(text))
}

fn evidence_list_item(entry: &EvidenceEntry) -> ListItem<'static> {
    let status = if let Some(err) = entry.error.as_ref() {
        format!("error: {err}")
    } else if !entry.exists {
        "no evidence".to_string()
    } else {
        let exit = entry
            .exit_code
            .map_or("?".to_string(), |code| code.to_string());
        let signal = entry
            .exit_signal
            .map_or("-".to_string(), |sig| sig.to_string());
        let timeout = entry.timed_out.unwrap_or(false);
        format!("exit={} signal={} timed_out={}", exit, signal, timeout)
    };
    let stdout = entry
        .stdout_preview
        .clone()
        .unwrap_or_else(|| "<empty>".to_string());
    let stderr = entry
        .stderr_preview
        .clone()
        .unwrap_or_else(|| "<empty>".to_string());
    let mut lines = Vec::new();
    let label = format!("{} | {}", entry.scenario_id, status);
    lines.push(Line::from(label));
    lines.push(Line::from(format!("stdout: {stdout}")));
    lines.push(Line::from(format!("stderr: {stderr}")));
    ListItem::new(lines)
}

fn gate_style(present: bool, stale: bool) -> Style {
    if !present {
        Style::default().fg(Color::Red)
    } else if stale {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::Green)
    }
}

fn decision_style(decision: &enrich::Decision) -> Style {
    match decision {
        enrich::Decision::Complete => Style::default().fg(Color::Green),
        enrich::Decision::Incomplete => Style::default().fg(Color::Yellow),
        enrich::Decision::Blocked => Style::default().fg(Color::Red),
    }
}

fn tab_color(tab: Tab) -> Color {
    match tab {
        Tab::Intent => Color::Cyan,
        Tab::Evidence => Color::Yellow,
        Tab::Outputs => Color::Green,
        Tab::History => Color::Magenta,
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}
