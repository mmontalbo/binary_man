//! View rendering for the inspector TUI.
//!
//! Implements the three main tabs:
//! - Work: Items needing attention (grouped by category)
//! - Log: LM invocation history
//! - Browse: File tree

use super::super::data::{BrowseEntry, WorkItem};
use super::super::format::{next_action_summary, truncate_text};
use super::super::{Tab, WorkCategory};
use super::App;
use crate::enrich::{self, LmLogEntry, LmOutcome};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Tabs, Wrap};
use ratatui::Frame;

// =============================================================================
// Color Theme - Semantic colors for content
// =============================================================================
//
// OUTCOME COLORS (status indicators):
//   Green   = Success, Complete, Verified
//   Yellow  = Partial, Pending, Warning
//   Red     = Failed, Error, Blocked
//
// CONTENT COLORS (data types):
//   Cyan       = LM Call Type (PrereqInference, Behavior, BehaviorRetry)
//   Blue       = Prompt content (what we sent to the LM)
//   Magenta    = Response content (what the LM returned)
//   LightYellow = Surface IDs (options like --flag, subcommands like config)
//
// UI CHROME (non-content):
//   White    = Active tab, hotkeys
//   DarkGray = Inactive tabs, muted/secondary text
//
mod theme {
    use ratatui::style::{Color, Modifier, Style};

    // Outcome/Status colors
    pub const SUCCESS: Color = Color::Green;
    pub const PARTIAL: Color = Color::Yellow;
    pub const FAILED: Color = Color::Red;

    // Content type colors
    pub const LM_KIND: Color = Color::Cyan;
    pub const PROMPT: Color = Color::Blue;
    pub const RESPONSE: Color = Color::Magenta;
    pub const SURFACE_ID: Color = Color::LightYellow;

    // UI chrome
    pub const TAB_ACTIVE: Color = Color::White;
    pub const TAB_INACTIVE: Color = Color::DarkGray;
    pub const MUTED: Color = Color::DarkGray;
    pub const HOTKEY: Color = Color::White;

