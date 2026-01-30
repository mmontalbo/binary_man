use crate::cli::InspectArgs;
use crate::docpack::doc_pack_root_for_status;
use crate::enrich::{self, DocPackPaths};
use crate::scenarios;
use crate::workflow;
use anyhow::{Context, Result};
use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Tabs, Wrap};
use ratatui::Frame;
use shell_words::split as shell_split;
use std::cmp::min;
use std::fs;
use std::io::{self, IsTerminal, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const PREVIEW_LIMIT: usize = 10;
const PREVIEW_MAX_LINES: usize = 2;
const PREVIEW_MAX_CHARS: usize = 160;
const EVENT_POLL_MS: u64 = 200;

pub fn run(args: InspectArgs) -> Result<()> {
    let doc_pack_root = doc_pack_root_for_status(&args.doc_pack)?;
    if !io::stdout().is_terminal() || !io::stdin().is_terminal() {
        return run_text_summary(&doc_pack_root);
    }
    run_tui(doc_pack_root)
}

fn run_text_summary(doc_pack_root: &Path) -> Result<()> {
    let show_all = [false; 4];
    let (summary, data) = load_state(doc_pack_root, &show_all)?;
    print_text_summary(doc_pack_root, &summary, &data)
}

fn run_tui(doc_pack_root: PathBuf) -> Result<()> {
    let show_all = [false; 4];
    let (summary, data) = load_state(&doc_pack_root, &show_all)?;
    let mut app = App::new(doc_pack_root, summary, data);

    let mut guard = TerminalGuard::enter()?;
    let mut terminal = {
        let stdout = io::stdout();
        let backend = CrosstermBackend::new(stdout);
        ratatui::Terminal::new(backend).context("init terminal")?
    };

    loop {
        terminal
            .draw(|frame| app.draw(frame))
            .context("draw inspect ui")?;

        if event::poll(Duration::from_millis(EVENT_POLL_MS)).context("poll event")? {
            if let Event::Key(key) = event::read().context("read event")? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if let Some(action) = action_from_key(key) {
                    match action {
                        Action::Quit => break,
                        Action::NextTab => app.next_tab(),
                        Action::PrevTab => app.prev_tab(),
                        Action::Up => app.move_selection(-1),
                        Action::Down => app.move_selection(1),
                        Action::Refresh => {
                            if let Err(err) = app.refresh() {
                                app.set_message(format!("refresh failed: {err}"));
                            } else {
                                app.set_message("refreshed".to_string());
                            }
                        }
                        Action::ToggleHelp => app.toggle_help(),
                        Action::ToggleShowAll => {
                            if let Err(err) = app.toggle_show_all() {
                                app.set_message(format!("show all failed: {err}"));
                            }
                        }
                        Action::OpenEditor => {
                            if let Err(err) = app.open_selected_in_editor(&mut guard, &mut terminal)
                            {
                                app.set_message(format!("open editor failed: {err}"));
                            }
                        }
                        Action::OpenPager => {
                            if let Err(err) = app.open_selected_in_pager(&mut guard, &mut terminal)
                            {
                                app.set_message(format!("open pager failed: {err}"));
                            }
                        }
                        Action::OpenMan => {
                            if let Err(err) = app.open_man_page(&mut guard, &mut terminal) {
                                app.set_message(format!("open man failed: {err}"));
                            }
                        }
                        Action::Copy => {
                            if let Err(err) = app.copy_selected(&mut guard, &mut terminal) {
                                app.set_message(format!("copy failed: {err}"));
                            }
                        }
                    }
                }
            }
        }
    }

    drop(guard);
    terminal.show_cursor().ok();
    Ok(())
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Tab {
    Intent,
    Evidence,
    Outputs,
    History,
}

impl Tab {
    const ALL: [Tab; 4] = [Tab::Intent, Tab::Evidence, Tab::Outputs, Tab::History];

    fn index(self) -> usize {
        match self {
            Tab::Intent => 0,
            Tab::Evidence => 1,
            Tab::Outputs => 2,
            Tab::History => 3,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Tab::Intent => "Intent",
            Tab::Evidence => "Evidence",
            Tab::Outputs => "Outputs",
            Tab::History => "History/Audit",
        }
    }

    fn color(self) -> Color {
        match self {
            Tab::Intent => Color::Cyan,
            Tab::Evidence => Color::Yellow,
            Tab::Outputs => Color::Green,
            Tab::History => Color::Magenta,
        }
    }
}

#[derive(Debug, Clone)]
struct ArtifactEntry {
    rel_path: String,
    path: PathBuf,
    exists: bool,
}

#[derive(Debug, Clone)]
struct EvidenceEntry {
    scenario_id: String,
    path: Option<PathBuf>,
    exists: bool,
    exit_code: Option<i32>,
    exit_signal: Option<i32>,
    timed_out: Option<bool>,
    stdout_preview: Option<String>,
    stderr_preview: Option<String>,
    error: Option<String>,
}

#[derive(Debug)]
struct EvidenceList {
    total_count: usize,
    entries: Vec<EvidenceEntry>,
}

#[derive(Debug)]
struct InspectData {
    intent: Vec<ArtifactEntry>,
    evidence: EvidenceList,
    outputs: Vec<ArtifactEntry>,
    history: Vec<ArtifactEntry>,
    man_warnings: Vec<String>,
    last_history: Option<HistoryEntryPreview>,
    last_txn_id: Option<String>,
    man_page_path: Option<PathBuf>,
}

impl InspectData {
    fn load(
        doc_pack_root: &Path,
        summary: &enrich::StatusSummary,
        show_all: &[bool; 4],
    ) -> Result<Self> {
        let paths = DocPackPaths::new(doc_pack_root.to_path_buf());
        let intent = build_intent_entries(&paths)?;
        let evidence = build_evidence_entries(&paths, show_all[Tab::Evidence.index()])?;
        let man_page_path = resolve_man_page_path(&paths, summary.binary_name.as_deref());
        let outputs = build_output_entries(&paths, &man_page_path)?;
        let history = build_history_entries(&paths)?;
        let last_history = read_last_history_entry(&paths).unwrap_or(None);
        let last_txn_id = find_last_txn_id(&paths);
        Ok(Self {
            intent,
            evidence,
            outputs,
            history,
            man_warnings: summary.man_warnings.clone(),
            last_history,
            last_txn_id,
            man_page_path,
        })
    }
}

struct App {
    doc_pack_root: PathBuf,
    summary: enrich::StatusSummary,
    data: InspectData,
    tab: Tab,
    selection: [usize; 4],
    show_all: [bool; 4],
    message: Option<String>,
    show_help: bool,
}

impl App {
    fn new(doc_pack_root: PathBuf, summary: enrich::StatusSummary, data: InspectData) -> Self {
        Self {
            doc_pack_root,
            summary,
            data,
            tab: Tab::Intent,
            selection: [0; 4],
            show_all: [false; 4],
            message: None,
            show_help: false,
        }
    }

    fn refresh(&mut self) -> Result<()> {
        let (summary, data) = load_state(&self.doc_pack_root, &self.show_all)?;
        self.summary = summary;
        self.data = data;
        self.clamp_selection();
        Ok(())
    }

    fn toggle_show_all(&mut self) -> Result<()> {
        let idx = self.tab.index();
        self.show_all[idx] = !self.show_all[idx];
        self.refresh()
    }

    fn next_tab(&mut self) {
        let idx = (self.tab.index() + 1) % Tab::ALL.len();
        self.tab = Tab::ALL[idx];
        self.clamp_selection();
    }

    fn prev_tab(&mut self) {
        let idx = if self.tab.index() == 0 {
            Tab::ALL.len() - 1
        } else {
            self.tab.index() - 1
        };
        self.tab = Tab::ALL[idx];
        self.clamp_selection();
    }

    fn move_selection(&mut self, delta: isize) {
        let idx = self.tab.index();
        let max = self.visible_items_len(self.tab);
        if max == 0 {
            self.selection[idx] = 0;
            return;
        }
        let current = self.selection[idx] as isize;
        let next = current + delta;
        let clamped = if next < 0 {
            0
        } else if next as usize >= max {
            max as isize - 1
        } else {
            next
        };
        self.selection[idx] = clamped as usize;
    }

    fn clamp_selection(&mut self) {
        for tab in Tab::ALL.iter().copied() {
            let idx = tab.index();
            let max = self.visible_items_len(tab);
            if max == 0 {
                self.selection[idx] = 0;
            } else if self.selection[idx] >= max {
                self.selection[idx] = max - 1;
            }
        }
    }

    fn visible_items_len(&self, tab: Tab) -> usize {
        match tab {
            Tab::Intent => visible_len(self.data.intent.len(), self.show_all[tab.index()]),
            Tab::Evidence => {
                visible_len(self.data.evidence.entries.len(), self.show_all[tab.index()])
            }
            Tab::Outputs => visible_len(self.data.outputs.len(), self.show_all[tab.index()]),
            Tab::History => visible_len(self.data.history.len(), self.show_all[tab.index()]),
        }
    }

    fn selected_artifact(&self) -> Option<ArtifactEntry> {
        let idx = self.tab.index();
        match self.tab {
            Tab::Intent => self.data.intent.get(self.selection[idx]).cloned(),
            Tab::Outputs => self.data.outputs.get(self.selection[idx]).cloned(),
            Tab::History => self.data.history.get(self.selection[idx]).cloned(),
            Tab::Evidence => None,
        }
    }

    fn selected_evidence(&self) -> Option<EvidenceEntry> {
        if self.tab != Tab::Evidence {
            return None;
        }
        self.data
            .evidence
            .entries
            .get(self.selection[self.tab.index()])
            .cloned()
    }

    fn selected_copy_target(&self) -> Option<String> {
        if let Some(artifact) = self.selected_artifact() {
            return Some(artifact.path.display().to_string());
        }
        if let Some(evidence) = self.selected_evidence() {
            if let Some(path) = evidence.path.as_ref() {
                return Some(path.display().to_string());
            }
        }
        Some(next_action_copy(&self.summary.next_action))
    }

    fn open_selected_in_editor(
        &mut self,
        guard: &mut TerminalGuard,
        terminal: &mut ratatui::Terminal<CrosstermBackend<io::Stdout>>,
    ) -> Result<()> {
        if let Some(artifact) = self.selected_artifact() {
            return run_external(guard, terminal, || open_in_editor(&artifact.path));
        }
        if let Some(evidence) = self.selected_evidence() {
            if let Some(path) = evidence.path.as_ref() {
                return run_external(guard, terminal, || open_in_editor(path));
            }
            return Err(anyhow::anyhow!("no evidence file to open"));
        }
        Err(anyhow::anyhow!("no file selected"))
    }

    fn open_selected_in_pager(
        &mut self,
        guard: &mut TerminalGuard,
        terminal: &mut ratatui::Terminal<CrosstermBackend<io::Stdout>>,
    ) -> Result<()> {
        if let Some(artifact) = self.selected_artifact() {
            return run_external(guard, terminal, || open_in_pager(&artifact.path));
        }
        if let Some(evidence) = self.selected_evidence() {
            if let Some(path) = evidence.path.as_ref() {
                return run_external(guard, terminal, || open_in_pager(path));
            }
            return Err(anyhow::anyhow!("no evidence file to open"));
        }
        Err(anyhow::anyhow!("no file selected"))
    }

    fn open_man_page(
        &mut self,
        guard: &mut TerminalGuard,
        terminal: &mut ratatui::Terminal<CrosstermBackend<io::Stdout>>,
    ) -> Result<()> {
        let Some(path) = self.data.man_page_path.as_ref() else {
            return Err(anyhow::anyhow!("no man page found"));
        };
        run_external(guard, terminal, || open_man_page(path))
    }

    fn copy_selected(
        &mut self,
        guard: &mut TerminalGuard,
        terminal: &mut ratatui::Terminal<CrosstermBackend<io::Stdout>>,
    ) -> Result<()> {
        let Some(target) = self.selected_copy_target() else {
            return Err(anyhow::anyhow!("nothing to copy"));
        };
        if try_copy_to_clipboard(&target)? {
            self.set_message("copied to clipboard".to_string());
            return Ok(());
        }
        let output = format!("copy this: {target}");
        run_external(guard, terminal, || {
            println!("{output}");
            io::stdout().flush().ok();
            Ok(())
        })?;
        self.set_message("copy line printed to stdout".to_string());
        Ok(())
    }

    fn toggle_help(&mut self) {
        self.show_help = !self.show_help;
    }

    fn set_message(&mut self, message: String) {
        self.message = Some(message);
    }

    fn draw(&mut self, frame: &mut Frame) {
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
                    .fg(tab.color())
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
        let visible = visible_len(total, self.show_all[self.tab.index()]);
        let title = list_title("Intent", total, visible, self.show_all[self.tab.index()]);
        let items = self
            .data
            .intent
            .iter()
            .take(visible)
            .map(intent_list_item)
            .collect::<Vec<_>>();
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(title))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        let mut state = ListState::default();
        state.select(Some(self.selection[self.tab.index()]));
        frame.render_stateful_widget(list, area, &mut state);
    }

    fn draw_evidence(&mut self, frame: &mut Frame, area: Rect) {
        let total = self.data.evidence.total_count;
        let visible = visible_len(
            self.data.evidence.entries.len(),
            self.show_all[self.tab.index()],
        );
        let title = list_title("Evidence", total, visible, self.show_all[self.tab.index()]);
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
        frame.render_stateful_widget(list, area, &mut state);
    }

    fn draw_outputs(&mut self, frame: &mut Frame, area: Rect) {
        let has_warnings = !self.data.man_warnings.is_empty();
        let list_area = if has_warnings {
            let layout = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(min(self.data.man_warnings.len() as u16 + 1, 4)),
                    Constraint::Min(2),
                ])
                .split(area);
            let warnings = self
                .data
                .man_warnings
                .iter()
                .take(3)
                .map(|warning| Line::from(format!("warning: {warning}")))
                .collect::<Vec<_>>();
            let paragraph = Paragraph::new(warnings)
                .block(Block::default().borders(Borders::ALL).title("Man warnings"));
            frame.render_widget(paragraph, layout[0]);
            layout[1]
        } else {
            area
        };

        let total = self.data.outputs.len();
        let visible = visible_len(total, self.show_all[self.tab.index()]);
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
        frame.render_stateful_widget(list, list_area, &mut state);
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
        let visible = visible_len(total, self.show_all[self.tab.index()]);
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
            "q quit | tab switch | enter view | o edit | m man | c copy | r refresh | ? help"
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
            Line::from("  c: copy selected path or next action"),
            Line::from("  r: refresh"),
            Line::from("  a: toggle show all"),
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

