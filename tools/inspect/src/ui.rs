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

// Border colors for message categories
fn bman_border() -> Span<'static> { Span::styled("\u{2502} ".to_string(), DIM) }
fn lm_border() -> Span<'static> { Span::styled("\u{2502} ".to_string(), Style::default().fg(Color::Magenta)) }
fn tool_border() -> Span<'static> { Span::styled("\u{2502} ".to_string(), Style::default().fg(Color::Yellow)) }
fn sys_border() -> Span<'static> { Span::styled("\u{2502} ".to_string(), Style::default().fg(Color::Green)) }

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

    // Header — dark background to separate from conversation below
    let hdr = Style::default().bg(Color::Rgb(40, 40, 40));
    let status_color = match surface.status.as_str() {
        "Verified" => Color::Green,
        "Pending" => Color::Yellow,
        _ => Color::Red,
    };
    lines.push(Line::styled("", hdr));
    lines.push(Line::from(vec![
        Span::styled(
            format!("  {} ", surface.id),
            hdr.add_modifier(Modifier::BOLD),
        ),
        Span::styled(" ", hdr),
        Span::styled(surface.status.clone(), hdr.fg(status_color)),
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
            hdr.fg(Color::Gray),
        ),
    ]));
    // Pre-wrap description and trigger with padding so they don't hit pane borders
    let hdr_wrap = area.width.saturating_sub(6) as usize; // 2 indent + 2 border + 2 margin
    if !surface.description.is_empty() {
        for line in wrap_text(&surface.description, hdr_wrap) {
            lines.push(Line::styled(format!("  {}", line), hdr));
        }
    }
    if let Some(trigger) = &surface.characterization {
        let prefixed = format!("trigger: {}", trigger);
        for line in wrap_text(&prefixed, hdr_wrap) {
            lines.push(Line::styled(format!("  {}", line), hdr));
        }
    }
    lines.push(Line::styled("", hdr));

    let w = area.width.saturating_sub(4) as usize;

    // Characterization conversation (pre-verification)
    let char_log = crate::data::find_char_log_for_surface(&app.char_logs, &surface.id);
    if let Some(log) = char_log {
        lines.push(Line::from(""));

        // Prompt — raw text, truncated to 10 lines (e to expand)
        render_truncated_bordered(&mut lines, &log.prompt, bman_border, DIM, 10, app.expanded, w);

        if !log.response.is_empty() {
            lines.push(Line::from(""));
            // Response — pretty-print JSON, truncated to 10 lines (e to expand)
            let display_text = pretty_print_json_response(&log.response);
            render_truncated_bordered(&mut lines, &display_text, lm_border, Style::default(), 10, app.expanded, w);
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
    let mut running_status = "Pending".to_string();

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

        // Track focus for auto-scroll
        let is_focused = focused_cycle == Some(cycle);
        if is_focused {
            focused_line = Some(lines.len() as u16);
        }
        let is_batch_probe = cycle == 0 && char_log.is_none();

        // BMAN: full raw prompt text
        let prompt_text = app.transcripts.iter()
            .find(|t| t.cycle == cycle)
            .map(|t| &t.prompt);

        if let Some(prompt) = prompt_text {
            let is_same = Some(prompt) == prev_prompt_excerpt.as_ref();
            if is_same && !app.expanded {
                render_truncated_bordered(&mut lines, prompt, bman_border, DIM, 3, false, w);
            } else {
                render_truncated_bordered(&mut lines, prompt, bman_border, DIM, 10, app.expanded, w);
            }
            prev_prompt_excerpt = Some(prompt.clone());
        } else if is_batch_probe {
            lines.push(Line::styled(
                "  verified mechanically against rich fixture (no LM involved)",
                DIM,
            ));
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

        // LM: raw response text for this cycle
        if let Some(response_text) = app.cycle_responses.get(&cycle) {
            lines.push(Line::from(""));
            let display = pretty_print_json_response(response_text);
            render_truncated_bordered(&mut lines, &display, lm_border, Style::default(), 10, app.expanded, w);
        }

        // Results for each event in this cycle group
        for (is_attempt, eidx) in group {
            if *is_attempt {
                let a = &surface.attempts[*eidx];
                let outcome = &a.outcome;

                // TOOL: raw attempt JSON from state.json
                let raw_json = serde_json::to_string_pretty(&a.raw).unwrap_or_default();
                let cleaned = strip_escapes(&raw_json);
                render_truncated_bordered(&mut lines, &cleaned, tool_border, Style::default(), 10, app.expanded, w);

                prior_events.push((cycle, format!("test \u{2192} {}", outcome)));
            } else {
                let p = &surface.probes[*eidx];
                let label = if p.setup_failed {
                    "SetupFailed"
                } else if p.outputs_differ {
                    "DIFFER"
                } else {
                    let empty = p.stdout_preview.as_ref().is_none_or(|s| s.is_empty())
                        && p.control_stdout_preview
                            .as_ref()
                            .is_none_or(|s| s.is_empty());
                    if empty { "identical (0 bytes)" } else { "identical" }
                };

                // TOOL: raw probe JSON from state.json
                let raw_json = serde_json::to_string_pretty(&p.raw).unwrap_or_default();
                let cleaned = strip_escapes(&raw_json);
                render_truncated_bordered(&mut lines, &cleaned, tool_border, Style::default(), 10, app.expanded, w);

                prior_events.push((cycle, format!("probe \u{2192} {}", label)));
            }
        }

        // SYSTEM: status transition (only when it changes)
        let mut new_status = running_status.clone();
        for (is_attempt, eidx) in group {
            if *is_attempt {
                let outcome = &surface.attempts[*eidx].outcome;
                if outcome == "Verified" {
                    new_status = "Verified".to_string();
                }
            }
        }
        // Check if this is the final cycle and surface ended up Excluded
        if cycle == cycle_groups.keys().last().copied().unwrap_or(0)
            && surface.status == "Excluded"
            && new_status != "Verified"
        {
            new_status = "Excluded".to_string();
        }
        if new_status != running_status {
            let (icon, color) = match new_status.as_str() {
                "Verified" => ("\u{2713}", Color::Green),
                "Excluded" => ("\u{2717}", Color::Red),
                _ => ("\u{25cb}", Color::Yellow),
            };
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                sys_border(),
                Span::styled(
                    format!("{} {} \u{2192} {}", icon, running_status, new_status),
                    Style::default().fg(color),
                ),
            ]));
            running_status = new_status;
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

/// Render text with a colored border, showing first `max_lines` lines.
/// If truncated, shows `... (N more lines)`. If `expanded`, shows all lines.
fn render_truncated_bordered(
    lines: &mut Vec<Line<'static>>,
    text: &str,
    border_fn: fn() -> Span<'static>,
    style: Style,
    max_lines: usize,
    expanded: bool,
    wrap_width: usize,
) {
    let all_lines: Vec<&str> = text.lines().collect();
    let total = all_lines.len();
    let show = if expanded { total } else { max_lines.min(total) };

    for line in &all_lines[..show] {
        push_bordered_wrapped(lines, border_fn, line, style, wrap_width);
    }

    if !expanded && total > max_lines {
        lines.push(Line::from(vec![
            border_fn(),
            Span::styled(
                format!("... ({} more lines)", total - max_lines),
                DIM,
            ),
        ]));
    }
}

/// Push a line with a colored border, pre-wrapping if it exceeds the given width.
/// Each wrapped segment gets its own border prefix.
fn push_bordered_wrapped(
    lines: &mut Vec<Line<'static>>,
    border_fn: fn() -> Span<'static>,
    text: &str,
    style: Style,
    max_width: usize,
) {
    if text.len() <= max_width {
        lines.push(Line::from(vec![border_fn(), Span::styled(text.to_string(), style)]));
    } else {
        let mut remaining = text;
        while !remaining.is_empty() {
            let split = if remaining.len() <= max_width {
                remaining.len()
            } else {
                remaining[..max_width].rfind(' ').map(|i| i + 1).unwrap_or(max_width)
            };
            let (chunk, rest) = remaining.split_at(split);
            lines.push(Line::from(vec![border_fn(), Span::styled(chunk.to_string(), style)]));
            remaining = rest;
        }
    }
}




/// Wrap text to a given width, splitting at word boundaries.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let mut result = Vec::new();
    let width = width.max(20);
    let mut remaining = text;
    while !remaining.is_empty() {
        if remaining.len() <= width {
            result.push(remaining.to_string());
            break;
        }
        let split = remaining[..width]
            .rfind(' ')
            .map(|i| i + 1)
            .unwrap_or(width);
        let (chunk, rest) = remaining.split_at(split);
        result.push(chunk.to_string());
        remaining = rest;
    }
    result
}

/// Try to pretty-print a JSON response string. Strips markdown fences first.
/// Falls back to the original text if parsing fails.
fn pretty_print_json_response(raw: &str) -> String {
    let trimmed = raw.trim();
    // Strip markdown fences if present
    let json_text = if trimmed.starts_with("```") {
        let body = trimmed
            .strip_prefix("```json")
            .or_else(|| trimmed.strip_prefix("```"))
            .unwrap_or(trimmed);
        body.strip_suffix("```").unwrap_or(body).trim()
    } else {
        trimmed
    };

    match serde_json::from_str::<serde_json::Value>(json_text) {
        Ok(val) => serde_json::to_string_pretty(&val).unwrap_or_else(|_| raw.to_string()),
        Err(_) => raw.to_string(),
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
