//! TUI for inspecting claims and validation results.
//!
//! Provides a claims list view and a source-text view linked to parsed claims.
//! Source view is available only when claim source paths are real files on disk.

use crate::schema::{
    Claim, ClaimSourceType, ClaimStatus, ClaimsFile, Determinism, Evidence, ValidationMethod,
    ValidationReport, ValidationResult, ValidationStatus,
};
use anyhow::{Context, Result};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};
use std::collections::{BTreeMap, HashMap};
use std::io;
use std::path::Path;
use std::time::Duration;

const LIST_LEGEND: &str = "[q quit] [t source] [a all] [v validated] [c/r/u/n filter]";
const SOURCE_LEGEND: &str = "[q quit] [t claims] [tab cycle] [a/v/c/r/u/n filter]";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ViewMode {
    Claims,
    Source,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DisplayStatus {
    Confirmed,
    Refuted,
    Undetermined,
    Unvalidated,
}

impl DisplayStatus {
    fn label(&self) -> &'static str {
        match self {
            DisplayStatus::Confirmed => "confirmed",
            DisplayStatus::Refuted => "refuted",
            DisplayStatus::Undetermined => "undetermined",
            DisplayStatus::Unvalidated => "unvalidated",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FilterMode {
    All,
    Validated,
    Confirmed,
    Refuted,
    Undetermined,
    Unvalidated,
}

impl FilterMode {
    fn label(&self) -> &'static str {
        match self {
            FilterMode::All => "all",
            FilterMode::Validated => "validated",
            FilterMode::Confirmed => "confirmed",
            FilterMode::Refuted => "refuted",
            FilterMode::Undetermined => "undetermined",
            FilterMode::Unvalidated => "unvalidated",
        }
    }

    fn matches(&self, entry: &Entry) -> bool {
        match self {
            FilterMode::All => true,
            FilterMode::Validated => entry.result.is_some(),
            FilterMode::Confirmed => entry.status == DisplayStatus::Confirmed,
            FilterMode::Refuted => entry.status == DisplayStatus::Refuted,
            FilterMode::Undetermined => entry.status == DisplayStatus::Undetermined,
            FilterMode::Unvalidated => entry.status == DisplayStatus::Unvalidated,
        }
    }
}

struct Entry {
    claim: Claim,
    result: Option<ValidationResult>,
    status: DisplayStatus,
}

struct HelpCache {
    files: HashMap<String, Vec<String>>,
}

impl HelpCache {
    fn new() -> Self {
        Self {
            files: HashMap::new(),
        }
    }

    fn context_lines(
        &mut self,
        path: &str,
        line: u64,
        radius: usize,
    ) -> Option<Vec<Line<'static>>> {
        if !Path::new(path).is_file() {
            return None;
        }
        let lines = if let Some(lines) = self.files.get(path) {
            lines
        } else {
            let content = std::fs::read_to_string(path).ok()?;
            let lines: Vec<String> = content.lines().map(|line| line.to_string()).collect();
            self.files.insert(path.to_string(), lines);
            self.files.get(path)?
        };

        if line == 0 {
            return None;
        }
        let line_idx = (line - 1) as usize;
        if line_idx >= lines.len() {
            return None;
        }
        let start = line_idx.saturating_sub(radius);
        let end = usize::min(lines.len().saturating_sub(1), line_idx + radius);

        let mut rendered = Vec::new();
        for idx in start..=end {
            let line_no = idx + 1;
            let prefix = format!("{:>4} | ", line_no);
            let content = lines[idx].clone();
            if idx == line_idx {
                rendered.push(Line::from(vec![
                    Span::styled(prefix, Style::default().fg(Color::Yellow)),
                    Span::styled(content, Style::default().add_modifier(Modifier::BOLD)),
                ]));
            } else {
                rendered.push(Line::from(format!("{prefix}{content}")));
            }
        }
        Some(rendered)
    }
}

struct SourceFile {
    path: String,
    lines: Vec<String>,
    line_claims: Vec<Vec<usize>>,
}

struct SourceView {
    sources: Vec<SourceFile>,
    active: usize,
    selected_line: usize,
    list_state: ListState,
    claim_cursor: usize,
}

impl SourceView {
    fn new(sources: Vec<SourceFile>) -> Self {
        let mut view = Self {
            sources,
            active: 0,
            selected_line: 0,
            list_state: ListState::default(),
            claim_cursor: 0,
        };
        view.sync_state();
        view
    }

    fn has_sources(&self) -> bool {
        !self.sources.is_empty()
    }

    fn active_source(&self) -> Option<&SourceFile> {
        self.sources.get(self.active)
    }

    fn line_count(&self) -> usize {
        self.active_source()
            .map(|source| source.lines.len())
            .unwrap_or(0)
    }

    fn sync_state(&mut self) {
        let line_count = self.line_count();
        if line_count == 0 {
            self.selected_line = 0;
            self.list_state = ListState::default();
            return;
        }
        if self.selected_line >= line_count {
            self.selected_line = line_count - 1;
        }
        self.list_state.select(Some(self.selected_line));
        let max_offset = line_count.saturating_sub(1);
        if self.list_state.offset() > max_offset {
            *self.list_state.offset_mut() = max_offset;
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let line_count = self.line_count();
        if line_count == 0 {
            return;
        }
        let len = line_count as isize;
        let mut next = self.selected_line as isize + delta;
        if next < 0 {
            next = 0;
        } else if next >= len {
            next = len - 1;
        }
        self.selected_line = next as usize;
        self.claim_cursor = 0;
        self.sync_state();
    }

    fn move_to_start(&mut self) {
        if self.line_count() > 0 {
            self.selected_line = 0;
            self.claim_cursor = 0;
            self.sync_state();
        }
    }

    fn move_to_end(&mut self) {
        let line_count = self.line_count();
        if line_count > 0 {
            self.selected_line = line_count - 1;
            self.claim_cursor = 0;
            self.sync_state();
        }
    }

    fn set_active_by_path(&mut self, path: &str) -> bool {
        if let Some(idx) = self.sources.iter().position(|source| source.path == path) {
            self.active = idx;
            self.selected_line = 0;
            self.claim_cursor = 0;
            self.sync_state();
            return true;
        }
        false
    }

    fn select_line(&mut self, line: usize) {
        if self.line_count() == 0 {
            return;
        }
        self.selected_line = line;
        self.claim_cursor = 0;
        self.sync_state();
    }

    fn line_status(
        &self,
        entries: &[Entry],
        filter: FilterMode,
        line_idx: usize,
    ) -> Option<(DisplayStatus, usize)> {
        let source = self.active_source()?;
        let line_claims = source.line_claims.get(line_idx)?;
        let mut count = 0;
        let mut has_confirmed = false;
        let mut has_refuted = false;
        let mut has_undetermined = false;
        let mut has_unvalidated = false;
        for claim_idx in line_claims {
            let entry = entries.get(*claim_idx)?;
            if !filter.matches(entry) {
                continue;
            }
            count += 1;
            match entry.status {
                DisplayStatus::Confirmed => has_confirmed = true,
                DisplayStatus::Refuted => has_refuted = true,
                DisplayStatus::Undetermined => has_undetermined = true,
                DisplayStatus::Unvalidated => has_unvalidated = true,
            }
        }
        if count == 0 {
            return None;
        }
        let status = if has_refuted {
            DisplayStatus::Refuted
        } else if has_undetermined {
            DisplayStatus::Undetermined
        } else if has_confirmed {
            DisplayStatus::Confirmed
        } else if has_unvalidated {
            DisplayStatus::Unvalidated
        } else {
            DisplayStatus::Unvalidated
        };
        Some((status, count))
    }

    fn filtered_claims_for_line(
        &self,
        entries: &[Entry],
        filter: FilterMode,
        line_idx: usize,
    ) -> Vec<usize> {
        let Some(source) = self.active_source() else {
            return Vec::new();
        };
        let Some(line_claims) = source.line_claims.get(line_idx) else {
            return Vec::new();
        };
        line_claims
            .iter()
            .copied()
            .filter(|idx| {
                entries
                    .get(*idx)
                    .map(|entry| filter.matches(entry))
                    .unwrap_or(false)
            })
            .collect()
    }

    fn selected_claim_index(&mut self, entries: &[Entry], filter: FilterMode) -> Option<usize> {
        let claims = self.filtered_claims_for_line(entries, filter, self.selected_line);
        if claims.is_empty() {
            self.claim_cursor = 0;
            return None;
        }
        if self.claim_cursor >= claims.len() {
            self.claim_cursor = 0;
        }
        claims.get(self.claim_cursor).copied()
    }

    fn cycle_claim(&mut self, entries: &[Entry], filter: FilterMode) {
        let claims = self.filtered_claims_for_line(entries, filter, self.selected_line);
        if claims.is_empty() {
            self.claim_cursor = 0;
            return;
        }
        self.claim_cursor = (self.claim_cursor + 1) % claims.len();
    }

    fn sync_claim_cursor(&mut self, entries: &[Entry], filter: FilterMode) {
        let claims = self.filtered_claims_for_line(entries, filter, self.selected_line);
        if claims.is_empty() || self.claim_cursor >= claims.len() {
            self.claim_cursor = 0;
        }
    }

    fn select_claim(&mut self, claim_idx: usize, entries: &[Entry], filter: FilterMode) {
        let claims = self.filtered_claims_for_line(entries, filter, self.selected_line);
        if let Some(pos) = claims.iter().position(|idx| *idx == claim_idx) {
            self.claim_cursor = pos;
        } else {
            self.claim_cursor = 0;
        }
    }

    fn detail_snapshot(&mut self, entries: &[Entry], filter: FilterMode) -> SourceDetail {
        let source = self.active_source();
        let source_path = source.map(|source| source.path.clone());
        let line_text = source
            .and_then(|source| source.lines.get(self.selected_line))
            .cloned();
        let line_no = if source_path.is_some() {
            Some(self.selected_line + 1)
        } else {
            None
        };
        let claims = self.filtered_claims_for_line(entries, filter, self.selected_line);
        let claim_count = claims.len();
        let claim_idx = if claim_count == 0 {
            self.claim_cursor = 0;
            None
        } else {
            if self.claim_cursor >= claim_count {
                self.claim_cursor = 0;
            }
            claims.get(self.claim_cursor).copied()
        };
        SourceDetail {
            source_path,
            line_no,
            line_text,
            claim_idx,
            claim_count,
            claim_cursor: self.claim_cursor,
        }
    }
}

struct SourceDetail {
    source_path: Option<String>,
    line_no: Option<usize>,
    line_text: Option<String>,
    claim_idx: Option<usize>,
    claim_count: usize,
    claim_cursor: usize,
}

struct App {
    entries: Vec<Entry>,
    visible_indices: Vec<usize>,
    selected: usize,
    list_state: ListState,
    filter: FilterMode,
    help_cache: HelpCache,
    view_mode: ViewMode,
    source_view: SourceView,
}

impl App {
    fn new(entries: Vec<Entry>) -> Self {
        let sources = build_sources(&entries);
        let mut app = Self {
            entries,
            visible_indices: Vec::new(),
            selected: 0,
            list_state: ListState::default(),
            filter: FilterMode::All,
            help_cache: HelpCache::new(),
            view_mode: ViewMode::Claims,
            source_view: SourceView::new(sources),
        };
        app.rebuild_visible();
        app
    }

    fn rebuild_visible(&mut self) {
        self.visible_indices.clear();
        for (idx, entry) in self.entries.iter().enumerate() {
            if self.filter.matches(entry) {
                self.visible_indices.push(idx);
            }
        }
        if self.visible_indices.is_empty() {
            self.selected = 0;
            self.list_state = ListState::default();
            return;
        }
        if self.selected >= self.visible_indices.len() {
            self.selected = self.visible_indices.len() - 1;
        }
        self.sync_state();
    }

    fn sync_state(&mut self) {
        if self.visible_indices.is_empty() {
            self.list_state = ListState::default();
            return;
        }
        if self.selected >= self.visible_indices.len() {
            self.selected = self.visible_indices.len() - 1;
        }
        self.list_state.select(Some(self.selected));
        let max_offset = self.visible_indices.len().saturating_sub(1);
        if self.list_state.offset() > max_offset {
            *self.list_state.offset_mut() = max_offset;
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if self.visible_indices.is_empty() {
            return;
        }
        let len = self.visible_indices.len() as isize;
        let mut next = self.selected as isize + delta;
        if next < 0 {
            next = 0;
        } else if next >= len {
            next = len - 1;
        }
        self.selected = next as usize;
        self.sync_state();
    }

    fn move_to_start(&mut self) {
        if !self.visible_indices.is_empty() {
            self.selected = 0;
            self.sync_state();
        }
    }

    fn move_to_end(&mut self) {
        if !self.visible_indices.is_empty() {
            self.selected = self.visible_indices.len() - 1;
            self.sync_state();
        }
    }

    fn set_filter(&mut self, filter: FilterMode) {
        self.filter = filter;
        self.rebuild_visible();
        self.source_view
            .sync_claim_cursor(&self.entries, self.filter);
    }

    fn selected_claim_index(&self) -> Option<usize> {
        self.visible_indices.get(self.selected).copied()
    }

    fn focus_list_on_claim(&mut self, claim_idx: usize) {
        if let Some(pos) = self
            .visible_indices
            .iter()
            .position(|idx| *idx == claim_idx)
        {
            self.selected = pos;
            self.sync_state();
        }
    }

    fn focus_source_on_claim(&mut self, claim_idx: usize) {
        let entry = match self.entries.get(claim_idx) {
            Some(entry) => entry,
            None => return,
        };
        let path = entry.claim.source.path.as_str();
        if !self.source_view.set_active_by_path(path) {
            return;
        }
        if let Some(line) = entry.claim.source.line {
            let line_idx = line.saturating_sub(1) as usize;
            self.source_view.select_line(line_idx);
        }
        self.source_view
            .select_claim(claim_idx, &self.entries, self.filter);
    }

    fn toggle_view_mode(&mut self) {
        match self.view_mode {
            ViewMode::Claims => {
                if let Some(claim_idx) = self.selected_claim_index() {
                    self.focus_source_on_claim(claim_idx);
                }
                self.view_mode = ViewMode::Source;
            }
            ViewMode::Source => {
                if let Some(claim_idx) = self
                    .source_view
                    .selected_claim_index(&self.entries, self.filter)
                {
                    self.focus_list_on_claim(claim_idx);
                }
                self.view_mode = ViewMode::Claims;
            }
        }
    }
}

/// Launch the inspection TUI for claims, optionally with validation results.
pub fn run(claims_path: &Path, results_path: Option<&Path>) -> Result<()> {
    let entries = load_entries(claims_path, results_path)?;
    let mut terminal = setup_terminal()?;
    let result = render_loop(&mut terminal, App::new(entries));
    cleanup_terminal(&mut terminal)?;
    result
}

fn load_entries(claims_path: &Path, results_path: Option<&Path>) -> Result<Vec<Entry>> {
    let claims_file: ClaimsFile =
        read_json(claims_path).with_context(|| format!("read claims {}", claims_path.display()))?;
    let results_map = results_path
        .map(|path| {
            read_json::<ValidationReport>(path)
                .with_context(|| format!("read results {}", path.display()))
        })
        .transpose()?
        .map(|report| {
            report
                .results
                .into_iter()
                .map(|result| (result.claim_id.clone(), result))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();

    let mut entries = Vec::with_capacity(claims_file.claims.len());
    for claim in claims_file.claims {
        let result = results_map.get(&claim.id).cloned();
        let status = status_from_claim(&claim, result.as_ref());
        entries.push(Entry {
            claim,
            result,
            status,
        });
    }
    Ok(entries)
}

fn build_sources(entries: &[Entry]) -> Vec<SourceFile> {
    let mut sources = Vec::new();
    let mut index_by_path: HashMap<String, usize> = HashMap::new();

    for (claim_idx, entry) in entries.iter().enumerate() {
        let path = entry.claim.source.path.as_str();
        let line = match entry.claim.source.line {
            Some(line) if line > 0 => line as usize,
            _ => continue,
        };
        if !Path::new(path).is_file() {
            continue;
        }
        let source_idx = if let Some(idx) = index_by_path.get(path) {
            *idx
        } else {
            let content = match std::fs::read_to_string(path) {
                Ok(content) => content,
                Err(_) => continue,
            };
            let lines: Vec<String> = content.lines().map(|line| line.to_string()).collect();
            let line_claims = vec![Vec::new(); lines.len()];
            let idx = sources.len();
            sources.push(SourceFile {
                path: path.to_string(),
                lines,
                line_claims,
            });
            index_by_path.insert(path.to_string(), idx);
            idx
        };

        if let Some(source) = sources.get_mut(source_idx) {
            if line == 0 || line > source.lines.len() {
                continue;
            }
            source.line_claims[line - 1].push(claim_idx);
        }
    }

    sources
}

fn status_from_claim(claim: &Claim, result: Option<&ValidationResult>) -> DisplayStatus {
    if let Some(result) = result {
        return match result.status {
            ValidationStatus::Confirmed => DisplayStatus::Confirmed,
            ValidationStatus::Refuted => DisplayStatus::Refuted,
            ValidationStatus::Undetermined => DisplayStatus::Undetermined,
        };
    }
    match claim.status {
        ClaimStatus::Confirmed => DisplayStatus::Confirmed,
        ClaimStatus::Refuted => DisplayStatus::Refuted,
        ClaimStatus::Undetermined => DisplayStatus::Undetermined,
        ClaimStatus::Unvalidated => DisplayStatus::Unvalidated,
    }
}

fn render_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    mut app: App,
) -> Result<()> {
    let mut needs_redraw = true;
    loop {
        if needs_redraw {
            terminal.draw(|f| render_app(f, &mut app))?;
            needs_redraw = false;
        }
        if event::poll(Duration::from_millis(250))? {
            match event::read()? {
                Event::Key(key) => {
                    if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                        continue;
                    }
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => break,
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            break
                        }
                        KeyCode::Char('t') => app.toggle_view_mode(),
                        KeyCode::Up => match app.view_mode {
                            ViewMode::Claims => app.move_selection(-1),
                            ViewMode::Source => app.source_view.move_selection(-1),
                        },
                        KeyCode::Down => match app.view_mode {
                            ViewMode::Claims => app.move_selection(1),
                            ViewMode::Source => app.source_view.move_selection(1),
                        },
                        KeyCode::PageUp => match app.view_mode {
                            ViewMode::Claims => app.move_selection(-20),
                            ViewMode::Source => app.source_view.move_selection(-20),
                        },
                        KeyCode::PageDown => match app.view_mode {
                            ViewMode::Claims => app.move_selection(20),
                            ViewMode::Source => app.source_view.move_selection(20),
                        },
                        KeyCode::Home | KeyCode::Char('g') => match app.view_mode {
                            ViewMode::Claims => app.move_to_start(),
                            ViewMode::Source => app.source_view.move_to_start(),
                        },
                        KeyCode::End | KeyCode::Char('G') => match app.view_mode {
                            ViewMode::Claims => app.move_to_end(),
                            ViewMode::Source => app.source_view.move_to_end(),
                        },
                        KeyCode::Tab => {
                            if matches!(app.view_mode, ViewMode::Source) {
                                app.source_view.cycle_claim(&app.entries, app.filter);
                            }
                        }
                        KeyCode::Char('a') => app.set_filter(FilterMode::All),
                        KeyCode::Char('v') => app.set_filter(FilterMode::Validated),
                        KeyCode::Char('c') => app.set_filter(FilterMode::Confirmed),
                        KeyCode::Char('r') => app.set_filter(FilterMode::Refuted),
                        KeyCode::Char('u') => app.set_filter(FilterMode::Undetermined),
                        KeyCode::Char('n') => app.set_filter(FilterMode::Unvalidated),
                        _ => {}
                    }
                    needs_redraw = true;
                }
                Event::Resize(_, _) => needs_redraw = true,
                _ => {}
            }
        }
    }
    Ok(())
}

fn render_app(frame: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
        .split(frame.size());

    match app.view_mode {
        ViewMode::Claims => render_claim_list(frame, chunks[0], app),
        ViewMode::Source => render_source_view(frame, chunks[0], app),
    }
    render_details(frame, chunks[1], app);
}

fn render_claim_list(frame: &mut Frame, area: Rect, app: &mut App) {
    app.sync_state();
    let items: Vec<ListItem> = app
        .visible_indices
        .iter()
        .filter_map(|idx| app.entries.get(*idx))
        .map(render_list_item)
        .collect();

    let total = app.entries.len();
    let visible = app.visible_indices.len();
    let title = format!(
        "Claims [filter: {}] [showing {visible}/{total}] {LIST_LEGEND}",
        app.filter.label()
    );
    let block = Block::default().title(title).borders(Borders::ALL);
    let list = List::new(items).block(block).highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );

    frame.render_stateful_widget(list, area, &mut app.list_state);
}