#[derive(Debug, PartialEq, Eq)]
enum Action {
    Quit,
    NextTab,
    PrevTab,
    Up,
    Down,
    Refresh,
    OpenEditor,
    OpenPager,
    OpenMan,
    Copy,
    ToggleHelp,
    ToggleShowAll,
}

fn action_from_key(key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => Some(Action::Quit),
        KeyCode::Tab => Some(Action::NextTab),
        KeyCode::BackTab => Some(Action::PrevTab),
        KeyCode::Up | KeyCode::Char('k') => Some(Action::Up),
        KeyCode::Down | KeyCode::Char('j') => Some(Action::Down),
        KeyCode::Char('r') => Some(Action::Refresh),
        KeyCode::Char('o') => Some(Action::OpenEditor),
        KeyCode::Enter => Some(Action::OpenPager),
        KeyCode::Char('m') => Some(Action::OpenMan),
        KeyCode::Char('c') => Some(Action::Copy),
        KeyCode::Char('?') => Some(Action::ToggleHelp),
        KeyCode::Char('a') => Some(Action::ToggleShowAll),
        _ => None,
    }
}

fn load_state(
    doc_pack_root: &Path,
    show_all: &[bool; 4],
) -> Result<(enrich::StatusSummary, InspectData)> {
    let computation =
        workflow::status_summary_for_doc_pack(doc_pack_root.to_path_buf(), false, false)?;
    let summary = computation.summary;
    let data = InspectData::load(doc_pack_root, &summary, show_all)?;
    Ok((summary, data))
}

