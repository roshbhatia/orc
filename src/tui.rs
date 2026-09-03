use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
    sync::{
        OnceLock,
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant},
};

use ansi_to_tui::IntoText as _;
use anyhow::Result;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use minijinja::{Environment, context};
use rataflow::{
    Direction, Edge, EdgeContent, EdgePathContext, EdgeRenderContext, EdgeStyle, FitViewOptions,
    Flow, Handle, HandlePosition, Node, NodeContent, NodeRenderContext, Path as EdgePath,
    Reconnectable, StepEdge, Sugiyama, Theme,
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    buffer::Buffer,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{
        Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Widget, Wrap,
    },
};
use unicode_width::UnicodeWidthStr;

use crate::{
    config::Config,
    control,
    domain::{
        CompletionTarget, JudgePolicy, LifecycleStatus, Session, WorkflowNode, WorkflowRun,
        WorkspaceState,
    },
    preferences::{self, WorkspacePreferences},
    provider::{self, Action, Manifest},
    workflow,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MainTab {
    Work,
    Integrations,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExplorerView {
    Tree,
    Graph,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Focus {
    Main,
    Inspector,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OutputTab {
    Summary,
    Timeline,
    Result,
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
    Approve {
        run_id: String,
        gate_id: String,
    },
    Cancel {
        run_id: String,
    },
    Prune {
        session_id: String,
        title: String,
    },
    DeleteNode {
        run_id: String,
        node_id: String,
        title: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EditField {
    Goal,
    ExpectedOutput,
    Criteria,
    Harness,
    Model,
    Execution,
    Judge,
    Dependencies,
}

#[derive(Clone, Debug)]
struct NodeEditor {
    run_id: String,
    node_id: String,
    field: EditField,
    goal: String,
    expected_output: String,
    criteria: String,
    harness: String,
    model: String,
    execution: String,
    judge: String,
    dependencies: String,
}

impl NodeEditor {
    fn current_mut(&mut self) -> &mut String {
        match self.field {
            EditField::Goal => &mut self.goal,
            EditField::ExpectedOutput => &mut self.expected_output,
            EditField::Criteria => &mut self.criteria,
            EditField::Harness => &mut self.harness,
            EditField::Model => &mut self.model,
            EditField::Execution => &mut self.execution,
            EditField::Judge => &mut self.judge,
            EditField::Dependencies => &mut self.dependencies,
        }
    }

    fn next(&mut self, backwards: bool) {
        const FIELDS: &[EditField] = &[
            EditField::Goal,
            EditField::ExpectedOutput,
            EditField::Criteria,
            EditField::Harness,
            EditField::Model,
            EditField::Execution,
            EditField::Judge,
            EditField::Dependencies,
        ];
        let at = FIELDS
            .iter()
            .position(|field| *field == self.field)
            .unwrap_or(0) as isize;
        let by = if backwards { -1 } else { 1 };
        self.field = FIELDS[(at + by).rem_euclid(FIELDS.len() as isize) as usize];
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RuntimeActivity {
    Active,
    Idle,
    Stalled,
}

impl RuntimeActivity {
    fn for_session(session: &Session) -> Option<Self> {
        if !session.status.active() {
            return None;
        }
        let age = chrono::Utc::now()
            .signed_duration_since(session.updated_at)
            .num_seconds()
            .max(0);
        Some(if age <= 30 {
            Self::Active
        } else if age <= 300 {
            Self::Idle
        } else {
            Self::Stalled
        })
    }

    fn label(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Idle => "idle",
            Self::Stalled => "stalled",
        }
    }
}

fn session_placement(session: &Session) -> String {
    session
        .providers
        .iter()
        .find(|binding| {
            binding.kind == crate::domain::ProviderKind::Execution
                && binding.status == crate::domain::BindingStatus::Active
        })
        .map(|binding| binding.label.clone())
        .or_else(|| session.provider_ref.clone())
        .unwrap_or_else(|| "external".into())
}

fn run_phase(run: &WorkflowRun) -> String {
    if run.status == LifecycleStatus::Queued && run.current_node.is_none() {
        "proposed".into()
    } else {
        run.status.to_string()
    }
}

#[derive(Clone, Debug)]
struct AgentCard {
    kind: String,
    title: String,
    subtitle: String,
    goal: String,
    status: LifecycleStatus,
    active: bool,
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
                format!(
                    "{} ",
                    if self.active {
                        spinner_glyph()
                    } else {
                        status_glyph(self.status)
                    }
                ),
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
    relation: String,
    lane: usize,
}

impl EdgeContent for RelationEdge {
    fn compute_path(&self, ctx: &EdgePathContext) -> EdgePath {
        if self.relation != "feedback" {
            return self.inner.compute_path(ctx);
        }
        let target = ctx.target_bounds.unwrap_or(ctx.source_bounds);
        let lane_y = ctx.source_bounds.bottom().max(target.bottom()) + 2.0 + self.lane as f64 * 2.0;
        EdgePath::new(
            vec![
                ctx.from,
                rataflow::Position::new(ctx.from.x, lane_y),
                rataflow::Position::new(ctx.to.x, lane_y),
                ctx.to,
            ],
            ctx.source_position,
            ctx.target_position,
        )
        .with_label_position(rataflow::Position::new(
            (ctx.from.x + ctx.to.x) / 2.0,
            lane_y,
        ))
    }
    fn render(&self, ctx: &EdgeRenderContext, buf: &mut Buffer) {
        let color = if self.active {
            ctx.theme.palette().success
        } else {
            ctx.theme.palette().muted
        };
        let style = if self.relation == "feedback" {
            EdgeStyle::dotted()
        } else {
            EdgeStyle::default()
        }
        .with_stroke_style(Style::default().fg(color))
        .with_label_style(Style::default().fg(color));
        let label = edge_label(&self.relation).map(Text::raw);
        ctx.render_path(&style, label.as_ref(), buf);
    }
}

type AgentFlow = Flow<AgentCard, RelationEdge>;
type GraphEdge = (String, String, String, bool);

fn edge_label(relation: &str) -> Option<&str> {
    match relation {
        "depends_on" | "spawned" | "delegates" => None,
        "reviewed_by" => Some("review"),
        "feedback" => Some("retry"),
        "reports" => Some("reports"),
        "conditional" | "routes_to" => Some("condition"),
        other if other.starts_with("route") => Some("condition"),
        other => Some(other),
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct HitAreas {
    tabs: Rect,
    main: Rect,
    graph: Rect,
    inspector: Option<Rect>,
}

#[derive(Clone, Debug)]
enum BootState {
    Loading { started_at: Instant },
    Ready,
    Failed(String),
}

enum BackgroundResult {
    Refresh(Result<(WorkspaceState, Vec<Manifest>), String>),
    Enrichment {
        generation: u64,
        result: Result<WorkspaceState, String>,
    },
    Activity {
        session_id: String,
        result: Result<String, String>,
    },
    ProviderActivity {
        provider_name: String,
        result: String,
    },
    Changes(Result<String, String>),
    Validation(Result<String, String>),
    Action(Result<String, String>),
}

struct App {
    scope: PathBuf,
    config: Config,
    state: WorkspaceState,
    providers: Vec<Manifest>,
    flow: AgentFlow,
    graph_items: BTreeMap<String, ItemRef>,
    tree: Vec<TreeRow>,
    tree_at: usize,
    active_run: Option<String>,
    provider_at: usize,
    expanded: BTreeSet<String>,
    main_tab: MainTab,
    explorer_view: ExplorerView,
    focus: Focus,
    output_tab: OutputTab,
    inspector_scroll: u16,
    dock: Dock,
    leader: bool,
    pending: Option<char>,
    help: bool,
    confirmation: Option<Confirmation>,
    status: String,
    status_at: Option<Instant>,
    activity: BTreeMap<String, String>,
    activity_loaded_at: BTreeMap<String, Instant>,
    activity_loading: BTreeSet<String>,
    provider_activity: BTreeMap<String, String>,
    provider_activity_loaded_at: BTreeMap<String, Instant>,
    provider_activity_loading: BTreeSet<String>,
    provider_report: String,
    changes: String,
    changes_loaded: bool,
    changes_loading: bool,
    last_refresh: Instant,
    refresh_inflight: bool,
    refresh_requested: bool,
    enrichment_inflight: bool,
    enrichment_requested: bool,
    enrichment_due_at: Instant,
    state_generation: u64,
    resize_at: Option<Instant>,
    hit: HitAreas,
    graph_signature: String,
    boot: BootState,
    preferences: WorkspacePreferences,
    editor: Option<NodeEditor>,
}

impl App {
    fn loading(config: Config, scope: PathBuf) -> Self {
        let preferences = preferences::read(&scope).unwrap_or_default();
        let mut config = config;
        config.ui.inspector_percent = preferences.inspector_percent;
        let mut app = Self::new(
            config,
            scope.clone(),
            WorkspaceState::empty(scope.display().to_string()),
            Vec::new(),
        );
        app.boot = BootState::Loading {
            started_at: Instant::now(),
        };
        app.refresh_inflight = false;
        app.enrichment_requested = true;
        app.apply_preferences(preferences);
        app
    }

    fn new(
        config: Config,
        scope: PathBuf,
        state: WorkspaceState,
        providers: Vec<Manifest>,
    ) -> Self {
        let enrichment_due_at = Instant::now() + enrichment_interval(&config);
        let active_run = state
            .runs
            .iter()
            .find(|run| run.status.active())
            .map(|run| run.id.clone());
        let expanded = default_expansions(&state);
        let mut app = Self {
            scope,
            config,
            state,
            providers,
            flow: new_flow(),
            graph_items: BTreeMap::new(),
            tree: Vec::new(),
            tree_at: 0,
            active_run,
            provider_at: 0,
            expanded,
            main_tab: MainTab::Work,
            explorer_view: ExplorerView::Tree,
            focus: Focus::Main,
            output_tab: OutputTab::Summary,
            inspector_scroll: 0,
            dock: Dock::Bottom,
            leader: false,
            pending: None,
            help: false,
            confirmation: None,
            status: String::new(),
            status_at: None,
            activity: BTreeMap::new(),
            activity_loaded_at: BTreeMap::new(),
            activity_loading: BTreeSet::new(),
            provider_activity: BTreeMap::new(),
            provider_activity_loaded_at: BTreeMap::new(),
            provider_activity_loading: BTreeSet::new(),
            provider_report: String::new(),
            changes: String::new(),
            changes_loaded: false,
            changes_loading: false,
            last_refresh: Instant::now(),
            refresh_inflight: false,
            refresh_requested: false,
            enrichment_inflight: false,
            enrichment_requested: false,
            enrichment_due_at,
            state_generation: 0,
            resize_at: None,
            hit: HitAreas::default(),
            graph_signature: String::new(),
            boot: BootState::Ready,
            preferences: WorkspacePreferences::default(),
            editor: None,
        };
        app.rebuild(true);
        app
    }

    fn apply_preferences(&mut self, preferences: WorkspacePreferences) {
        self.explorer_view = if preferences.view == "graph" {
            ExplorerView::Graph
        } else {
            ExplorerView::Tree
        };
        self.output_tab = match preferences.inspector_tab.as_str() {
            "timeline" => OutputTab::Timeline,
            "result" => OutputTab::Result,
            "changes" => OutputTab::Changes,
            _ => OutputTab::Summary,
        };
        self.dock = match preferences.inspector_dock.as_str() {
            "top" => Dock::Top,
            "left" => Dock::Left,
            "right" => Dock::Right,
            "hidden" => Dock::Hidden,
            _ => Dock::Bottom,
        };
        self.config.ui.inspector_percent = preferences.inspector_percent;
        self.preferences = preferences;
    }

    fn persist_preferences(&mut self) {
        self.preferences.view = if self.explorer_view == ExplorerView::Graph {
            "graph"
        } else {
            "tree"
        }
        .into();
        self.preferences.inspector_tab = format!("{:?}", self.output_tab).to_lowercase();
        self.preferences.inspector_dock = format!("{:?}", self.dock).to_lowercase();
        self.preferences.inspector_percent = self.config.ui.inspector_percent;
        self.preferences.selected_item = self.tree.get(self.tree_at).map(|row| row.id.clone());
        if let Err(error) = preferences::write(&self.scope, &self.preferences) {
            self.set_status(format!("could not save workspace view: {error:#}"));
        }
    }

    fn set_status(&mut self, status: impl Into<String>) {
        self.status = status.into();
        self.status_at = Some(Instant::now());
    }

    fn clear_status(&mut self) {
        self.status.clear();
        self.status_at = None;
    }

    fn switch_main_tab(&mut self, tab: MainTab) {
        self.clear_status();
        self.main_tab = tab;
        self.focus = Focus::Main;
    }

    fn visible_status(&self) -> Option<&str> {
        self.status_at
            .filter(|at| at.elapsed() < Duration::from_secs(3))
            .map(|_| self.status.as_str())
            .filter(|status| !status.is_empty())
    }

    fn rebuild(&mut self, force_layout: bool) {
        let selected_tree_id = self.tree.get(self.tree_at).map(|row| row.id.clone());
        self.tree = tree_rows(&self.state, &self.expanded);
        self.tree_at = selected_tree_id
            .as_deref()
            .and_then(|id| self.tree.iter().position(|row| row.id == id))
            .unwrap_or_else(|| self.tree_at.min(self.tree.len().saturating_sub(1)));
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
            refresh_flow_content(&mut self.flow, &self.state, self.active_run.as_deref());
        }
    }

    fn request_refresh(&mut self, tx: &Sender<BackgroundResult>) {
        if self.refresh_inflight || self.enrichment_inflight {
            self.refresh_requested = true;
            return;
        }
        self.refresh_inflight = true;
        self.refresh_requested = false;
        let config = self.config.clone();
        let scope = self.scope.clone();
        let tx = tx.clone();
        thread::spawn(move || {
            let result = control::read_workspace(&scope)
                .and_then(|state| provider::discover(&config).map(|providers| (state, providers)))
                .map_err(|error| format!("{error:#}"));
            let _ = tx.send(BackgroundResult::Refresh(result));
        });
    }

    fn request_enrichment(&mut self, tx: &Sender<BackgroundResult>) {
        if !self.enrichment_ready(Instant::now()) {
            return;
        }
        self.enrichment_inflight = true;
        self.enrichment_requested = false;
        let config = self.config.clone();
        let scope = self.scope.clone();
        let tx = tx.clone();
        let generation = self.state_generation;
        thread::spawn(move || {
            let result = control::reconcile(&config, &scope).map_err(|error| format!("{error:#}"));
            let _ = tx.send(BackgroundResult::Enrichment { generation, result });
        });
    }

    fn enrichment_ready(&self, now: Instant) -> bool {
        !self.enrichment_inflight
            && !self.refresh_inflight
            && (self.enrichment_requested || now >= self.enrichment_due_at)
    }

    fn apply_background(&mut self, result: BackgroundResult) {
        match result {
            BackgroundResult::Refresh(result) => {
                self.refresh_inflight = false;
                self.last_refresh = Instant::now();
                match result {
                    Ok((state, providers)) => {
                        let first_load = matches!(self.boot, BootState::Loading { .. });
                        self.state = state;
                        self.state_generation = self.state_generation.wrapping_add(1);
                        self.providers = providers;
                        if first_load {
                            self.expanded = default_expansions(&self.state);
                        }
                        if self
                            .active_run
                            .as_ref()
                            .is_none_or(|id| !self.state.runs.iter().any(|run| run.id == *id))
                        {
                            self.active_run = self
                                .state
                                .runs
                                .iter()
                                .find(|run| run.status.active())
                                .map(|run| run.id.clone());
                        }
                        self.boot = BootState::Ready;
                        self.rebuild(false);
                        if first_load
                            && let Some(selected) = self.preferences.selected_item.as_deref()
                            && let Some(index) = self.tree.iter().position(|row| row.id == selected)
                        {
                            self.tree_at = index;
                            if let Some(graph_id) =
                                self.graph_items.iter().find_map(|(graph_id, item)| {
                                    (item == &self.tree[index].item).then(|| graph_id.clone())
                                })
                            {
                                self.flow.select_node(&graph_id);
                            }
                        }
                    }
                    Err(error) => {
                        if matches!(self.boot, BootState::Loading { .. }) {
                            self.boot = BootState::Failed(error);
                        } else {
                            self.set_status(format!("refresh failed: {error}"));
                        }
                    }
                }
            }
            BackgroundResult::Enrichment { generation, result } => {
                self.enrichment_inflight = false;
                if generation != self.state_generation {
                    self.enrichment_due_at =
                        Instant::now() + enrichment_retry_interval(&self.config);
                    return;
                }
                match result {
                    Ok(state) => {
                        self.state = state;
                        self.state_generation = self.state_generation.wrapping_add(1);
                        self.enrichment_due_at = Instant::now() + enrichment_interval(&self.config);
                        self.rebuild(false);
                    }
                    Err(error) => {
                        self.enrichment_due_at =
                            Instant::now() + enrichment_retry_interval(&self.config);
                        self.set_status(format!("session discovery failed: {error}"));
                    }
                }
            }
            BackgroundResult::Activity { session_id, result } => {
                self.activity_loading.remove(&session_id);
                self.activity_loaded_at
                    .insert(session_id.clone(), Instant::now());
                self.activity.insert(
                    session_id,
                    result.unwrap_or_else(|error| format!("Activity provider failed: {error}")),
                );
            }
            BackgroundResult::ProviderActivity {
                provider_name,
                result,
            } => {
                self.provider_activity_loading.remove(&provider_name);
                self.provider_activity_loaded_at
                    .insert(provider_name.clone(), Instant::now());
                self.provider_activity.insert(provider_name, result);
            }
            BackgroundResult::Changes(result) => {
                self.changes_loading = false;
                self.changes_loaded = true;
                self.changes =
                    result.unwrap_or_else(|error| format!("Changes provider failed: {error}"));
            }
            BackgroundResult::Validation(result) => {
                self.provider_report =
                    result.unwrap_or_else(|error| format!("Provider validation failed: {error}"));
            }
            BackgroundResult::Action(result) => {
                self.set_status(result.unwrap_or_else(|error| format!("Action failed: {error}")));
                self.refresh_requested = true;
            }
        }
    }

    fn request_changes(&mut self, tx: &Sender<BackgroundResult>, force: bool) {
        if self.changes_loading || (!force && self.changes_loaded) || self.providers.is_empty() {
            return;
        }
        self.changes_loading = true;
        let config = self.config.clone();
        let providers = self.providers.clone();
        let scope = self.scope.clone();
        let tx = tx.clone();
        thread::spawn(move || {
            let request = provider::action_request(Action::Changes, &scope, None, "right");
            let result = provider::resolve_plan(&config, &providers, Action::Changes, request)
                .and_then(|plan| provider::capture_plan(&plan, &scope, config.provider_timeout()))
                .map_err(|error| format!("{error:#}"));
            let _ = tx.send(BackgroundResult::Changes(result));
        });
    }

    fn request_activity(&mut self, tx: &Sender<BackgroundResult>, force: bool) {
        let Some(session) = self.selected_session().cloned() else {
            return;
        };
        if self.activity_loading.contains(&session.id) {
            return;
        }
        let fresh = self.activity_loaded_at.get(&session.id).is_some_and(|at| {
            at.elapsed() < Duration::from_millis(self.config.ui.activity_refresh_ms)
        });
        if !force && fresh {
            return;
        }
        self.activity_loading.insert(session.id.clone());
        let session_id = session.id.clone();
        let config = self.config.clone();
        let providers = self.providers.clone();
        let scope = self.scope.clone();
        let tx = tx.clone();
        thread::spawn(move || {
            let request =
                provider::action_request(Action::Activity, &scope, Some(&session), "right");
            let result = provider::resolve_activity_plan(&config, &providers, request)
                .and_then(|plan| provider::capture_plan(&plan, &scope, config.provider_timeout()))
                .map_err(|error| format!("{error:#}"));
            let _ = tx.send(BackgroundResult::Activity { session_id, result });
        });
    }

    fn request_provider_activity(&mut self, tx: &Sender<BackgroundResult>, force: bool) {
        let Some(ItemRef::Provider(provider_name)) = self.selected() else {
            return;
        };
        if self.provider_activity_loading.contains(&provider_name) {
            return;
        }
        let fresh = self
            .provider_activity_loaded_at
            .get(&provider_name)
            .is_some_and(|at| at.elapsed() < Duration::from_secs(2));
        if !force && fresh {
            return;
        }
        self.provider_activity_loading.insert(provider_name.clone());
        let tx = tx.clone();
        thread::spawn(move || {
            let result = provider::recent_activity(&provider_name);
            let _ = tx.send(BackgroundResult::ProviderActivity {
                provider_name,
                result,
            });
        });
    }

    fn selected(&self) -> Option<ItemRef> {
        match self.main_tab {
            MainTab::Integrations => self
                .providers
                .get(self.provider_at)
                .map(|provider| ItemRef::Provider(provider.name.clone())),
            MainTab::Work if self.explorer_view == ExplorerView::Tree => {
                self.tree.get(self.tree_at).map(|row| row.item.clone())
            }
            MainTab::Work => self
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
            ItemRef::Run(id) => self
                .state
                .runs
                .iter()
                .find(|run| run.id == id)
                .and_then(|run| {
                    run.orchestrator_id.as_deref().or_else(|| {
                        run.current_node.as_deref().and_then(|current| {
                            run.nodes
                                .iter()
                                .find(|node| node.id == current)
                                .and_then(|node| node.session_id.as_deref())
                        })
                    })
                })
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
        if let Some(session) = self.selected_session() {
            self.confirmation = Some(Confirmation::Prune {
                session_id: session.id.clone(),
                title: session.title.clone(),
            });
            return;
        }
        if let Some(run_id) = self.selected_run_id() {
            self.confirmation = Some(Confirmation::Cancel { run_id });
        } else {
            self.set_status("select a run first");
        }
    }

    fn confirm(&mut self, tx: &Sender<BackgroundResult>) {
        let Some(confirmation) = self.confirmation.take() else {
            return;
        };
        self.set_status("applying action…");
        let config = self.config.clone();
        let scope = self.scope.clone();
        let tx = tx.clone();
        thread::spawn(move || {
            let result: Result<(String, LifecycleStatus)> = match confirmation {
                Confirmation::Approve { run_id, gate_id } => {
                    workflow::approve(&config, &scope, &run_id, Some(&gate_id), false)
                        .and_then(|_| workflow::spawn(&scope, &run_id))
                        .map(|run| (run.name, run.status))
                }
                Confirmation::Cancel { run_id } => {
                    workflow::cancel(&scope, &run_id).map(|run| (run.name, run.status))
                }
                Confirmation::Prune { session_id, .. } => {
                    control::prune(&config, &scope, &session_id)
                        .map(|session| (session.title, session.status))
                }
                Confirmation::DeleteNode {
                    run_id,
                    node_id,
                    title,
                } => workflow::delete_run_node(&config, &scope, &run_id, &node_id)
                    .map(|_| (title, LifecycleStatus::Archived)),
            };
            let result = result
                .map(|(name, status)| format!("{name} is {status}"))
                .map_err(|error| format!("{error:#}"));
            let _ = tx.send(BackgroundResult::Action(result));
        });
    }

    fn move_main(&mut self, direction: Direction) {
        match self.main_tab {
            MainTab::Integrations => match direction {
                Direction::Up => self.provider_at = self.provider_at.saturating_sub(1),
                Direction::Down => {
                    self.provider_at =
                        (self.provider_at + 1).min(self.providers.len().saturating_sub(1))
                }
                _ => {}
            },
            MainTab::Work if self.explorer_view == ExplorerView::Tree => match direction {
                Direction::Up => self.tree_at = self.tree_at.saturating_sub(1),
                Direction::Down => {
                    self.tree_at = (self.tree_at + 1).min(self.tree.len().saturating_sub(1))
                }
                Direction::Left => self.collapse(),
                Direction::Right => self.expand(),
            },
            MainTab::Work => self.flow.select_node_in_direction(direction),
        }
        self.inspector_scroll = 0;
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

    fn open_selected(&mut self, tx: &Sender<BackgroundResult>) {
        if let Some(session) = self.selected_session().cloned() {
            self.set_status("opening session through providers");
            let config = self.config.clone();
            let scope = self.scope.clone();
            let tx = tx.clone();
            thread::spawn(move || {
                let result =
                    control::attach_quiet(&config, &scope, &session.id, Action::Attach, "right")
                        .and_then(|outcome| {
                            if outcome.code == 0 {
                                let verb = match outcome.disposition {
                                    control::AttachDisposition::Focused => "focused",
                                    control::AttachDisposition::Launched => "launch requested for",
                                };
                                Ok(format!("{verb} {}", session.title))
                            } else {
                                anyhow::bail!("attach exited with {}", outcome.code)
                            }
                        })
                        .map_err(|error| format!("{error:#}"));
                let _ = tx.send(BackgroundResult::Action(result));
            });
        } else if self.main_tab == MainTab::Integrations {
            self.validate_provider(tx);
        } else if matches!(self.selected(), Some(ItemRef::Run(_))) {
            self.set_status("this run has no associated agent display");
        } else if matches!(self.selected(), Some(ItemRef::Node(_, _))) {
            self.set_status("this step has no associated live agent");
        }
    }

    fn drill_down(&mut self) {
        let Some(ItemRef::Run(run_id)) = self.selected() else {
            self.set_status("select a run to open its graph");
            return;
        };
        self.active_run = Some(run_id);
        self.explorer_view = ExplorerView::Graph;
        self.focus = Focus::Main;
        self.rebuild(true);
        self.flow.request_fit_view();
        self.set_status("opened workflow graph");
        self.persist_preferences();
    }

    fn load_output(&mut self, action: Action, tx: &Sender<BackgroundResult>) {
        match action {
            Action::Activity => {
                if self.selected_session().is_none() {
                    self.set_status("select an agent first");
                    return;
                }
                self.request_activity(tx, true);
                self.output_tab = OutputTab::Timeline;
            }
            Action::Changes => {
                self.request_changes(tx, true);
                self.output_tab = OutputTab::Changes;
            }
            _ => return,
        }
        self.focus = Focus::Inspector;
        self.inspector_scroll = 0;
    }

    fn validate_provider(&mut self, tx: &Sender<BackgroundResult>) {
        let name = match self.selected() {
            Some(ItemRef::Provider(name)) => name,
            _ => return,
        };
        let config = self.config.clone();
        let scope = self.scope.clone();
        let tx = tx.clone();
        thread::spawn(move || {
            let result = provider::validate_all(&config, &scope, Some(&name))
                .map(|results| {
                    results
                        .into_iter()
                        .flat_map(|result| {
                            result.checks.into_iter().map(|check| {
                                format!("{:?}  {}  {}", check.status, check.name, check.message)
                            })
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .map_err(|error| format!("{error:#}"));
            let _ = tx.send(BackgroundResult::Validation(result));
        });
        self.output_tab = OutputTab::Timeline;
        self.focus = Focus::Inspector;
    }

    fn open_node_editor(&mut self) {
        let Some(ItemRef::Node(run_id, node_id)) = self.selected() else {
            self.set_status("select a workflow step to edit");
            return;
        };
        let Some(run) = self.state.runs.iter().find(|run| run.id == run_id) else {
            return;
        };
        let Some(node) = run.nodes.iter().find(|node| node.id == node_id) else {
            return;
        };
        let dependencies = run
            .edges
            .iter()
            .filter(|edge| edge.to == node_id && edge.relationship == "depends_on")
            .map(|edge| edge.from.clone())
            .collect::<Vec<_>>()
            .join(", ");
        self.editor = Some(NodeEditor {
            run_id,
            node_id,
            field: EditField::Goal,
            goal: node.goal.clone(),
            expected_output: node.expected_output.clone(),
            criteria: node.success_criteria.join(", "),
            harness: node.harness.clone(),
            model: node.model.clone().unwrap_or_default(),
            execution: node.execution.clone().unwrap_or_else(|| "local".into()),
            judge: node.judge_policy.to_string(),
            dependencies,
        });
    }

    fn save_node_editor(&mut self, tx: &Sender<BackgroundResult>) {
        let Some(editor) = self.editor.take() else {
            return;
        };
        let prior_dependencies = self
            .state
            .runs
            .iter()
            .find(|run| run.id == editor.run_id)
            .map(|run| {
                run.edges
                    .iter()
                    .filter(|edge| edge.to == editor.node_id && edge.relationship == "depends_on")
                    .map(|edge| edge.from.clone())
                    .collect::<BTreeSet<_>>()
            })
            .unwrap_or_default();
        let dependencies = editor
            .dependencies
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        let judge = editor.judge.parse::<JudgePolicy>();
        let config = self.config.clone();
        let scope = self.scope.clone();
        let tx = tx.clone();
        self.set_status("saving versioned workflow edit…");
        thread::spawn(move || {
            let result = (|| -> Result<String> {
                let judge_policy = judge.map_err(anyhow::Error::msg)?;
                workflow::edit_run_node(
                    &config,
                    &scope,
                    &editor.run_id,
                    &editor.node_id,
                    workflow::NodeEdit {
                        goal: Some(editor.goal),
                        expected_output: Some(editor.expected_output),
                        success_criteria: Some(
                            editor
                                .criteria
                                .split(',')
                                .map(str::trim)
                                .filter(|value| !value.is_empty())
                                .map(str::to_owned)
                                .collect(),
                        ),
                        harness: Some(editor.harness),
                        model: (!editor.model.is_empty()).then_some(editor.model),
                        execution: Some(editor.execution),
                        judge_policy: Some(judge_policy),
                    },
                )?;
                for dependency in prior_dependencies.difference(&dependencies) {
                    workflow::set_run_dependency(
                        &config,
                        &scope,
                        &editor.run_id,
                        &editor.node_id,
                        dependency,
                        false,
                    )?;
                }
                for dependency in dependencies.difference(&prior_dependencies) {
                    workflow::set_run_dependency(
                        &config,
                        &scope,
                        &editor.run_id,
                        &editor.node_id,
                        dependency,
                        true,
                    )?;
                }
                Ok(format!(
                    "saved {} and versioned its workflow",
                    editor.node_id
                ))
            })()
            .map_err(|error| format!("{error:#}"));
            let _ = tx.send(BackgroundResult::Action(result));
        });
    }

    fn request_delete_node(&mut self) {
        let Some(ItemRef::Node(run_id, node_id)) = self.selected() else {
            self.set_status("select a workflow step to delete");
            return;
        };
        let title = self
            .state
            .runs
            .iter()
            .find(|run| run.id == run_id)
            .and_then(|run| run.nodes.iter().find(|node| node.id == node_id))
            .map(|node| node.name.clone())
            .unwrap_or_else(|| node_id.clone());
        self.confirmation = Some(Confirmation::DeleteNode {
            run_id,
            node_id,
            title,
        });
    }

    fn handle_key(&mut self, key: KeyEvent, tx: &Sender<BackgroundResult>) -> bool {
        if key.kind == KeyEventKind::Release {
            return false;
        }
        if terminal_reply(key) {
            return false;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if matches!(key.code, KeyCode::Char('c')) && ctrl {
            return true;
        }
        if self.editor.is_some() {
            let mut save = false;
            let mut cancel = false;
            if let Some(editor) = self.editor.as_mut() {
                match key.code {
                    KeyCode::Esc => cancel = true,
                    KeyCode::Enter => save = true,
                    KeyCode::Tab => editor.next(false),
                    KeyCode::BackTab => editor.next(true),
                    KeyCode::Backspace => {
                        editor.current_mut().pop();
                    }
                    KeyCode::Char(character) if !ctrl => editor.current_mut().push(character),
                    _ => {}
                }
            }
            if cancel {
                self.editor = None;
                self.set_status("edit cancelled");
            }
            if save {
                self.save_node_editor(tx);
            }
            return false;
        }
        if matches!(self.boot, BootState::Failed(_)) {
            match key.code {
                KeyCode::Char('q') => return true,
                KeyCode::Char('r') => {
                    self.boot = BootState::Loading {
                        started_at: Instant::now(),
                    };
                    self.request_refresh(tx);
                }
                _ => {}
            }
            return false;
        }
        if self.confirmation.is_some() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Enter => self.confirm(tx),
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
                    self.set_status("inspector: i toggle, h/j/k/l dock");
                }
                KeyCode::Char('?') => self.help = true,
                _ => self.set_status("unknown leader action"),
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
                    };
                    if self.dock == Dock::Hidden {
                        self.focus = Focus::Main;
                    }
                }
                ('i', KeyCode::Char('h')) => self.dock = Dock::Left,
                ('i', KeyCode::Char('j')) => self.dock = Dock::Bottom,
                ('i', KeyCode::Char('k')) => self.dock = Dock::Top,
                ('i', KeyCode::Char('l')) => self.dock = Dock::Right,
                ('w', KeyCode::Char('j')) if self.dock != Dock::Hidden => {
                    self.focus = Focus::Inspector
                }
                ('w', KeyCode::Char('k')) => self.focus = Focus::Main,
                ('w', KeyCode::Char('l')) if self.dock != Dock::Hidden => {
                    self.focus = Focus::Inspector
                }
                ('w', KeyCode::Char('h')) => self.focus = Focus::Main,
                _ => self.set_status("unknown key sequence"),
            }
            self.persist_preferences();
            return false;
        }
        match (key.code, ctrl) {
            (KeyCode::Char('q'), _) => return true,
            (KeyCode::Char('?'), _) => self.help = true,
            (KeyCode::Char(' '), _) => self.leader = true,
            (KeyCode::Char('w'), true) => self.pending = Some('w'),
            (KeyCode::Char('j'), true) if self.dock != Dock::Hidden => {
                self.focus = Focus::Inspector
            }
            (KeyCode::Char('k'), true) => self.focus = Focus::Main,
            (KeyCode::Char('l'), true) if self.dock != Dock::Hidden => {
                self.focus = Focus::Inspector
            }
            (KeyCode::Char('h'), true) => self.focus = Focus::Main,
            (KeyCode::Char('d'), true) => self.page(1),
            (KeyCode::Char('u'), true) => self.page(-1),
            (KeyCode::Tab, _) | (KeyCode::BackTab, _) if self.focus == Focus::Inspector => {
                self.next_inspector(if key.code == KeyCode::BackTab { -1 } else { 1 });
                self.persist_preferences();
            }
            (KeyCode::Tab, _) | (KeyCode::BackTab, _) => {
                self.clear_status();
                if self.main_tab == MainTab::Integrations {
                    self.switch_main_tab(MainTab::Work);
                    return false;
                }
                self.main_tab = MainTab::Work;
                self.explorer_view = if self.explorer_view == ExplorerView::Tree {
                    ExplorerView::Graph
                } else {
                    ExplorerView::Tree
                };
                self.focus = Focus::Main;
                self.flow.request_fit_view();
                self.persist_preferences();
            }
            (KeyCode::Char('['), _) => self.next_inspector(-1),
            (KeyCode::Char(']'), _) => self.next_inspector(1),
            (KeyCode::Char('p'), _) => {
                self.switch_main_tab(MainTab::Integrations);
            }
            (KeyCode::Esc, _) if self.main_tab == MainTab::Integrations => {
                self.switch_main_tab(MainTab::Work);
            }
            (KeyCode::Char('a'), _) if self.focus == Focus::Main => {
                if !self.request_gate() {
                    self.set_status("the selected run has no pending gate");
                }
            }
            (KeyCode::Char('m'), _) => {
                self.preferences.autonomy = self.preferences.autonomy.next();
                self.set_status(format!("autonomy: {}", self.preferences.autonomy));
                self.persist_preferences();
            }
            (KeyCode::Char('g'), _) if self.focus == Focus::Main => self.drill_down(),
            (KeyCode::Char('e'), _) if self.focus == Focus::Main => self.open_node_editor(),
            (KeyCode::Char('D'), _) if self.focus == Focus::Main => self.request_delete_node(),
            (KeyCode::Char('r'), _) => {
                self.enrichment_requested = true;
                self.request_refresh(tx);
            }
            (KeyCode::Char('R'), _) => {
                self.rebuild(true);
                self.flow.request_fit_view();
            }
            (KeyCode::Char('o'), _)
                if self.focus == Focus::Main && self.explorer_view == ExplorerView::Graph =>
            {
                self.flow.request_fit_view();
            }
            (KeyCode::Char('+' | '=' | '-' | '_'), _)
                if self.focus == Focus::Main && self.explorer_view == ExplorerView::Graph =>
            {
                let _ = self.flow.handle_controls_key_event(key);
            }
            (KeyCode::Char('='), _) if self.focus == Focus::Inspector => {
                self.config.ui.inspector_percent = (self.config.ui.inspector_percent + 5).min(80);
                self.persist_preferences();
            }
            (KeyCode::Char('-'), _) if self.focus == Focus::Inspector => {
                self.config.ui.inspector_percent =
                    self.config.ui.inspector_percent.saturating_sub(5).max(20);
                self.persist_preferences();
            }
            (KeyCode::Char('i'), _) => self.load_output(Action::Activity, tx),
            (KeyCode::Char('c'), _) => self.load_output(Action::Changes, tx),
            (KeyCode::Char('v'), _) if self.main_tab == MainTab::Integrations => {
                self.validate_provider(tx)
            }
            (KeyCode::Char('x'), _) => self.request_cancel(),
            (KeyCode::Enter, _) => self.open_selected(tx),
            (KeyCode::Char('j'), _) | (KeyCode::Down, _) => self.motion(Direction::Down),
            (KeyCode::Char('k'), _) | (KeyCode::Up, _) => self.motion(Direction::Up),
            (KeyCode::Char('h'), _) | (KeyCode::Left, _) => self.motion(Direction::Left),
            (KeyCode::Char('l'), _) | (KeyCode::Right, _) => self.motion(Direction::Right),
            _ => {}
        }
        false
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        let x = mouse.column;
        let y = mouse.row;
        if self.main_tab == MainTab::Work
            && self.explorer_view == ExplorerView::Graph
            && matches!(mouse.kind, MouseEventKind::Up(MouseButton::Left))
            && self.flow.is_dragging()
        {
            let _ = self.flow.handle_mouse_event(mouse);
            clamp_flow_viewport(&mut self.flow);
            self.inspector_scroll = 0;
            return;
        }
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            && contains(self.hit.tabs, x, y)
        {
            self.main_tab = MainTab::Integrations;
            self.focus = Focus::Main;
            return;
        }

        if let Some(inspector) = self.hit.inspector
            && contains(inspector, x, y)
        {
            self.focus = Focus::Inspector;
            match mouse.kind {
                MouseEventKind::ScrollUp => {
                    self.inspector_scroll = self.inspector_scroll.saturating_sub(3)
                }
                MouseEventKind::ScrollDown => {
                    self.inspector_scroll = self.inspector_scroll.saturating_add(3)
                }
                MouseEventKind::Down(MouseButton::Left) if y == inspector.y => {
                    if let Some(tab) =
                        output_tab_at(inspector_tabs(self.selected().as_ref()), inspector, x)
                    {
                        self.output_tab = tab;
                        self.inspector_scroll = 0;
                    }
                }
                _ => {}
            }
            return;
        }

        if !contains(self.hit.main, x, y) {
            return;
        }
        self.focus = Focus::Main;
        if self.main_tab == MainTab::Work && self.explorer_view == ExplorerView::Graph {
            if contains(self.hit.graph, x, y) {
                let clamp_after = matches!(
                    mouse.kind,
                    MouseEventKind::Up(MouseButton::Left)
                        | MouseEventKind::ScrollUp
                        | MouseEventKind::ScrollDown
                );
                let _ = self.flow.handle_mouse_event(mouse);
                if clamp_after {
                    clamp_flow_viewport(&mut self.flow);
                }
                self.inspector_scroll = 0;
            }
            return;
        }

        let delta: Option<i32> = match mouse.kind {
            MouseEventKind::ScrollUp => Some(-3),
            MouseEventKind::ScrollDown => Some(3),
            _ => None,
        };
        if let Some(delta) = delta {
            for _ in 0..delta.unsigned_abs() {
                self.move_main(if delta > 0 {
                    Direction::Down
                } else {
                    Direction::Up
                });
            }
            return;
        }
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            let index = visible_row_at(
                self.hit.main,
                y,
                self.selected_list_index(),
                self.list_len(),
                0,
            );
            if let Some(index) = index {
                match self.main_tab {
                    MainTab::Integrations => self.provider_at = index,
                    MainTab::Work if self.explorer_view == ExplorerView::Tree => {
                        self.tree_at = index
                    }
                    MainTab::Work => {}
                }
                self.inspector_scroll = 0;
            }
        }
    }

    fn selected_list_index(&self) -> usize {
        match self.main_tab {
            MainTab::Integrations => self.provider_at,
            MainTab::Work if self.explorer_view == ExplorerView::Tree => self.tree_at,
            MainTab::Work => 0,
        }
    }

    fn list_len(&self) -> usize {
        match self.main_tab {
            MainTab::Integrations => self.providers.len(),
            MainTab::Work if self.explorer_view == ExplorerView::Tree => self.tree.len(),
            MainTab::Work => 0,
        }
    }

    fn motion(&mut self, direction: Direction) {
        match self.focus {
            Focus::Main => self.move_main(direction),
            Focus::Inspector => match direction {
                Direction::Up => self.inspector_scroll = self.inspector_scroll.saturating_sub(1),
                Direction::Down => self.inspector_scroll = self.inspector_scroll.saturating_add(1),
                _ => {}
            },
        }
        if self.focus == Focus::Main && self.explorer_view == ExplorerView::Graph {
            clamp_flow_viewport(&mut self.flow);
        }
    }

    fn page(&mut self, by: i32) {
        match self.focus {
            Focus::Inspector => {
                self.inspector_scroll = (self.inspector_scroll as i32 + by * 10).max(0) as u16
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
        let selected = self.selected();
        let tabs = inspector_tabs(selected.as_ref());
        let current = tabs
            .iter()
            .position(|(tab, _)| *tab == self.output_tab)
            .unwrap_or(0) as i32;
        self.output_tab = tabs[((current + by).rem_euclid(tabs.len() as i32)) as usize].0;
        self.inspector_scroll = 0;
    }
}

fn inspector_tabs(item: Option<&ItemRef>) -> &'static [(OutputTab, &'static str)] {
    const RUN: &[(OutputTab, &str)] = &[
        (OutputTab::Summary, "Overview"),
        (OutputTab::Timeline, "Activity"),
        (OutputTab::Result, "Gates"),
        (OutputTab::Changes, "Changes"),
    ];
    const STAGE: &[(OutputTab, &str)] = &[
        (OutputTab::Summary, "Contract"),
        (OutputTab::Timeline, "Activity"),
        (OutputTab::Result, "Output"),
        (OutputTab::Changes, "Changes"),
    ];
    const AGENT: &[(OutputTab, &str)] = &[
        (OutputTab::Summary, "Details"),
        (OutputTab::Timeline, "Activity"),
        (OutputTab::Changes, "Changes"),
    ];
    const PROVIDER: &[(OutputTab, &str)] = &[
        (OutputTab::Summary, "Details"),
        (OutputTab::Result, "Health"),
        (OutputTab::Timeline, "Activity"),
    ];
    match item {
        Some(ItemRef::Run(_)) => RUN,
        Some(ItemRef::Node(_, _)) => STAGE,
        Some(ItemRef::Provider(_)) => PROVIDER,
        Some(ItemRef::Session(_)) | None => AGENT,
    }
}

fn enrichment_interval(config: &Config) -> Duration {
    Duration::from_millis(
        config
            .cache
            .provider_ttl_ms
            .max(config.ui.activity_refresh_ms),
    )
}

fn enrichment_retry_interval(config: &Config) -> Duration {
    Duration::from_millis(config.ui.refresh_ms.clamp(500, 5_000))
}

fn default_expansions(state: &WorkspaceState) -> BTreeSet<String> {
    state
        .sessions
        .iter()
        .filter(|session| session.status != LifecycleStatus::Archived)
        .map(|session| format!("session:{}", session.id))
        .chain(
            state
                .runs
                .iter()
                .filter(|run| run.status.active())
                .map(|run| format!("run:{}", run.id)),
        )
        .collect()
}

fn new_flow() -> AgentFlow {
    configure_flow(Flow::new())
}

fn configure_flow(flow: AgentFlow) -> AgentFlow {
    let mut palette = Theme::Dark.palette();
    palette.canvas_bg = Color::Reset;
    palette.surface = Color::Reset;
    palette.accent = Color::Cyan;
    palette.text = Color::Reset;
    flow.with_theme(Theme::Custom(palette))
        .with_min_zoom(0.75)
        .with_max_zoom(2.0)
        .with_deselect_on_pane_click(false)
        .with_selection_reveal(rataflow::SelectionReveal::EnsureVisible)
}

fn clamp_axis(offset: f64, min: f64, max: f64, zoom: f64, size: f64, margin: f64) -> f64 {
    let margin = margin.min(size / 2.0);
    let content = (max - min) * zoom;
    if content <= size - margin * 2.0 {
        return (size - content) / 2.0 - min * zoom;
    }
    offset.clamp(size - margin - max * zoom, margin - min * zoom)
}

fn clamp_flow_viewport(flow: &mut AgentFlow) -> bool {
    let canvas = flow.canvas_area();
    if canvas.width == 0 || canvas.height == 0 {
        return false;
    }
    let ids = flow.nodes().map(|node| node.id.clone()).collect::<Vec<_>>();
    let Some(bounds) = ids
        .iter()
        .filter_map(|id| flow.node_bounds(id))
        .reduce(|left, right| left.union(&right))
    else {
        return false;
    };
    let mut snapshot = flow.to_snapshot();
    let x = clamp_axis(
        snapshot.viewport.x,
        bounds.x(),
        bounds.right(),
        snapshot.viewport.zoom,
        f64::from(canvas.width),
        4.0,
    );
    let y = clamp_axis(
        snapshot.viewport.y,
        bounds.y(),
        bounds.bottom(),
        snapshot.viewport.zoom,
        f64::from(canvas.height),
        2.0,
    );
    if (x - snapshot.viewport.x).abs() < f64::EPSILON
        && (y - snapshot.viewport.y).abs() < f64::EPSILON
    {
        return false;
    }
    snapshot.viewport.set_offset(x, y);
    if let Ok(restored) = AgentFlow::from_snapshot(snapshot) {
        *flow = configure_flow(restored);
        true
    } else {
        false
    }
}

fn graph_signature(state: &WorkspaceState, active_run: Option<&str>) -> String {
    let mut value = String::new();
    if let Some(run) = active_run.and_then(|id| state.runs.iter().find(|run| run.id == id)) {
        value.push_str(&format!(
            "r:{}:{}:{}:{};",
            run.id,
            run.orchestrator_id.as_deref().unwrap_or(""),
            run.status,
            run.current_node.as_deref().unwrap_or("")
        ));
        for node in &run.nodes {
            value.push_str(&format!(
                "n:{}:{}:{}:{}:{};",
                node.id,
                node.session_id.as_deref().unwrap_or(""),
                node.status,
                node.attempt,
                node.completion
            ));
        }
        for edge in &run.edges {
            value.push_str(&format!(
                "e:{}:{}:{};",
                edge.from, edge.to, edge.relationship
            ));
        }
    }
    for session in state
        .sessions
        .iter()
        .filter(|session| session.status != LifecycleStatus::Archived)
    {
        value.push_str(&format!(
            "s:{}:{}:{}:{}:{};",
            session.id,
            session.parent_id.as_deref().unwrap_or(""),
            session.run_id.as_deref().unwrap_or(""),
            session.node_id.as_deref().unwrap_or(""),
            session.status
        ));
    }
    value
}

fn add_session_card(
    flow: &mut AgentFlow,
    items: &mut BTreeMap<String, ItemRef>,
    known: &mut BTreeSet<String>,
    _state: &WorkspaceState,
    session: &Session,
) {
    let id = format!("session:{}", session.id);
    let card = AgentCard {
        kind: session.role.to_string(),
        title: session.title.clone(),
        subtitle: format!("{} · {}", session_placement(session), session.harness),
        goal: session.goal.clone(),
        status: session.status,
        active: RuntimeActivity::for_session(session) == Some(RuntimeActivity::Active),
    };
    let node = Node::new(&id, (0.0, 0.0), (38.0, 5.0), card)
        .with_deletable(false)
        .with_connectable(false)
        .with_draggable(false)
        .with_handles(graph_handles());
    let _ = flow.add_node(node);
    known.insert(id.clone());
    items.insert(id, ItemRef::Session(session.id.clone()));
}

fn refresh_flow_content(flow: &mut AgentFlow, state: &WorkspaceState, active_run: Option<&str>) {
    let active_run = active_run.and_then(|id| state.runs.iter().find(|run| run.id == id));
    let active_orchestrator = active_run
        .and_then(|run| run.orchestrator_id.as_deref())
        .and_then(|id| state.sessions.iter().find(|session| session.id == id))
        .or_else(|| state.current_session());
    let orchestrator_id = active_orchestrator.map(|session| session.id.as_str());
    for session in &state.sessions {
        if orchestrator_id == Some(session.id.as_str()) {
            continue;
        }
        if let Some(card) = flow.node_content_mut(&format!("session:{}", session.id)) {
            card.title.clone_from(&session.title);
            card.kind = session.role.to_string();
            card.subtitle = format!("{} · {}", session_placement(session), session.harness);
            card.goal.clone_from(&session.goal);
            card.status = session.status;
            card.active = RuntimeActivity::for_session(session) == Some(RuntimeActivity::Active);
        }
    }
    for run in &state.runs {
        if active_run.is_some_and(|active| active.id == run.id) {
            let orchestrator = active_orchestrator;
            let root_id = orchestrator
                .map(|session| format!("session:{}", session.id))
                .unwrap_or_else(|| format!("run:{}", run.id));
            if let Some(card) = flow.node_content_mut(&root_id) {
                card.kind = "orchestrator".into();
                card.title = orchestrator
                    .map_or_else(|| "Orchestrator".into(), |session| session.title.clone());
                card.subtitle = format!("{} · {} stages", run_phase(run), run.nodes.len());
                card.goal.clone_from(&run.goal);
                card.status = orchestrator.map_or(run.status, |session| session.status);
                card.active = orchestrator.and_then(RuntimeActivity::for_session)
                    == Some(RuntimeActivity::Active);
            }
        }
        if !active_run.is_some_and(|active| active.id == run.id)
            && let Some(card) = flow.node_content_mut(&format!("run:{}", run.id))
        {
            card.title.clone_from(&run.name);
            card.subtitle = format!("{} stages · {}", run.nodes.len(), run_phase(run));
            card.goal.clone_from(&run.goal);
            card.status = run.status;
        }
        for node in &run.nodes {
            if let Some(card) = flow.node_content_mut(&format!("node:{}:{}", run.id, node.id)) {
                card.title.clone_from(&node.name);
                let runtime = node
                    .execution
                    .as_deref()
                    .map(|execution| format!("{execution} · "))
                    .unwrap_or_default();
                card.subtitle = format!(
                    "{runtime}{}{}",
                    node.harness,
                    node.model
                        .as_deref()
                        .map(|model| format!("/{model}"))
                        .unwrap_or_default()
                );
                card.goal.clone_from(&node.goal);
                card.status = node.status;
                card.active = run.current_node.as_deref() == Some(&node.id)
                    && node.status == LifecycleStatus::Working;
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
    if run.is_none()
        && let Some(session) = orchestrator
    {
        add_session_card(&mut flow, &mut items, &mut known, state, session);
    }
    if let Some(run) = run {
        for workflow_node in &run.nodes {
            let node_id = format!("node:{}:{}", run.id, workflow_node.id);
            let model = workflow_node
                .model
                .as_deref()
                .map_or_else(String::new, |model| format!("/{model}"));
            let runtime = workflow_node
                .execution
                .as_deref()
                .map(|execution| format!("{execution} · "))
                .unwrap_or_default();
            let card = AgentCard {
                kind: workflow_node.role.to_string(),
                title: workflow_node.name.clone(),
                subtitle: format!("{runtime}{}{}", workflow_node.harness, model),
                goal: workflow_node.goal.clone(),
                status: workflow_node.status,
                active: run.current_node.as_deref() == Some(&workflow_node.id)
                    && workflow_node.status == LifecycleStatus::Working,
            };
            let node = Node::new(&node_id, (0.0, 0.0), (38.0, 5.0), card)
                .with_deletable(false)
                .with_connectable(false)
                .with_draggable(false)
                .with_handles(graph_handles());
            let _ = flow.add_node(node);
            known.insert(node_id.clone());
            items.insert(
                node_id,
                ItemRef::Node(run.id.clone(), workflow_node.id.clone()),
            );
        }
        let mut topology_edges = Vec::new();
        let mut overlay_edges = Vec::new();
        let review_pairs = run
            .edges
            .iter()
            .filter(|edge| edge.relationship == "reviewed_by")
            .map(|edge| (edge.from.as_str(), edge.to.as_str()))
            .collect::<BTreeSet<_>>();
        for edge in &run.edges {
            if edge.relationship == "depends_on"
                && review_pairs.contains(&(edge.from.as_str(), edge.to.as_str()))
            {
                continue;
            }
            let active = run.current_node.as_deref() == Some(&edge.to)
                || run
                    .nodes
                    .iter()
                    .find(|node| node.id == edge.to)
                    .is_some_and(|node| node.status == LifecycleStatus::Working);
            topology_edges.push((
                format!("node:{}:{}", run.id, edge.from),
                format!("node:{}:{}", run.id, edge.to),
                edge.relationship.clone(),
                active,
            ));
            if edge.relationship == "reviewed_by" {
                let implementer = run.nodes.iter().find(|node| node.id == edge.from);
                let reviewer = run.nodes.iter().find(|node| node.id == edge.to);
                let retrying = implementer.is_some_and(|node| {
                    node.attempt > 1
                        && matches!(
                            node.status,
                            LifecycleStatus::Queued | LifecycleStatus::Working
                        )
                }) || reviewer.is_some_and(|node| {
                    matches!(
                        node.status,
                        LifecycleStatus::Failed | LifecycleStatus::Blocked
                    )
                });
                overlay_edges.push((
                    format!("node:{}:{}", run.id, edge.to),
                    format!("node:{}:{}", run.id, edge.from),
                    "feedback".into(),
                    retrying,
                ));
            }
        }
        add_graph_edges(&mut flow, &known, topology_edges, "topology");
        flow.apply_layout(
            Sugiyama::horizontal()
                .with_rank_spacing(5.0)
                .with_node_spacing(3.0),
        );

        let stage_bounds = run
            .nodes
            .iter()
            .filter_map(|node| flow.node_bounds(&format!("node:{}:{}", run.id, node.id)))
            .reduce(|left, right| left.union(&right));
        let root_id = orchestrator
            .map(|session| format!("session:{}", session.id))
            .unwrap_or_else(|| format!("run:{}", run.id));
        let root_position = stage_bounds.map_or((0.0, -8.0), |bounds| {
            (bounds.center().x - 19.0, bounds.y() - 8.0)
        });
        let lifecycle = orchestrator.map_or(run.status, |session| session.status);
        let root = Node::new(
            &root_id,
            root_position,
            (38.0, 5.0),
            AgentCard {
                kind: "orchestrator".into(),
                title: orchestrator
                    .map_or_else(|| "Orchestrator".into(), |session| session.title.clone()),
                subtitle: format!("{} · {} stages", run_phase(run), run.nodes.len()),
                goal: run.goal.clone(),
                status: lifecycle,
                active: orchestrator.and_then(RuntimeActivity::for_session)
                    == Some(RuntimeActivity::Active),
            },
        )
        .with_deletable(false)
        .with_connectable(false)
        .with_draggable(false)
        .with_handles(graph_handles());
        let _ = flow.add_node(root);
        known.insert(root_id.clone());
        items.insert(
            root_id.clone(),
            orchestrator.map_or_else(
                || ItemRef::Run(run.id.clone()),
                |session| ItemRef::Session(session.id.clone()),
            ),
        );

        let mut orchestration_edges = run
            .nodes
            .iter()
            .filter(|node| {
                !run.edges.iter().any(|edge| {
                    edge.to == node.id
                        && edge.relationship != "feedback"
                        && edge.relationship != "reports"
                })
            })
            .map(|node| {
                (
                    root_id.clone(),
                    format!("node:{}:{}", run.id, node.id),
                    "delegates".into(),
                    run.current_node.as_deref() == Some(&node.id),
                )
            })
            .collect::<Vec<_>>();
        orchestration_edges.extend(
            run.nodes
                .iter()
                .filter(|node| node.completion == CompletionTarget::Orchestrator)
                .map(|node| {
                    (
                        format!("node:{}:{}", run.id, node.id),
                        root_id.clone(),
                        "reports".into(),
                        node.status == LifecycleStatus::Done,
                    )
                }),
        );
        add_graph_edges(&mut flow, &known, orchestration_edges, "orchestration");
        add_graph_edges(&mut flow, &known, overlay_edges, "overlay");
    } else {
        let sessions: Vec<_> = state
            .sessions
            .iter()
            .filter(|session| session.status != LifecycleStatus::Archived)
            .collect();
        for session in &sessions {
            if !known.contains(&format!("session:{}", session.id)) {
                add_session_card(&mut flow, &mut items, &mut known, state, session);
            }
        }
        let edges = sessions
            .iter()
            .filter_map(|session| {
                session.parent_id.as_deref().map(|parent| {
                    (
                        format!("session:{parent}"),
                        format!("session:{}", session.id),
                        "spawned".into(),
                        session.status.active(),
                    )
                })
            })
            .collect();
        add_graph_edges(&mut flow, &known, edges, "lineage");
        flow.apply_layout(Sugiyama::vertical());
    }
    flow.request_fit_view_with_options(FitViewOptions::default().with_padding(1.0));
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
                relation: relation.clone(),
                lane: index,
                ..RelationEdge::default()
            })
            .with_selectable(false)
            .with_deletable(false)
            .with_reconnectable(Reconnectable::None);
        let edge = match relation.as_str() {
            "delegates" => edge
                .with_source_side(HandlePosition::Bottom)
                .with_target_side(HandlePosition::Top),
            "feedback" => edge
                .with_source_side(HandlePosition::Bottom)
                .with_target_side(HandlePosition::Bottom),
            "reports" => edge
                .with_source_side(HandlePosition::Right)
                .with_target_side(HandlePosition::Right),
            _ => edge
                .with_source_side(HandlePosition::Right)
                .with_target_side(HandlePosition::Left),
        };
        let _ = flow.add_edge(edge);
    }
}

fn graph_handles() -> Vec<Handle> {
    [
        (HandlePosition::Bottom, true),
        (HandlePosition::Left, true),
        (HandlePosition::Right, true),
        (HandlePosition::Top, false),
        (HandlePosition::Bottom, false),
        (HandlePosition::Left, false),
        (HandlePosition::Right, false),
    ]
    .into_iter()
    .map(|(position, source)| {
        let handle = if source {
            Handle::source(position)
        } else {
            Handle::target(position)
        };
        handle.with_id(position.side_name()).with_hidden(true)
    })
    .collect()
}

fn tree_rows(state: &WorkspaceState, expanded: &BTreeSet<String>) -> Vec<TreeRow> {
    let mut rows = Vec::new();
    let mut visited = BTreeSet::new();
    let roots: Vec<_> = state
        .sessions
        .iter()
        .filter(|session| {
            session.status != LifecycleStatus::Archived
                && (session.parent_id.is_none()
                    || !state.sessions.iter().any(|candidate| {
                        candidate.id == session.parent_id.as_deref().unwrap_or_default()
                            && candidate.status != LifecycleStatus::Archived
                    }))
        })
        .collect();
    for session in roots {
        push_session_rows(state, expanded, &mut rows, &mut visited, session, 0);
    }
    let remaining: Vec<_> = state
        .sessions
        .iter()
        .filter(|session| {
            session.status != LifecycleStatus::Archived && !visited.contains(&session.id)
        })
        .collect();
    for session in remaining {
        push_session_rows(state, expanded, &mut rows, &mut visited, session, 0);
    }
    rows
}

fn push_session_rows(
    state: &WorkspaceState,
    expanded: &BTreeSet<String>,
    rows: &mut Vec<TreeRow>,
    visited: &mut BTreeSet<String>,
    session: &Session,
    depth: usize,
) {
    if !visited.insert(session.id.clone()) {
        return;
    }
    let id = format!("session:{}", session.id);
    let runs: Vec<_> = state
        .runs
        .iter()
        .filter(|run| run.orchestrator_id.as_deref() == Some(&session.id))
        .collect();
    let represented: BTreeSet<_> = runs
        .iter()
        .flat_map(|run| run.nodes.iter().filter_map(|node| node.session_id.clone()))
        .collect();
    let children: Vec<_> = state
        .sessions
        .iter()
        .filter(|child| {
            child.status != LifecycleStatus::Archived
                && child.parent_id.as_deref() == Some(&session.id)
                && !represented.contains(&child.id)
        })
        .collect();
    rows.push(TreeRow {
        id: id.clone(),
        depth,
        title: session.title.clone(),
        subtitle: [
            session.role.to_string(),
            session_placement(session),
            session.harness.clone(),
            RuntimeActivity::for_session(session)
                .map(|runtime| runtime.label().to_owned())
                .unwrap_or_else(|| session.status.to_string()),
        ]
        .join(" · "),
        status: Some(session.status),
        item: ItemRef::Session(session.id.clone()),
        children: !runs.is_empty() || !children.is_empty(),
    });
    if !expanded.contains(&id) {
        return;
    }
    for run in runs {
        let run_id = format!("run:{}", run.id);
        rows.push(TreeRow {
            id: run_id.clone(),
            depth: depth + 1,
            title: run.name.clone(),
            subtitle: format!("workflow · {} stages · {}", run.nodes.len(), run_phase(run)),
            status: Some(run.status),
            item: ItemRef::Run(run.id.clone()),
            children: !run.nodes.is_empty(),
        });
        if !expanded.contains(&run_id) {
            continue;
        }
        for node in &run.nodes {
            let assigned = node
                .session_id
                .as_deref()
                .and_then(|id| state.sessions.iter().find(|candidate| candidate.id == id));
            let agent_children: Vec<_> = assigned
                .into_iter()
                .flat_map(|assigned| {
                    state.sessions.iter().filter(move |child| {
                        child.status != LifecycleStatus::Archived
                            && child.parent_id.as_deref() == Some(&assigned.id)
                    })
                })
                .collect();
            let node_id = format!("node:{}:{}", run.id, node.id);
            rows.push(TreeRow {
                id: node_id.clone(),
                depth: depth + 2,
                title: node.name.clone(),
                subtitle: format!(
                    "{} · {} · {}",
                    node.role,
                    node.execution.as_deref().unwrap_or("unassigned"),
                    node.harness
                ),
                status: Some(node.status),
                item: ItemRef::Node(run.id.clone(), node.id.clone()),
                children: !agent_children.is_empty(),
            });
            if expanded.contains(&node_id) {
                for child in agent_children {
                    push_session_rows(state, expanded, rows, visited, child, depth + 3);
                }
            }
            if let Some(assigned) = assigned {
                visited.insert(assigned.id.clone());
            }
        }
    }
    for child in children {
        push_session_rows(state, expanded, rows, visited, child, depth + 1);
    }
}

fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    app.hit = HitAreas::default();
    match &app.boot {
        BootState::Loading { started_at } => {
            render_loading(frame, area, *started_at);
            return;
        }
        BootState::Failed(error) => {
            render_startup_error(frame, area, error);
            return;
        }
        BootState::Ready => {}
    }
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(area);
    render_header(frame, header, app);
    let (main, inspector) = split_body(body, app.dock, app.config.ui.inspector_percent);
    app.hit.main = main;
    app.hit.inspector = inspector;
    render_main(frame, main, app);
    if let Some(inspector) = inspector {
        render_inspector(frame, inspector, app);
    }
    render_footer(frame, footer, app);
    if app.help {
        render_help(frame, area);
    }
    if let Some(confirmation) = &app.confirmation {
        render_confirmation(frame, area, confirmation);
    }
    if let Some(editor) = &app.editor {
        render_node_editor(frame, area, editor);
    }
}

const ORC_ART: &[&str] = &[
    "⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⢀⡀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀",
    "⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⢠⣿⣿⡄⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀",
    "⠀⠀⠀⠀⠀⠀⠀⠀⠀⣀⣤⣶⣧⣄⣉⣉⣠⣼⣶⣤⣀⠀⠀⠀⠀⠀⠀⠀⠀⠀",
    "⠀⠀⠀⠀⠀⠀⠀⢰⣿⣿⣿⣿⡿⣿⣿⣿⣿⢿⣿⣿⣿⣿⡆⠀⠀⠀⠀⠀⠀⠀",
    "⠀⠀⠀⠀⠀⠀⠀⣼⣤⣤⣈⠙⠳⢄⣉⣋⡡⠞⠋⣁⣤⣤⣧⠀⠀⠀⠀⠀⠀⠀",
    "⠀⢲⣶⣤⣄⡀⢀⣿⣄⠙⠿⣿⣦⣤⡿⢿⣤⣴⣿⠿⠋⣠⣿⠀⢀⣠⣤⣶⡖⠀",
    "⠀⠀⠙⣿⠛⠇⢸⣿⣿⡟⠀⡄⢉⠉⢀⡀⠉⡉⢠⠀⢻⣿⣿⡇⠸⠛⣿⠋⠀⠀",
    "⠀⠀⠀⠘⣷⠀⢸⡏⠻⣿⣤⣤⠂⣠⣿⣿⣄⠑⣤⣤⣿⠟⢹⡇⠀⣾⠃⠀⠀⠀",
    "⠀⠀⠀⠀⠘⠀⢸⣿⡀⢀⠙⠻⢦⣌⣉⣉⣡⡴⠟⠋⡀⢀⣿⡇⠀⠃⠀⠀⠀⠀",
    "__MOUTH_TOP__",
    "__MOUTH_MIDDLE__",
    "__MOUTH_JAW__",
    "⠀⠀⠀⠀⠀⠀⠀⠀⠀⠈⠉⠉⠉⠛⠛⠛⠛⠉⠉⠉⠁⠀⠀⠀⠀⠀⠀⠀⠀⠀",
];
const CLOSED_MOUTH: [&str; 3] = [
    "⠀⠀⠀⠀⠀⠀⢸⣿⣧⠈⠛⠂⠀⠉⠛⠛⠉⠀⠐⠛⠁⣼⣿⡇⠀⠀⠀⠀⠀⠀",
    "⠀⠀⠀⠀⠀⠀⠸⣏⠀⣤⡶⠖⠛⠋⠉⠉⠙⠛⠲⢶⣤⠀⣹⠇⠀⠀⠀⠀⠀⠀",
    "⠀⠀⠀⠀⠀⠀⠀⠀⠀⢹⣿⣶⣿⣿⣿⣿⣿⣿⣶⣿⡏⠀⠀⠀⠀⠀⠀⠀⠀⠀",
];
const SMILING_MOUTH: [&str; 3] = [
    "⠀⠀⠀⠀⠀⠀⢸⣿⣧⠀⢠⡿⠛⠛⢿⡄⠀⣼⣿⡇⠀⠀⠀⠀⠀⠀",
    "⠀⠀⠀⠀⠀⠀⠸⣏⠀⠀⢸⡇⠀⠀⢸⡇⠀⠀⣹⠇⠀⠀⠀⠀⠀⠀",
    "⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠘⢷⣤⣤⡾⠃⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀",
];
const LAUGHING_MOUTH: [&str; 3] = [
    "⠀⠀⠀⠀⠀⠀⢸⣿⣧⠀⢀⣤⣶⣶⣶⣤⡀⠀⣼⣿⡇⠀⠀⠀⠀⠀⠀",
    "⠀⠀⠀⠀⠀⠀⠸⣏⠀⠀⣿⡏⠀⠀⠀⠀⢹⣿⠀⠀⣹⠇⠀⠀⠀⠀⠀⠀",
    "⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠻⣷⣦⣤⣤⣴⣿⠟⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀",
];

#[derive(Clone, Copy)]
struct LaughFrame {
    mouth: &'static [&'static str; 3],
    bob: u16,
}

const ORC_LAUGH: &[LaughFrame] = &[
    LaughFrame {
        mouth: &CLOSED_MOUTH,
        bob: 0,
    },
    LaughFrame {
        mouth: &SMILING_MOUTH,
        bob: 0,
    },
    LaughFrame {
        mouth: &LAUGHING_MOUTH,
        bob: 0,
    },
    LaughFrame {
        mouth: &LAUGHING_MOUTH,
        bob: 1,
    },
    LaughFrame {
        mouth: &LAUGHING_MOUTH,
        bob: 0,
    },
    LaughFrame {
        mouth: &SMILING_MOUTH,
        bob: 0,
    },
];

fn render_loading(frame: &mut Frame, area: Rect, started_at: Instant) {
    let tick = (started_at.elapsed().as_millis() / 110) as usize;
    let laugh = ORC_LAUGH[tick % ORC_LAUGH.len()];
    let spinner = spinner_glyph();
    if area.width < 31 || area.height < 11 {
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("⚔ ", title()),
                Span::styled(format!("{spinner} "), Style::default().fg(Color::Yellow)),
                Span::styled("loading Orc…", plain()),
            ]))
            .alignment(Alignment::Center),
            Rect::new(area.x, area.y + area.height / 2, area.width, 1),
        );
        return;
    }
    let first = usize::from(area.height < 16) * 4;
    let mut lines = Vec::new();
    for row in &ORC_ART[first..] {
        match *row {
            "__MOUTH_TOP__" => lines.push(Line::from(Span::styled(
                laugh.mouth[0],
                accent().add_modifier(Modifier::BOLD),
            ))),
            "__MOUTH_MIDDLE__" => lines.push(Line::from(Span::styled(
                laugh.mouth[1],
                accent().add_modifier(Modifier::BOLD),
            ))),
            "__MOUTH_JAW__" => lines.push(Line::from(Span::styled(
                laugh.mouth[2],
                accent().add_modifier(Modifier::BOLD),
            ))),
            _ => lines.push(Line::from(Span::styled(*row, plain()))),
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(format!("{spinner} "), Style::default().fg(Color::Yellow)),
        Span::styled("loading workspace…", plain()),
    ]));
    let height = lines.len().min(area.height as usize) as u16;
    let width = 31.min(area.width);
    let target = Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2 + laugh.bob,
        width,
        height,
    );
    frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center), target);
}