fn render_source_view(frame: &mut Frame, area: Rect, app: &mut App) {
    if !app.source_view.has_sources() {
        let title = format!("Source [filter: {}] {SOURCE_LEGEND}", app.filter.label());
        let block = Block::default().title(title).borders(Borders::ALL);
        let widget = Paragraph::new(Text::raw("No source files available."))
            .block(block)
            .wrap(Wrap { trim: false });
        frame.render_widget(widget, area);
        return;
    }

    app.source_view.sync_state();
    let Some(source) = app.source_view.active_source() else {
        return;
    };
    let title = format!(
        "Source: {} [filter: {}] {SOURCE_LEGEND}",
        source.path,
        app.filter.label()
    );
    let block = Block::default().title(title).borders(Borders::ALL);
    let items: Vec<ListItem> = source
        .lines
        .iter()
        .enumerate()
        .map(|(idx, line)| render_source_line(app, idx, line))
        .collect();
    let list = List::new(items).block(block).highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_stateful_widget(list, area, &mut app.source_view.list_state);
}

fn render_list_item(entry: &Entry) -> ListItem<'static> {
    ListItem::new(render_list_line(entry))
}

fn render_list_line(entry: &Entry) -> Line<'static> {
    let mut spans = Vec::new();
    spans.push(status_marker(entry.status));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        entry.claim.id.clone(),
        Style::default().fg(Color::White),
    ));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        entry.claim.text.clone(),
        Style::default().fg(Color::Gray),
    ));
    Line::from(spans)
}