fn build_intent_entries(paths: &DocPackPaths) -> Result<Vec<ArtifactEntry>> {
    let mut entries = Vec::new();
    let add = |entries: &mut Vec<ArtifactEntry>, path: PathBuf| {
        let rel_path = paths
            .rel_path(&path)
            .unwrap_or_else(|_| path.display().to_string());
        let exists = path.exists();
        entries.push(ArtifactEntry {
            rel_path,
            path,
            exists,
        });
    };

    add(&mut entries, paths.scenarios_plan_path());
    add(&mut entries, paths.semantics_path());
    add(&mut entries, paths.config_path());
    add(
        &mut entries,
        paths.root().join("binary_lens").join("export_plan.json"),
    );

    let queries_dir = paths.root().join("queries");
    if queries_dir.is_dir() {
        let mut query_paths = fs::read_dir(&queries_dir)
            .with_context(|| format!("read queries dir {}", queries_dir.display()))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .and_then(|ext| ext.to_str())
                    .map(|ext| ext.eq_ignore_ascii_case("sql"))
                    .unwrap_or(false)
            })
            .collect::<Vec<_>>();
        query_paths.sort();
        for path in query_paths {
            let rel_path = paths
                .rel_path(&path)
                .unwrap_or_else(|_| path.display().to_string());
            let exists = path.exists();
            entries.push(ArtifactEntry {
                rel_path,
                path,
                exists,
            });
        }
    }

    Ok(entries)
}

