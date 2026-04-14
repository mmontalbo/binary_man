use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
    Frame,
};

use crate::app::{App, Focus};

pub fn draw(frame: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(frame.area());

    match &app.cell_state {
        None => draw_experiment_picker(frame, app, chunks[0]),
        Some(_) => draw_cell_view(frame, app, chunks[0]),
    }

    draw_status_bar(frame, app, chunks[1]);
}

// -- Helpers --

fn border_block(title: &str, active: bool) -> Block<'_> {
    let color = if active { Color::Cyan } else { Color::DarkGray };
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(color))
}

const DIM: Style = Style::new().fg(Color::DarkGray);

// -- Experiment picker --

fn draw_experiment_picker(frame: &mut Frame, app: &mut App, area: Rect) {
    let items: Vec<ListItem> = app
        .experiments
        .iter()
        .map(|exp| ListItem::new(format!("{} ({} cells)", exp.name, exp.cells.len())))
        .collect();

    let max_name_len = app
        .experiments
        .iter()
        .map(|e| e.name.len() + 12)
        .max()
        .unwrap_or(30);
    let min_left = (area.width as usize / 4).max(20);
    let max_left = area.width as usize / 2;
    let left_w = max_name_len.clamp(min_left, max_left) as u16;

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(left_w), Constraint::Min(30)])
        .split(area);
    let list = List::new(items)
        .block(border_block(" Experiments ", app.focus == Focus::Experiments))
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
        .highlight_symbol("▸ ");
    frame.render_widget(Clear, cols[0]);
    frame.render_stateful_widget(list, cols[0], &mut app.experiment_list_state);

    let cell_items: Vec<ListItem> = app
        .selected_experiment()
        .map(|exp| {
            exp.cells
                .iter()
                .map(|cell| {
                    let key_spans: Vec<Span> = if let Some((model, binary)) = cell.key.split_once("__") {
                        vec![
                            Span::raw(model.to_string()),
                            Span::styled(" / ", DIM),
                            Span::styled(binary.to_string(), Style::default().fg(Color::Cyan)),
                        ]
                    } else {
                        vec![Span::raw(cell.key.clone())]
                    };

                    let stats_span = cell.summary.as_ref().map(|s| {
                        let pct = if s.total_surfaces > 0 {
                            s.mean_verified / s.total_surfaces as f64 * 100.0
                        } else {
                            0.0
                        };
                        let color = if pct >= 70.0 {
                            Color::Green
                        } else if pct >= 30.0 {
                            Color::Yellow
                        } else {
                            Color::Red
                        };
                        Span::styled(
                            format!("{:.0}/{} ({:.0}%)", s.mean_verified, s.total_surfaces, pct),
                            Style::default().fg(color),
                        )
                    });

                    let mut spans = key_spans;
                    spans.push(Span::raw(": "));
                    spans.push(stats_span.unwrap_or_else(|| Span::styled("—", DIM)));
                    ListItem::new(Line::from(spans))
                })
                .collect()
        })
        .unwrap_or_default();
    let cell_list = List::new(cell_items)
        .block(border_block(" Cells ", app.focus == Focus::Cells))
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
        .highlight_symbol("▸ ");
    frame.render_widget(Clear, cols[1]);
    frame.render_stateful_widget(cell_list, cols[1], &mut app.cell_list_state);
}

// -- Cell view --

fn draw_cell_view(frame: &mut Frame, app: &mut App, area: Rect) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);
    draw_stats_banner(frame, app, outer[0]);
    let content_area = outer[1];

    let rows = if app.filter_active || !app.filter.is_empty() {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(content_area)
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(0), Constraint::Min(0)])
            .split(content_area)
    };

    if app.filter_active || !app.filter.is_empty() {
        let cursor = if app.filter_active { "▎" } else { "" };
        let filter_text = format!(" /{}{}", app.filter, cursor);
        let bar = Paragraph::new(filter_text)
            .style(Style::default().fg(Color::Cyan));
        frame.render_widget(Clear, rows[0]);
        frame.render_widget(bar, rows[0]);
    }

    let left_w = {
        let state = app.cell_state.as_ref();
        let max_id_len = state
            .map(|s| {
                app.filtered_indices
                    .iter()
                    .map(|&i| s.surfaces[i].id.len())
                    .max()
                    .unwrap_or(10)
            })
            .unwrap_or(10);
        let content_w = (max_id_len + 14) as u16;
        let min_left = (rows[1].width / 4).max(20);
        let max_left = rows[1].width / 2;
        content_w.clamp(min_left, max_left)
    };
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(left_w), Constraint::Min(30)])
        .split(rows[1]);
    draw_surface_list(frame, app, cols[0]);
    draw_surface_detail(frame, app, cols[1]);
}