fn render_source_line(app: &App, line_idx: usize, line: &str) -> ListItem<'static> {
    let mut spans = Vec::new();
    let status = app
        .source_view
        .line_status(&app.entries, app.filter, line_idx);
    let marker = status
        .map(|(status, _)| status_marker(status))
        .unwrap_or_else(|| Span::styled("   ", Style::default().fg(Color::DarkGray)));
    spans.push(marker);
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        format!("{:>4} | ", line_idx + 1),
        Style::default().fg(Color::Yellow),
    ));
    spans.push(Span::raw(line.to_string()));
    if let Some((_, count)) = status {
        if count > 1 {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                format!("({count} claims)"),
                Style::default().fg(Color::DarkGray),
            ));
        }
    }
    ListItem::new(Line::from(spans))
}

fn render_details(frame: &mut Frame, area: Rect, app: &mut App) {
    let detail = match app.view_mode {
        ViewMode::Claims => {
            let entry_idx = app.visible_indices.get(app.selected).copied();
            if let Some(idx) = entry_idx {
                let entry = &app.entries[idx];
                Text::from(detail_lines(entry, &mut app.help_cache))
            } else {
                Text::raw("no selection")
            }
        }
        ViewMode::Source => source_detail_text(app),
    };

    let widget = Paragraph::new(detail)
        .block(Block::default().title("Details").borders(Borders::ALL))
        .wrap(Wrap { trim: false });
    frame.render_widget(widget, area);
}