fn build_output_entries(
    paths: &DocPackPaths,
    man_page_path: &Option<PathBuf>,
) -> Result<Vec<ArtifactEntry>> {
    let mut entries = Vec::new();
    let add = |entries: &mut Vec<ArtifactEntry>, path: PathBuf| {
        let rel_path = paths
            .rel_path(&path)
            .unwrap_or_else(|_| path.display().to_string());
        let exists = path.exists();
        entries.push(ArtifactEntry {
            rel_path,
            path,
            exists,
        });
    };
    add(&mut entries, paths.surface_path());
    add(&mut entries, paths.root().join("verification_ledger.json"));
    add(&mut entries, paths.man_dir().join("meta.json"));
    if let Some(path) = man_page_path.as_ref() {
        let rel_path = paths
            .rel_path(path)
            .unwrap_or_else(|_| path.display().to_string());
        entries.push(ArtifactEntry {
            rel_path,
            path: path.clone(),
            exists: path.exists(),
        });
    }
    Ok(entries)
}

fn build_history_entries(paths: &DocPackPaths) -> Result<Vec<ArtifactEntry>> {
    let mut entries = Vec::new();
    let add = |entries: &mut Vec<ArtifactEntry>, path: PathBuf| {
        let rel_path = paths
            .rel_path(&path)
            .unwrap_or_else(|_| path.display().to_string());
        let exists = path.exists();
        entries.push(ArtifactEntry {
            rel_path,
            path,
            exists,
        });
    };
    add(&mut entries, paths.report_path());
    add(&mut entries, paths.history_path());
    Ok(entries)
}