fn draw_stats_banner(frame: &mut Frame, app: &App, area: Rect) {
    let state = match &app.cell_state {
        Some(s) => s,
        None => return,
    };
    let verified = state.surfaces.iter().filter(|s| s.status == "Verified").count();
    let pending = state.surfaces.iter().filter(|s| s.status == "Pending").count();
    let excluded = state.surfaces.len() - verified - pending;

    let mut spans = vec![
        Span::raw(" "),
        Span::styled(format!("\u{2713} {} verified", verified), Style::default().fg(Color::Green)),
        Span::styled("  \u{00b7}  ", DIM),
        Span::styled(format!("\u{25cb} {} pending", pending), Style::default().fg(Color::Yellow)),
        Span::styled("  \u{00b7}  ", DIM),
        Span::styled(format!("\u{2717} {} excluded", excluded), Style::default().fg(Color::DarkGray)),
    ];

    if let Some(cell) = app.selected_cell() {
        if let Some(summary) = &cell.summary {
            spans.push(Span::styled("  \u{2502}  ", DIM));
            spans.push(Span::styled(
                format!("{:.0} cycles", summary.mean_cycles),
                DIM,
            ));
            spans.push(Span::styled("  \u{2502}  ", DIM));
            spans.push(Span::styled(
                format!("{:.1}s", summary.mean_elapsed),
                DIM,
            ));
        }
    }

    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_surface_list(frame: &mut Frame, app: &mut App, area: Rect) {
    let state = match &app.cell_state {
        Some(s) => s,
        None => return,
    };

    let items: Vec<ListItem> = app
        .filtered_indices
        .iter()
        .map(|&i| {
            let s = &state.surfaces[i];
            let (icon, color) = match s.status.as_str() {
                "Verified" => ("✓", Color::Green),
                "Pending" => ("○", Color::Yellow),
                _ => ("✗", Color::DarkGray),
            };
            let hint = if s.status == "Verified" {
                String::new()
            } else {
                let (_, _, short) = crate::app::failure_mode_key(s);
                if short.is_empty() {
                    String::new()
                } else {
                    format!(" {}", short)
                }
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{} ", icon), Style::default().fg(color)),
                Span::raw(s.id.clone()),
                Span::styled(hint, DIM),
            ]))
        })
        .collect();

    let shown = app.filtered_indices.len();
    let total = state.surfaces.len();
    let title = if shown < total {
        format!(" Surfaces ({}/{}) ", shown, total)
    } else {
        format!(" Surfaces ({}) ", total)
    };
    let list = List::new(items)
        .block(border_block(&title, app.active_pane == crate::app::Pane::SurfaceList))
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
        .highlight_symbol("▸ ");
    frame.render_widget(Clear, area);
    frame.render_stateful_widget(list, area, &mut app.surface_list_state);
}

