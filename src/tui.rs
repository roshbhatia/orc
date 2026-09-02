use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use ansi_to_tui::IntoText as _;
use anyhow::Result;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers, MouseEvent,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use rataflow::{
    Direction, Edge, EdgeContent, EdgePathContext, EdgeRenderContext, EdgeStyle, Flow, Handle,
    HandlePosition, Node, NodeContent, NodeRenderContext, Path as EdgePath, Reconnectable,
    StepEdge, Sugiyama, Theme,
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    buffer::Buffer,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Row, Table, Tabs,
        Widget, Wrap,
    },
};
use unicode_width::UnicodeWidthStr;

use crate::{
    config::Config,
    control,
    domain::{LifecycleStatus, Session, SessionRole, WorkspaceState},
    provider::{self, Action, Manifest},
    workflow,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MainTab {
    Explorer,
    Providers,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExplorerView {
    Graph,
    Tree,
    Fleet,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Focus {
    Main,
    Detail,
    Output,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OutputTab {
    Log,
    Activity,
    Output,
    Changes,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Dock {
    Bottom,
    Top,
    Left,
    Right,
    Hidden,
}

#[derive(Clone, Debug)]
enum Confirmation {
    Approve { run_id: String, gate_id: String },
    Cancel { run_id: String },
}

#[derive(Clone, Debug)]
enum ItemRef {
    Session(String),
    Run(String),
    Node(String, String),
    Provider(String),
}

#[derive(Clone, Debug)]
struct TreeRow {
    id: String,
    depth: usize,
    title: String,
    subtitle: String,
    status: Option<LifecycleStatus>,
    item: ItemRef,
    children: bool,
}

#[derive(Clone, Debug)]
struct AgentCard {
    kind: String,
    title: String,
    subtitle: String,
    goal: String,
    status: LifecycleStatus,
}

impl NodeContent for AgentCard {
    fn render(&self, ctx: &NodeRenderContext, buf: &mut Buffer) {
        if ctx.area.width < 4 || ctx.area.height < 3 {
            return;
        }
        let palette = ctx.theme.palette();
        let border = if ctx.selected {
            palette.accent
        } else {
            palette.muted
        };
        let status = status_color(self.status);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border));
        let inner = block.inner(ctx.area);
        block.render(ctx.area, buf);
        if inner.width == 0 || inner.height == 0 {
            return;
        }
        let width = inner.width as usize;
        let mut lines = vec![Line::from(vec![
            Span::styled(
                format!("{} ", status_glyph(self.status)),
                Style::default().fg(status),
            ),
            Span::styled(
                truncate(&self.title, width.saturating_sub(2)),
                Style::default()
                    .fg(palette.text)
                    .add_modifier(Modifier::BOLD),
            ),
        ])];
        lines.push(Line::from(Span::styled(
            truncate(&format!("{} · {}", self.kind, self.subtitle), width),
            Style::default().fg(palette.subtle),
        )));
        if inner.height > 2 {
            lines.push(Line::from(Span::styled(
                truncate(&self.goal, width),
                Style::default().fg(palette.muted),
            )));
        }
        Paragraph::new(lines).render(inner, buf);
    }
}

#[derive(Clone, Debug, Default)]
struct RelationEdge {
    inner: StepEdge,
    active: bool,
}

impl EdgeContent for RelationEdge {
    fn compute_path(&self, ctx: &EdgePathContext) -> EdgePath {
        self.inner.compute_path(ctx)
    }
    fn render(&self, ctx: &EdgeRenderContext, buf: &mut Buffer) {
        let color = if self.active {
            ctx.theme.palette().success
        } else {
            ctx.theme.palette().muted
        };
        ctx.render_path(
            &EdgeStyle::default().with_stroke_style(Style::default().fg(color)),
            None,
            buf,
        );
    }
}

type AgentFlow = Flow<AgentCard, RelationEdge>;
type GraphEdge = (String, String, String, bool);

struct App {
    scope: PathBuf,
    config: Config,
    state: WorkspaceState,
    providers: Vec<Manifest>,
    flow: AgentFlow,
    graph_items: BTreeMap<String, ItemRef>,
    tree: Vec<TreeRow>,
    tree_at: usize,
    fleet_at: usize,
    active_run: Option<String>,
    provider_at: usize,
    expanded: BTreeSet<String>,
    main_tab: MainTab,
    explorer_view: ExplorerView,
    focus: Focus,
    output_tab: OutputTab,
    detail_scroll: u16,
    output_scroll: u16,
    dock: Dock,
    leader: bool,
    pending: Option<char>,
    help: bool,
    confirmation: Option<Confirmation>,
    status: String,
    activity: String,
    changes: String,
    last_refresh: Instant,
    graph_signature: String,
}

impl App {
    fn new(
        config: Config,
        scope: PathBuf,
        state: WorkspaceState,
        providers: Vec<Manifest>,
    ) -> Self {
        let active_run = state
            .runs
            .iter()
            .find(|run| run.status.active())
            .or_else(|| state.runs.first())
            .map(|run| run.id.clone());
        let mut app = Self {
            scope,
            config,
            state,
            providers,
            flow: new_flow(),
            graph_items: BTreeMap::new(),
            tree: Vec::new(),
            tree_at: 0,
            fleet_at: 0,
            active_run,
            provider_at: 0,
            expanded: BTreeSet::new(),
            main_tab: MainTab::Explorer,
            explorer_view: ExplorerView::Graph,
            focus: Focus::Main,
            output_tab: OutputTab::Log,
            detail_scroll: 0,
            output_scroll: 0,
            dock: Dock::Bottom,
            leader: false,
            pending: None,
            help: false,
            confirmation: None,
            status: String::new(),
            activity: String::new(),
            changes: String::new(),
            last_refresh: Instant::now(),
            graph_signature: String::new(),
        };
        app.rebuild(true);
        app
    }

    fn rebuild(&mut self, force_layout: bool) {
        self.tree = tree_rows(&self.state, &self.expanded);
        self.tree_at = self.tree_at.min(self.tree.len().saturating_sub(1));
        self.fleet_at = self.fleet_at.min(self.state.runs.len().saturating_sub(1));
        self.provider_at = self.provider_at.min(self.providers.len().saturating_sub(1));
        let signature = graph_signature(&self.state, self.active_run.as_deref());
        if signature != self.graph_signature || force_layout {
            let selected = self.flow.first_selected_node_id();
            let (mut flow, items) = build_flow(&self.state, self.active_run.as_deref());
            if let Some(selected) = selected {
                flow.select_node(&selected);
            }
            if flow.first_selected_node_id().is_none() {
                flow.select_next_node();
            }
            self.flow = flow;
            self.graph_items = items;
            self.graph_signature = signature;
        } else {
            refresh_flow_content(&mut self.flow, &self.state);
        }
    }

    fn refresh(&mut self) {
        match control::reconcile(&self.config, &self.scope) {
            Ok(state) => {
                self.state = state;
                self.providers = provider::discover(&self.config).unwrap_or_default();
                self.rebuild(false);
                self.last_refresh = Instant::now();
            }
            Err(error) => self.status = format!("refresh failed: {error:#}"),
        }
    }

    fn selected(&self) -> Option<ItemRef> {
        match self.main_tab {
            MainTab::Providers => self
                .providers
                .get(self.provider_at)
                .map(|provider| ItemRef::Provider(provider.name.clone())),
            MainTab::Explorer if self.explorer_view == ExplorerView::Tree => {
                self.tree.get(self.tree_at).map(|row| row.item.clone())
            }
            MainTab::Explorer if self.explorer_view == ExplorerView::Fleet => self
                .state
                .runs
                .get(self.fleet_at)
                .map(|run| ItemRef::Run(run.id.clone())),
            MainTab::Explorer => self
                .flow
                .first_selected_node_id()
                .and_then(|id| self.graph_items.get(&id).cloned()),
        }
    }

    fn selected_session(&self) -> Option<&Session> {
        match self.selected()? {
            ItemRef::Session(id) => self.state.sessions.iter().find(|session| session.id == id),
            ItemRef::Node(run, node) => self
                .state
                .runs
                .iter()
                .find(|candidate| candidate.id == run)
                .and_then(|run| run.nodes.iter().find(|candidate| candidate.id == node))
                .and_then(|node| node.session_id.as_deref())
                .and_then(|id| self.state.sessions.iter().find(|session| session.id == id)),
            _ => None,
        }
    }

    fn selected_run_id(&self) -> Option<String> {
        match self.selected()? {
            ItemRef::Run(id) | ItemRef::Node(id, _) => Some(id),
            ItemRef::Session(id) => self
                .state
                .sessions
                .iter()
                .find(|session| session.id == id)
                .and_then(|session| session.run_id.clone()),
            ItemRef::Provider(_) => None,
        }
    }

    fn request_gate(&mut self) -> bool {
        let Some(run_id) = self.selected_run_id() else {
            return false;
        };
        let Some(gate) = self
            .state
            .runs
            .iter()
            .find(|run| run.id == run_id)
            .and_then(|run| run.pending_gates.first())
        else {
            return false;
        };
        self.confirmation = Some(Confirmation::Approve {
            run_id,
            gate_id: gate.id.clone(),
        });
        true
    }

    fn request_cancel(&mut self) {
        if let Some(run_id) = self.selected_run_id() {
            self.confirmation = Some(Confirmation::Cancel { run_id });
        } else {
            self.status = "select a run first".into();
        }
    }

    fn confirm(&mut self) {
        let Some(confirmation) = self.confirmation.take() else {
            return;
        };
        let result = match confirmation {
            Confirmation::Approve { run_id, gate_id } => {
                workflow::approve(&self.config, &self.scope, &run_id, Some(&gate_id), false)
                    .and_then(|_| workflow::spawn(&self.scope, &run_id))
            }
            Confirmation::Cancel { run_id } => workflow::cancel(&self.scope, &run_id),
        };
        match result {
            Ok(run) => {
                self.status = format!("{} is {}", run.name, run.status);
                self.refresh();
            }
            Err(error) => self.status = format!("action failed: {error:#}"),
        }
    }

    fn move_main(&mut self, direction: Direction) {
        match self.main_tab {
            MainTab::Providers => match direction {
                Direction::Up => self.provider_at = self.provider_at.saturating_sub(1),
                Direction::Down => {
                    self.provider_at =
                        (self.provider_at + 1).min(self.providers.len().saturating_sub(1))
                }
                _ => {}
            },
            MainTab::Explorer if self.explorer_view == ExplorerView::Tree => match direction {
                Direction::Up => self.tree_at = self.tree_at.saturating_sub(1),
                Direction::Down => {
                    self.tree_at = (self.tree_at + 1).min(self.tree.len().saturating_sub(1))
                }
                Direction::Left => self.collapse(),
                Direction::Right => self.expand(),
            },
            MainTab::Explorer if self.explorer_view == ExplorerView::Fleet => match direction {
                Direction::Up => self.fleet_at = self.fleet_at.saturating_sub(1),
                Direction::Down => {
                    self.fleet_at = (self.fleet_at + 1).min(self.state.runs.len().saturating_sub(1))
                }
                _ => {}
            },
            MainTab::Explorer => self.flow.select_node_in_direction(direction),
        }
        self.detail_scroll = 0;
        self.output_scroll = 0;
    }

    fn expand(&mut self) {
        if let Some(row) = self.tree.get(self.tree_at).filter(|row| row.children) {
            self.expanded.insert(row.id.clone());
            self.rebuild(false);
        }
    }

    fn collapse(&mut self) {
        if let Some(row) = self.tree.get(self.tree_at)
            && self.expanded.remove(&row.id)
        {
            self.rebuild(false);
        }
    }

    fn open_selected(&mut self) {
        if let Some(ItemRef::Run(run_id)) = self.selected() {
            self.active_run = Some(run_id);
            self.explorer_view = ExplorerView::Graph;
            self.focus = Focus::Main;
            self.rebuild(true);
            self.flow.request_fit_view();
            self.status = "opened workflow graph".into();
            return;
        }
        if let Some(session) = self.selected_session().cloned() {
            self.status = "opening session through providers".into();
            match control::attach(
                &self.config,
                &self.scope,
                &session.id,
                Action::Attach,
                "right",
            ) {
                Ok(0) => self.status = format!("opened {}", session.title),
                Ok(code) => self.status = format!("attach exited with {code}"),
                Err(error) => self.status = format!("attach failed: {error:#}"),
            }
        } else if self.main_tab == MainTab::Providers {
            self.validate_provider();
        }
    }

    fn load_output(&mut self, action: Action) {
        let session = self.selected_session().cloned();
        if session.is_none() && !matches!(action, Action::Changes) {
            self.status = "select a session first".into();
            return;
        }
        let request = provider::action_request(action, &self.scope, session.as_ref(), "right");
        match provider::resolve_plan(&self.config, &self.providers, action, request)
            .and_then(|plan| provider::capture_plan(&plan, &self.scope))
        {
            Ok(output) => match action {
                Action::Activity => {
                    self.activity = output;
                    self.output_tab = OutputTab::Activity;
                }
                Action::Changes => {
                    self.changes = output;
                    self.output_tab = OutputTab::Changes;
                }
                _ => {}
            },
            Err(error) => self.status = format!("provider failed: {error:#}"),
        }
        self.focus = Focus::Output;
        self.output_scroll = 0;
    }

    fn validate_provider(&mut self) {
        let name = match self.selected() {
            Some(ItemRef::Provider(name)) => name,
            _ => return,
        };
        match provider::validate_all(&self.config, &self.scope, Some(&name)) {
            Ok(results) => {
                let lines = results
                    .into_iter()
                    .flat_map(|result| {
                        result.checks.into_iter().map(|check| {
                            format!("{:?}  {}  {}", check.status, check.name, check.message)
                        })
                    })
                    .collect::<Vec<_>>();
                self.activity = lines.join("\n");
                self.output_tab = OutputTab::Activity;
                self.focus = Focus::Output;
            }
            Err(error) => self.status = format!("validation failed: {error:#}"),
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> bool {
        if key.kind == KeyEventKind::Release {
            return false;
        }
        if terminal_reply(key) {
            return false;
        }
        if self.confirmation.is_some() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Enter => self.confirm(),
                KeyCode::Char('n') | KeyCode::Esc => self.confirmation = None,
                _ => {}
            }
            return false;
        }
        if self.help {
            if matches!(
                key.code,
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q')
            ) {
                self.help = false;
            }
            return false;
        }
        if self.leader {
            self.leader = false;
            match key.code {
                KeyCode::Char('i') => {
                    self.pending = Some('i');
                    self.status = "inspector: i toggle, h/j/k/l dock".into();
                }
                KeyCode::Char('?') => self.help = true,
                _ => self.status = "unknown leader action".into(),
            }
            return false;
        }
        if let Some(prefix) = self.pending.take() {
            match (prefix, key.code) {
                ('i', KeyCode::Char('i')) => {
                    self.dock = if self.dock == Dock::Hidden {
                        Dock::Bottom
                    } else {
                        Dock::Hidden
                    }
                }
                ('i', KeyCode::Char('h')) => self.dock = Dock::Left,
                ('i', KeyCode::Char('j')) => self.dock = Dock::Bottom,
                ('i', KeyCode::Char('k')) => self.dock = Dock::Top,
                ('i', KeyCode::Char('l')) => self.dock = Dock::Right,
                ('w', KeyCode::Char('j')) => self.focus = Focus::Output,
                ('w', KeyCode::Char('k')) => self.focus = Focus::Main,
                ('w', KeyCode::Char('l')) => self.focus = Focus::Detail,
                ('w', KeyCode::Char('h')) => self.focus = Focus::Main,
                _ => self.status = "unknown key sequence".into(),
            }
            return false;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match (key.code, ctrl) {
            (KeyCode::Char('q'), _) | (KeyCode::Char('c'), true) => return true,
            (KeyCode::Char('?'), _) => self.help = true,
            (KeyCode::Char(' '), _) => self.leader = true,
            (KeyCode::Char('w'), true) => self.pending = Some('w'),
            (KeyCode::Char('j'), true) => self.focus = Focus::Output,
            (KeyCode::Char('k'), true) => self.focus = Focus::Main,
            (KeyCode::Char('l'), true) => self.focus = Focus::Detail,
            (KeyCode::Char('h'), true) => self.focus = Focus::Main,
            (KeyCode::Char('d'), true) => self.page(1),
            (KeyCode::Char('u'), true) => self.page(-1),
            (KeyCode::Tab, _) => self.next_inspector(1),
            (KeyCode::BackTab, _) => self.next_inspector(-1),
            (KeyCode::Char('1'), _) => self.main_tab = MainTab::Explorer,
            (KeyCode::Char('2'), _) => self.main_tab = MainTab::Providers,
            (KeyCode::Char('g'), _) if self.focus == Focus::Main => {
                if !self.request_gate() {
                    self.explorer_view = ExplorerView::Graph;
                }
            }
            (KeyCode::Char('t'), _) if self.focus == Focus::Main => {
                self.explorer_view = ExplorerView::Tree
            }
            (KeyCode::Char('f'), _) if self.focus == Focus::Main => {
                self.explorer_view = ExplorerView::Fleet
            }
            (KeyCode::Char('r'), _) => self.refresh(),
            (KeyCode::Char('R'), _) => {
                self.rebuild(true);
                self.flow.request_fit_view();
            }
            (KeyCode::Char('o'), _)
                if self.focus == Focus::Main && self.explorer_view == ExplorerView::Graph =>
            {
                self.flow.request_fit_view();
            }
            (KeyCode::Char('+' | '=' | '-' | '_' | '0'), _)
                if self.focus == Focus::Main && self.explorer_view == ExplorerView::Graph =>
            {
                let _ = self.flow.handle_controls_key_event(key);
            }
            (KeyCode::Char('='), _) if self.focus == Focus::Output => {
                self.config.ui.inspector_percent = (self.config.ui.inspector_percent + 5).min(80);
            }
            (KeyCode::Char('-'), _) if self.focus == Focus::Output => {
                self.config.ui.inspector_percent =
                    self.config.ui.inspector_percent.saturating_sub(5).max(20);
            }
            (KeyCode::Char('i'), _) => self.load_output(Action::Activity),
            (KeyCode::Char('c'), _) => self.load_output(Action::Changes),
            (KeyCode::Char('v'), _) if self.main_tab == MainTab::Providers => {
                self.validate_provider()
            }
            (KeyCode::Char('x'), _) => self.request_cancel(),
            (KeyCode::Enter, _) => self.open_selected(),
            (KeyCode::Char('j'), _) | (KeyCode::Down, _) => self.motion(Direction::Down),
            (KeyCode::Char('k'), _) | (KeyCode::Up, _) => self.motion(Direction::Up),
            (KeyCode::Char('h'), _) | (KeyCode::Left, _) => self.motion(Direction::Left),
            (KeyCode::Char('l'), _) | (KeyCode::Right, _) => self.motion(Direction::Right),
            _ => {}
        }
        false
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        if self.main_tab == MainTab::Explorer
            && self.explorer_view == ExplorerView::Graph
            && self.focus == Focus::Main
        {
            let _ = self.flow.handle_mouse_event(mouse);
            self.detail_scroll = 0;
            self.output_scroll = 0;
        }
    }

    fn motion(&mut self, direction: Direction) {
        match self.focus {
            Focus::Main => self.move_main(direction),
            Focus::Detail => match direction {
                Direction::Up => self.detail_scroll = self.detail_scroll.saturating_sub(1),
                Direction::Down => self.detail_scroll = self.detail_scroll.saturating_add(1),
                _ => {}
            },
            Focus::Output => match direction {
                Direction::Up => self.output_scroll = self.output_scroll.saturating_sub(1),
                Direction::Down => self.output_scroll = self.output_scroll.saturating_add(1),
                _ => {}
            },
        }
    }

    fn page(&mut self, by: i32) {
        match self.focus {
            Focus::Detail => {
                self.detail_scroll = (self.detail_scroll as i32 + by * 10).max(0) as u16
            }
            Focus::Output => {
                self.output_scroll = (self.output_scroll as i32 + by * 10).max(0) as u16
            }
            Focus::Main => {
                for _ in 0..10 {
                    self.move_main(if by > 0 {
                        Direction::Down
                    } else {
                        Direction::Up
                    });
                }
            }
        }
    }

    fn next_inspector(&mut self, by: i32) {
        let tabs = [
            OutputTab::Log,
            OutputTab::Activity,
            OutputTab::Output,
            OutputTab::Changes,
        ];
        let current = tabs
            .iter()
            .position(|tab| *tab == self.output_tab)
            .unwrap_or(0) as i32;
        self.output_tab = tabs[((current + by).rem_euclid(tabs.len() as i32)) as usize];
        self.output_scroll = 0;
    }
}

fn new_flow() -> AgentFlow {
    let mut palette = Theme::Dark.palette();
    palette.canvas_bg = Color::Reset;
    palette.surface = Color::Reset;
    palette.accent = Color::Indexed(109);
    palette.text = Color::Indexed(252);
    Flow::new()
        .with_theme(Theme::Custom(palette))
        .with_deselect_on_pane_click(false)
        .with_selection_reveal(rataflow::SelectionReveal::EnsureVisible)
}

fn graph_signature(state: &WorkspaceState, active_run: Option<&str>) -> String {
    let Some(run) = active_run.and_then(|id| state.runs.iter().find(|run| run.id == id)) else {
        return state
            .current_session()
            .map_or_else(String::new, |session| format!("s:{}", session.id));
    };
    let mut value = format!(
        "r:{}:{};",
        run.id,
        run.orchestrator_id.as_deref().unwrap_or("")
    );
    for node in &run.nodes {
        value.push_str(&format!("n:{};", node.id));
    }
    for edge in &run.edges {
        value.push_str(&format!(
            "e:{}:{}:{};",
            edge.from, edge.to, edge.relationship
        ));
    }
    value
}

fn refresh_flow_content(flow: &mut AgentFlow, state: &WorkspaceState) {
    for session in &state.sessions {
        if let Some(card) = flow.node_content_mut(&format!("session:{}", session.id)) {
            card.title.clone_from(&session.title);
            card.subtitle = format!("{} · {}", session.harness, session.status);
            card.goal.clone_from(&session.goal);
            card.status = session.status;
        }
    }
    for run in &state.runs {
        if let Some(card) = flow.node_content_mut(&format!("run:{}", run.id)) {
            card.title.clone_from(&run.name);
            card.subtitle = format!("{} steps · {}", run.nodes.len(), run.status);
            card.goal.clone_from(&run.goal);
            card.status = run.status;
        }
        for node in &run.nodes {
            if let Some(card) = flow.node_content_mut(&format!("node:{}:{}", run.id, node.id)) {
                card.title.clone_from(&node.name);
                card.subtitle = format!("{} · {}", node.harness, node.status);
                card.goal.clone_from(&node.goal);
                card.status = node.status;
            }
        }
    }
}

fn build_flow(
    state: &WorkspaceState,
    active_run: Option<&str>,
) -> (AgentFlow, BTreeMap<String, ItemRef>) {
    let mut flow = new_flow();
    let mut items = BTreeMap::new();
    let mut known = BTreeSet::new();
    let run = active_run.and_then(|id| state.runs.iter().find(|run| run.id == id));
    let orchestrator = run
        .and_then(|run| run.orchestrator_id.as_deref())
        .and_then(|id| state.sessions.iter().find(|session| session.id == id))
        .or_else(|| state.current_session());
    if let Some(session) = orchestrator {
        let id = format!("session:{}", session.id);
        let card = AgentCard {
            kind: "orchestrator".into(),
            title: session.title.clone(),
            subtitle: format!("{} · {}", session.harness, session.status),
            goal: session.goal.clone(),
            status: session.status,
        };
        let node = Node::new(&id, (0.0, 0.0), (40.0, 5.0), card)
            .with_deletable(false)
            .with_connectable(false)
            .with_draggable(false)
            .with_handles(vec![
                Handle::source(HandlePosition::Bottom).with_hidden(true),
                Handle::target(HandlePosition::Top).with_hidden(true),
            ]);
        let _ = flow.add_node(node);
        known.insert(id.clone());
        items.insert(id, ItemRef::Session(session.id.clone()));
    }
    if let Some(run) = run {
        for workflow_node in &run.nodes {
            let node_id = format!("node:{}:{}", run.id, workflow_node.id);
            let card = AgentCard {
                kind: workflow_node.role.to_string(),
                title: workflow_node.name.clone(),
                subtitle: format!("{} · {}", workflow_node.harness, workflow_node.status),
                goal: workflow_node.goal.clone(),
                status: workflow_node.status,
            };
            let node = Node::new(&node_id, (0.0, 0.0), (40.0, 5.0), card)
                .with_deletable(false)
                .with_connectable(false)
                .with_draggable(false)
                .with_handles(vec![
                    Handle::source(HandlePosition::Bottom).with_hidden(true),
                    Handle::target(HandlePosition::Top).with_hidden(true),
                ]);
            let _ = flow.add_node(node);
            known.insert(node_id.clone());
            items.insert(
                node_id,
                ItemRef::Node(run.id.clone(), workflow_node.id.clone()),
            );
        }
        let mut topology_edges = Vec::new();
        let mut overlay_edges = Vec::new();
        let dependencies: BTreeSet<_> = run.edges.iter().map(|edge| edge.to.as_str()).collect();
        for node in &run.nodes {
            if !dependencies.contains(node.id.as_str())
                && let Some(orchestrator) = &run.orchestrator_id
            {
                topology_edges.push((
                    format!("session:{orchestrator}"),
                    format!("node:{}:{}", run.id, node.id),
                    "delegates".into(),
                    node.status.active(),
                ));
            }
        }
        for edge in &run.edges {
            topology_edges.push((
                format!("node:{}:{}", run.id, edge.from),
                format!("node:{}:{}", run.id, edge.to),
                edge.relationship.clone(),
                run.status.active(),
            ));
            if edge.relationship == "reviewed_by" {
                overlay_edges.push((
                    format!("node:{}:{}", run.id, edge.to),
                    format!("node:{}:{}", run.id, edge.from),
                    "feedback".into(),
                    run.status.active(),
                ));
            }
        }
        if let Some(orchestrator) = &run.orchestrator_id {
            for node in run
                .nodes
                .iter()
                .filter(|node| node.completion == crate::domain::CompletionTarget::Orchestrator)
                .filter(|node| {
                    !run.edges
                        .iter()
                        .any(|edge| edge.from == node.id && edge.relationship == "depends_on")
                })
            {
                overlay_edges.push((
                    format!("node:{}:{}", run.id, node.id),
                    format!("session:{orchestrator}"),
                    "reports".into(),
                    node.status == LifecycleStatus::Done,
                ));
            }
        }
        add_graph_edges(&mut flow, &known, topology_edges, "topology");
        flow.apply_layout(Sugiyama::vertical());
        add_graph_edges(&mut flow, &known, overlay_edges, "overlay");
    } else {
        flow.apply_layout(Sugiyama::vertical());
    }
    flow.request_fit_view();
    (flow, items)
}

fn add_graph_edges(
    flow: &mut AgentFlow,
    known: &BTreeSet<String>,
    edges: Vec<GraphEdge>,
    kind: &str,
) {
    for (index, (from, to, relation, active)) in edges.into_iter().enumerate() {
        if !known.contains(&from) || !known.contains(&to) {
            continue;
        }
        let edge = Edge::new(format!("edge:{kind}:{index}"), from, to)
            .with_content(RelationEdge {
                active,
                ..RelationEdge::default()
            })
            .with_label(relation)
            .with_selectable(false)
            .with_deletable(false)
            .with_reconnectable(Reconnectable::None);
        let _ = flow.add_edge(edge);
    }
}

fn tree_rows(state: &WorkspaceState, expanded: &BTreeSet<String>) -> Vec<TreeRow> {
    let mut rows = Vec::new();
    let orchestrators: Vec<_> = state
        .sessions
        .iter()
        .filter(|session| {
            session.role == SessionRole::Orchestrator && session.status != LifecycleStatus::Archived
        })
        .collect();
    for session in orchestrators {
        let id = format!("session:{}", session.id);
        let runs: Vec<_> = state
            .runs
            .iter()
            .filter(|run| run.orchestrator_id.as_deref() == Some(&session.id))
            .collect();
        rows.push(TreeRow {
            id: id.clone(),
            depth: 0,
            title: session.title.clone(),
            subtitle: format!("{} · {}", session.harness, session.status),
            status: Some(session.status),
            item: ItemRef::Session(session.id.clone()),
            children: !runs.is_empty(),
        });
        if expanded.contains(&id) || expanded.is_empty() {
            for run in runs {
                let run_id = format!("run:{}", run.id);
                rows.push(TreeRow {
                    id: run_id.clone(),
                    depth: 1,
                    title: run.name.clone(),
                    subtitle: format!("{} steps · {}", run.nodes.len(), run.status),
                    status: Some(run.status),
                    item: ItemRef::Run(run.id.clone()),
                    children: !run.nodes.is_empty(),
                });
                if expanded.contains(&run_id) || expanded.is_empty() {
                    for node in &run.nodes {
                        rows.push(TreeRow {
                            id: format!("node:{}:{}", run.id, node.id),
                            depth: 2,
                            title: node.name.clone(),
                            subtitle: format!("{} · {} · {}", node.role, node.harness, node.status),
                            status: Some(node.status),
                            item: ItemRef::Node(run.id.clone(), node.id.clone()),
                            children: false,
                        });
                    }
                }
            }
        }
    }
    let included: BTreeSet<_> = rows
        .iter()
        .filter_map(|row| match &row.item {
            ItemRef::Session(id) => Some(id.clone()),
            _ => None,
        })
        .collect();
    for session in state.sessions.iter().filter(|session| {
        session.status != LifecycleStatus::Archived && !included.contains(&session.id)
    }) {
        rows.push(TreeRow {
            id: format!("session:{}", session.id),
            depth: session.parent_id.is_some() as usize,
            title: session.title.clone(),
            subtitle: format!(
                "{} · {} · {}",
                session.role, session.harness, session.status
            ),
            status: Some(session.status),
            item: ItemRef::Session(session.id.clone()),
            children: false,
        });
    }
    rows
}

fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(area);
    render_header(frame, header, app);
    let (workspace, output) = split_body(body, app.dock, app.config.ui.inspector_percent);
    let [main, detail] =
        Layout::horizontal([Constraint::Percentage(64), Constraint::Percentage(36)])
            .areas(workspace);
    render_main(frame, main, app);
    render_detail(frame, detail, app);
    if let Some(output) = output {
        render_output(frame, output, app);
    }
    render_footer(frame, footer, app);
    if app.help {
        render_help(frame, area);
    }
    if let Some(confirmation) = &app.confirmation {
        render_confirmation(frame, area, confirmation);
    }
}

fn split_body(area: Rect, dock: Dock, percent: u16) -> (Rect, Option<Rect>) {
    if dock == Dock::Hidden {
        return (area, None);
    }
    let main = Constraint::Percentage(100 - percent);
    let inspect = Constraint::Percentage(percent);
    match dock {
        Dock::Bottom => {
            let [a, b] = Layout::vertical([main, inspect]).areas(area);
            (a, Some(b))
        }
        Dock::Top => {
            let [b, a] = Layout::vertical([inspect, main]).areas(area);
            (a, Some(b))
        }
        Dock::Left => {
            let [b, a] = Layout::horizontal([inspect, main]).areas(area);
            (a, Some(b))
        }
        Dock::Right => {
            let [a, b] = Layout::horizontal([main, inspect]).areas(area);
            (a, Some(b))
        }
        Dock::Hidden => (area, None),
    }
}

fn render_header(frame: &mut Frame, area: Rect, app: &App) {
    let titles = ["1  explorer", "2  providers"]
        .into_iter()
        .map(Line::from)
        .collect::<Vec<_>>();
    let selected = if app.main_tab == MainTab::Explorer {
        0
    } else {
        1
    };
    let tabs = Tabs::new(titles)
        .select(selected)
        .style(dim())
        .highlight_style(accent().add_modifier(Modifier::BOLD))
        .divider("  ");
    frame.render_widget(tabs, Rect::new(area.x, area.y, area.width, 1));
    let title = format!(
        "⚔ orc  {}  {}",
        app.scope
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("workspace"),
        app.scope.display()
    );
    frame.render_widget(
        Paragraph::new(title).style(dim()),
        Rect::new(area.x, area.y + 1, area.width, 1),
    );
}

fn render_main(frame: &mut Frame, area: Rect, app: &mut App) {
    let focused = app.focus == Focus::Main;
    let border = if focused { accent() } else { dim() };
    match app.main_tab {
        MainTab::Providers => render_providers(frame, area, app, border),
        MainTab::Explorer if app.explorer_view == ExplorerView::Tree => {
            render_tree(frame, area, app, border)
        }
        MainTab::Explorer if app.explorer_view == ExplorerView::Fleet => {
            render_fleet(frame, area, app, border)
        }
        MainTab::Explorer => {
            let run_title = app
                .active_run
                .as_deref()
                .and_then(|id| app.state.runs.iter().find(|run| run.id == id))
                .map_or_else(
                    || "session graph".into(),
                    |run| format!("{} · {}", run.name, run.status),
                );
            let block = Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(border)
                .title(format!(" {run_title}  graph g · tree t · fleet f "));
            let inner = block.inner(area);
            frame.render_widget(block, area);
            frame.render_widget(&mut app.flow, inner);
        }
    }
}

fn render_tree(frame: &mut Frame, area: Rect, app: &mut App, border: Style) {
    let items = app
        .tree
        .iter()
        .map(|row| {
            let branch = if row.children {
                if app.expanded.contains(&row.id) || app.expanded.is_empty() {
                    "▾"
                } else {
                    "▸"
                }
            } else {
                " "
            };
            let prefix = format!("{}{} ", "  ".repeat(row.depth), branch);
            ListItem::new(Line::from(vec![
                Span::styled(prefix, dim()),
                Span::styled(
                    status_glyph(row.status.unwrap_or(LifecycleStatus::Queued)).to_string(),
                    Style::default()
                        .fg(status_color(row.status.unwrap_or(LifecycleStatus::Queued))),
                ),
                Span::raw(" "),
                Span::styled(
                    row.title.clone(),
                    Style::default()
                        .fg(Color::Indexed(252))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("  {}", row.subtitle), dim()),
            ]))
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default().with_selected(Some(app.tree_at));
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(border)
                .title(" tree  graph  g · fleet  f "),
        )
        .highlight_style(
            Style::default()
                .bg(Color::Indexed(237))
                .fg(Color::Indexed(255)),
        )
        .highlight_symbol("  ");
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_providers(frame: &mut Frame, area: Rect, app: &mut App, border: Style) {
    let items = app
        .providers
        .iter()
        .map(|provider| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    provider.name.clone(),
                    Style::default()
                        .fg(Color::Indexed(252))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(
                        "  {} · {} actions  ",
                        provider.kind,
                        provider.all_capabilities().len()
                    ),
                    dim(),
                ),
                Span::raw(provider.description.clone()),
            ]))
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default().with_selected(Some(app.provider_at));
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(border)
                .title(" providers "),
        )
        .highlight_style(
            Style::default()
                .bg(Color::Indexed(237))
                .fg(Color::Indexed(255)),
        )
        .highlight_symbol("  ");
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_fleet(frame: &mut Frame, area: Rect, app: &mut App, border: Style) {
    let header = Row::new([
        "run", "state", "current", "elapsed", "tokens", "cost", "burn",
    ])
    .style(dim());
    let rows = app.state.runs.iter().map(|run| {
        let elapsed = UtcNow::elapsed(run.created_at, run.updated_at, run.status.active());
        Row::new([
            run.name.clone(),
            run.status.to_string(),
            run.current_node.clone().unwrap_or_else(|| "—".into()),
            elapsed,
            run.tokens.to_string(),
            format!("${:.2}", run.cost_usd),
            sparkline(&run.token_burn),
        ])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Percentage(24),
            Constraint::Length(11),
            Constraint::Percentage(20),
            Constraint::Length(9),
            Constraint::Length(9),
            Constraint::Length(9),
            Constraint::Fill(1),
        ],
    )
    .header(header)
    .row_highlight_style(
        Style::default()
            .bg(Color::Indexed(237))
            .fg(Color::Indexed(255)),
    )
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(border)
            .title(" fleet  graph  g · tree  t "),
    );
    let mut state = ratatui::widgets::TableState::default().with_selected(Some(app.fleet_at));
    frame.render_stateful_widget(table, area, &mut state);
}

struct UtcNow;
impl UtcNow {
    fn millis() -> u128 {
        chrono::Utc::now().timestamp_millis().unsigned_abs() as u128
    }

    fn elapsed(
        start: chrono::DateTime<chrono::Utc>,
        updated: chrono::DateTime<chrono::Utc>,
        active: bool,
    ) -> String {
        let end = if active { chrono::Utc::now() } else { updated };
        let seconds = (end - start).num_seconds().max(0);
        if seconds >= 3600 {
            format!("{}h{:02}m", seconds / 3600, seconds % 3600 / 60)
        } else {
            format!("{}m{:02}s", seconds / 60, seconds % 60)
        }
    }
}

fn sparkline(values: &[u64]) -> String {
    const BARS: &[char] = &['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let values = &values[values.len().saturating_sub(12)..];
    let max = values.iter().copied().max().unwrap_or(0);
    values
        .iter()
        .map(|value| BARS[value.saturating_mul(7).checked_div(max).unwrap_or(0) as usize])
        .collect()
}

fn render_detail(frame: &mut Frame, area: Rect, app: &App) {
    let border = if app.focus == Focus::Detail {
        accent()
    } else {
        dim()
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border)
        .title(" detail ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(details(app))
            .scroll((app.detail_scroll, 0))
            .wrap(Wrap { trim: false }),
        inner,
    );
}

fn render_output(frame: &mut Frame, area: Rect, app: &App) {
    let border = if app.focus == Focus::Output {
        accent()
    } else {
        dim()
    };
    let tabs = [
        OutputTab::Log,
        OutputTab::Activity,
        OutputTab::Output,
        OutputTab::Changes,
    ];
    let title = Line::from(
        tabs.iter()
            .map(|tab| {
                let name = format!(" {} ", format!("{tab:?}").to_lowercase());
                Span::styled(
                    name,
                    if *tab == app.output_tab {
                        accent().add_modifier(Modifier::BOLD)
                    } else {
                        dim()
                    },
                )
            })
            .collect::<Vec<_>>(),
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border)
        .title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let body = match app.output_tab {
        OutputTab::Log => selected_log(app),
        OutputTab::Activity => {
            if app.activity.is_empty() {
                "No activity loaded. Press i on a session.".into()
            } else {
                app.activity.clone()
            }
        }
        OutputTab::Output => selected_output(app),
        OutputTab::Changes => {
            if app.changes.is_empty() {
                "No changes loaded. Press c.".into()
            } else {
                app.changes.clone()
            }
        }
    };
    let body = body.into_text().unwrap_or_else(|_| body.into());
    frame.render_widget(
        Paragraph::new(body)
            .scroll((app.output_scroll, 0))
            .wrap(Wrap { trim: false }),
        inner,
    );
}

fn selected_log(app: &App) -> String {
    match app.selected() {
        Some(ItemRef::Node(run_id, node_id)) => app
            .state
            .runs
            .iter()
            .find(|run| run.id == run_id)
            .and_then(|run| run.nodes.iter().find(|node| node.id == node_id))
            .map(|node| {
                node.activity
                    .iter()
                    .map(|event| {
                        format!(
                            "{}  {:<10} {}",
                            event.at.format("%H:%M:%S"),
                            event.kind,
                            event.message
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "No events for this step.".into()),
        Some(ItemRef::Run(id)) => app
            .state
            .runs
            .iter()
            .find(|run| run.id == id)
            .map(|run| {
                if let Some(path) = &run.log_path
                    && let Ok(log) = fs::read_to_string(path)
                    && !log.trim().is_empty()
                {
                    return log;
                }
                let mut lines = vec![format!(
                    "{}  {}",
                    run.updated_at.format("%H:%M:%S"),
                    run.status
                )];
                for gate in &run.pending_gates {
                    lines.push(format!(
                        "{}  gate       {} before {} · {}",
                        gate.created_at.format("%H:%M:%S"),
                        gate.id,
                        gate.before,
                        gate.reason
                    ));
                }
                lines.join("\n")
            })
            .unwrap_or_default(),
        _ => "Select a run or step.".into(),
    }
}

fn selected_output(app: &App) -> String {
    match app.selected() {
        Some(ItemRef::Node(run_id, node_id)) => app
            .state
            .runs
            .iter()
            .find(|run| run.id == run_id)
            .and_then(|run| run.nodes.iter().find(|node| node.id == node_id))
            .and_then(|node| node.output.as_ref())
            .and_then(|value| serde_json::to_string_pretty(value).ok())
            .unwrap_or_else(|| "No output for this step.".into()),
        _ => "Select a workflow step.".into(),
    }
}

fn details(app: &App) -> String {
    match app.selected() {
        Some(ItemRef::Session(id)) => app.state.sessions.iter().find(|session| session.id == id).map(|session| format!("{}\n\nrole             {}\nharness          {}\nmodel            {}\nstatus           {}\npurpose          {}\ngoal             {}\nexpected output  {}\nsuccess          {}\ncompletion       {}\nnative session   {}\norc session      {}\nparent           {}\n\nproviders\n{}",
            session.title, session.role, session.harness, session.model.as_deref().unwrap_or("default"), session.status, session.purpose, session.goal, session.expected_output,
            if session.success_criteria.is_empty() { "unspecified".into() } else { session.success_criteria.join("; ") }, session.completion, session.native_id, session.id,
            session.parent_id.as_deref().unwrap_or("root"), if session.providers.is_empty() { "  none".into() } else { session.providers.iter().map(|binding| format!("  {:<14} {} · {:?} · {}", binding.kind, binding.provider, binding.status, binding.label)).collect::<Vec<_>>().join("\n") })).unwrap_or_default(),
        Some(ItemRef::Run(id)) => app.state.runs.iter().find(|run| run.id == id).map(|run| format!("{}\n\nstatus           {}\ngoal             {}\nexpected output  {}\nsteps            {}\ndefinition       {}\nrevision         {}\nrun              {}", run.name, run.status, run.goal, run.expected_output, run.nodes.len(), run.definition.as_deref().unwrap_or("dynamic"), run.revision.as_deref().unwrap_or("uncommitted"), run.id)).unwrap_or_default(),
        Some(ItemRef::Node(run_id, node_id)) => app.state.runs.iter().find(|run| run.id == run_id).and_then(|run| run.nodes.iter().find(|node| node.id == node_id)).map(|node| format!("{}\n\nrole             {}\nharness          {}\nmodel            {}\nstatus           {}\npurpose          {}\ngoal             {}\nexpected output  {}\nsuccess          {}\ncompletion       {}\nreview by        {}\nsession          {}", node.name, node.role, node.harness, node.model.as_deref().unwrap_or("default"), node.status, node.purpose, node.goal, node.expected_output, if node.success_criteria.is_empty() { "unspecified".into() } else { node.success_criteria.join("; ") }, node.completion, node.review_by.as_deref().unwrap_or("orchestrator"), node.session_id.as_deref().unwrap_or("unassigned"))).unwrap_or_default(),
        Some(ItemRef::Provider(name)) => app.providers.iter().find(|provider| provider.name == name).map(|provider| format!("{}\n\n{}\n\nkind      {}\npriority  {}\ncommand   {}\n\nactions\n{}", provider.name, provider.description, provider.kind, provider.priority, provider.command, provider.actions.iter().map(|(capability, description)| format!("  {:<20} {description}", capability.to_string())).collect::<Vec<_>>().join("\n"))).unwrap_or_default(),
        None => "Select an item.".into(),
    }
}

fn render_footer(frame: &mut Frame, area: Rect, app: &App) {
    let focus = match app.focus {
        Focus::Main => "main",
        Focus::Detail => "detail",
        Focus::Output => "output",
    };
    let actions = if app.focus == Focus::Main {
        &[
            "line",
            "focus-output",
            "focus-detail",
            "inspect-tabs",
            "help",
        ][..]
    } else {
        &["line", "page", "focus-main", "inspect-tabs", "help"][..]
    };
    let hints = BINDINGS
        .iter()
        .filter(|binding| actions.contains(&binding.id))
        .map(|binding| format!("{} {}", binding.keys, binding.short))
        .collect::<Vec<_>>()
        .join("   ");
    let status = if app.status.is_empty() {
        format!("{focus}   {hints}")
    } else {
        app.status.clone()
    };
    frame.render_widget(Paragraph::new(status).style(dim()), area);
}

struct Binding {
    id: &'static str,
    keys: &'static str,
    short: &'static str,
    description: &'static str,
}
const BINDINGS: &[Binding] = &[
    Binding {
        id: "focus-output",
        keys: "ctrl+j",
        short: "output",
        description: "focus the output pane",
    },
    Binding {
        id: "focus-detail",
        keys: "ctrl+l",
        short: "detail",
        description: "focus the detail pane",
    },
    Binding {
        id: "focus-main",
        keys: "ctrl+k",
        short: "main",
        description: "focus the main pane",
    },
    Binding {
        id: "line",
        keys: "h/j/k/l",
        short: "navigate",
        description: "move in the focused pane",
    },
    Binding {
        id: "page",
        keys: "ctrl+d/u",
        short: "page",
        description: "half-page the focused pane",
    },
    Binding {
        id: "inspect-tabs",
        keys: "tab/S-tab",
        short: "inspector tab",
        description: "change inspector tab",
    },
    Binding {
        id: "view",
        keys: "g/t/f",
        short: "graph/tree/fleet",
        description: "change Explorer view",
    },
    Binding {
        id: "top-tabs",
        keys: "1/2",
        short: "top tab",
        description: "open Explorer or Providers",
    },
    Binding {
        id: "open",
        keys: "enter",
        short: "open",
        description: "attach the selected session or validate a provider",
    },
    Binding {
        id: "gate",
        keys: "g",
        short: "gate",
        description: "answer a selected pending gate or open graph view",
    },
    Binding {
        id: "cancel",
        keys: "x",
        short: "stop",
        description: "stop the selected run after confirmation",
    },
    Binding {
        id: "viewport",
        keys: "+/-/0/o",
        short: "viewport",
        description: "zoom, reset, or fit the graph",
    },
    Binding {
        id: "resize",
        keys: "+/-",
        short: "resize",
        description: "resize the focused output pane",
    },
    Binding {
        id: "activity",
        keys: "i",
        short: "activity",
        description: "load session activity",
    },
    Binding {
        id: "changes",
        keys: "c",
        short: "changes",
        description: "load workspace changes",
    },
    Binding {
        id: "relayout",
        keys: "R",
        short: "relayout",
        description: "tidy and fit the graph",
    },
    Binding {
        id: "refresh",
        keys: "r",
        short: "refresh",
        description: "refresh state and providers",
    },
    Binding {
        id: "dock",
        keys: "space i h/j/k/l",
        short: "dock",
        description: "move or hide the inspector",
    },
    Binding {
        id: "help",
        keys: "?",
        short: "help",
        description: "show every key",
    },
    Binding {
        id: "quit",
        keys: "q/ctrl+c",
        short: "quit",
        description: "leave Orc",
    },
];

fn render_help(frame: &mut Frame, area: Rect) {
    let width = area.width.min(72);
    let height = area.height.min((BINDINGS.len() + 4) as u16);
    if width < 30 || height < 8 {
        return;
    }
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, popup);
    let lines = BINDINGS
        .iter()
        .map(|binding| {
            Line::from(vec![
                Span::styled(format!(" {:<20}", binding.keys), accent()),
                Span::raw(binding.description),
            ])
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(accent())
                .title(" keys ")
                .title_bottom(Line::from(" ? / esc to close ").alignment(Alignment::Center)),
        ),
        popup,
    );
}

fn render_confirmation(frame: &mut Frame, area: Rect, confirmation: &Confirmation) {
    let message = match confirmation {
        Confirmation::Approve { run_id, gate_id } => {
            format!("Approve {gate_id} for {run_id} and resume?")
        }
        Confirmation::Cancel { run_id } => format!("Stop {run_id} and discard in-flight work?"),
    };
    let width = area.width.min(72);
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + area.height.saturating_sub(5) / 2,
        width,
        5.min(area.height),
    );
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(message).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(accent())
                .title(" confirm ")
                .title_bottom(
                    Line::from(" y/enter confirm · n/esc cancel ").alignment(Alignment::Center),
                ),
        ),
        popup,
    );
}

fn terminal_reply(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char(']')) && key.modifiers.contains(KeyModifiers::ALT)
}

fn truncate(value: &str, width: usize) -> String {
    if value.width() <= width {
        return value.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let mut output = String::new();
    for ch in value.chars() {
        if format!("{output}{ch}…").width() > width {
            break;
        }
        output.push(ch);
    }
    output.push('…');
    output
}

fn status_glyph(status: LifecycleStatus) -> char {
    match status {
        LifecycleStatus::Working => {
            const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
            SPINNER[(UtcNow::millis() / 90 % SPINNER.len() as u128) as usize]
        }
        LifecycleStatus::Done => '✓',
        LifecycleStatus::Failed => '×',
        LifecycleStatus::Blocked => '!',
        LifecycleStatus::Waiting | LifecycleStatus::Queued => '○',
        LifecycleStatus::Archived | LifecycleStatus::Disconnected | LifecycleStatus::Cancelled => {
            '·'
        }
    }
}
fn status_color(status: LifecycleStatus) -> Color {
    match status {
        LifecycleStatus::Working => Color::Indexed(114),
        LifecycleStatus::Done => Color::Indexed(108),
        LifecycleStatus::Failed => Color::Indexed(167),
        LifecycleStatus::Blocked => Color::Indexed(179),
        _ => Color::Indexed(244),
    }
}
fn accent() -> Style {
    Style::default().fg(Color::Indexed(109))
}
fn dim() -> Style {
    Style::default().fg(Color::Indexed(244))
}

struct TerminalGuard;
impl TerminalGuard {
    fn enter() -> Result<(Self, Terminal<CrosstermBackend<io::Stdout>>)> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        Ok((Self, terminal))
    }
}
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
    }
}

pub fn run(config: Config, scope: &Path) -> Result<()> {
    let scope = crate::state::resolve_scope(scope)?;
    let state = control::reconcile(&config, &scope)?;
    let providers = provider::discover(&config)?;
    let mut app = App::new(config, scope, state, providers);
    let (_guard, mut terminal) = TerminalGuard::enter()?;
    loop {
        terminal.draw(|frame| render(frame, &mut app))?;
        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) if app.handle_key(key) => break,
                Event::Mouse(mouse) => app.handle_mouse(mouse),
                Event::Resize(_, _) => app.flow.request_fit_view(),
                _ => {}
            }
        }
        if app.last_refresh.elapsed() >= Duration::from_millis(app.config.ui.refresh_ms) {
            app.refresh();
        }
        app.flow.tick_animation(Duration::from_millis(50));
        let _ = app.flow.tick_auto_pan(Duration::from_millis(50));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_is_generated_from_bindings() {
        assert!(BINDINGS.iter().any(|binding| binding.id == "focus-output"));
        assert!(
            BINDINGS
                .iter()
                .all(|binding| !binding.description.is_empty())
        );
    }
}