fn resolve_man_page_path(paths: &DocPackPaths, binary_name: Option<&str>) -> Option<PathBuf> {
    if let Some(name) = binary_name {
        let path = paths.man_page_path(name);
        if path.is_file() {
            return Some(path);
        }
    }
    let man_dir = paths.man_dir();
    let entries = fs::read_dir(&man_dir).ok()?;
    let mut man_pages = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.eq_ignore_ascii_case("1"))
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    man_pages.sort();
    if man_pages.len() == 1 {
        return Some(man_pages.remove(0));
    }
    None
}

#[derive(serde::Deserialize)]
struct EvidencePreview {
    scenario_id: String,
    generated_at_epoch_ms: u128,
    exit_code: Option<i32>,
    exit_signal: Option<i32>,
    timed_out: bool,
    stdout: String,
    stderr: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct HistoryEntryPreview {
    step: String,
    success: bool,
    force_used: bool,
}

fn build_evidence_entries(paths: &DocPackPaths, show_all: bool) -> Result<EvidenceList> {
    let index_path = paths.inventory_scenarios_dir().join("index.json");
    if index_path.is_file() {
        let bytes =
            fs::read(&index_path).with_context(|| format!("read {}", index_path.display()))?;
        let mut index: scenarios::ScenarioIndex = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse {}", index_path.display()))?;
        index
            .scenarios
            .sort_by(|a, b| a.scenario_id.cmp(&b.scenario_id));
        let total_count = index.scenarios.len();
        let entries = index
            .scenarios
            .into_iter()
            .take(if show_all { total_count } else { PREVIEW_LIMIT })
            .map(|entry| evidence_entry_from_index(paths, entry))
            .collect::<Vec<_>>();
        return Ok(EvidenceList {
            total_count,
            entries,
        });
    }

    let scenarios_dir = paths.inventory_scenarios_dir();
    let mut map: std::collections::BTreeMap<String, (u128, EvidenceEntry)> =
        std::collections::BTreeMap::new();
    if scenarios_dir.is_dir() {
        for entry in fs::read_dir(&scenarios_dir)
            .with_context(|| format!("read {}", scenarios_dir.display()))?
            .filter_map(Result::ok)
        {
            let path = entry.path();
            if path.file_name().and_then(|name| name.to_str()) == Some("index.json") {
                continue;
            }
            if path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| !ext.eq_ignore_ascii_case("json"))
                .unwrap_or(true)
            {
                continue;
            }
            let preview = read_evidence_preview(&path);
            let entry = match preview {
                Ok(preview) => {
                    let generated_at = preview.generated_at_epoch_ms;
                    let entry = evidence_entry_from_preview(&path, preview);
                    map.entry(entry.scenario_id.clone())
                        .and_modify(|(existing_at, existing)| {
                            if generated_at > *existing_at {
                                *existing_at = generated_at;
                                *existing = entry.clone();
                            }
                        })
                        .or_insert((generated_at, entry));
                    continue;
                }
                Err(err) => EvidenceEntry {
                    scenario_id: path
                        .file_stem()
                        .and_then(|stem| stem.to_str())
                        .unwrap_or("unknown")
                        .to_string(),
                    path: Some(path.clone()),
                    exists: true,
                    exit_code: None,
                    exit_signal: None,
                    timed_out: None,
                    stdout_preview: None,
                    stderr_preview: None,
                    error: Some(err.to_string()),
                },
            };
            map.entry(entry.scenario_id.clone()).or_insert((0, entry));
        }
    }
    let total_count = map.len();
    let entries = map
        .into_values()
        .map(|(_, entry)| entry)
        .take(if show_all { total_count } else { PREVIEW_LIMIT })
        .collect();
    Ok(EvidenceList {
        total_count,
        entries,
    })
}