fn draw_surface_detail(frame: &mut Frame, app: &mut App, area: Rect) {
    let surface = match app.selected_surface().cloned() {
        Some(s) => s,
        None => {
            frame.render_widget(Clear, area);
            let p = Paragraph::new("No surface selected")
                .block(border_block(" Detail ", false));
            frame.render_widget(p, area);
            return;
        }
    };

    let mut lines: Vec<Line<'static>> = Vec::new();

    // Header
    let status_color = match surface.status.as_str() {
        "Verified" => Color::Green,
        "Pending" => Color::Yellow,
        _ => Color::Red,
    };
    lines.push(Line::from(vec![
        Span::styled(
            surface.id.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(surface.status.clone(), Style::default().fg(status_color)),
        Span::styled(
            {
                let np = surface.probes.len();
                let na = surface.attempts.len();
                format!(
                    "  {} {} \u{00b7} {} {}",
                    np, if np == 1 { "probe" } else { "probes" },
                    na, if na == 1 { "attempt" } else { "attempts" },
                )
            },
            DIM,
        ),
    ]));
    if !surface.description.is_empty() {
        lines.push(Line::styled(surface.description.clone(), DIM));
    }
    if let Some(trigger) = &surface.characterization {
        lines.push(Line::styled(format!("trigger: {}", trigger), DIM));
    }

    // Characterization conversation (pre-verification)
    // Find the chunk that contains this surface and show full prompt + response verbatim
    let char_log = crate::data::find_char_log_for_surface(&app.char_logs, &surface.id);
    if let Some(log) = char_log {
        lines.push(Line::from(""));
        lines.push(Line::styled(
            "\u{2500}\u{2500} characterization \u{2500}\u{2500}",
            DIM,
        ));

        // Full prompt (collapsible)
        let prompt_lines = log.prompt.lines().count();
        if app.expanded {
            lines.push(Line::from(Span::styled("\u{2502}", DIM)));
            render_bordered_markdown(&mut lines, &log.prompt, area.width as usize);
        } else {
            lines.push(Line::styled(
                format!("  (prompt — {} lines — press e to expand)", prompt_lines),
                DIM,
            ));
        }

        // Full response verbatim
        if !log.response.is_empty() {
            let response_lines = log.response.lines().count();
            lines.push(Line::from(""));
            if app.expanded {
                for line in log.response.lines() {
                    lines.push(Line::styled(format!("  {}", line), Style::default()));
                }
            } else {
                lines.push(Line::styled(
                    format!("  (response — {} lines — press e to expand)", response_lines),
                    DIM,
                ));
            }
        }
    }

    // Group events by cycle
    let mut cycle_groups: std::collections::BTreeMap<u32, Vec<(bool, usize)>> =
        std::collections::BTreeMap::new();
    for (i, p) in surface.probes.iter().enumerate() {
        cycle_groups.entry(p.cycle).or_default().push((false, i));
    }
    for (i, a) in surface.attempts.iter().enumerate() {
        cycle_groups.entry(a.cycle).or_default().push((true, i));
    }

    let focused_cycle = app.event_cycles.get(app.focused_event).copied();
    let mut focused_line: Option<u16> = None;
    let mut prev_cycle_num: Option<u32> = None;
    let mut prior_events: Vec<(u32, String)> = Vec::new();
    let mut show_context = true;
    let mut prev_prompt_excerpt: Option<String> = None;
    let mut prev_prompt_cycle: Option<u32> = None;

    for (&cycle, group) in &cycle_groups {
        // Gap annotation
        if let Some(prev) = prev_cycle_num {
            let gap = cycle.saturating_sub(prev);
            if gap > 1 {
                lines.push(Line::styled(
                    format!("  \u{22ef} {} cycles", gap - 1),
                    DIM,
                ));
                show_context = true;
            }
        }
        prev_cycle_num = Some(cycle);

        lines.push(Line::from(""));

        // Cycle header with focus marker
        let is_focused = focused_cycle == Some(cycle);
        if is_focused {
            focused_line = Some(lines.len() as u16);
        }
        let marker = if is_focused { "\u{25b8} " } else { "" };
        lines.push(Line::styled(
            format!("{}\u{2500}\u{2500} c{} \u{2500}\u{2500}", marker, cycle),
            DIM,
        ));

        // BMAN prompt excerpt (│ bordered, with markdown rendering)
        let prompt_excerpt = app.transcripts.iter()
            .find(|t| t.cycle == cycle)
            .and_then(|t| {
                extract_surface_from_prompt(&t.prompt, &surface.id)
            });

        if let Some(excerpt) = &prompt_excerpt {
            // Collapse identical prompts (unless expanded)
            if Some(excerpt) == prev_prompt_excerpt.as_ref() && !app.expanded {
                lines.push(Line::styled(
                    format!("  (same prompt as c{} — press e to expand)", prev_prompt_cycle.unwrap_or(0)),
                    DIM,
                ));
            } else {
                lines.push(Line::from(Span::styled("\u{2502}", DIM)));
                render_bordered_markdown(&mut lines, excerpt, area.width as usize);
            }
            prev_prompt_excerpt = Some(excerpt.clone());
            prev_prompt_cycle = Some(cycle);
        } else if show_context {
            if prior_events.is_empty() {
                lines.push(Line::styled("  no prior activity", DIM));
            } else {
                let total = prior_events.len();
                let shown: Vec<String> = prior_events
                    .iter()
                    .rev()
                    .take(2)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .map(|(c, desc)| format!("c{} {}", c, desc))
                    .collect();
                let prefix = if total > 2 {
                    format!("{} prior, last: ", total)
                } else {
                    "prior: ".to_string()
                };
                lines.push(Line::styled(
                    format!("  {}{}", prefix, shown.join(", ")),
                    DIM,
                ));
            }
        }
        show_context = false;

        // LM analysis text
        if let Some(analysis) = app.analysis_for(cycle, &surface.id) {
            lines.push(Line::from(""));
            lines.push(Line::styled(format!("  {}", analysis), Style::default()));
        }

        // Results for each event in this cycle group
        for (is_attempt, eidx) in group {
            lines.push(Line::from(""));
            if *is_attempt {
                let a = &surface.attempts[*eidx];
                let color = match a.outcome.as_str() {
                    "Verified" => Color::Green,
                    "OutputsEqual" => Color::Yellow,
                    "SetupFailed" => Color::Red,
                    _ => Color::White,
                };
                lines.push(Line::from(vec![
                    Span::styled("  TEST \u{2192} ", Style::default().add_modifier(Modifier::BOLD)),
                    Span::styled(a.outcome.clone(), Style::default().fg(color)),
                ]));
                render_action_json(&mut lines, app, cycle, &surface.id, true);

                render_seed(&mut lines, &a.setup_commands, &a.files);
                push_optional(&mut lines, "stdout", &a.stdout_preview);
                push_optional(&mut lines, "stderr", &a.stderr_preview);

                if let Some(pred) = &a.prediction {
                    if !pred.is_empty() {
                        lines.push(Line::from(vec![
                            Span::styled(
                                "    predicted: ",
                                Style::default().add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(pred.clone(), Style::default().fg(Color::Cyan)),
                        ]));
                    }
                }

                prior_events.push((cycle, format!("test \u{2192} {}", a.outcome)));
            } else {
                let p = &surface.probes[*eidx];
                let (label, color) = if p.setup_failed {
                    ("SetupFailed", Color::Red)
                } else if p.outputs_differ {
                    ("DIFFER", Color::Green)
                } else {
                    let empty = p.stdout_preview.as_ref().is_none_or(|s| s.is_empty())
                        && p.control_stdout_preview
                            .as_ref()
                            .is_none_or(|s| s.is_empty());
                    if empty {
                        ("identical (0 bytes)", Color::Red)
                    } else {
                        ("identical", Color::Yellow)
                    }
                };
                lines.push(Line::from(vec![
                    Span::styled("  PROBE \u{2192} ", Style::default().add_modifier(Modifier::BOLD)),
                    Span::styled(label.to_string(), Style::default().fg(color)),
                ]));
                render_action_json(&mut lines, app, cycle, &surface.id, false);

                render_seed(&mut lines, &p.setup_commands, &p.files);
                if let Some(detail) = &p.setup_detail {
                    lines.push(Line::styled(
                        format!("  error: {}", detail),
                        Style::default().fg(Color::Red),
                    ));
                }
                push_optional(&mut lines, "control", &p.control_stdout_preview);
                push_optional(&mut lines, "stdout", &p.stdout_preview);

                prior_events.push((cycle, format!("probe \u{2192} {}", label)));
            }
        }
    }

    if cycle_groups.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::styled("(no interactions)", DIM));
    }

    // Scroll clamping
    let inner_width = area.width.saturating_sub(2) as usize;
    let wrapped_count: u16 = if inner_width == 0 {
        lines.len() as u16
    } else {
        lines
            .iter()
            .map(|l| {
                let w: usize = l.spans.iter().map(|s| s.content.len()).sum();
                (w / inner_width.max(1) + 1) as u16
            })
            .sum()
    };
    let visible = area.height.saturating_sub(2);
    let max_scroll = wrapped_count.saturating_sub(visible);

    if app.detail_scroll == u16::MAX {
        if let Some(fl) = focused_line {
            app.detail_scroll = fl.saturating_sub(2).min(max_scroll);
        } else {
            app.detail_scroll = 0;
        }
    }
    app.detail_scroll = app.detail_scroll.min(max_scroll);

    let detail_title = if max_scroll > 0 {
        format!(
            " {} [{}/{}] ",
            surface.id,
            app.detail_scroll + 1,
            wrapped_count,
        )
    } else {
        format!(" {} ", surface.id)
    };

    frame.render_widget(Clear, area);
    let paragraph = Paragraph::new(lines)
        .block(border_block(&detail_title, app.active_pane == crate::app::Pane::Detail))
        .wrap(Wrap { trim: false })
        .scroll((app.detail_scroll, 0));
    frame.render_widget(paragraph, area);
}