fn render_startup_error(frame: &mut Frame, area: Rect, error: &str) {
    let width = area.width.min(88);
    let height = area.height.min(16);
    let target = Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Red))
        .title_style(title())
        .title(" ⚔ Orc could not open this workspace ")
        .title_bottom(Line::from(" r retry · q quit ").alignment(Alignment::Center));
    let inner = block.inner(target);
    frame.render_widget(Clear, target);
    frame.render_widget(block, target);
    frame.render_widget(
        Paragraph::new(error.to_owned())
            .style(plain())
            .wrap(Wrap { trim: false }),
        inner,
    );
}

fn split_body(area: Rect, dock: Dock, percent: u16) -> (Rect, Option<Rect>) {
    if dock == Dock::Hidden {
        return (area, None);
    }
    match dock {
        Dock::Bottom => {
            let size = (area.height.saturating_mul(percent) / 100).clamp(8, 18);
            let [a, b] =
                Layout::vertical([Constraint::Min(8), Constraint::Length(size)]).areas(area);
            (a, Some(b))
        }
        Dock::Top => {
            let size = (area.height.saturating_mul(percent) / 100).clamp(8, 18);
            let [b, a] =
                Layout::vertical([Constraint::Length(size), Constraint::Min(8)]).areas(area);
            (a, Some(b))
        }
        Dock::Left => {
            let size = (area.width.saturating_mul(percent) / 100).clamp(32, 60);
            let [b, a] =
                Layout::horizontal([Constraint::Length(size), Constraint::Min(40)]).areas(area);
            (a, Some(b))
        }
        Dock::Right => {
            let size = (area.width.saturating_mul(percent) / 100).clamp(32, 60);
            let [a, b] =
                Layout::horizontal([Constraint::Min(40), Constraint::Length(size)]).areas(area);
            (a, Some(b))
        }
        Dock::Hidden => (area, None),
    }
}