fn evidence_entry_from_index(
    paths: &DocPackPaths,
    entry: scenarios::ScenarioIndexEntry,
) -> EvidenceEntry {
    let evidence_path = entry
        .evidence_paths
        .last()
        .map(|rel| paths.root().join(rel));
    if let Some(path) = evidence_path.as_ref() {
        if let Ok(preview) = read_evidence_preview(path) {
            return evidence_entry_from_preview(path, preview);
        }
        return EvidenceEntry {
            scenario_id: entry.scenario_id,
            path: Some(path.clone()),
            exists: path.exists(),
            exit_code: None,
            exit_signal: None,
            timed_out: None,
            stdout_preview: None,
            stderr_preview: None,
            error: Some("failed to parse evidence".to_string()),
        };
    }
    EvidenceEntry {
        scenario_id: entry.scenario_id,
        path: None,
        exists: false,
        exit_code: None,
        exit_signal: None,
        timed_out: None,
        stdout_preview: None,
        stderr_preview: None,
        error: Some("no evidence".to_string()),
    }
}

fn evidence_entry_from_preview(path: &Path, preview: EvidencePreview) -> EvidenceEntry {
    EvidenceEntry {
        scenario_id: preview.scenario_id,
        path: Some(path.to_path_buf()),
        exists: true,
        exit_code: preview.exit_code,
        exit_signal: preview.exit_signal,
        timed_out: Some(preview.timed_out),
        stdout_preview: Some(preview_text(&preview.stdout)),
        stderr_preview: Some(preview_text(&preview.stderr)),
        error: None,
    }
}

fn read_evidence_preview(path: &Path) -> Result<EvidencePreview> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let preview: EvidencePreview =
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
    Ok(preview)
}

fn read_last_history_entry(paths: &DocPackPaths) -> Result<Option<HistoryEntryPreview>> {
    let path = paths.history_path();
    if !path.is_file() {
        return Ok(None);
    }
    let tail = read_tail(&path, 16 * 1024)?;
    let line = tail.lines().rev().find(|line| !line.trim().is_empty());
    let Some(line) = line else {
        return Ok(None);
    };
    let entry: HistoryEntryPreview = serde_json::from_str(line)
        .with_context(|| format!("parse history line from {}", path.display()))?;
    Ok(Some(entry))
}

fn read_tail(path: &Path, max_bytes: usize) -> Result<String> {
    let mut file = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let len = file
        .metadata()
        .with_context(|| format!("metadata {}", path.display()))?
        .len();
    let start = len.saturating_sub(max_bytes as u64);
    file.seek(SeekFrom::Start(start))
        .with_context(|| format!("seek {}", path.display()))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)
        .with_context(|| format!("read {}", path.display()))?;
    Ok(String::from_utf8_lossy(&buf).to_string())
}