// -- Seed rendering helpers --

fn render_seed(lines: &mut Vec<Line<'static>>, setup: &[String], files: &[(String, String)]) {
    if !setup.is_empty() {
        lines.push(Line::styled("  \u{2500}\u{2500} setup", DIM));
    }
    for cmd in setup {
        lines.push(Line::styled(format!("  $ {}", cmd), Style::default()));
    }
    if !files.is_empty() {
        lines.push(Line::styled("  \u{2500}\u{2500} files", DIM));
    }
    for (path, content) in files {
        lines.push(Line::styled(
            format!("  file: {}", path),
            Style::default().add_modifier(Modifier::BOLD),
        ));
        for line in content.lines().take(8) {
            lines.push(Line::styled(format!("    {}", strip_escapes(line)), DIM));
        }
        let total = content.lines().count();
        if total > 8 {
            lines.push(Line::styled(
                format!("    ... ({} more lines)", total - 8),
                DIM,
            ));
        }
    }
}

/// Extract the per-surface section from a cycle prompt.
/// Handles both `### surface_id` (full prompt) and `- **surface_id**` (compact prompt) formats.
fn extract_surface_from_prompt(prompt: &str, surface_id: &str) -> Option<String> {
    // Try ### heading format first (full prompt)
    let heading = format!("### {}\n", surface_id);
    if let Some(start) = prompt.find(&heading) {
        let section = &prompt[start..];
        let end = section[heading.len()..]
            .find("\n### ")
            .map(|i| i + heading.len())
            .or_else(|| section[heading.len()..].find("\n## ").map(|i| i + heading.len()))
            .unwrap_or(section.len());
        return Some(section[..end].trim_end().to_string());
    }

    // Try **surface_id** in bullet list format (compact prompt)
    let bullet = format!("- **{}**", surface_id);
    if let Some(start) = prompt.find(&bullet) {
        let section = &prompt[start..];
        // Read until next bullet at same indent level or section heading
        let end = section[bullet.len()..]
            .find("\n- **")
            .map(|i| i + bullet.len())
            .or_else(|| section[bullet.len()..].find("\n## ").map(|i| i + bullet.len()))
            .unwrap_or(section.len());
        return Some(section[..end].trim_end().to_string());
    }

    None
}