fn detail_lines(entry: &Entry, cache: &mut HelpCache) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    lines.push(Line::from(vec![
        Span::raw("Status: "),
        status_span(entry.status),
    ]));
    lines.push(Line::from(format!("Claim: {}", entry.claim.id)));
    lines.push(Line::from(format!("Text: {}", entry.claim.text)));
    let source_line = entry
        .claim
        .source
        .line
        .map(|line| line.to_string())
        .unwrap_or_else(|| "<none>".to_string());
    lines.push(Line::from(format!(
        "Source: {} {}:{}",
        source_type_label(&entry.claim.source.source_type),
        entry.claim.source.path,
        source_line
    )));
    lines.push(Line::from(format!("Extractor: {}", entry.claim.extractor)));
    if let Some(confidence) = entry.claim.confidence {
        lines.push(Line::from(format!("Confidence: {:.2}", confidence)));
    }
    lines.push(Line::from("Raw excerpt:"));
    lines.push(Line::from(entry.claim.raw_excerpt.clone()));

    lines.push(Line::from(""));
    if let Some(result) = &entry.result {
        lines.extend(render_validation_details(result));
    } else {
        lines.push(Line::from("Validation: <none>"));
    }

    lines.push(Line::from(""));
    lines.push(Line::from("Help context:"));
    if let Some(line) = entry.claim.source.line {
        if let Some(context) = cache.context_lines(&entry.claim.source.path, line, 2) {
            lines.extend(context);
        } else {
            lines.push(Line::from("  <no help context available>"));
        }
    } else {
        lines.push(Line::from("  <no line information available>"));
    }

    lines
}