fn find_last_txn_id(paths: &DocPackPaths) -> Option<String> {
    let txns_dir = paths.txns_root();
    let entries = fs::read_dir(&txns_dir).ok()?;
    let mut ids = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect::<Vec<_>>();
    ids.sort();
    ids.pop()
}

fn preview_text(text: &str) -> String {
    if text.trim().is_empty() {
        return "<empty>".to_string();
    }
    let mut out = String::new();
    let mut truncated = false;
    for (idx, line) in text.lines().enumerate() {
        if idx >= PREVIEW_MAX_LINES {
            truncated = true;
            break;
        }
        if !out.is_empty() {
            out.push_str(" ");
        }
        out.push_str(line);
        if out.len() >= PREVIEW_MAX_CHARS {
            out.truncate(PREVIEW_MAX_CHARS);
            truncated = true;
            break;
        }
    }
    if truncated {
        out.push_str("...");
    }
    out
}

fn list_title(label: &str, total: usize, visible: usize, show_all: bool) -> String {
    if show_all || total <= visible {
        format!("{label} ({total})")
    } else {
        format!("{label} (showing {visible} of {total}, press 'a' to show all)")
    }
}

fn visible_len(total: usize, show_all: bool) -> usize {
    if show_all {
        total
    } else {
        min(total, PREVIEW_LIMIT)
    }
}

fn intent_list_item(entry: &ArtifactEntry) -> ListItem<'static> {
    let status = if entry.exists { "present" } else { "missing" };
    let text = format!("{} ({status})", entry.rel_path);
    ListItem::new(Line::from(text))
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

fn gate_label(present: bool, stale: bool) -> &'static str {
    if !present {
        "missing"
    } else if stale {
        "stale"
    } else {
        "fresh"
    }
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

fn next_action_summary(action: &enrich::NextAction) -> String {
    match action {
        enrich::NextAction::Command { command, reason } => {
            format!("command: {command} ({reason})")
        }
        enrich::NextAction::Edit { path, reason, .. } => {
            format!("edit: {path} ({reason})")
        }
    }
}

fn next_action_copy(action: &enrich::NextAction) -> String {
    match action {
        enrich::NextAction::Command { command, .. } => command.clone(),
        enrich::NextAction::Edit { path, .. } => path.clone(),
    }
}

fn truncate_text(text: &str, max_len: usize) -> String {
    if text.len() <= max_len || max_len <= 3 {
        return text.to_string();
    }
    let mut truncated = text[..max_len.saturating_sub(3)].to_string();
    truncated.push_str("...");
    truncated
}