fn render_header(frame: &mut Frame, area: Rect, app: &mut App) {
    let active = app
        .state
        .sessions
        .iter()
        .filter(|session| RuntimeActivity::for_session(session) == Some(RuntimeActivity::Active))
        .count();
    let stalled = app
        .state
        .sessions
        .iter()
        .filter(|session| RuntimeActivity::for_session(session) == Some(RuntimeActivity::Stalled))
        .count();
    let status = if app.refresh_inflight || app.enrichment_inflight {
        format!("{} syncing", spinner_glyph())
    } else {
        format!(
            "{} agents · {} runs",
            app.state.sessions.len(),
            app.state.runs.len()
        )
    };
    let status = if stalled > 0 {
        format!("{stalled} stalled · {active} active · {status}")
    } else if active > 0 {
        format!("{active} active · {status}")
    } else {
        status
    };
    let status_width = status.width().min(area.width.saturating_sub(1) as usize) as u16;
    let [title_area, status_area] =
        Layout::horizontal([Constraint::Fill(1), Constraint::Length(status_width)])
            .areas(Rect::new(area.x, area.y, area.width, 1));
    let workspace = app
        .scope
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("workspace");
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("⚔ orc", title()),
            Span::styled(
                format!("  {workspace}"),
                plain().add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("  {}", app.preferences.autonomy), dim()),
            Span::styled("  ", dim()),
            Span::styled(
                if app.main_tab == MainTab::Work && app.explorer_view == ExplorerView::Tree {
                    "[tree] graph"
                } else if app.main_tab == MainTab::Work {
                    "tree [graph]"
                } else {
                    "integrations"
                },
                accent(),
            ),
        ])),
        title_area,
    );
    frame.render_widget(
        Paragraph::new(status)
            .style(if stalled > 0 {
                Style::default().fg(Color::Yellow)
            } else if active > 0 {
                live()
            } else {
                dim()
            })
            .alignment(Alignment::Right),
        status_area,
    );
    app.hit.tabs = Rect::default();
}