    pub fn hotkey_style() -> Style {
        Style::default()
            .fg(HOTKEY)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
    }
}

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

        if self.detail_view {
            self.draw_detail(frame, layout[2]);
        } else {
            self.draw_main(frame, layout[2]);
        }

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
        let decision_label = self.summary.decision.as_str();
        let next_action = next_action_summary(&self.summary.next_action);
        let next_action = truncate_text(&next_action, area.width.saturating_sub(12) as usize);

        // Build "Binary: git" or "Binary: git config" with scope
        let binary_display = if let Some(scope) = &self.data.scope {
            format!("{binary} {scope}")
        } else {
            binary
        };

        let reserved = "Doc pack: ".len() + " | Scope: ".len() + binary_display.len();
        let max_doc_pack_width = (area.width as usize).saturating_sub(reserved);
        let doc_pack_path = truncate_text(
            &self.doc_pack_root.display().to_string(),
            max_doc_pack_width,
        );

        let line1 = Line::from(vec![
            Span::raw("Doc pack: "),
            Span::styled(doc_pack_path, Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" | Scope: "),
            Span::styled(
                binary_display,
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]);

        let unverified_count = self.data.work.unverified_count();
        let total_count = self.data.work.total_count();
        let log_count = self.data.log.len();

        let line2 = Line::from(vec![
            Span::raw("Decision: "),
            Span::styled(decision_label, decision_style(&self.summary.decision)),
            Span::raw(" | Surfaces: "),
            Span::styled(
                format!("{}/{}", total_count - unverified_count, total_count),
                if unverified_count > 0 {
                    Style::default().fg(theme::PARTIAL)
                } else {
                    Style::default().fg(theme::SUCCESS)
                },
            ),
            Span::raw(" verified | LM calls: "),
            Span::raw(log_count.to_string()),
            Span::raw(" | Next: "),
            Span::raw(next_action),
        ]);

        let paragraph = Paragraph::new(vec![line1, line2]).wrap(Wrap { trim: true });
        frame.render_widget(paragraph, area);
    }

    fn draw_tabs(&self, frame: &mut Frame, area: Rect) {
        let titles = Tab::ALL.iter().map(|tab| {
            let color = if *tab == self.tab {
                theme::TAB_ACTIVE
            } else {
                theme::TAB_INACTIVE
            };
            Span::styled(
                format!(" {} ", tab.label()),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            )
        });
        let tabs = Tabs::new(titles)
            .select(self.tab.index())
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        frame.render_widget(tabs, area);
    }

    fn draw_main(&mut self, frame: &mut Frame, area: Rect) {
        match self.tab {
            Tab::Work => self.draw_work(frame, area),
            Tab::Log => self.draw_log(frame, area),
            Tab::Browse => self.draw_browse(frame, area),
        }
    }

    fn draw_work(&mut self, frame: &mut Frame, area: Rect) {
        let flat_items = self.data.work.flat_items();
        let total = flat_items.len();
        let visible = self.visible_items_len(self.tab);
        let title = list_title(
            "Work Queue",
            total,
            visible,
            self.show_all[self.tab.index()],
        );

        let items: Vec<ListItem> = flat_items
            .iter()
            .take(visible)
            .map(|(category, work_item)| work_list_item(*category, *work_item))
            .collect();

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(title))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        let mut state = ListState::default();
        state.select(Some(self.selection[self.tab.index()]));
        frame.render_stateful_widget(list, area, &mut state);
    }

    fn draw_log(&mut self, frame: &mut Frame, area: Rect) {
        // Split area: list on top, legend at bottom
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(5), Constraint::Length(2)])
            .split(area);

        let total = self.data.log.len();
        let visible = self.visible_items_len(self.tab);
        let title = list_title("LM Log", total, visible, self.show_all[self.tab.index()]);

        let items: Vec<ListItem> = self
            .data
            .log
            .iter()
            .take(visible)
            .map(log_list_item)
            .collect();

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(title))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        let mut state = ListState::default();
        state.select(Some(self.selection[self.tab.index()]));
        frame.render_stateful_widget(list, chunks[0], &mut state);

        // Color legend
        let legend = Line::from(vec![
            Span::styled(" ● ", Style::default().fg(theme::LM_KIND)),
            Span::styled("LM Call Type ", Style::default().fg(theme::MUTED)),
            Span::styled("● ", Style::default().fg(theme::SUCCESS)),
            Span::styled("Success ", Style::default().fg(theme::MUTED)),
            Span::styled("● ", Style::default().fg(theme::PARTIAL)),
            Span::styled("Partial ", Style::default().fg(theme::MUTED)),
            Span::styled("● ", Style::default().fg(theme::FAILED)),
            Span::styled("Failed ", Style::default().fg(theme::MUTED)),
            Span::styled("● ", Style::default().fg(theme::SURFACE_ID)),
            Span::styled("Surfaces ", Style::default().fg(theme::MUTED)),
            Span::styled("● ", Style::default().fg(theme::PROMPT)),
            Span::styled("Prompt ", Style::default().fg(theme::MUTED)),
            Span::styled("● ", Style::default().fg(theme::RESPONSE)),
            Span::styled("Response", Style::default().fg(theme::MUTED)),
        ]);
        let legend_widget = Paragraph::new(legend);
        frame.render_widget(legend_widget, chunks[1]);
    }

    fn draw_browse(&mut self, frame: &mut Frame, area: Rect) {
        // Split: file list on left (30%), preview on right (70%)
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
            .split(area);

        // File list - show focus state
        let list_focused = !self.browse_preview_focus;
        let list_border = if list_focused {
            Style::default().fg(theme::TAB_ACTIVE)
        } else {
            Style::default().fg(theme::MUTED)
        };

        let total = self.data.browse.len();
        let visible = self.visible_items_len(self.tab);
        let title = list_title("Files", total, visible, self.show_all[self.tab.index()]);

        let items: Vec<ListItem> = self
            .data
            .browse
            .iter()
            .take(visible)
            .map(browse_list_item)
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(list_border)
                    .title(title),
            )
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        let mut state = ListState::default();
        state.select(Some(self.selection[self.tab.index()]));
        frame.render_stateful_widget(list, chunks[0], &mut state);

        // Preview pane for selected item
        self.draw_browse_preview(frame, chunks[1]);
    }

    fn draw_browse_preview(&self, frame: &mut Frame, area: Rect) {
        let focused = self.browse_preview_focus;
        let border_style = if focused {
            Style::default().fg(theme::TAB_ACTIVE)
        } else {
            Style::default().fg(theme::MUTED)
        };

        let Some(entry) = self.selected_browse_entry() else {
            let paragraph = Paragraph::new("No file selected").block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(border_style)
                    .title("Preview"),
            );
            frame.render_widget(paragraph, area);
            return;
        };

        if entry.is_dir {
            // For directories, list contents
            let mut lines = vec![];
            if let Ok(entries) = std::fs::read_dir(&entry.path) {
                let mut files: Vec<_> = entries.filter_map(|e| e.ok()).collect();
                files.sort_by_key(|e| e.file_name());
                for file_entry in files.iter() {
                    let name = file_entry.file_name().to_string_lossy().to_string();
                    let is_dir = file_entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                    lines.push(Line::from(format!(
                        "{}{}",
                        name,
                        if is_dir { "/" } else { "" }
                    )));
                }
            }

            let title = format!("{}/", entry.rel_path);
            let paragraph = Paragraph::new(lines)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(border_style)
                        .title(title),
                )
                .scroll((self.detail_scroll, 0));
            frame.render_widget(paragraph, area);
        } else {
            // For files, show content
            let content = std::fs::read_to_string(&entry.path)
                .unwrap_or_else(|_| "<binary or unreadable>".to_string());

            let lines: Vec<Line> = content
                .lines()
                .enumerate()
                .map(|(i, line)| {
                    Line::from(vec![
                        Span::styled(format!("{:4} ", i + 1), Style::default().fg(theme::MUTED)),
                        Span::raw(line.to_string()),
                    ])
                })
                .collect();

            let title = if focused {
                format!("{} (↑↓ scroll, ← back)", entry.rel_path)
            } else {
                format!("{} (→ to focus)", entry.rel_path)
            };
            let paragraph = Paragraph::new(lines)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(border_style)
                        .title(title),
                )
                .scroll((self.detail_scroll, 0));
            frame.render_widget(paragraph, area);
        }
    }

    fn draw_detail(&mut self, frame: &mut Frame, area: Rect) {
        match self.tab {
            Tab::Work => self.draw_work_detail(frame, area),
            Tab::Log => self.draw_log_detail(frame, area),
            Tab::Browse => self.draw_browse(frame, area), // Browse uses split view, not detail
        }
    }

    fn draw_work_detail(&self, frame: &mut Frame, area: Rect) {
        let Some(item) = self.selected_work_item() else {
            let paragraph = Paragraph::new("No item selected")
                .block(Block::default().borders(Borders::ALL).title("Detail"));
            frame.render_widget(paragraph, area);
            return;
        };

        let mut lines = Vec::new();
        lines.push(Line::from(vec![
            Span::raw("Surface ID: "),
            Span::styled(
                &item.surface_id,
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]));

        lines.push(Line::from(format!("Category: {}", item.category.label())));
        lines.push(Line::from(format!("Reason: {}", item.reason_code)));

        if !item.forms.is_empty() {
            lines.push(Line::from(format!("Forms: {}", item.forms.join(", "))));
        }

        if let Some(desc) = &item.description {
            lines.push(Line::from(""));
            lines.push(Line::from("Description:"));
            lines.push(Line::from(desc.clone()));
        }

        if let Some(exit_code) = item.exit_code {
            lines.push(Line::from(""));
            lines.push(Line::from(format!("Last exit code: {}", exit_code)));
        }

        if let Some(stderr) = &item.stderr_preview {
            lines.push(Line::from(format!("Stderr: {}", stderr)));
        }

        if let Some(prereq) = &item.suggested_prereq {
            lines.push(Line::from(format!("Suggested prereq: {}", prereq)));
        }

        if let Some(scenario_id) = &item.scenario_id {
            lines.push(Line::from(format!("Scenario: {}", scenario_id)));
        }

        lines.push(Line::from(""));
        lines.push(Line::from("Press Esc to close"));

        let paragraph = Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Work Item Detail"),
            )
            .wrap(Wrap { trim: true });
        frame.render_widget(paragraph, area);
    }

    fn draw_log_detail(&self, frame: &mut Frame, area: Rect) {
        let Some(entry) = self.selected_log_entry() else {
            let paragraph = Paragraph::new("No entry selected")
                .block(Block::default().borders(Borders::ALL).title("Detail"));
            frame.render_widget(paragraph, area);
            return;
        };

        let mut lines = Vec::new();

        // Header info
        lines.push(Line::from(vec![
            Span::styled(
                format!("Cycle #{}", entry.cycle),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(" • "),
            Span::styled(
                format!("{:?}", entry.kind),
                Style::default().fg(theme::LM_KIND),
            ),
            Span::raw(" • "),
            Span::styled(
                format!("{:?}", entry.outcome),
                match entry.outcome {
                    LmOutcome::Success => Style::default().fg(theme::SUCCESS),
                    LmOutcome::Partial => Style::default().fg(theme::PARTIAL),
                    LmOutcome::Failed => Style::default().fg(theme::FAILED),
                },
            ),
            Span::raw(format!(
                " • {}ms • {} items",
                entry.duration_ms, entry.items_count
            )),
        ]));

        if let Some(error) = &entry.error {
            lines.push(Line::from(vec![
                Span::styled("Error: ", Style::default().fg(theme::FAILED)),
                Span::raw(error.clone()),
            ]));
        }

        // Load and show full prompt/response
        let prompt = self.load_selected_prompt();
        let response = self.load_selected_response();

        if prompt.is_some() || response.is_some() {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled(
                    "═══ PROMPT ",
                    Style::default()
                        .fg(theme::PROMPT)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("═".repeat(40), Style::default().fg(theme::MUTED)),
            ]));

            if let Some(prompt_text) = &prompt {
                for line in prompt_text.lines().take(30) {
                    lines.push(Line::from(line.to_string()));
                }
                let line_count = prompt_text.lines().count();
                if line_count > 30 {
                    lines.push(Line::from(Span::styled(
                        format!(
                            "... ({} more lines, press 'p' to view full)",
                            line_count - 30
                        ),
                        Style::default().fg(theme::MUTED),
                    )));
                }
            } else {
                lines.push(Line::from(Span::styled(
                    "(prompt not stored - run with --verbose)",
                    Style::default().fg(theme::MUTED),
                )));
            }

            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled(
                    "═══ RESPONSE ",
                    Style::default()
                        .fg(theme::RESPONSE)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("═".repeat(38), Style::default().fg(theme::MUTED)),
            ]));

            if let Some(response_text) = &response {
                for line in response_text.lines().take(30) {
                    lines.push(Line::from(line.to_string()));
                }
                let line_count = response_text.lines().count();
                if line_count > 30 {
                    lines.push(Line::from(Span::styled(
                        format!(
                            "... ({} more lines, press 'r' to view full)",
                            line_count - 30
                        ),
                        Style::default().fg(theme::MUTED),
                    )));
                }
            } else {
                lines.push(Line::from(Span::styled(
                    "(response not stored - run with --verbose)",
                    Style::default().fg(theme::MUTED),
                )));
            }
        } else {
            // Fallback: show items if no full content
            if !entry.items.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from("Items processed:"));
                for item_id in entry.items.iter().take(20) {
                    lines.push(Line::from(Span::styled(
                        format!("  {}", item_id),
                        Style::default().fg(theme::SURFACE_ID),
                    )));
                }
                if entry.items.len() > 20 {
                    lines.push(Line::from(format!(
                        "  ... and {} more",
                        entry.items.len() - 20
                    )));
                }
            }

            if let Some(preview) = &entry.prompt_preview {
                lines.push(Line::from(""));
                lines.push(Line::from("Prompt preview:"));
                for line in preview.lines().take(10) {
                    lines.push(Line::from(line.to_string()));
                }
            }

            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "(run bman with --verbose to store full prompt/response)",
                Style::default().fg(theme::MUTED),
            )));
        }

        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("p", theme::hotkey_style()),
            Span::raw(" prompt  "),
            Span::styled("r", theme::hotkey_style()),
            Span::raw(" response  "),
            Span::styled("←→", theme::hotkey_style()),
            Span::raw(" prev/next  "),
            Span::styled("↑↓", theme::hotkey_style()),
            Span::raw(" scroll  "),
            Span::styled("Esc", Style::default().fg(theme::MUTED)),
            Span::raw(" close"),
        ]));

        let paragraph = Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title("LM Exchange"))
            .wrap(Wrap { trim: true })
            .scroll((self.detail_scroll, 0));
        frame.render_widget(paragraph, area);
    }

    fn draw_footer(&self, frame: &mut Frame, area: Rect) {
        let message = self.message.clone().unwrap_or_else(|| {
            if self.detail_view {
                "Esc close | o edit | c copy".to_string()
            } else {
                "q quit | Tab switch | Enter detail | o edit | c copy | r refresh | ? help"
                    .to_string()
            }
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
            Line::from("  q / Esc: quit (or close detail view)"),
            Line::from("  Tab: next tab"),
            Line::from("  Shift+Tab: previous tab"),
            Line::from("  j/k or Up/Down: move selection"),
            Line::from("  Enter: open detail view"),
            Line::from("  o: open in editor (Browse tab)"),
            Line::from("  m: open man page"),
            Line::from("  c: copy next action command or selected item"),
            Line::from("  r: refresh (or view response in Log detail)"),
            Line::from("  p: view prompt (in Log detail view)"),
            Line::from("  a: toggle show all"),
            Line::from("  ?: toggle help"),
            Line::from(""),
            Line::from("Tabs:"),
            Line::from("  Work: Items needing attention"),
            Line::from("  Log: LM invocation history (p/r to view full transcript)"),
            Line::from("  Browse: File tree"),
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

fn work_list_item<'a>(
    category: Option<WorkCategory>,
    work_item: Option<&'a WorkItem>,
) -> ListItem<'a> {
    if let Some(cat) = category {
        // Category header
        let style = match cat {
            WorkCategory::NeedsScenario => Style::default()
                .fg(theme::PARTIAL)
                .add_modifier(Modifier::BOLD),
            WorkCategory::NeedsFix => Style::default()
                .fg(theme::FAILED)
                .add_modifier(Modifier::BOLD),
            WorkCategory::Excluded => Style::default()
                .fg(theme::MUTED)
                .add_modifier(Modifier::BOLD),
            WorkCategory::Verified => Style::default()
                .fg(theme::SUCCESS)
                .add_modifier(Modifier::BOLD),
        };
        return ListItem::new(Line::from(Span::styled(cat.label(), style)));
    }

    if let Some(item) = work_item {
        let desc_preview = item
            .description
            .as_deref()
            .map(|s| truncate_text(s, 40))
            .unwrap_or_else(|| "".to_string());

        // Color the surface_id based on category
        let id_color = match item.category {
            WorkCategory::Verified => theme::SUCCESS,
            WorkCategory::NeedsScenario => theme::PARTIAL,
            WorkCategory::NeedsFix => theme::FAILED,
            WorkCategory::Excluded => theme::MUTED,
        };

        let line = Line::from(vec![
            Span::raw("  "),
            Span::styled(
                &item.surface_id,
                Style::default().fg(id_color).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(
                format!("[{}]", item.reason_code),
                Style::default().fg(theme::MUTED),
            ),
            Span::raw(" "),
            Span::raw(desc_preview),
        ]);
        return ListItem::new(line);
    }

    ListItem::new(Line::from(""))
}

fn log_list_item(entry: &LmLogEntry) -> ListItem<'static> {
    let outcome_style = match entry.outcome {
        LmOutcome::Success => Style::default().fg(theme::SUCCESS),
        LmOutcome::Partial => Style::default().fg(theme::PARTIAL),
        LmOutcome::Failed => Style::default().fg(theme::FAILED),
    };

    let ts = chrono::DateTime::from_timestamp_millis(entry.ts as i64)
        .map(|dt| dt.format("%H:%M:%S").to_string())
        .unwrap_or_else(|| "?".to_string());

    let summary = entry
        .summary
        .clone()
        .unwrap_or_else(|| format!("{} items", entry.items_count));

    let mut lines = Vec::new();

    // Header line: cycle, kind, outcome, duration, timestamp, summary
    lines.push(Line::from(vec![
        Span::raw(format!("#{} ", entry.cycle)),
        Span::styled(
            format!("{:?}", entry.kind),
            Style::default().fg(theme::LM_KIND),
        ),
        Span::raw(" "),
        Span::styled(format!("{:?}", entry.outcome), outcome_style),
        Span::raw(format!(" {}ms ", entry.duration_ms)),
        Span::styled(format!("[{}]", ts), Style::default().fg(theme::MUTED)),
        Span::raw(" "),
        Span::raw(summary),
    ]));

    // Show items processed (surface IDs)
    if !entry.items.is_empty() {
        let items_preview: Vec<&str> = entry.items.iter().take(8).map(|s| s.as_str()).collect();
        let items_str = if entry.items.len() > 8 {
            format!(
                "  → {} (+{} more)",
                items_preview.join(", "),
                entry.items.len() - 8
            )
        } else {
            format!("  → {}", items_preview.join(", "))
        };
        lines.push(Line::from(Span::styled(
            items_str,
            Style::default().fg(theme::SURFACE_ID),
        )));
    }

    // Prompt preview lines (show first 3 meaningful lines)
    if let Some(preview) = &entry.prompt_preview {
        let meaningful_lines: Vec<&str> = preview
            .lines()
            .filter(|l| !l.trim().is_empty())
            .take(3)
            .collect();

        for line in meaningful_lines {
            let truncated = if line.len() > 90 {
                format!("  │ {}…", &line[..90])
            } else {
                format!("  │ {}", line)
            };
            lines.push(Line::from(Span::styled(
                truncated,
                Style::default().fg(theme::PROMPT),
            )));
        }
    }

    // Blank line for visual separation
    lines.push(Line::from(""));

    ListItem::new(lines)
}

fn browse_list_item(entry: &BrowseEntry) -> ListItem<'static> {
    let indent = "  ".repeat(entry.depth);
    let suffix = if entry.is_dir { "/" } else { "" };

    let filename = entry
        .path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&entry.rel_path);

    let line = Line::from(vec![
        Span::raw(indent),
        Span::styled(
            format!("{}{}", filename, suffix),
            if entry.is_dir {
                Style::default()
                    .fg(theme::LM_KIND)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            },
        ),
    ]);

    ListItem::new(line)
}

fn decision_style(decision: &enrich::Decision) -> Style {
    match decision {
        enrich::Decision::Complete => Style::default().fg(theme::SUCCESS),
        enrich::Decision::Incomplete => Style::default().fg(theme::PARTIAL),
        enrich::Decision::Blocked => Style::default().fg(theme::FAILED),
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