fn source_detail_text(app: &mut App) -> Text<'static> {
    let detail = app.source_view.detail_snapshot(&app.entries, app.filter);
    let mut lines = Vec::new();
    if let Some(path) = detail.source_path.as_ref() {
        lines.push(Line::from(format!("Source file: {path}")));
    } else {
        lines.push(Line::from("Source file: <none>"));
    }
    if let (Some(line_no), Some(line_text)) = (detail.line_no, detail.line_text.as_ref()) {
        lines.push(Line::from(format!("Line {line_no}: {line_text}")));
    }
    if detail.claim_count > 0 {
        lines.push(Line::from(format!(
            "Claims on line: {} (showing {})",
            detail.claim_count,
            detail.claim_cursor + 1
        )));
    } else {
        lines.push(Line::from("Claims on line: 0"));
    }
    lines.push(Line::from(""));

    if let Some(claim_idx) = detail.claim_idx {
        if let Some(entry) = app.entries.get(claim_idx) {
            lines.extend(detail_lines(entry, &mut app.help_cache));
        } else {
            lines.push(Line::from("Claim: <missing entry>"));
        }
    } else {
        lines.push(Line::from("No claim on this line."));
    }

    Text::from(lines)
}

fn render_validation_details(result: &ValidationResult) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    lines.push(Line::from("Validation:"));
    lines.push(Line::from(format!(
        "  status = {}",
        validation_status_label(&result.status)
    )));
    lines.push(Line::from(format!(
        "  method = {}",
        validation_method_label(&result.method)
    )));
    if let Some(determinism) = result.determinism.as_ref() {
        lines.push(Line::from(format!(
            "  determinism = {}",
            determinism_label(determinism)
        )));
    }

    if result.attempts.is_empty() {
        lines.push(Line::from("  attempts = <none>"));
        return lines;
    }

    let attempt = &result.attempts[0];
    lines.push(Line::from(format!(
        "  attempts = {} (showing first)",
        result.attempts.len()
    )));
    lines.extend(render_attempt(attempt));
    lines
}