fn render_main(frame: &mut Frame, area: Rect, app: &mut App) {
    let focused = app.focus == Focus::Main;
    let border = if focused { accent() } else { dim() };
    match app.main_tab {
        MainTab::Integrations => render_providers(frame, area, app, border),
        MainTab::Work if app.explorer_view == ExplorerView::Tree => {
            render_tree(frame, area, app, border)
        }
        MainTab::Work => {
            let block = Block::default()
                .style(Style::default().bg(Color::Reset))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(border)
                .title_style(title())
                .title(" workflow ");
            let inner = block.inner(area);
            frame.render_widget(block, area);
            app.hit.graph = inner;
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
            let glyph = match &row.item {
                ItemRef::Session(id) => app
                    .state
                    .sessions
                    .iter()
                    .find(|session| session.id == *id)
                    .and_then(RuntimeActivity::for_session)
                    .filter(|runtime| *runtime == RuntimeActivity::Active)
                    .map_or_else(
                        || status_glyph(row.status.unwrap_or(LifecycleStatus::Queued)),
                        |_| spinner_glyph(),
                    ),
                _ => status_glyph(row.status.unwrap_or(LifecycleStatus::Queued)),
            };
            ListItem::new(Line::from(vec![
                Span::styled(prefix, dim()),
                Span::styled(
                    glyph.to_string(),
                    Style::default()
                        .fg(status_color(row.status.unwrap_or(LifecycleStatus::Queued))),
                ),
                Span::raw(" "),
                Span::styled(row.title.clone(), plain().add_modifier(Modifier::BOLD)),
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
                .title_style(title())
                .title(" work · tree "),
        )
        .highlight_style(accent().add_modifier(Modifier::BOLD))
        .highlight_symbol("▌ ");
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_providers(frame: &mut Frame, area: Rect, app: &mut App, border: Style) {
    let items = app
        .providers
        .iter()
        .map(|provider| {
            ListItem::new(Line::from(vec![
                Span::styled(provider.name.clone(), plain().add_modifier(Modifier::BOLD)),
                Span::styled(
                    format!(
                        "  {} · {} capabilities  ",
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
                .title_style(title())
                .title(" integrations "),
        )
        .highlight_style(accent().add_modifier(Modifier::BOLD))
        .highlight_symbol("▌ ");
    frame.render_stateful_widget(list, area, &mut state);
}

struct UtcNow;
impl UtcNow {
    fn millis() -> u128 {
        chrono::Utc::now().timestamp_millis().unsigned_abs() as u128
    }
}

fn styled_details(body: &str) -> Text<'static> {
    Text::from(
        body.lines()
            .enumerate()
            .map(|(index, line)| {
                if index == 0
                    || matches!(
                        line,
                        "integrations"
                            | "runtime"
                            | "execution"
                            | "capabilities"
                            | "success criteria"
                            | "needs attention"
                            | "recent calls"
                    )
                {
                    return Line::from(Span::styled(
                        line.to_owned(),
                        accent().add_modifier(Modifier::BOLD),
                    ));
                }
                if line.len() >= 16 && !line.starts_with(' ') {
                    let (label, value) = line.split_at(16);
                    let value_style = if label.trim() == "status" {
                        value.trim().parse::<LifecycleStatus>().map_or_else(
                            |_| Style::default(),
                            |status| Style::default().fg(status_color(status)),
                        )
                    } else {
                        plain()
                    };
                    return Line::from(vec![
                        Span::styled(label.to_owned(), dim()),
                        Span::styled(value.to_owned(), value_style),
                    ]);
                }
                Line::from(Span::raw(line.to_owned()))
            })
            .collect::<Vec<_>>(),
    )
}

fn render_inspector(frame: &mut Frame, area: Rect, app: &mut App) {
    let border = if app.focus == Focus::Inspector {
        accent()
    } else {
        dim()
    };
    let selected = app.selected();
    let tabs = inspector_tabs(selected.as_ref());
    if !tabs.iter().any(|(tab, _)| *tab == app.output_tab) {
        app.output_tab = tabs[0].0;
    }
    let title = Line::from(
        tabs.iter()
            .map(|(tab, label)| {
                let name = format!(" {label} ");
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
        OutputTab::Summary => details(app),
        OutputTab::Timeline => selected_timeline(app),
        OutputTab::Result => selected_result(app),
        OutputTab::Changes => {
            if app.changes_loading {
                "Scanning workspace changes…".into()
            } else if !app.changes_loaded {
                "No changes integration is available.".into()
            } else if app.changes.is_empty() {
                "Workspace is clean.".into()
            } else {
                app.changes.clone()
            }
        }
    };
    let body = if app.output_tab == OutputTab::Summary {
        styled_details(&body)
    } else {
        body.clone().into_text().unwrap_or_else(|_| body.into())
    };
    let width = inner.width.max(1) as usize;
    let rendered_lines = body
        .lines
        .iter()
        .map(|line| line.width().max(1).div_ceil(width))
        .sum::<usize>();
    let max_scroll = rendered_lines.saturating_sub(inner.height as usize) as u16;
    app.inspector_scroll = app.inspector_scroll.min(max_scroll);
    frame.render_widget(
        Paragraph::new(body)
            .scroll((app.inspector_scroll, 0))
            .wrap(Wrap { trim: false }),
        inner,
    );
}

fn selected_result(app: &App) -> String {
    match app.selected() {
        Some(ItemRef::Run(id)) => app
            .state
            .runs
            .iter()
            .find(|run| run.id == id)
            .map(|run| {
                if run.pending_gates.is_empty() {
                    "No gates need attention.".into()
                } else {
                    run.pending_gates
                        .iter()
                        .map(|gate| format!("{} before {}\n{}", gate.id, gate.before, gate.reason))
                        .collect::<Vec<_>>()
                        .join("\n\n")
                }
            })
            .unwrap_or_default(),
        Some(ItemRef::Provider(_)) => {
            if app.provider_report.is_empty() {
                "Press v to validate this provider.".into()
            } else {
                app.provider_report.clone()
            }
        }
        _ => selected_output(app),
    }
}

fn selected_timeline(app: &App) -> String {
    if app.main_tab == MainTab::Integrations && !app.provider_report.is_empty() {
        let calls = selected_log(app);
        return if calls.is_empty() {
            app.provider_report.clone()
        } else {
            format!("{}\n\nrecent calls\n{}", app.provider_report, calls)
        };
    }
    selected_log(app)
}

fn selected_log(app: &App) -> String {
    match app.selected() {
        Some(ItemRef::Session(id)) => session_activity(app, &id),
        Some(ItemRef::Node(run_id, node_id)) => {
            let Some(node) = app
                .state
                .runs
                .iter()
                .find(|run| run.id == run_id)
                .and_then(|run| run.nodes.iter().find(|node| node.id == node_id))
            else {
                return String::new();
            };
            let local = node
                .activity
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
                .join("\n");
            let provider = node
                .session_id
                .as_deref()
                .map(|id| session_activity(app, id))
                .unwrap_or_default();
            match (local.is_empty(), provider.is_empty()) {
                (false, false) => format!("{local}\n\n{provider}"),
                (false, true) => local,
                (true, false) => provider,
                (true, true) => "Waiting for this agent to report activity.".into(),
            }
        }
        Some(ItemRef::Run(id)) => app
            .state
            .runs
            .iter()
            .find(|run| run.id == id)
            .map(|run| {
                let provider = run
                    .orchestrator_id
                    .as_deref()
                    .map(|session_id| session_activity(app, session_id))
                    .unwrap_or_default();
                if let Some(path) = &run.log_path
                    && let Ok(log) = fs::read_to_string(path)
                    && !log.trim().is_empty()
                {
                    return if provider.is_empty() {
                        log
                    } else {
                        format!("{provider}\n\nworkflow log\n{log}")
                    };
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
                if !provider.is_empty() {
                    lines.push(String::new());
                    lines.push(provider);
                }
                lines.join("\n")
            })
            .unwrap_or_default(),
        Some(ItemRef::Provider(name)) => {
            if app.provider_activity_loading.contains(&name)
                && !app.provider_activity.contains_key(&name)
            {
                "Loading provider activity…".into()
            } else {
                app.provider_activity
                    .get(&name)
                    .cloned()
                    .unwrap_or_else(|| "Waiting for provider activity…".into())
            }
        }
        _ => "Select an agent, run, or step.".into(),
    }
}

fn session_activity(app: &App, id: &str) -> String {
    let runtime = app
        .state
        .sessions
        .iter()
        .find(|session| session.id == id)
        .and_then(RuntimeActivity::for_session);
    if app.activity_loading.contains(id)
        && runtime == Some(RuntimeActivity::Active)
        && !app.activity.contains_key(id)
    {
        return "Loading live agent activity…".into();
    }
    if let Some(activity) = app.activity.get(id) {
        return activity.clone();
    }
    match runtime {
        Some(RuntimeActivity::Active) => "Waiting for recent agent activity…".into(),
        Some(RuntimeActivity::Idle) => "No recent activity · runtime appears idle.".into(),
        Some(RuntimeActivity::Stalled) => "No recent activity · runtime may be stalled.".into(),
        None => "No recent activity for this agent.".into(),
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
        Some(ItemRef::Session(id)) => app
            .state
            .sessions
            .iter()
            .find(|session| session.id == id)
            .map_or_else(String::new, session_details),
        Some(ItemRef::Run(id)) => app
            .state
            .runs
            .iter()
            .find(|run| run.id == id)
            .map_or_else(String::new, run_details),
        Some(ItemRef::Node(run_id, node_id)) => app
            .state
            .runs
            .iter()
            .find(|run| run.id == run_id)
            .and_then(|run| run.nodes.iter().find(|node| node.id == node_id))
            .map_or_else(String::new, node_details),
        Some(ItemRef::Provider(name)) => app
            .providers
            .iter()
            .find(|provider| provider.name == name)
            .map_or_else(String::new, provider_details),
        None => "Select an item.".into(),
    }
}

fn detail_templates() -> &'static Environment<'static> {
    static TEMPLATES: OnceLock<Environment<'static>> = OnceLock::new();
    TEMPLATES.get_or_init(|| {
        let mut environment = Environment::new();
        for (name, source) in [
            ("session", include_str!("../templates/session.txt")),
            ("run", include_str!("../templates/run.txt")),
            ("node", include_str!("../templates/node.txt")),
            ("provider", include_str!("../templates/provider.txt")),
        ] {
            environment
                .add_template(name, source)
                .unwrap_or_else(|error| panic!("invalid {name} detail template: {error}"));
        }
        environment
    })
}

fn render_detail_template(name: &str, context: minijinja::Value) -> String {
    detail_templates()
        .get_template(name)
        .and_then(|template| template.render(context))
        .unwrap_or_else(|error| format!("Could not render {name} details: {error}"))
        .trim_end()
        .into()
}

fn session_details(session: &Session) -> String {
    let placement = session_placement(session);
    let activity = RuntimeActivity::for_session(session)
        .map(|runtime| runtime.label())
        .unwrap_or("inactive");
    render_detail_template("session", context! { session, placement, activity })
}

fn run_details(run: &WorkflowRun) -> String {
    let phase = run_phase(run);
    render_detail_template("run", context! { run, phase })
}

fn node_details(node: &WorkflowNode) -> String {
    render_detail_template("node", context! { node })
}

fn provider_details(provider: &Manifest) -> String {
    let actions = provider
        .actions
        .iter()
        .map(|(capability, description)| {
            serde_json::json!({
                "capability": capability.to_string(),
                "description": description,
            })
        })
        .collect::<Vec<_>>();
    render_detail_template("provider", context! { provider, actions })
}

fn render_footer(frame: &mut Frame, area: Rect, app: &App) {
    let focus = match app.focus {
        Focus::Main => "main",
        Focus::Inspector => "inspector",
    };
    let actions = if app.focus == Focus::Main {
        &["line", "open", "drill", "focus-inspector", "view", "help"][..]
    } else {
        &["line", "page", "focus-main", "inspect-tabs", "help"][..]
    };
    let hints = BINDINGS
        .iter()
        .filter(|binding| actions.contains(&binding.id))
        .map(|binding| format!("{} {}", binding.keys, binding.short))
        .collect::<Vec<_>>()
        .join("   ");
    let status = app
        .visible_status()
        .map_or_else(|| format!("{focus}   {hints}"), str::to_owned);
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
        id: "focus-inspector",
        keys: "ctrl+j/l",
        short: "inspector",
        description: "focus the inspector",
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
        keys: "tab/shift-tab",
        short: "inspector tab",
        description: "change inspector tab",
    },
    Binding {
        id: "view",
        keys: "tab",
        short: "tree/graph",
        description: "toggle the work tree and workflow graph",
    },
    Binding {
        id: "integrations",
        keys: "p/esc",
        short: "integrations",
        description: "inspect provider integrations or return to work",
    },
    Binding {
        id: "open",
        keys: "enter",
        short: "open",
        description: "attach the selected session or validate an integration",
    },
    Binding {
        id: "drill",
        keys: "g",
        short: "graph",
        description: "open the selected run's workflow graph",
    },
    Binding {
        id: "edit-node",
        keys: "e",
        short: "edit step",
        description: "edit the selected workflow step",
    },
    Binding {
        id: "delete-node",
        keys: "D",
        short: "delete step",
        description: "delete the selected step after confirmation",
    },
    Binding {
        id: "mode",
        keys: "m",
        short: "autonomy",
        description: "cycle supervised, approval-gated, and autonomous modes",
    },
    Binding {
        id: "gate",
        keys: "a",
        short: "gate",
        description: "answer a pending human gate for the selected run",
    },
    Binding {
        id: "cancel",
        keys: "x",
        short: "stop",
        description: "stop the selected run after confirmation",
    },
    Binding {
        id: "viewport",
        keys: "+/-/o",
        short: "viewport",
        description: "zoom, reset, or fit the graph",
    },
    Binding {
        id: "resize",
        keys: "+/-",
        short: "resize",
        description: "resize the inspector",
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
        description: "refresh state and integrations",
    },
    Binding {
        id: "dock",
        keys: "space i h/j/k/l",
        short: "dock",
        description: "move or hide the inspector",
    },
    Binding {
        id: "mouse",
        keys: "click/wheel",
        short: "select/scroll",
        description: "select panes and rows, choose tabs, pan the graph, or scroll",
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

fn render_node_editor(frame: &mut Frame, area: Rect, editor: &NodeEditor) {
    let width = area.width.min(92);
    let height = area.height.min(20);
    if width < 38 || height < 12 {
        return;
    }
    let popup = Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    );
    let fields = [
        (EditField::Goal, "goal", editor.goal.as_str()),
        (
            EditField::ExpectedOutput,
            "expected output",
            editor.expected_output.as_str(),
        ),
        (EditField::Criteria, "criteria", editor.criteria.as_str()),
        (EditField::Harness, "harness", editor.harness.as_str()),
        (EditField::Model, "model", editor.model.as_str()),
        (
            EditField::Execution,
            "execution provider",
            editor.execution.as_str(),
        ),
        (EditField::Judge, "judge policy", editor.judge.as_str()),
        (
            EditField::Dependencies,
            "depends on",
            editor.dependencies.as_str(),
        ),
    ];
    let lines = fields
        .into_iter()
        .map(|(field, label, value)| {
            Line::from(vec![
                Span::styled(
                    format!("{label:<19}"),
                    if editor.field == field {
                        accent()
                    } else {
                        dim()
                    },
                ),
                Span::styled(
                    if value.is_empty() { "—" } else { value },
                    if editor.field == field {
                        plain().add_modifier(Modifier::BOLD)
                    } else {
                        plain()
                    },
                ),
            ])
        })
        .collect::<Vec<_>>();
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(accent())
                .title(format!(" edit {} ", editor.node_id))
                .title_bottom(
                    Line::from(" tab field · enter save · esc cancel ")
                        .alignment(Alignment::Center),
                ),
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
        Confirmation::Prune { title, .. } => {
            format!("Stop and archive agent {title}? This ends its managed process.")
        }
        Confirmation::DeleteNode { title, .. } => {
            format!("Delete step {title}? Orc will version the workflow change.")
        }
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

fn contains(area: Rect, x: u16, y: u16) -> bool {
    x >= area.x
        && x < area.x.saturating_add(area.width)
        && y >= area.y
        && y < area.y.saturating_add(area.height)
}

fn visible_row_at(
    area: Rect,
    y: u16,
    selected: usize,
    len: usize,
    header_rows: usize,
) -> Option<usize> {
    let height = area
        .height
        .saturating_sub(2)
        .saturating_sub(header_rows as u16) as usize;
    if len == 0 || height == 0 {
        return None;
    }
    let first = selected
        .saturating_sub(height.saturating_sub(1))
        .min(len.saturating_sub(height));
    let top = area.y.saturating_add(1 + header_rows as u16);
    let row = y.checked_sub(top)? as usize;
    (row < height && first + row < len).then_some(first + row)
}

fn output_tab_at(tabs: &[(OutputTab, &str)], area: Rect, x: u16) -> Option<OutputTab> {
    let mut start = area.x.saturating_add(1);
    for (tab, label) in tabs {
        let width = label.width() as u16 + 2;
        let end = start.saturating_add(width);
        if x >= start && x < end {
            return Some(*tab);
        }
        start = end;
    }
    None
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
        LifecycleStatus::Working => '●',
        LifecycleStatus::Done => '✓',
        LifecycleStatus::Failed => '×',
        LifecycleStatus::Blocked => '!',
        LifecycleStatus::Waiting | LifecycleStatus::Queued => '○',
        LifecycleStatus::Archived | LifecycleStatus::Disconnected | LifecycleStatus::Cancelled => {
            '·'
        }
    }
}

fn spinner_glyph() -> char {
    const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    SPINNER[(UtcNow::millis() / 90 % SPINNER.len() as u128) as usize]
}
fn status_color(status: LifecycleStatus) -> Color {
    match status {
        LifecycleStatus::Working => Color::Yellow,
        LifecycleStatus::Done => Color::Green,
        LifecycleStatus::Failed => Color::Red,
        LifecycleStatus::Blocked => Color::Magenta,
        _ => Color::DarkGray,
    }
}
fn accent() -> Style {
    Style::default().fg(Color::Cyan)
}
fn title() -> Style {
    Style::default()
        .fg(Color::Blue)
        .add_modifier(Modifier::BOLD)
}
fn plain() -> Style {
    Style::default().fg(Color::Reset)
}
fn live() -> Style {
    Style::default().fg(Color::Green)
}
fn dim() -> Style {
    Style::default()
        .fg(Color::Reset)
        .add_modifier(Modifier::DIM)
}

#[derive(Default)]
struct TerminalGuard {
    raw: bool,
    alternate: bool,
    mouse: bool,
}
impl TerminalGuard {
    fn enter() -> Result<(Self, Terminal<CrosstermBackend<io::Stdout>>)> {
        let mut guard = Self::default();
        enable_raw_mode()?;
        guard.raw = true;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        guard.alternate = true;
        execute!(stdout, EnableMouseCapture)?;
        guard.mouse = true;
        let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        Ok((guard, terminal))
    }
}
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if self.mouse {
            let _ = execute!(io::stdout(), DisableMouseCapture);
        }
        if self.alternate {
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
        }
        if self.raw {
            let _ = disable_raw_mode();
        }
    }
}

pub fn run(config: Config, scope: &Path) -> Result<()> {
    let scope = crate::state::resolve_scope(scope)?;
    let (_guard, mut terminal) = TerminalGuard::enter()?;
    let (tx, rx): (Sender<BackgroundResult>, Receiver<BackgroundResult>) = mpsc::channel();
    let mut app = App::loading(config, scope);
    app.request_refresh(&tx);
    let mut last_tick = Instant::now();
    loop {
        while let Ok(result) = rx.try_recv() {
            app.apply_background(result);
        }
        if matches!(app.boot, BootState::Ready)
            && (app.refresh_requested
                || (!app.refresh_inflight
                    && app.last_refresh.elapsed()
                        >= Duration::from_millis(app.config.ui.refresh_ms)))
        {
            app.request_refresh(&tx);
        }
        if matches!(app.boot, BootState::Ready) {
            app.request_enrichment(&tx);
            app.request_activity(&tx, false);
            app.request_provider_activity(&tx, false);
            app.request_changes(&tx, false);
        }
        if app
            .resize_at
            .is_some_and(|at| at.elapsed() >= Duration::from_millis(120))
        {
            clamp_flow_viewport(&mut app.flow);
            app.resize_at = None;
        }
        terminal.draw(|frame| render(frame, &mut app))?;
        let mut quit = false;
        if event::poll(Duration::from_millis(50))? {
            let mut processed = 0;
            loop {
                processed += 1;
                match event::read()? {
                    Event::Key(key) if app.handle_key(key, &tx) => quit = true,
                    Event::Mouse(mouse) => app.handle_mouse(mouse),
                    Event::Resize(_, _) => app.resize_at = Some(Instant::now()),
                    _ => {}
                }
                if quit || processed >= 128 || !event::poll(Duration::ZERO)? {
                    break;
                }
            }
        }
        if quit {
            app.persist_preferences();
            break;
        }
        let elapsed = last_tick.elapsed();
        last_tick = Instant::now();
        app.flow.tick_animation(elapsed);
        let _ = app.flow.tick_auto_pan(elapsed);
    }
    Ok(())
}

pub fn preview_loading() -> Result<()> {
    let (_guard, mut terminal) = TerminalGuard::enter()?;
    let started_at = Instant::now();
    loop {
        terminal.draw(|frame| render_loading(frame, frame.area(), started_at))?;
        if event::poll(Duration::from_millis(50))?
            && let Event::Key(key) = event::read()?
            && key.kind != KeyEventKind::Release
            && (matches!(key.code, KeyCode::Char('q'))
                || (matches!(key.code, KeyCode::Char('c'))
                    && key.modifiers.contains(KeyModifiers::CONTROL)))
        {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use crossterm::event::KeyEventState;
    use ratatui::backend::TestBackend;

    fn session(id: &str, parent_id: Option<&str>, harness: &str) -> Session {
        Session {
            id: id.into(),
            native_id: format!("native-{id}"),
            trace_id: Some(format!("trace-{id}")),
            harness: harness.into(),
            model: None,
            role: if parent_id.is_none() {
                crate::domain::SessionRole::Orchestrator
            } else {
                crate::domain::SessionRole::Researcher
            },
            title: id.into(),
            purpose: format!("Purpose for {id}"),
            goal: format!("Goal for {id}"),
            expected_output: "Verified result".into(),
            success_criteria: vec!["It passes".into()],
            completion: crate::domain::CompletionTarget::Orchestrator,
            review_by: None,
            parent_id: parent_id.map(str::to_owned),
            run_id: None,
            node_id: None,
            provider_ref: None,
            providers: Vec::new(),
            directory: "/tmp/orc-test".into(),
            registration: crate::domain::RegistrationSource::Managed,
            status: LifecycleStatus::Working,
            connected_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn app() -> App {
        let mut state = WorkspaceState::empty("/tmp/orc-test".into());
        state.sessions = vec![
            session("root", None, "codex"),
            session("native-child", Some("root"), "codex"),
            session("harness-child", Some("root"), "claude"),
        ];
        App::new(
            Config::default(),
            PathBuf::from("/tmp/orc-test"),
            state,
            Vec::new(),
        )
    }

    fn workflow_run() -> WorkflowRun {
        WorkflowRun {
            id: "run".into(),
            name: "Provider migration".into(),
            goal: "Move integrations behind contracts".into(),
            expected_output: "A verified migration".into(),
            status: LifecycleStatus::Queued,
            orchestrator_id: Some("root".into()),
            definition: None,
            revision: None,
            checkpoint: None,
            mode: crate::domain::RunMode::default(),
            process_id: None,
            log_path: None,
            current_node: None,
            tokens: 0,
            cost_usd: 0.0,
            token_burn: Vec::new(),
            pending_gates: Vec::new(),
            approved_gates: Vec::new(),
            agents: Vec::new(),
            nodes: Vec::new(),
            edges: Vec::new(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn workflow_node(id: &str, status: LifecycleStatus, attempt: u32) -> WorkflowNode {
        WorkflowNode {
            id: id.into(),
            name: format!("{id} stage"),
            purpose: format!("Perform {id}"),
            role: crate::domain::SessionRole::Implementer,
            harness: "codex".into(),
            model: None,
            execution: Some("local".into()),
            judge_policy: JudgePolicy::Llm,
            goal: format!("Complete {id}"),
            expected_output: format!("Verified {id}"),
            success_criteria: vec![format!("{id} passes")],
            completion: CompletionTarget::Orchestrator,
            review_by: None,
            session_id: None,
            status,
            attempt,
            prompt: None,
            input: None,
            output: None,
            activity: Vec::new(),
            tokens: 0,
            cost_usd: 0.0,
            updated_at: Utc::now(),
        }
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    #[test]
    fn help_is_generated_from_bindings() {
        assert!(
            BINDINGS
                .iter()
                .any(|binding| binding.id == "focus-inspector")
        );
        assert!(
            BINDINGS
                .iter()
                .all(|binding| !binding.description.is_empty())
        );
    }

    #[test]
    fn keyboard_navigation_is_not_gated_by_refresh() {
        let mut app = app();
        app.explorer_view = ExplorerView::Tree;
        app.refresh_inflight = true;
        let (tx, _rx) = mpsc::channel();
        assert!(!app.handle_key(key(KeyCode::Char('j'), KeyModifiers::NONE), &tx));
        assert_eq!(app.tree_at, 1);
        assert_eq!(app.focus, Focus::Main);
    }

    #[test]
    fn tree_shows_role_execution_placement_and_harness() {
        let app = app();
        assert_eq!(app.tree.len(), 3);
        assert_eq!(app.tree[1].depth, 1);
        assert!(
            app.tree[1]
                .subtitle
                .contains("researcher · external · codex")
        );
        assert!(
            app.tree[2]
                .subtitle
                .contains("researcher · external · claude")
        );
    }

    #[test]
    fn detail_templates_render_typed_domain_data() {
        let app = app();
        let rendered = session_details(&app.state.sessions[0]);
        assert!(rendered.contains("orchestrator · external · codex"));
        assert!(rendered.contains("activity      active"));
        assert!(!rendered.contains("integrations"));
        assert!(rendered.contains("expected output Verified result"));
        assert!(rendered.contains("success criteria\n  - It passes"));
    }

    #[test]
    fn mouse_selects_rows_and_scrolls_each_pane() {
        let mut app = app();
        app.explorer_view = ExplorerView::Tree;
        app.hit.main = Rect::new(0, 2, 80, 10);
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 4,
            row: 4,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.tree_at, 1);

        app.hit.inspector = Some(Rect::new(80, 2, 40, 10));
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 90,
            row: 5,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.focus, Focus::Inspector);
        assert_eq!(app.inspector_scroll, 3);
    }

    #[test]
    fn mouse_selects_output_tabs() {
        let mut app = app();
        app.hit.inspector = Some(Rect::new(0, 20, 120, 12));
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 12,
            row: 20,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.output_tab, OutputTab::Timeline);
        assert_eq!(app.focus, Focus::Inspector);
    }

    #[test]
    fn layout_survives_wide_and_narrow_resizes() {
        for (width, height) in [(160, 50), (72, 24), (40, 14)] {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).expect("test terminal");
            let mut app = app();
            terminal
                .draw(|frame| render(frame, &mut app))
                .expect("render after resize");
            assert!(app.hit.main.width > 0);
            assert!(app.hit.inspector.is_some_and(|area| area.height > 0));
        }
    }

    #[test]
    fn work_opens_as_tree_and_tab_toggles_graph() {
        let mut app = app();
        let (tx, _rx) = mpsc::channel();
        assert_eq!(app.main_tab, MainTab::Work);
        assert_eq!(app.explorer_view, ExplorerView::Tree);
        app.handle_key(key(KeyCode::Tab, KeyModifiers::NONE), &tx);
        assert_eq!(app.explorer_view, ExplorerView::Graph);
        app.handle_key(key(KeyCode::Tab, KeyModifiers::NONE), &tx);
        assert_eq!(app.explorer_view, ExplorerView::Tree);
    }

    #[test]
    fn tab_cycles_contextual_inspector_tabs_when_inspector_is_focused() {
        let mut app = app();
        let (tx, _rx) = mpsc::channel();
        app.focus = Focus::Inspector;
        app.output_tab = OutputTab::Summary;

        app.handle_key(key(KeyCode::Tab, KeyModifiers::NONE), &tx);
        assert_eq!(app.output_tab, OutputTab::Timeline);
        assert_eq!(app.explorer_view, ExplorerView::Tree);

        app.handle_key(key(KeyCode::BackTab, KeyModifiers::SHIFT), &tx);
        assert_eq!(app.output_tab, OutputTab::Summary);
        assert_eq!(app.explorer_view, ExplorerView::Tree);
    }

    #[test]
    fn lifecycle_status_is_static_while_runtime_activity_can_animate() {
        assert_eq!(status_glyph(LifecycleStatus::Working), '●');
        assert_eq!(
            RuntimeActivity::for_session(&app().state.sessions[0]),
            Some(RuntimeActivity::Active)
        );
    }

    #[test]
    fn startup_refresh_expands_active_work() {
        let mut state = app().state;
        state.runs.push(workflow_run());
        let mut app = App::loading(Config::default(), PathBuf::from("/tmp/orc-test"));
        app.apply_background(BackgroundResult::Refresh(Ok((state, Vec::new()))));

        assert_eq!(app.active_run.as_deref(), Some("run"));
        assert_eq!(app.tree[0].title, "root");
        assert_eq!(app.tree[1].title, "Provider migration");
        assert!(app.enrichment_requested);
        assert!(!app.enrichment_inflight);
    }

    #[test]
    fn refresh_keeps_a_selected_completed_run_open() {
        let mut run = workflow_run();
        run.status = LifecycleStatus::Done;
        let mut state = app().state;
        state.runs.push(run);
        let mut app = App::new(
            Config::default(),
            PathBuf::from("/tmp/orc-test"),
            state.clone(),
            Vec::new(),
        );
        app.active_run = Some("run".into());
        app.explorer_view = ExplorerView::Graph;

        app.apply_background(BackgroundResult::Refresh(Ok((state, Vec::new()))));

        assert_eq!(app.active_run.as_deref(), Some("run"));
        assert!(
            app.graph_items
                .values()
                .any(|item| { matches!(item, ItemRef::Session(id) if id == "root") })
        );
    }

    #[test]
    fn refresh_preserves_tree_and_graph_selection_by_stable_id() {
        let mut app = app();
        app.tree_at = app
            .tree
            .iter()
            .position(|row| row.id == "session:native-child")
            .expect("child row");
        app.state
            .sessions
            .insert(0, session("another-root", None, "codex"));
        app.rebuild(false);
        assert_eq!(app.tree[app.tree_at].id, "session:native-child");

        let mut run = workflow_run();
        run.nodes
            .push(workflow_node("implement", LifecycleStatus::Working, 1));
        app.state.runs.push(run);
        app.active_run = Some("run".into());
        app.explorer_view = ExplorerView::Graph;
        app.rebuild(true);
        app.flow.select_node("node:run:implement");
        app.state.sessions[0].status = LifecycleStatus::Done;
        app.rebuild(false);
        assert!(
            matches!(app.selected(), Some(ItemRef::Node(run, node)) if run == "run" && node == "implement")
        );
    }

    #[test]
    fn refresh_preserves_workflow_data_on_the_orchestrator_card() {
        let mut app = app();
        let mut run = workflow_run();
        run.orchestrator_id = None;
        run.nodes
            .push(workflow_node("implement", LifecycleStatus::Queued, 1));
        app.state.runs.push(run);
        app.active_run = Some("run".into());
        app.rebuild(true);

        app.state.sessions[0].goal = "Raw session goal".into();
        app.state.runs[0].goal = "Current workflow goal".into();
        app.rebuild(false);

        let root = app
            .flow
            .nodes()
            .find(|node| node.id == "session:root")
            .expect("orchestrator card");
        assert_eq!(root.content.goal, "Current workflow goal");
        assert_eq!(root.content.subtitle, "proposed · 1 stages");
    }

    #[test]
    fn enrichment_retries_and_rejects_stale_results() {
        let mut app = app();
        let now = Instant::now();
        app.enrichment_requested = false;
        app.enrichment_due_at = now + Duration::from_secs(30);
        assert!(!app.enrichment_ready(now));
        assert!(app.enrichment_ready(now + Duration::from_secs(31)));

        app.state_generation = 2;
        app.enrichment_inflight = true;
        let mut stale = app.state.clone();
        stale.sessions[0].title = "stale title".into();
        app.apply_background(BackgroundResult::Enrichment {
            generation: 1,
            result: Ok(stale),
        });
        assert_eq!(app.state.sessions[0].title, "root");
        assert!(!app.enrichment_inflight);
        assert!(app.enrichment_due_at > Instant::now());

        app.enrichment_inflight = true;
        app.apply_background(BackgroundResult::Enrichment {
            generation: 2,
            result: Err("provider unavailable".into()),
        });
        assert!(app.status.contains("provider unavailable"));
        assert!(app.enrichment_due_at > Instant::now());
    }

    #[test]
    fn graph_signature_tracks_runtime_edge_state() {
        let mut state = WorkspaceState::empty("/tmp/orc-test".into());
        let mut run = workflow_run();
        run.nodes
            .push(workflow_node("implement", LifecycleStatus::Queued, 1));
        state.runs.push(run);
        let initial = graph_signature(&state, Some("run"));

        state.runs[0].current_node = Some("implement".into());
        let current = graph_signature(&state, Some("run"));
        assert_ne!(initial, current);

        state.runs[0].nodes[0].status = LifecycleStatus::Working;
        let working = graph_signature(&state, Some("run"));
        assert_ne!(current, working);

        state.runs[0].nodes[0].attempt = 2;
        let retrying = graph_signature(&state, Some("run"));
        assert_ne!(working, retrying);
    }

    #[test]
    fn workflow_graph_selects_the_orchestrator_above_stages() {
        let mut app = app();
        let mut run = workflow_run();
        run.nodes
            .push(workflow_node("implement", LifecycleStatus::Working, 1));
        run.current_node = Some("implement".into());
        run.status = LifecycleStatus::Working;
        app.state.runs.push(run);
        app.active_run = Some("run".into());
        app.explorer_view = ExplorerView::Graph;
        app.rebuild(true);

        let root = app.flow.node_bounds("session:root").expect("root node");
        let stage = app
            .flow
            .node_bounds("node:run:implement")
            .expect("stage node");
        assert!(root.bottom() < stage.y());

        app.flow.select_node("session:root");
        assert!(matches!(app.selected(), Some(ItemRef::Session(id)) if id == "root"));
        assert_eq!(
            app.selected_session().map(|session| session.id.as_str()),
            Some("root")
        );

        app.flow.select_node("node:run:implement");
        app.move_main(Direction::Up);
        assert!(matches!(app.selected(), Some(ItemRef::Session(id)) if id == "root"));
        app.move_main(Direction::Down);
        assert!(
            matches!(app.selected(), Some(ItemRef::Node(run, node)) if run == "run" && node == "implement")
        );
    }

    #[test]
    fn transient_status_expires_and_view_changes_clear_it() {
        let mut app = app();
        app.set_status("opened workflow graph");
        app.status_at = Some(Instant::now() - Duration::from_secs(4));
        assert_eq!(app.visible_status(), None);

        app.set_status("stale view status");
        app.switch_main_tab(MainTab::Integrations);
        assert_eq!(app.visible_status(), None);
    }

    #[test]
    fn ctrl_c_quits_from_modal_states() {
        let mut app = app();
        let (tx, _rx) = mpsc::channel();
        app.help = true;
        assert!(app.handle_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL), &tx));
        app.help = false;
        app.confirmation = Some(Confirmation::Cancel {
            run_id: "run".into(),
        });
        assert!(app.handle_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL), &tx));
    }

    #[test]
    fn loading_art_is_responsive() {
        for (width, height) in [(120, 40), (31, 16), (31, 11), (30, 9)] {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).expect("test terminal");
            terminal
                .draw(|frame| render_loading(frame, frame.area(), Instant::now()))
                .expect("loading art renders");
        }
        assert_eq!(ORC_LAUGH.len(), 6);
        for frame in ORC_LAUGH {
            for line in frame.mouth {
                assert!(line.width() <= 31, "mouth line exceeds the art width");
            }
        }
    }

    #[test]
    fn activity_loading_message_replaces_empty_event_noise() {
        let mut app = app();
        app.activity_loading.insert("root".into());
        assert_eq!(selected_log(&app), "Loading live agent activity…");
    }

    #[test]
    fn inspector_tabs_match_the_selected_kind_and_wrap() {
        assert_eq!(
            inspector_tabs(Some(&ItemRef::Run("run".into()))),
            &[
                (OutputTab::Summary, "Overview"),
                (OutputTab::Timeline, "Activity"),
                (OutputTab::Result, "Gates"),
                (OutputTab::Changes, "Changes"),
            ]
        );
        assert_eq!(
            inspector_tabs(Some(&ItemRef::Provider("local".into()))),
            &[
                (OutputTab::Summary, "Details"),
                (OutputTab::Result, "Health"),
                (OutputTab::Timeline, "Activity"),
            ]
        );
        let mut app = app();
        app.focus = Focus::Inspector;
        app.output_tab = OutputTab::Changes;
        app.next_inspector(1);
        assert_eq!(app.output_tab, OutputTab::Summary);
        app.next_inspector(-1);
        assert_eq!(app.output_tab, OutputTab::Changes);
    }

    #[test]
    fn hidden_inspector_cannot_keep_focus() {
        let mut app = app();
        let (tx, _rx) = mpsc::channel();
        app.focus = Focus::Inspector;
        app.leader = true;
        app.handle_key(key(KeyCode::Char('i'), KeyModifiers::NONE), &tx);
        app.handle_key(key(KeyCode::Char('i'), KeyModifiers::NONE), &tx);
        assert_eq!(app.dock, Dock::Hidden);
        assert_eq!(app.focus, Focus::Main);
    }

    #[test]
    fn viewport_clamp_centers_small_content_and_keeps_large_content_visible() {
        assert_eq!(clamp_axis(500.0, 0.0, 20.0, 1.0, 100.0, 4.0), 40.0);
        let clamped = clamp_axis(500.0, 0.0, 200.0, 1.0, 100.0, 4.0);
        assert_eq!(clamped, 4.0);
        let clamped = clamp_axis(-500.0, 0.0, 200.0, 1.0, 100.0, 4.0);
        assert_eq!(clamped, -104.0);
    }

    #[test]
    fn semantic_edges_and_terminal_styles_are_distinct() {
        assert_eq!(edge_label("depends_on"), None);
        assert_eq!(edge_label("reviewed_by"), Some("review"));
        assert_eq!(edge_label("feedback"), Some("retry"));
        assert_eq!(edge_label("reports"), Some("reports"));
        assert_eq!(edge_label("conditional"), Some("condition"));
        assert_ne!(
            status_color(LifecycleStatus::Blocked),
            status_color(LifecycleStatus::Working)
        );
        assert_eq!(plain().fg, Some(Color::Reset));
        assert_eq!(dim().fg, Some(Color::Reset));
        assert!(dim().add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn planned_report_edge_activates_only_after_completion() {
        let mut state = app().state;
        let mut run = workflow_run();
        run.nodes
            .push(workflow_node("implement", LifecycleStatus::Queued, 1));
        let mut verifier = workflow_node("verify", LifecycleStatus::Queued, 1);
        verifier.completion = CompletionTarget::Judge;
        run.nodes.push(verifier);
        run.edges.push(crate::domain::WorkflowEdge {
            from: "implement".into(),
            to: "verify".into(),
            relationship: "reviewed_by".into(),
        });
        state.runs.push(run);

        let (flow, _) = build_flow(&state, Some("run"));
        let reports = flow
            .edges()
            .iter()
            .find(|edge| edge.content.relation == "reports")
            .expect("planned report edge");
        assert_eq!(reports.source, "node:run:implement");
        assert_eq!(reports.target, "session:root");
        assert!(!reports.content.active);

        state.runs[0].nodes[0].status = LifecycleStatus::Done;
        let (flow, _) = build_flow(&state, Some("run"));
        let reports = flow
            .edges()
            .iter()
            .find(|edge| edge.content.relation == "reports")
            .expect("completed report edge");
        assert!(reports.content.active);
    }

    #[test]
    fn mouse_release_outside_graph_ends_pan_and_clamps() {
        let mut app = app();
        let mut run = workflow_run();
        run.nodes
            .push(workflow_node("implement", LifecycleStatus::Queued, 1));
        app.state.runs.push(run);
        app.active_run = Some("run".into());
        app.explorer_view = ExplorerView::Graph;
        app.rebuild(true);
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| render(frame, &mut app))
            .expect("graph renders");
        let graph = app.hit.graph;
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: graph.right().saturating_sub(2),
            row: graph.bottom().saturating_sub(2),
            modifiers: KeyModifiers::NONE,
        });
        assert!(app.flow.is_dragging());

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
        assert!(!app.flow.is_dragging());
    }

    #[test]
    fn active_run_keeps_a_graph_viewport_at_supported_sizes() {
        for (width, height) in [(160, 50), (72, 24), (40, 14)] {
            let mut app = app();
            app.state.runs.push(workflow_run());
            app.active_run = Some("run".into());
            app.explorer_view = ExplorerView::Graph;
            app.rebuild(true);
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).expect("test terminal");
            terminal
                .draw(|frame| render(frame, &mut app))
                .expect("active run renders");
            assert!(app.hit.graph.width > 0);
            assert!(app.hit.graph.height > 0);
        }
    }
}