/// Render text with │ border, applying basic markdown formatting.
/// ## headers get bold, **text** gets bold, everything else is dim.
fn render_bordered_markdown(lines: &mut Vec<Line<'static>>, text: &str, width: usize) {
    // Available width for text content: pane width minus borders (2) minus │ prefix (2)
    let wrap_width = width.saturating_sub(4).max(20);

    for line in text.lines() {
        let trimmed = line.trim_start();
        let style = if trimmed.starts_with("## ") || trimmed.starts_with("### ") || trimmed.starts_with("# ") {
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD)
        } else if trimmed.starts_with("**") || trimmed.starts_with("- **") || trimmed.contains("**Trigger**") || trimmed.contains("**Expected diff**") || trimmed.contains("**Probes:**") || trimmed.contains("**Attempts:**") || trimmed.starts_with("\u{26a0}") {
            Style::default().fg(Color::Gray)
        } else if trimmed.starts_with("stderr:") || trimmed.starts_with("outcome:") || trimmed.starts_with("HINT") {
            Style::default().fg(Color::Yellow)
        } else {
            DIM
        };

        // Pre-wrap long lines so each segment gets the │ border
        if line.len() <= wrap_width {
            lines.push(Line::from(vec![
                Span::styled("\u{2502} ", DIM),
                Span::styled(line.to_string(), style),
            ]));
        } else {
            let mut remaining = line;
            while !remaining.is_empty() {
                let split_at = if remaining.len() <= wrap_width {
                    remaining.len()
                } else {
                    // Try to split at a space near the wrap width
                    remaining[..wrap_width]
                        .rfind(' ')
                        .map(|i| i + 1)
                        .unwrap_or(wrap_width)
                };
                let (chunk, rest) = remaining.split_at(split_at);
                lines.push(Line::from(vec![
                    Span::styled("\u{2502} ", DIM),
                    Span::styled(chunk.to_string(), style),
                ]));
                remaining = rest;
            }
        }
    }
}