fn render_attempt(attempt: &Evidence) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    lines.push(Line::from(format!("    args = {}", attempt.args.join(" "))));
    lines.push(Line::from(format!(
        "    env = {}",
        format_env(&attempt.env)
    )));
    lines.push(Line::from(format!(
        "    exit_code = {}",
        attempt
            .exit_code
            .map(|code| code.to_string())
            .unwrap_or_else(|| "<none>".to_string())
    )));
    if let Some(stdout) = &attempt.stdout {
        lines.push(Line::from(format!("    stdout = {stdout}")));
    }
    if let Some(stderr) = &attempt.stderr {
        lines.push(Line::from(format!("    stderr = {stderr}")));
    }
    if let Some(notes) = &attempt.notes {
        lines.push(Line::from(format!("    notes = {notes}")));
    }
    lines
}

fn format_env(env: &BTreeMap<String, String>) -> String {
    if env.is_empty() {
        return "<empty>".to_string();
    }
    env.iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn status_marker(status: DisplayStatus) -> Span<'static> {
    match status {
        DisplayStatus::Confirmed => Span::styled("[C]", Style::default().fg(Color::Green)),
        DisplayStatus::Refuted => Span::styled("[R]", Style::default().fg(Color::Red)),
        DisplayStatus::Undetermined => Span::styled("[U]", Style::default().fg(Color::Yellow)),
        DisplayStatus::Unvalidated => Span::styled("[ ]", Style::default().fg(Color::DarkGray)),
    }
}