fn print_text_summary(
    doc_pack_root: &Path,
    summary: &enrich::StatusSummary,
    data: &InspectData,
) -> Result<()> {
    let binary = summary
        .binary_name
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    let lock_label = gate_label(summary.lock.present, summary.lock.stale);
    let plan_label = gate_label(summary.plan.present, summary.plan.stale);
    let decision_label = summary.decision.as_str();
    let next_action = next_action_summary(&summary.next_action);

    println!("doc_pack: {}", doc_pack_root.display());
    println!("binary: {binary}");
    println!("lock: {lock_label}");
    println!("plan: {plan_label}");
    println!("decision: {decision_label}");
    println!("next_action: {next_action}");
    println!();

    let intent_preview = data
        .intent
        .iter()
        .take(3)
        .map(|entry| entry.rel_path.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    println!(
        "intent: {} items (preview: {})",
        data.intent.len(),
        if intent_preview.is_empty() {
            "none"
        } else {
            &intent_preview
        }
    );

    let evidence_preview = data
        .evidence
        .entries
        .iter()
        .take(3)
        .map(|entry| entry.scenario_id.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    println!(
        "evidence: {} scenarios (preview: {})",
        data.evidence.total_count,
        if evidence_preview.is_empty() {
            "none"
        } else {
            &evidence_preview
        }
    );

    let outputs_preview = data
        .outputs
        .iter()
        .take(3)
        .map(|entry| entry.rel_path.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    println!(
        "outputs: {} items (preview: {})",
        data.outputs.len(),
        if outputs_preview.is_empty() {
            "none"
        } else {
            &outputs_preview
        }
    );

    let history_preview = data
        .history
        .iter()
        .take(2)
        .map(|entry| entry.rel_path.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    println!(
        "history: {} items (preview: {})",
        data.history.len(),
        if history_preview.is_empty() {
            "none"
        } else {
            &history_preview
        }
    );
    Ok(())
}

fn open_in_editor(path: &Path) -> Result<()> {
    let cmd = resolve_command(&["VISUAL", "EDITOR"], "vi");
    run_command(cmd, Some(path))
}

fn open_in_pager(path: &Path) -> Result<()> {
    if !path.exists() {
        return Err(anyhow::anyhow!("missing file {}", path.display()));
    }
    let cmd = resolve_command(&["PAGER"], "less");
    run_command(cmd, Some(path))
}

fn open_man_page(path: &Path) -> Result<()> {
    if !path.exists() {
        return Err(anyhow::anyhow!("missing man page {}", path.display()));
    }
    let cmd = vec![
        "man".to_string(),
        "-l".to_string(),
        path.display().to_string(),
    ];
    run_command(cmd, None)
}

fn resolve_command(vars: &[&str], fallback: &str) -> Vec<String> {
    for var in vars {
        if let Ok(value) = std::env::var(var) {
            if !value.trim().is_empty() {
                if let Ok(parts) = shell_split(&value) {
                    if !parts.is_empty() {
                        return parts;
                    }
                }
            }
        }
    }
    vec![fallback.to_string()]
}

fn run_command(mut cmd: Vec<String>, path: Option<&Path>) -> Result<()> {
    if cmd.is_empty() {
        return Err(anyhow::anyhow!("missing command"));
    }
    let program = cmd.remove(0);
    let mut command = Command::new(program);
    if !cmd.is_empty() {
        command.args(cmd);
    }
    if let Some(path) = path {
        command.arg(path);
    }
    let status = command
        .status()
        .with_context(|| format!("run external command"))?;
    if !status.success() {
        return Err(anyhow::anyhow!("external command failed"));
    }
    Ok(())
}

fn try_copy_to_clipboard(text: &str) -> Result<bool> {
    let candidates = vec![
        ("pbcopy", vec![]),
        ("wl-copy", vec![]),
        ("xclip", vec!["-selection", "clipboard"]),
        ("xsel", vec!["--clipboard", "--input"]),
    ];
    for (program, args) in candidates {
        let mut command = Command::new(program);
        command.args(args).stdin(std::process::Stdio::piped());
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(_) => continue,
        };
        if let Some(stdin) = child.stdin.as_mut() {
            stdin.write_all(text.as_bytes()).ok();
        }
        let status = child.wait()?;
        if status.success() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn run_external<F>(
    guard: &mut TerminalGuard,
    terminal: &mut ratatui::Terminal<CrosstermBackend<io::Stdout>>,
    f: F,
) -> Result<()>
where
    F: FnOnce() -> Result<()>,
{
    guard.suspend()?;
    let result = f();
    guard.resume()?;
    terminal.clear().ok();
    result
}

struct TerminalGuard {
    active: bool,
}

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("enable raw mode")?;
        execute!(io::stdout(), EnterAlternateScreen, Hide).context("enter alt screen")?;
        Ok(Self { active: true })
    }

    fn suspend(&mut self) -> Result<()> {
        if self.active {
            disable_raw_mode().ok();
            execute!(io::stdout(), LeaveAlternateScreen, Show).ok();
            self.active = false;
        }
        Ok(())
    }

    fn resume(&mut self) -> Result<()> {
        if !self.active {
            execute!(io::stdout(), EnterAlternateScreen, Hide).ok();
            enable_raw_mode().ok();
            self.active = true;
        }
        Ok(())
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = self.suspend();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    #[test]
    fn gate_label_variants() {
        assert_eq!(gate_label(false, false), "missing");
        assert_eq!(gate_label(true, true), "stale");
        assert_eq!(gate_label(true, false), "fresh");
    }

    #[test]
    fn key_mapping() {
        let key = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        assert_eq!(action_from_key(key), Some(Action::Quit));
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(action_from_key(key), Some(Action::OpenPager));
        let key = KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE);
        assert_eq!(action_from_key(key), Some(Action::OpenEditor));
    }

    #[test]
    fn preview_truncates() {
        let text = "line1\nline2\nline3";
        let preview = preview_text(text);
        assert!(preview.contains("line1"));
        assert!(preview.ends_with("..."));
    }
}