/// Render the raw JSON action from the cycle response for a surface.
fn render_action_json(
    lines: &mut Vec<Line<'static>>,
    app: &App,
    cycle: u32,
    surface_id: &str,
    is_attempt: bool,
) {
    if let Some(actions) = app.cycle_actions.get(&cycle) {
        let kind_match = if is_attempt { "Test" } else { "Probe" };
        for (sid, kind) in actions {
            if sid == surface_id && kind == kind_match {
                // Found the matching action — show compact JSON
                lines.push(Line::styled(
                    format!("  {{ kind: {}, surface_id: {} }}", kind, sid),
                    DIM,
                ));
                return;
            }
        }
    }
}

fn push_optional(lines: &mut Vec<Line<'static>>, label: &str, value: &Option<String>) {
    if let Some(v) = value {
        if !v.is_empty() {
            lines.push(Line::styled(format!("  {}: {}", label, strip_escapes(v)), DIM));
        }
    }
}

/// Strip ANSI and OSC escape sequences from text to prevent terminal corruption.
/// Handles CSI sequences (\x1b[...), OSC sequences (\x1b]...\x07), and BEL (\x07).
fn strip_escapes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            match chars.peek() {
                Some('[') => {
                    // CSI sequence: consume until letter
                    chars.next();
                    while let Some(&ch) = chars.peek() {
                        chars.next();
                        if ch.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
                Some(']') => {
                    // OSC sequence: consume until BEL (\x07) or ST (\x1b\\)
                    chars.next();
                    while let Some(&ch) = chars.peek() {
                        chars.next();
                        if ch == '\x07' {
                            break;
                        }
                        if ch == '\x1b' {
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                _ => {} // skip lone ESC
            }
        } else if c == '\x07' {
            // stray BEL
        } else {
            out.push(c);
        }
    }
    out
}

// -- Status bar --

fn draw_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let breadcrumb = build_breadcrumb(app);
    let keys = match app.focus {
        Focus::Experiments => "j/k:nav  l/Enter:open  q:quit",
        Focus::Cells => "j/k:nav  h:back  l/Enter:open  q:quit",
        Focus::CellView if app.filter_active => "type to filter  Enter/Esc:close filter",
        Focus::CellView => {
            match app.active_pane {
                crate::app::Pane::SurfaceList => "j/k:nav  l:detail  n/p:event  e:expand  /:find  q:quit",
                crate::app::Pane::Detail => "j/k:scroll  h:list  n/p:event  e:expand  PgDn/Up:page  q:quit",
            }
        }
    };

    let bar = Line::from(vec![
        Span::styled(
            format!(" {} ", breadcrumb),
            Style::default().fg(Color::Black).bg(Color::Cyan),
        ),
        Span::raw(" "),
        Span::styled(keys, DIM),
    ]);
    frame.render_widget(Paragraph::new(bar), area);
}

fn build_breadcrumb(app: &App) -> String {
    let mut parts = Vec::new();

    if let Some(exp) = app.selected_experiment() {
        parts.push(exp.name.clone());

        if matches!(app.focus, Focus::Cells | Focus::CellView) {
            if let Some(cell) = app.selected_cell() {
                parts.push(cell.key.clone());
            }
        }

        if app.focus == Focus::CellView {
            if let Some(surface) = app.selected_surface() {
                parts.push(surface.id.clone());
            }
        }
    }

    if parts.is_empty() {
        "Experiments".to_string()
    } else {
        parts.join(" \u{203a} ")
    }
}