fn status_span(status: DisplayStatus) -> Span<'static> {
    let label = status.label();
    let color = match status {
        DisplayStatus::Confirmed => Color::Green,
        DisplayStatus::Refuted => Color::Red,
        DisplayStatus::Undetermined => Color::Yellow,
        DisplayStatus::Unvalidated => Color::DarkGray,
    };
    Span::styled(label.to_string(), Style::default().fg(color))
}

fn source_type_label(source_type: &ClaimSourceType) -> &'static str {
    match source_type {
        ClaimSourceType::Man => "man",
        ClaimSourceType::Help => "help",
        ClaimSourceType::Source => "source",
    }
}

fn validation_status_label(status: &ValidationStatus) -> &'static str {
    match status {
        ValidationStatus::Confirmed => "confirmed",
        ValidationStatus::Refuted => "refuted",
        ValidationStatus::Undetermined => "undetermined",
    }
}

fn validation_method_label(method: &ValidationMethod) -> &'static str {
    match method {
        ValidationMethod::AcceptanceTest => "acceptance_test",
        ValidationMethod::BehaviorFixture => "behavior_fixture",
        ValidationMethod::StderrMatch => "stderr_match",
        ValidationMethod::ExitCode => "exit_code",
        ValidationMethod::OutputDiff => "output_diff",
        ValidationMethod::Other => "other",
    }
}

fn determinism_label(determinism: &Determinism) -> &'static str {
    match determinism {
        Determinism::Deterministic => "deterministic",
        Determinism::EnvSensitive => "env_sensitive",
        Determinism::Flaky => "flaky",
    }
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<std::io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

fn cleanup_terminal(terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    Ok(())
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let content = std::fs::read_to_string(path)?;
    let value = serde_json::from_str(&content)?;
    Ok(value)
}
