use std::{
    collections::{BTreeMap, BTreeSet},
    io,
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
use rs_utils::animation::{AnimationConfig, Style as AnimationStyle};
use unicode_width::UnicodeWidthStr;

use crate::{
    animation,
    config::Config,
    control, daemon,
    domain::{
        BindingStatus, CompletionTarget, JudgePolicy, LifecycleStatus, ProviderKind,
        RegistrationSource, Session, WorkflowNode, WorkflowRun, WorkspaceState,
    },
    preferences::{self, WorkspacePreferences},
    provider::{self, Action, Capability, Manifest},
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
    History,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AttachReadiness {
    Focus,
    Reattach,
    Unavailable,
}

impl AttachReadiness {
    fn label(self) -> &'static str {
        match self {
            Self::Focus => "open ready · focus",
            Self::Reattach => "open ready · reattach",
            Self::Unavailable => "open unavailable",
        }
    }
}

fn provider_binding_ready(
    session: &Session,
    providers: &[Manifest],
    kind: ProviderKind,
    capability: Capability,
) -> bool {
    session.providers.iter().any(|binding| {
        binding.kind == kind
            && binding.status == BindingStatus::Active
            && binding
                .r#ref
                .as_deref()
                .is_some_and(|reference| !reference.is_empty())
            && providers.iter().any(|provider| {
                provider.name == binding.provider
                    && provider.supports(capability)
                    && provider.available_on_host()
            })
    })
}

fn attach_readiness(session: &Session, providers: &[Manifest]) -> AttachReadiness {
    if session.status.active()
        && provider_binding_ready(
            session,
            providers,
            ProviderKind::Display,
            Capability::TerminalFocus,
        )
    {
        return AttachReadiness::Focus;
    }
    let persistent = provider_binding_ready(
        session,
        providers,
        ProviderKind::Persistence,
        Capability::SessionPersist,
    );
    let display = providers.iter().any(|provider| {
        provider.supports(Capability::TerminalOpen) && provider.available_on_host()
    });
    if persistent && display {
        AttachReadiness::Reattach
    } else {
        AttachReadiness::Unavailable
    }
}

impl RuntimeActivity {
    fn for_session(session: &Session) -> Option<Self> {
        if !session.status.active() {
            return None;
        }
        let observed_at = session.heartbeat_at.unwrap_or(session.updated_at);
        let age = chrono::Utc::now()
            .signed_duration_since(observed_at)
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
    contract: String,
    goal: String,
    attention: Option<String>,
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
                truncate(&self.contract, width),
                Style::default().fg(palette.subtle),
            )));
        }
        if inner.height > 3 {
            let (content, style) = self.attention.as_ref().map_or_else(
                || (self.goal.as_str(), Style::default().fg(palette.muted)),
                |attention| {
                    (
                        attention.as_str(),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    )
                },
            );
            lines.push(Line::from(Span::styled(truncate(content, width), style)));
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
        match self.relation.as_str() {
            "delegates" | "spawned" => {
                let target = ctx.target_bounds.unwrap_or(ctx.source_bounds);
                let lane_y = target.y() - 4.0 - self.lane as f64 * 2.0;
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
            "feedback" => {
                let target = ctx.target_bounds.unwrap_or(ctx.source_bounds);
                let lane_y =
                    ctx.source_bounds.bottom().max(target.bottom()) + 2.0 + self.lane as f64 * 2.0;
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
            "reports" => {
                let target = ctx.target_bounds.unwrap_or(ctx.source_bounds);
                let lane_y = target.bottom() + 2.0 + self.lane as f64 * 2.0;
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
                    lane_y + 1.0,
                ))
            }
            _ => self.inner.compute_path(ctx),
        }
    }
    fn render(&self, ctx: &EdgeRenderContext, buf: &mut Buffer) {
        let color = if self.active {
            ctx.theme.palette().success
        } else {
            match self.relation.as_str() {
                "delegates" | "spawned" => Color::Cyan,
                "reviewed_by" => Color::Magenta,
                "feedback" => Color::Yellow,
                "reports" => Color::Blue,
                _ => ctx.theme.palette().muted,
            }
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

const LABELED_EDGE_RANK_SPACING: f64 = 26.0;
const EDGE_LABEL_WIDTH: usize = 24;
const AGENT_CARD_WIDTH: f64 = 42.0;
const AGENT_CARD_HEIGHT: f64 = 6.0;
const CONTROL_LANE_PADDING: f64 = 4.0;
const DISPLAY_ATTACH_WAIT: Duration = Duration::from_secs(5);
const DISPLAY_ATTACH_POLL: Duration = Duration::from_millis(100);

fn edge_label(relation: &str) -> Option<String> {
    match relation {
        "depends_on" => Some("depends".into()),
        "spawned" => Some("spawns".into()),
        "delegates" => Some("delegates".into()),
        "reviewed_by" => Some("reviews".into()),
        "feedback" => Some("retry".into()),
        "reports" => Some("reports".into()),
        "conditional" | "routes_to" => Some("condition".into()),
        other if other.starts_with("route") => Some("route".into()),
        other if other.starts_with("when ") => Some(format!(
            "if {}",
            compact_condition(other.trim_start_matches("when "))
        )),
        other => Some(truncate(other, EDGE_LABEL_WIDTH)),
    }
}

fn compact_condition(condition: &str) -> String {
    for (operator, compact) in [
        (" == ", "="),
        (" != ", "≠"),
        (" >= ", "≥"),
        (" <= ", "≤"),
        (" > ", ">"),
        (" < ", "<"),
    ] {
        if let Some((left, right)) = condition.split_once(operator) {
            let field = left.rsplit('.').next().unwrap_or(left).trim();
            let value = right.trim().trim_matches(['\'', '"']);
            return truncate(
                &format!("{field}{compact}{value}"),
                EDGE_LABEL_WIDTH.saturating_sub(3),
            );
        }
    }
    truncate(condition, EDGE_LABEL_WIDTH.saturating_sub(3))
}

#[derive(Clone, Copy, Debug, Default)]
struct HitAreas {
    tree_tab: Rect,
    graph_tab: Rect,
    integrations_tab: Rect,
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

type RefreshPayload = (WorkspaceState, Option<daemon::Status>, Option<String>);
type LaunchReadyNodes = BTreeSet<(String, String)>;
type ProviderRefreshPayload = (Vec<Manifest>, LaunchReadyNodes);

enum BackgroundResult {
    Refresh(Result<RefreshPayload, String>),
    Providers(Result<ProviderRefreshPayload, String>),
    Enrichment {
        generation: u64,
        rebind_current: bool,
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
    Validation {
        provider_name: String,
        result: Result<String, String>,
    },
    Action(Result<String, String>),
}

struct App {
    scope: PathBuf,
    config: Config,
    state: WorkspaceState,
    providers: Vec<Manifest>,
    launch_ready: LaunchReadyNodes,
    supervisor: Option<daemon::Status>,
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
    provider_validation_loading: BTreeSet<String>,
    provider_reports: BTreeMap<String, String>,
    changes: String,
    changes_loaded_at: Option<Instant>,
    changes_loading: bool,
    action_inflight: bool,
    last_refresh: Instant,
    refresh_inflight: bool,
    refresh_requested: bool,
    provider_refresh_inflight: bool,
    enrichment_inflight: bool,
    enrichment_requested: bool,
    rebind_current_pending: bool,
    enrichment_due_at: Instant,
    state_generation: u64,
    resize_at: Option<Instant>,
    hit: HitAreas,
    graph_signature: String,
    boot: BootState,
    preferences: WorkspacePreferences,
    editor: Option<NodeEditor>,
    loading_animation: AnimationConfig,
    startup_warning: Option<String>,
}

impl App {
    fn loading(config: Config, scope: PathBuf, loading_animation: animation::Loaded) -> Self {
        let mut preferences = preferences::read(&scope).unwrap_or_default();
        if preferences.reduced_motion.is_none() {
            preferences.reduced_motion = Some(config.ui.reduced_motion);
        }
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
        app.loading_animation = loading_animation.config;
        app.startup_warning = loading_animation.warning;
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
            .filter(|run| run.status.active())
            .max_by_key(|run| run.updated_at)
            .map(|run| run.id.clone());
        let expanded = default_expansions(&state);
        let mut app = Self {
            scope,
            config,
            state,
            providers,
            launch_ready: BTreeSet::new(),
            supervisor: daemon::status().ok().flatten(),
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
            provider_validation_loading: BTreeSet::new(),
            provider_reports: BTreeMap::new(),
            changes: String::new(),
            changes_loaded_at: None,
            changes_loading: false,
            action_inflight: false,
            last_refresh: Instant::now(),
            refresh_inflight: false,
            refresh_requested: false,
            provider_refresh_inflight: false,
            enrichment_inflight: false,
            enrichment_requested: false,
            rebind_current_pending: true,
            enrichment_due_at,
            state_generation: 0,
            resize_at: None,
            hit: HitAreas::default(),
            graph_signature: String::new(),
            boot: BootState::Ready,
            preferences: WorkspacePreferences::default(),
            editor: None,
            loading_animation: animation::fallback(),
            startup_warning: None,
        };
        app.rebuild(true);
        app
    }

    fn apply_preferences(&mut self, preferences: WorkspacePreferences) {
        self.active_run.clone_from(&preferences.active_run);
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
        self.preferences.active_run.clone_from(&self.active_run);
        self.preferences.selected_item = self.tree.get(self.tree_at).map(|row| row.id.clone());
        self.preferences.graph_selected_item = self.flow.first_selected_node_id();
        let viewport = self.flow.to_snapshot().viewport;
        self.preferences.graph_pan_x = viewport.x;
        self.preferences.graph_pan_y = viewport.y;
        self.preferences.graph_zoom = viewport.zoom;
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

    fn needs_animation(&self) -> bool {
        matches!(self.boot, BootState::Loading { .. })
            || self.refresh_inflight
            || self.enrichment_inflight
            || self.provider_refresh_inflight
            || self.changes_loading
            || !self.activity_loading.is_empty()
            || !self.provider_activity_loading.is_empty()
            || !self.provider_validation_loading.is_empty()
            || self.flow.is_dragging()
            || self.state.sessions.iter().any(|session| {
                RuntimeActivity::for_session(session) == Some(RuntimeActivity::Active)
            })
            || self.state.runs.iter().any(|run| {
                run.nodes
                    .iter()
                    .any(|node| node_runtime_active(&self.state, node))
            })
    }

    fn rebuild(&mut self, force_layout: bool) {
        let selected_tree_id = self.tree.get(self.tree_at).map(|row| row.id.clone());
        self.tree = tree_rows(&self.state, &self.expanded, &self.providers);
        self.tree_at = selected_tree_id
            .as_deref()
            .and_then(|id| self.tree.iter().position(|row| row.id == id))
            .unwrap_or_else(|| self.tree_at.min(self.tree.len().saturating_sub(1)));
        self.provider_at = self.provider_at.min(self.providers.len().saturating_sub(1));
        let signature = graph_signature(&self.state, self.active_run.as_deref());
        if signature != self.graph_signature || force_layout {
            let restore_viewport = self.preferences.graph_selected_item.is_some();
            let selected = self
                .preferences
                .graph_selected_item
                .clone()
                .or_else(|| self.flow.first_selected_node_id());
            let (mut flow, items) = build_flow(&self.state, self.active_run.as_deref());
            if let Some(selected) = selected {
                flow.select_node(&selected);
            }
            if flow.first_selected_node_id().is_none() {
                flow.select_next_node();
            }
            let mut snapshot = flow.to_snapshot();
            snapshot
                .viewport
                .set_offset(self.preferences.graph_pan_x, self.preferences.graph_pan_y);
            snapshot.viewport.zoom = self.preferences.graph_zoom.clamp(0.75, 2.0);
            if let Ok(restored) = AgentFlow::from_snapshot(snapshot) {
                flow = configure_flow(restored);
            }
            self.flow = flow;
            if !restore_viewport {
                self.flow
                    .request_fit_view_with_options(FitViewOptions::default().with_padding(3.0));
            }
            self.graph_items = items;
            self.graph_signature = signature;
        } else {
            refresh_flow_content(&mut self.flow, &self.state, self.active_run.as_deref());
        }
    }

    fn request_refresh(&mut self, tx: &Sender<BackgroundResult>) {
        self.request_provider_refresh(tx);
        if self.refresh_inflight {
            self.refresh_requested = true;
            return;
        }
        self.refresh_inflight = true;
        self.refresh_requested = false;
        let scope = self.scope.clone();
        let tx = tx.clone();
        thread::spawn(move || {
            let result = control::read_workspace(&scope)
                .map(|state| {
                    let mut warnings = Vec::new();
                    let supervisor = daemon::status().unwrap_or_else(|error| {
                        warnings.push(format!("supervisor status failed: {error:#}"));
                        None
                    });
                    (
                        state,
                        supervisor,
                        (!warnings.is_empty()).then(|| warnings.join("; ")),
                    )
                })
                .map_err(|error| format!("{error:#}"));
            let _ = tx.send(BackgroundResult::Refresh(result));
        });
    }

    fn request_provider_refresh(&mut self, tx: &Sender<BackgroundResult>) {
        if self.provider_refresh_inflight {
            return;
        }
        self.provider_refresh_inflight = true;
        let config = self.config.clone();
        let scope = self.scope.clone();
        let state = self.state.clone();
        let direction = self.display_direction().to_owned();
        let tx = tx.clone();
        thread::spawn(move || {
            let result = provider::discover(&config)
                .map(|providers| {
                    let launch_ready =
                        launch_ready_nodes(&config, &providers, &scope, &state, &direction);
                    (providers, launch_ready)
                })
                .map_err(|error| format!("{error:#}"));
            let _ = tx.send(BackgroundResult::Providers(result));
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
        let rebind_current = self.rebind_current_pending;
        self.rebind_current_pending = false;
        thread::spawn(move || {
            let result = control::reconcile_with_current(&config, &scope, rebind_current)
                .map_err(|error| format!("{error:#}"));
            let _ = tx.send(BackgroundResult::Enrichment {
                generation,
                rebind_current,
                result,
            });
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
                    Ok((state, supervisor, warning)) => {
                        let first_load = matches!(self.boot, BootState::Loading { .. });
                        self.state = state;
                        self.state_generation = self.state_generation.wrapping_add(1);
                        self.supervisor = supervisor;
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
                                .filter(|run| run.status.active())
                                .max_by_key(|run| run.updated_at)
                                .map(|run| run.id.clone());
                        }
                        self.boot = BootState::Ready;
                        self.rebuild(false);
                        let warnings = [self.startup_warning.take(), warning]
                            .into_iter()
                            .flatten()
                            .collect::<Vec<_>>();
                        if !warnings.is_empty() {
                            self.set_status(warnings.join(" · "));
                        }
                        if first_load {
                            if let Some(selected) = self.preferences.selected_item.as_deref()
                                && let Some(index) =
                                    self.tree.iter().position(|row| row.id == selected)
                            {
                                self.tree_at = index;
                            }
                            if let Some(selected) = self.preferences.graph_selected_item.as_deref()
                                && self.graph_items.contains_key(selected)
                            {
                                self.flow.select_node(selected);
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
            BackgroundResult::Providers(result) => {
                self.provider_refresh_inflight = false;
                match result {
                    Ok((providers, launch_ready)) => {
                        self.providers = providers;
                        self.launch_ready = launch_ready;
                        self.provider_at =
                            self.provider_at.min(self.providers.len().saturating_sub(1));
                        self.rebuild(false);
                    }
                    Err(error) => self.set_status(format!("provider discovery failed: {error}")),
                }
            }
            BackgroundResult::Enrichment {
                generation,
                rebind_current,
                result,
            } => {
                self.enrichment_inflight = false;
                if rebind_current && (result.is_err() || generation != self.state_generation) {
                    self.rebind_current_pending = true;
                }
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
                self.changes_loaded_at = Some(Instant::now());
                self.changes =
                    result.unwrap_or_else(|error| format!("Changes provider failed: {error}"));
            }
            BackgroundResult::Validation {
                provider_name,
                result,
            } => {
                self.provider_validation_loading.remove(&provider_name);
                self.provider_reports.insert(
                    provider_name,
                    result.unwrap_or_else(|error| format!("Provider validation failed: {error}")),
                );
            }
            BackgroundResult::Action(result) => {
                self.action_inflight = false;
                self.set_status(result.unwrap_or_else(|error| format!("Action failed: {error}")));
                self.refresh_requested = true;
            }
        }
    }

    fn request_changes(&mut self, tx: &Sender<BackgroundResult>, force: bool) {
        if !self.changes_need_loading(force) {
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

    fn changes_need_loading(&self, force: bool) -> bool {
        !self.changes_loading
            && (force || self.changes_loaded_at.is_none())
            && !self.providers.is_empty()
    }

    fn changes_view_is_open(&self) -> bool {
        self.main_tab == MainTab::Work
            && self.output_tab == OutputTab::Changes
            && inspector_tabs(self.selected().as_ref())
                .iter()
                .any(|(tab, _)| *tab == OutputTab::Changes)
    }

    fn display_direction(&self) -> &str {
        match self.preferences.display_direction.as_str() {
            "left" | "top" | "bottom" => &self.preferences.display_direction,
            _ => "right",
        }
    }

    fn cycle_display_direction(&mut self) {
        self.preferences.display_direction = match self.display_direction() {
            "right" => "bottom",
            "bottom" => "left",
            "left" => "top",
            _ => "right",
        }
        .into();
        self.set_status(format!(
            "new agent displays open {}",
            self.preferences.display_direction
        ));
        self.persist_preferences();
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
        let session = match self.selected()? {
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
        };
        session.filter(|session| session.status != LifecycleStatus::Archived)
    }

    fn selected_run_id(&self) -> Option<String> {
        match self.selected()? {
            ItemRef::Run(id) | ItemRef::Node(id, _) => Some(id),
            ItemRef::Session(id) => self
                .state
                .sessions
                .iter()
                .find(|session| session.id == id)
                .and_then(|session| {
                    session.run_id.clone().or_else(|| {
                        self.state
                            .runs
                            .iter()
                            .filter(|run| run.orchestrator_id.as_deref() == Some(id.as_str()))
                            .max_by_key(|run| {
                                (
                                    self.active_run.as_deref() == Some(run.id.as_str()),
                                    run.status.active(),
                                    run.updated_at,
                                )
                            })
                            .map(|run| run.id.clone())
                    })
                }),
            ItemRef::Provider(_) | ItemRef::History => None,
        }
    }

    fn selected_unassigned_stage(&self) -> Option<(String, String)> {
        let ItemRef::Node(run_id, node_id) = self.selected()? else {
            return None;
        };
        let run = self
            .state
            .runs
            .iter()
            .find(|run| run.id == run_id && run.status.active())?;
        run.nodes
            .iter()
            .find(|node| node.id == node_id && node.session_id.is_none() && node.status.active())?;
        Some((run_id, node_id))
    }

    fn managed_node_session<'a>(
        state: &'a WorkspaceState,
        run_id: &str,
        node_id: &str,
    ) -> Option<&'a Session> {
        let session_id = state
            .runs
            .iter()
            .find(|run| run.id == run_id)?
            .nodes
            .iter()
            .find(|node| node.id == node_id)?
            .session_id
            .as_deref()?;
        state.sessions.iter().find(|session| {
            session.id == session_id
                && session.registration == RegistrationSource::Managed
                && session.status != LifecycleStatus::Archived
        })
    }

    fn node_is_active(state: &WorkspaceState, run_id: &str, node_id: &str) -> bool {
        state
            .runs
            .iter()
            .find(|run| run.id == run_id && run.status.active())
            .and_then(|run| run.nodes.iter().find(|node| node.id == node_id))
            .is_some_and(|node| node.status.active())
    }

    fn selected_tree_run_id(&self) -> Option<String> {
        let item = self.tree.get(self.tree_at).map(|row| &row.item)?;
        match item {
            ItemRef::Run(id) | ItemRef::Node(id, _) => Some(id.clone()),
            ItemRef::Session(id) => {
                let session = self
                    .state
                    .sessions
                    .iter()
                    .find(|session| session.id == *id)?;
                session.run_id.clone().or_else(|| {
                    self.state
                        .runs
                        .iter()
                        .filter(|run| run.orchestrator_id.as_deref() == Some(id.as_str()))
                        .max_by_key(|run| (run.status.active(), run.updated_at))
                        .map(|run| run.id.clone())
                })
            }
            ItemRef::Provider(_) | ItemRef::History => None,
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
            return;
        }
        if let Some(session) = self.selected_session() {
            self.confirmation = Some(Confirmation::Prune {
                session_id: session.id.clone(),
                title: session.title.clone(),
            });
            return;
        }
        self.set_status("select a run first");
    }

    fn confirm(&mut self, tx: &Sender<BackgroundResult>) {
        if self.action_inflight {
            self.set_status("an action is already running");
            return;
        }
        let Some(confirmation) = self.confirmation.take() else {
            return;
        };
        self.action_inflight = true;
        self.set_status("applying action…");
        let config = self.config.clone();
        let scope = self.scope.clone();
        let tx = tx.clone();
        thread::spawn(move || {
            let result: Result<(String, LifecycleStatus)> = (|| {
                control::require_supervisor_control(&scope)?;
                match confirmation {
                    Confirmation::Approve { run_id, gate_id } => {
                        workflow::approve(&config, &scope, &run_id, Some(&gate_id), false)
                            .and_then(|_| workflow::spawn(&config, &scope, &run_id))
                            .map(|run| (run.name, run.status))
                    }
                    Confirmation::Cancel { run_id } => {
                        workflow::cancel(&config, &scope, &run_id).map(|run| (run.name, run.status))
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
                }
            })();
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
        if self.action_inflight {
            self.set_status("an action is already running");
            return;
        }
        if let Some(session) = self.selected_session().cloned() {
            if attach_readiness(&session, &self.providers) == AttachReadiness::Unavailable {
                self.set_status("this session has no ready display and persistence provider chain");
                return;
            }
            self.action_inflight = true;
            self.set_status("opening session through providers");
            let config = self.config.clone();
            let scope = self.scope.clone();
            let direction = self.display_direction().to_owned();
            let tx = tx.clone();
            thread::spawn(move || {
                let result =
                    control::attach_quiet(&config, &scope, &session.id, Action::Attach, &direction)
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
        } else if let Some((run_id, node_id)) = self.selected_unassigned_stage() {
            if !launch_attach_ready(self) {
                self.set_status("this stage has no ready persistence and display provider chain");
                return;
            }
            let (launch_request, execution_provider) = self
                .state
                .runs
                .iter()
                .find(|run| run.id == run_id)
                .and_then(|run| {
                    run.nodes
                        .iter()
                        .find(|node| node.id == node_id)
                        .map(|node| {
                            (
                                launch_preflight_request(
                                    &self.scope,
                                    run,
                                    node,
                                    self.display_direction(),
                                ),
                                node.execution.clone(),
                            )
                        })
                })
                .expect("selected unassigned stage remains present");
            self.action_inflight = true;
            self.set_status(format!("launching {node_id} through the workflow executor"));
            let config = self.config.clone();
            let scope = self.scope.clone();
            let direction = self.display_direction().to_owned();
            let tx = tx.clone();
            thread::spawn(move || {
                let result = (|| -> Result<String> {
                    let providers = provider::discover(&config)?;
                    provider::launch_attach_route_ready(
                        &config,
                        &providers,
                        launch_request,
                        execution_provider.as_deref(),
                    )?;
                    let started =
                        workflow::spawn_with_direction(&config, &scope, &run_id, &direction)?;
                    let deadline = Instant::now() + DISPLAY_ATTACH_WAIT;
                    loop {
                        let state = control::read_workspace(&scope)?;
                        if let Some(session) = Self::managed_node_session(&state, &run_id, &node_id)
                        {
                            let outcome = control::attach_quiet(
                                &config,
                                &scope,
                                &session.id,
                                Action::Attach,
                                &direction,
                            )?;
                            if outcome.code != 0 {
                                anyhow::bail!("attach exited with {}", outcome.code);
                            }
                            return Ok(format!("opened {} through providers", session.title));
                        }
                        if !Self::node_is_active(&state, &run_id, &node_id)
                            || Instant::now() >= deadline
                        {
                            return Ok(format!(
                                "{} started; its display is not ready yet",
                                started.name
                            ));
                        }
                        thread::sleep(DISPLAY_ATTACH_POLL);
                    }
                })()
                .map_err(|error| format!("{error:#}"));
                let _ = tx.send(BackgroundResult::Action(result));
            });
        } else if matches!(self.selected(), Some(ItemRef::Node(_, _))) {
            self.set_status("this completed stage has no live agent to open");
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
        if self.provider_validation_loading.contains(&name) {
            self.set_status(format!("validating {name}…"));
            return;
        }
        self.provider_validation_loading.insert(name.clone());
        self.set_status(format!("validating {name}…"));
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
            let _ = tx.send(BackgroundResult::Validation {
                provider_name: name,
                result,
            });
        });
        self.output_tab = OutputTab::Result;
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
            execution: node.execution.clone().unwrap_or_default(),
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
                control::require_supervisor_control(&scope)?;
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
                        model: non_empty(editor.model),
                        execution: non_empty(editor.execution),
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
        if key.kind == KeyEventKind::Repeat && provider_action_key(key) {
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
            if key.code == KeyCode::Char('q') {
                return true;
            }
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('?')) {
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
            (KeyCode::Char('j'), true) if binding_enabled(self, "focus-inspector") => {
                self.focus = Focus::Inspector
            }
            (KeyCode::Char('k'), true) if binding_enabled(self, "focus-main") => {
                self.focus = Focus::Main
            }
            (KeyCode::Char('l'), true) if binding_enabled(self, "focus-inspector") => {
                self.focus = Focus::Inspector
            }
            (KeyCode::Char('h'), true) if binding_enabled(self, "focus-main") => {
                self.focus = Focus::Main
            }
            (KeyCode::Char('d'), true) if binding_enabled(self, "page") => self.page(1),
            (KeyCode::Char('u'), true) if binding_enabled(self, "page") => self.page(-1),
            (KeyCode::Tab, _) | (KeyCode::BackTab, _) if binding_enabled(self, "inspect-tabs") => {
                self.next_inspector(if key.code == KeyCode::BackTab { -1 } else { 1 });
                self.persist_preferences();
            }
            (KeyCode::Tab, _) | (KeyCode::BackTab, _) if binding_enabled(self, "view") => {
                self.clear_status();
                self.main_tab = MainTab::Work;
                if self.explorer_view == ExplorerView::Tree {
                    self.active_run = self.selected_tree_run_id();
                }
                self.explorer_view = if self.explorer_view == ExplorerView::Tree {
                    ExplorerView::Graph
                } else {
                    ExplorerView::Tree
                };
                self.focus = Focus::Main;
                self.rebuild(true);
                self.persist_preferences();
            }
            (KeyCode::Char('['), _) if inspector(self) => self.next_inspector(-1),
            (KeyCode::Char(']'), _) if inspector(self) => self.next_inspector(1),
            (KeyCode::Char('p'), _) if binding_enabled(self, "integrations") => {
                self.switch_main_tab(MainTab::Integrations);
            }
            (KeyCode::Esc, _) if binding_enabled(self, "return-work") => {
                self.switch_main_tab(MainTab::Work);
            }
            (KeyCode::Char('a'), _) if binding_enabled(self, "gate") => {
                if !self.request_gate() {
                    self.set_status("the selected run has no pending gate");
                }
            }
            (KeyCode::Char('m'), _) if binding_enabled(self, "mode") => {
                self.preferences.autonomy = self.preferences.autonomy.next();
                self.set_status(format!("autonomy: {}", self.preferences.autonomy));
                self.persist_preferences();
            }
            (KeyCode::Char('M'), _) if binding_enabled(self, "reduced-motion") => {
                let reduced = !self
                    .preferences
                    .reduced_motion
                    .unwrap_or(self.config.ui.reduced_motion);
                self.preferences.reduced_motion = Some(reduced);
                self.set_status(if reduced {
                    "reduced motion enabled"
                } else {
                    "reduced motion disabled"
                });
                self.persist_preferences();
            }
            (KeyCode::Char('g'), _) if binding_enabled(self, "drill") => self.drill_down(),
            (KeyCode::Char('e'), _) if binding_enabled(self, "edit-node") => {
                self.open_node_editor()
            }
            (KeyCode::Char('D'), _) if binding_enabled(self, "delete-node") => {
                self.request_delete_node()
            }
            (KeyCode::Char('r'), _) => {
                self.enrichment_requested = true;
                self.request_refresh(tx);
                if self.changes_view_is_open() {
                    self.request_changes(tx, true);
                }
            }
            (KeyCode::Char('R'), _) if binding_enabled(self, "relayout") => {
                self.rebuild(true);
                self.flow.request_fit_view();
            }
            (KeyCode::Char('o'), _) if binding_enabled(self, "viewport") => {
                self.flow.request_fit_view();
            }
            (KeyCode::Char('+' | '=' | '-' | '_'), _) if binding_enabled(self, "viewport") => {
                let _ = self.flow.handle_controls_key_event(key);
                clamp_flow_viewport(&mut self.flow);
                self.persist_preferences();
            }
            (KeyCode::Char('='), _) if binding_enabled(self, "resize") => {
                self.config.ui.inspector_percent = (self.config.ui.inspector_percent + 5).min(80);
                self.persist_preferences();
            }
            (KeyCode::Char('-'), _) if binding_enabled(self, "resize") => {
                self.config.ui.inspector_percent =
                    self.config.ui.inspector_percent.saturating_sub(5).max(20);
                self.persist_preferences();
            }
            (KeyCode::Char('i'), _) if binding_enabled(self, "activity") => {
                self.load_output(Action::Activity, tx)
            }
            (KeyCode::Char('c'), _) if binding_enabled(self, "changes") => {
                self.load_output(Action::Changes, tx)
            }
            (KeyCode::Char('d'), _) if binding_enabled(self, "display-direction") => {
                self.cycle_display_direction()
            }
            (KeyCode::Char('v'), _) if binding_enabled(self, "provider-validate") => {
                self.validate_provider(tx)
            }
            (KeyCode::Char('x'), _) if binding_enabled(self, "cancel") => self.request_cancel(),
            (KeyCode::Enter, _)
                if binding_enabled(self, "open") || binding_enabled(self, "provider-validate") =>
            {
                self.open_selected(tx)
            }
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
            self.persist_preferences();
            return;
        }
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            if contains(self.hit.tree_tab, x, y) {
                self.switch_main_tab(MainTab::Work);
                self.explorer_view = ExplorerView::Tree;
                self.rebuild(true);
                self.persist_preferences();
                return;
            }
            if contains(self.hit.graph_tab, x, y) {
                self.switch_main_tab(MainTab::Work);
                if self.explorer_view == ExplorerView::Tree {
                    self.active_run = self.selected_tree_run_id();
                }
                self.explorer_view = ExplorerView::Graph;
                self.rebuild(true);
                self.flow.request_fit_view();
                self.persist_preferences();
                return;
            }
            if contains(self.hit.integrations_tab, x, y) {
                self.switch_main_tab(MainTab::Integrations);
                return;
            }
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
                    self.persist_preferences();
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
                        self.tree_at = index;
                        self.persist_preferences();
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
            if let Some(selected) = self.flow.first_selected_node_id() {
                self.flow.ensure_node_visible(&selected);
            }
            clamp_flow_viewport(&mut self.flow);
            self.persist_preferences();
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

fn non_empty(value: String) -> Option<String> {
    (!value.trim().is_empty()).then_some(value)
}

fn provider_action_key(key: KeyEvent) -> bool {
    matches!(
        key.code,
        KeyCode::Enter | KeyCode::Char('a' | 'c' | 'i' | 'v' | 'x') | KeyCode::Char('D')
    )
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
        Some(ItemRef::Session(_) | ItemRef::History) | None => AGENT,
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

fn node_session<'a>(state: &'a WorkspaceState, node: &WorkflowNode) -> Option<&'a Session> {
    let session_id = node.session_id.as_deref()?;
    state
        .sessions
        .iter()
        .find(|session| session.id == session_id && session.status != LifecycleStatus::Archived)
}

fn node_placement(state: &WorkspaceState, node: &WorkflowNode) -> String {
    if let Some(session) = node_session(state, node) {
        return session_placement(session);
    }
    let execution = node.execution.as_deref().unwrap_or("execution unassigned");
    if matches!(
        node.status,
        LifecycleStatus::Pending | LifecycleStatus::Queued | LifecycleStatus::Waiting
    ) {
        format!("proposed · {execution} · no agent assigned")
    } else {
        format!("{execution} · no agent assigned")
    }
}

fn node_runtime_active(state: &WorkspaceState, node: &WorkflowNode) -> bool {
    node.status == LifecycleStatus::Working
        && node_session(state, node).and_then(RuntimeActivity::for_session)
            == Some(RuntimeActivity::Active)
}

fn launch_ready_nodes(
    config: &Config,
    providers: &[Manifest],
    scope: &Path,
    state: &WorkspaceState,
    direction: &str,
) -> LaunchReadyNodes {
    state
        .runs
        .iter()
        .filter(|run| run.status.active())
        .flat_map(|run| {
            run.nodes
                .iter()
                .filter(|node| node.status.active() && node.session_id.is_none())
                .filter(move |node| {
                    provider::launch_attach_route_ready_cached(
                        config,
                        providers,
                        launch_preflight_request(scope, run, node, direction),
                        node.execution.as_deref(),
                    )
                    .is_ok()
                })
                .map(|node| (run.id.clone(), node.id.clone()))
        })
        .collect()
}

fn launch_preflight_request(
    scope: &Path,
    run: &WorkflowRun,
    node: &WorkflowNode,
    direction: &str,
) -> serde_json::Value {
    let session_id = format!("preflight-{}-{}", run.id, node.id);
    let prompt = node.prompt.clone().unwrap_or_else(|| {
        format!(
            "Goal: {}\nExpected output: {}\nSuccess criteria:\n{}",
            node.goal,
            node.expected_output,
            node.success_criteria
                .iter()
                .map(|criterion| format!("- {criterion}"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    });
    let mut selected_providers = serde_json::Map::new();
    if let Some(execution) = &node.execution {
        selected_providers.insert(
            Capability::ExecutionRun.to_string(),
            serde_json::Value::String(execution.clone()),
        );
    }
    serde_json::json!({
        "version": "orc.provider/v1",
        "action": "launch",
        "preflight": true,
        "scope": scope,
        "direction": direction,
        "session": {
            "id": session_id,
            "nativeId": session_id,
            "harness": node.harness,
            "model": node.model,
            "role": node.role,
            "title": node.name,
            "purpose": node.purpose,
            "goal": node.goal,
            "expectedOutput": node.expected_output,
            "successCriteria": node.success_criteria,
            "completion": node.completion,
            "reviewBy": node.review_by,
            "parentId": run.orchestrator_id,
            "runId": run.id,
            "nodeId": node.id,
            "providerRef": serde_json::Value::Null,
            "providers": [],
            "directory": scope,
            "registration": RegistrationSource::Managed,
            "status": node.status,
        },
        "command": [node.harness, prompt],
        "prompt": prompt,
        "environment": {
            "ORC_SCOPE": scope,
            "ORC_SESSION_ID": session_id,
            "ORC_NATIVE_SESSION_ID": session_id,
            "ORC_PARENT_SESSION_ID": run.orchestrator_id,
            "ORC_RUN_ID": run.id,
            "ORC_NODE_ID": node.id,
        },
        "providers": selected_providers,
    })
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

fn harness_label(harness: &str, model: Option<&str>) -> String {
    model.map_or_else(|| harness.to_owned(), |model| format!("{harness}/{model}"))
}

fn node_attention(run: &WorkflowRun, node: &WorkflowNode) -> Option<String> {
    run.pending_gates
        .iter()
        .find(|gate| gate.before == node.id)
        .map(|gate| format!("! human gate · {}", gate.reason))
}

fn run_attention(run: &WorkflowRun) -> Option<String> {
    let count = run.pending_gates.len();
    (count > 0).then(|| {
        format!(
            "! {count} human gate{} waiting",
            if count == 1 { "" } else { "s" }
        )
    })
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
        subtitle: session_placement(session),
        contract: harness_label(&session.harness, session.model.as_deref()),
        goal: session.goal.clone(),
        attention: None,
        status: session.status,
        active: RuntimeActivity::for_session(session) == Some(RuntimeActivity::Active),
    };
    let node = Node::new(&id, (0.0, 0.0), (AGENT_CARD_WIDTH, AGENT_CARD_HEIGHT), card)
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
    let active_orchestrator = if let Some(run) = active_run {
        run.orchestrator_id.as_deref().and_then(|id| {
            state
                .sessions
                .iter()
                .find(|session| session.id == id && session.status != LifecycleStatus::Archived)
        })
    } else {
        state.current_session()
    };
    let orchestrator_id = active_orchestrator.map(|session| session.id.as_str());
    for session in &state.sessions {
        if orchestrator_id == Some(session.id.as_str()) {
            continue;
        }
        if let Some(card) = flow.node_content_mut(&format!("session:{}", session.id)) {
            card.title.clone_from(&session.title);
            card.kind = session.role.to_string();
            card.subtitle = session_placement(session);
            card.contract = harness_label(&session.harness, session.model.as_deref());
            card.goal.clone_from(&session.goal);
            card.attention = None;
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
                card.title.clone_from(&run.name);
                card.subtitle = format!("{} · {} stages", run_phase(run), run.nodes.len());
                card.contract = orchestrator.map_or_else(
                    || "workflow root".into(),
                    |session| harness_label(&session.harness, session.model.as_deref()),
                );
                card.goal.clone_from(&run.goal);
                card.attention = run_attention(run);
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
                card.subtitle = node_placement(state, node);
                card.contract = format!(
                    "{} · judge {}",
                    harness_label(&node.harness, node.model.as_deref()),
                    node.judge_policy
                );
                card.goal.clone_from(&node.goal);
                card.attention = node_attention(run, node);
                card.status = node.status;
                card.active = node_runtime_active(state, node);
            }
        }
    }
}

fn orchestration_edges(run: &WorkflowRun, root_id: &str) -> Vec<GraphEdge> {
    let mut edges = run
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
                root_id.to_owned(),
                format!("node:{}:{}", run.id, node.id),
                "delegates".into(),
                node.status == LifecycleStatus::Working,
            )
        })
        .collect::<Vec<_>>();
    edges.extend(
        run.nodes
            .iter()
            .filter(|node| node.completion == CompletionTarget::Orchestrator)
            .map(|node| {
                (
                    format!("node:{}:{}", run.id, node.id),
                    root_id.to_owned(),
                    "reports".into(),
                    node.status == LifecycleStatus::Done,
                )
            }),
    );
    edges
}

fn orchestration_lane_clearance(edges: &[GraphEdge]) -> f64 {
    let lanes = edges
        .iter()
        .filter(|(_, _, relation, _)| matches!(relation.as_str(), "delegates" | "reports"))
        .count();
    CONTROL_LANE_PADDING + lanes.max(1) as f64 * 2.0
}

fn build_flow(
    state: &WorkspaceState,
    active_run: Option<&str>,
) -> (AgentFlow, BTreeMap<String, ItemRef>) {
    let mut flow = new_flow();
    let mut items = BTreeMap::new();
    let mut known = BTreeSet::new();
    let run = active_run.and_then(|id| state.runs.iter().find(|run| run.id == id));
    let orchestrator = if let Some(run) = run {
        run.orchestrator_id.as_deref().and_then(|id| {
            state
                .sessions
                .iter()
                .find(|session| session.id == id && session.status != LifecycleStatus::Archived)
        })
    } else {
        state.current_session()
    };
    if run.is_none()
        && let Some(session) = orchestrator
    {
        add_session_card(&mut flow, &mut items, &mut known, state, session);
    }
    if let Some(run) = run {
        for workflow_node in &run.nodes {
            let node_id = format!("node:{}:{}", run.id, workflow_node.id);
            let card = AgentCard {
                kind: workflow_node.role.to_string(),
                title: workflow_node.name.clone(),
                subtitle: node_placement(state, workflow_node),
                contract: format!(
                    "{} · judge {}",
                    harness_label(&workflow_node.harness, workflow_node.model.as_deref()),
                    workflow_node.judge_policy
                ),
                goal: workflow_node.goal.clone(),
                attention: node_attention(run, workflow_node),
                status: workflow_node.status,
                active: node_runtime_active(state, workflow_node),
            };
            let node = Node::new(
                &node_id,
                (0.0, 0.0),
                (AGENT_CARD_WIDTH, AGENT_CARD_HEIGHT),
                card,
            )
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
        let mut return_edges = Vec::new();
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
            let rendered = (
                format!("node:{}:{}", run.id, edge.from),
                format!("node:{}:{}", run.id, edge.to),
                edge.relationship.clone(),
                active,
            );
            if edge.relationship == "feedback" {
                return_edges.push(rendered);
            } else {
                topology_edges.push(rendered);
            }
        }
        add_graph_edges(&mut flow, &known, topology_edges, "topology");
        flow.apply_layout(
            Sugiyama::horizontal()
                .with_rank_spacing(LABELED_EDGE_RANK_SPACING)
                .with_node_spacing(3.0),
        );
        add_graph_edges(&mut flow, &known, return_edges, "feedback");

        let root_id = orchestrator
            .map(|session| format!("session:{}", session.id))
            .unwrap_or_else(|| format!("run:{}", run.id));
        let orchestration_edges = orchestration_edges(run, &root_id);
        let control_clearance = orchestration_lane_clearance(&orchestration_edges);
        let stage_bounds = run
            .nodes
            .iter()
            .filter_map(|node| flow.node_bounds(&format!("node:{}:{}", run.id, node.id)))
            .reduce(|left, right| left.union(&right));
        let root_position =
            stage_bounds.map_or((0.0, -AGENT_CARD_HEIGHT - control_clearance), |bounds| {
                (
                    bounds.center().x - AGENT_CARD_WIDTH / 2.0,
                    bounds.y() - AGENT_CARD_HEIGHT - control_clearance,
                )
            });
        let lifecycle = orchestrator.map_or(run.status, |session| session.status);
        let root = Node::new(
            &root_id,
            root_position,
            (AGENT_CARD_WIDTH, AGENT_CARD_HEIGHT),
            AgentCard {
                kind: "orchestrator".into(),
                title: run.name.clone(),
                subtitle: format!("{} · {} stages", run_phase(run), run.nodes.len()),
                contract: orchestrator.map_or_else(
                    || "workflow root".into(),
                    |session| harness_label(&session.harness, session.model.as_deref()),
                ),
                goal: run.goal.clone(),
                attention: run_attention(run),
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

        add_graph_edges(&mut flow, &known, orchestration_edges, "orchestration");
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
    flow.request_fit_view_with_options(FitViewOptions::default().with_padding(3.0));
    (flow, items)
}

fn add_graph_edges(
    flow: &mut AgentFlow,
    known: &BTreeSet<String>,
    edges: Vec<GraphEdge>,
    kind: &str,
) {
    let mut relation_lanes = BTreeMap::<String, usize>::new();
    for (index, (from, to, relation, active)) in edges.into_iter().enumerate() {
        if !known.contains(&from) || !known.contains(&to) {
            continue;
        }
        let lane = relation_lanes.entry(relation.clone()).or_default();
        let relation_lane = *lane;
        *lane += 1;
        let edge = Edge::new(format!("edge:{kind}:{index}"), from, to)
            .with_content(RelationEdge {
                active,
                relation: relation.clone(),
                lane: relation_lane,
                ..RelationEdge::default()
            })
            .with_selectable(false)
            .with_deletable(false)
            .with_reconnectable(Reconnectable::None);
        let edge = match relation.as_str() {
            "delegates" | "spawned" => edge
                .with_source_side(HandlePosition::Bottom)
                .with_target_side(HandlePosition::Top),
            "feedback" => edge
                .with_source_side(HandlePosition::Bottom)
                .with_target_side(HandlePosition::Bottom),
            "reports" => edge
                .with_source_side(HandlePosition::Top)
                .with_target_side(HandlePosition::Bottom),
            _ => edge
                .with_source_side(HandlePosition::Right)
                .with_target_side(HandlePosition::Left),
        };
        let _ = flow.add_edge(edge);
    }
}

fn graph_handles() -> Vec<Handle> {
    [
        (HandlePosition::Top, true),
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

fn tree_rows(
    state: &WorkspaceState,
    expanded: &BTreeSet<String>,
    providers: &[Manifest],
) -> Vec<TreeRow> {
    let mut rows = Vec::new();
    let mut visited = BTreeSet::new();
    let mut roots: Vec<_> = state
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
    roots.sort_by_key(|session| std::cmp::Reverse(session.updated_at));
    for session in roots {
        push_session_rows(
            state,
            expanded,
            providers,
            &mut rows,
            &mut visited,
            session,
            0,
        );
    }
    let mut remaining: Vec<_> = state
        .sessions
        .iter()
        .filter(|session| {
            session.status != LifecycleStatus::Archived && !visited.contains(&session.id)
        })
        .collect();
    remaining.sort_by_key(|session| std::cmp::Reverse(session.updated_at));
    for session in remaining {
        push_session_rows(
            state,
            expanded,
            providers,
            &mut rows,
            &mut visited,
            session,
            0,
        );
    }
    let active_session_ids = state
        .sessions
        .iter()
        .filter(|session| session.status != LifecycleStatus::Archived)
        .map(|session| session.id.as_str())
        .collect::<BTreeSet<_>>();
    let mut orphaned_active_runs = state
        .runs
        .iter()
        .filter(|run| {
            run.status.active()
                && !run
                    .orchestrator_id
                    .as_deref()
                    .is_some_and(|id| active_session_ids.contains(id))
        })
        .collect::<Vec<_>>();
    orphaned_active_runs.sort_by_key(|run| std::cmp::Reverse(run.updated_at));
    for run in orphaned_active_runs {
        push_run_rows(state, expanded, providers, &mut rows, &mut visited, run, 0);
    }
    let mut history = state
        .runs
        .iter()
        .filter(|run| {
            !run.status.active()
                && !run
                    .orchestrator_id
                    .as_deref()
                    .is_some_and(|id| active_session_ids.contains(id))
        })
        .collect::<Vec<_>>();
    history.sort_by_key(|run| std::cmp::Reverse(run.updated_at));
    if !history.is_empty() {
        let history_id = "history".to_owned();
        rows.push(TreeRow {
            id: history_id.clone(),
            depth: 0,
            title: "recent history".into(),
            subtitle: format!("{} completed runs · newest first", history.len()),
            status: None,
            item: ItemRef::History,
            children: true,
        });
        if expanded.contains(&history_id) {
            for run in history {
                push_run_rows(state, expanded, providers, &mut rows, &mut visited, run, 1);
            }
        }
    }
    rows
}

fn push_session_rows(
    state: &WorkspaceState,
    expanded: &BTreeSet<String>,
    providers: &[Manifest],
    rows: &mut Vec<TreeRow>,
    visited: &mut BTreeSet<String>,
    session: &Session,
    depth: usize,
) {
    if !visited.insert(session.id.clone()) {
        return;
    }
    let id = format!("session:{}", session.id);
    let mut runs: Vec<_> = state
        .runs
        .iter()
        .filter(|run| run.orchestrator_id.as_deref() == Some(&session.id))
        .collect();
    runs.sort_by_key(|run| std::cmp::Reverse(run.updated_at));
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
            attach_readiness(session, providers).label().to_owned(),
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
        push_run_rows(state, expanded, providers, rows, visited, run, depth + 1);
    }
    for child in children {
        push_session_rows(state, expanded, providers, rows, visited, child, depth + 1);
    }
}

fn push_run_rows(
    state: &WorkspaceState,
    expanded: &BTreeSet<String>,
    providers: &[Manifest],
    rows: &mut Vec<TreeRow>,
    visited: &mut BTreeSet<String>,
    run: &WorkflowRun,
    depth: usize,
) {
    let run_id = format!("run:{}", run.id);
    let orchestrator_unavailable = run.status.active()
        && !run.orchestrator_id.as_deref().is_some_and(|id| {
            state
                .sessions
                .iter()
                .any(|session| session.id == id && session.status != LifecycleStatus::Archived)
        });
    rows.push(TreeRow {
        id: run_id.clone(),
        depth,
        title: run.name.clone(),
        subtitle: format!(
            "workflow · {} stages · {}{}",
            run.nodes.len(),
            run_phase(run),
            if orchestrator_unavailable {
                " · orchestrator unavailable"
            } else {
                ""
            }
        ),
        status: Some(run.status),
        item: ItemRef::Run(run.id.clone()),
        children: !run.nodes.is_empty(),
    });
    if !expanded.contains(&run_id) {
        return;
    }
    for node in &run.nodes {
        let assigned = node_session(state, node);
        let node_id = format!("node:{}:{}", run.id, node.id);
        rows.push(TreeRow {
            id: node_id.clone(),
            depth: depth + 1,
            title: node.name.clone(),
            subtitle: format!(
                "{} · {} · {}",
                node.role,
                node_placement(state, node),
                node.harness
            ),
            status: Some(node.status),
            item: ItemRef::Node(run.id.clone(), node.id.clone()),
            children: assigned.is_some(),
        });
        if expanded.contains(&node_id)
            && let Some(assigned) = assigned
        {
            push_session_rows(
                state,
                expanded,
                providers,
                rows,
                visited,
                assigned,
                depth + 2,
            );
        } else if let Some(assigned) = assigned {
            visited.insert(assigned.id.clone());
        }
    }
}

fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    app.hit = HitAreas::default();
    match &app.boot {
        BootState::Loading { started_at } => {
            render_loading(
                frame,
                area,
                *started_at,
                &app.loading_animation,
                app.preferences.reduced_motion.unwrap_or(false),
                app.startup_warning.as_deref(),
            );
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
    let (mut main, mut inspector) = split_body(body, app.dock, app.config.ui.inspector_percent);
    if graph_needs_full_body(app, main) {
        main = body;
        inspector = None;
    }
    if inspector.is_none() && app.focus == Focus::Inspector {
        app.focus = Focus::Main;
    }
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

fn render_loading(
    frame: &mut Frame,
    area: Rect,
    started_at: Instant,
    config: &AnimationConfig,
    reduced_motion: bool,
    warning: Option<&str>,
) {
    let elapsed_ms = u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
    let sampled = animation::sample(config, elapsed_ms, area.width, area.height, reduced_motion);
    let animation_style = match sampled.style {
        AnimationStyle::Default => plain(),
        AnimationStyle::Accent => accent().add_modifier(Modifier::BOLD),
        AnimationStyle::Muted => dim(),
        AnimationStyle::Success => Style::default().fg(Color::Green),
        AnimationStyle::Warning => Style::default().fg(Color::Yellow),
        AnimationStyle::Danger => Style::default().fg(Color::Red),
    };
    let height = sampled
        .height
        .saturating_add(2)
        .saturating_add(u16::from(warning.is_some()))
        .min(area.height);
    let warning_width = warning
        .map(UnicodeWidthStr::width)
        .and_then(|width| u16::try_from(width).ok())
        .unwrap_or_default();
    let width = sampled.width.max(20).max(warning_width).min(area.width);
    let target = Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, target);
    let art_height = sampled.height.min(target.height);
    frame.render_widget(
        Paragraph::new(sampled.content)
            .style(animation_style)
            .alignment(Alignment::Center),
        Rect::new(target.x, target.y, target.width, art_height),
    );
    let label_y = target.y.saturating_add(art_height).saturating_add(1);
    if label_y < target.bottom() {
        frame.render_widget(
            Paragraph::new("loading workspace…")
                .style(plain())
                .alignment(Alignment::Center),
            Rect::new(target.x, label_y, target.width, 1),
        );
    }
    let warning_y = label_y.saturating_add(1);
    if let Some(warning) = warning
        && warning_y < target.bottom()
    {
        frame.render_widget(
            Paragraph::new(warning)
                .style(Style::default().fg(Color::Yellow))
                .alignment(Alignment::Center),
            Rect::new(target.x, warning_y, target.width, 1),
        );
    }
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
    let too_small_for_dock = match dock {
        Dock::Bottom | Dock::Top => area.height < 18,
        Dock::Left | Dock::Right => area.width < 76,
        Dock::Hidden => true,
    };
    if too_small_for_dock {
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

fn graph_needs_full_body(app: &App, graph_area: Rect) -> bool {
    if app.main_tab != MainTab::Work || app.explorer_view != ExplorerView::Graph {
        return false;
    }
    let minimum_width = (AGENT_CARD_WIDTH * 0.75).ceil() as u16 + 4;
    let minimum_height =
        ((AGENT_CARD_HEIGHT * 2.0 + CONTROL_LANE_PADDING + 4.0) * 0.75).ceil() as u16 + 4;
    graph_area.width < minimum_width || graph_area.height < minimum_height
}

fn render_header(frame: &mut Frame, area: Rect, app: &mut App) {
    let agent_count = visible_agent_count(&app.state);
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
    let pending_gates = app
        .state
        .runs
        .iter()
        .map(|run| run.pending_gates.len())
        .sum::<usize>();
    let compact = area.width < 100;
    let syncing = app.refresh_inflight || app.enrichment_inflight;
    let status = if syncing {
        if compact {
            spinner_glyph().to_string()
        } else {
            format!("{} syncing", spinner_glyph())
        }
    } else if compact {
        format!("{agent_count}a · {}r", app.state.runs.len())
    } else {
        let run_count = app.state.runs.len();
        format!(
            "{} agent{} · {run_count} run{}",
            agent_count,
            if agent_count == 1 { "" } else { "s" },
            if run_count == 1 { "" } else { "s" }
        )
    };
    let status = if stalled > 0 {
        if compact {
            format!("{stalled}! · {status}")
        } else {
            format!("{stalled} stalled · {active} active · {status}")
        }
    } else if active > 0 {
        if compact {
            format!("{active}● · {status}")
        } else {
            format!("{active} active · {status}")
        }
    } else {
        status
    };
    let status = if pending_gates > 0 {
        if compact {
            format!("{pending_gates}? · {status}")
        } else {
            format!(
                "{pending_gates} gate{} · {status}",
                if pending_gates == 1 { "" } else { "s" }
            )
        }
    } else {
        status
    };
    let status_width = status.width().min(area.width.saturating_sub(1) as usize) as u16;
    let [title_area, status_area] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(status_width.saturating_add(1)),
    ])
    .areas(Rect::new(area.x, area.y, area.width, 1));
    let workspace = app
        .scope
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("workspace");
    let mut prefix = "⚔ orc".to_owned();
    if area.width >= 64 && workspace != "orc" {
        prefix.push_str(&format!("  {workspace}  {}", app.preferences.autonomy));
    } else if area.width >= 64 {
        prefix.push_str(&format!("  {}", app.preferences.autonomy));
    }
    prefix.push_str("  ");
    let tree = if app.main_tab == MainTab::Work && app.explorer_view == ExplorerView::Tree {
        "[tree]"
    } else {
        "tree"
    };
    let graph = if app.main_tab == MainTab::Work && app.explorer_view == ExplorerView::Graph {
        "[graph]"
    } else {
        "graph"
    };
    let integrations = if app.main_tab == MainTab::Integrations {
        "[integrations]"
    } else {
        "integrations"
    };
    let spans = vec![
        Span::styled(prefix.clone(), title()),
        Span::styled(tree, accent()),
        Span::raw(" "),
        Span::styled(graph, accent()),
        Span::raw(" "),
        Span::styled(integrations, accent()),
    ];
    frame.render_widget(Paragraph::new(Line::from(spans)), title_area);
    frame.render_widget(
        Paragraph::new(format!(" {status}"))
            .style(if pending_gates > 0 || stalled > 0 {
                Style::default().fg(Color::Yellow)
            } else if active > 0 {
                live()
            } else {
                dim()
            })
            .alignment(Alignment::Right),
        status_area,
    );
    let tab_y = title_area.y;
    let mut tab_x = title_area.x.saturating_add(prefix.width() as u16);
    let mut tab_area = |label: &str| {
        let width = label.width() as u16;
        let rect = if tab_x.saturating_add(width) <= title_area.right() {
            Rect::new(tab_x, tab_y, width, 1)
        } else {
            Rect::default()
        };
        tab_x = tab_x.saturating_add(width + 1);
        rect
    };
    app.hit.tree_tab = tab_area(tree);
    app.hit.graph_tab = tab_area(graph);
    app.hit.integrations_tab = tab_area(integrations);
}

fn visible_agent_count(state: &WorkspaceState) -> usize {
    state
        .sessions
        .iter()
        .filter(|session| session.status != LifecycleStatus::Archived)
        .count()
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
    let body = match app.output_tab {
        OutputTab::Summary => details(app),
        OutputTab::Timeline => selected_timeline(app),
        OutputTab::Result => selected_result(app),
        OutputTab::Changes => {
            if app.changes_loading {
                "Scanning workspace changes…".into()
            } else if app.changes_loaded_at.is_none() {
                "No changes integration is available.".into()
            } else if app.changes.is_empty() {
                "Workspace is clean.".into()
            } else {
                app.changes.clone()
            }
        }
    };
    let body = bounded_inspector_body(&body);
    let body = if app.output_tab == OutputTab::Summary {
        styled_details(&body)
    } else {
        body.clone().into_text().unwrap_or_else(|_| body.into())
    };
    let provisional_inner = Block::default().borders(Borders::ALL).inner(area);
    let width = provisional_inner.width.max(1) as usize;
    let rendered_lines = body
        .lines
        .iter()
        .map(|line| line.width().max(1).div_ceil(width))
        .sum::<usize>();
    let max_scroll = rendered_lines.saturating_sub(provisional_inner.height as usize) as u16;
    app.inspector_scroll = app.inspector_scroll.min(max_scroll);
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border)
        .title(title);
    if max_scroll > 0 {
        block = block.title_bottom(
            Line::from(format!(
                " j/k scroll · {}/{} ",
                app.inspector_scroll + 1,
                max_scroll + 1
            ))
            .style(dim())
            .alignment(Alignment::Right),
        );
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(body)
            .scroll((app.inspector_scroll, 0))
            .wrap(Wrap { trim: false }),
        inner,
    );
}

const MAX_INSPECTOR_BYTES: usize = 128 * 1024;
const MAX_INSPECTOR_LINES: usize = 2_000;

fn bounded_inspector_body(body: &str) -> String {
    let line_start = body
        .match_indices('\n')
        .rev()
        .nth(MAX_INSPECTOR_LINES.saturating_sub(1))
        .map_or(0, |(at, _)| at + 1);
    let byte_floor = body.len().saturating_sub(MAX_INSPECTOR_BYTES);
    let mut start = line_start.max(byte_floor);
    if start == byte_floor
        && start > line_start
        && let Some(next_line) = body[start..].find('\n')
    {
        start += next_line + 1;
    }
    while start < body.len() && !body.is_char_boundary(start) {
        start += 1;
    }
    if start == 0 {
        body.to_owned()
    } else {
        format!("… earlier activity omitted\n{}", &body[start..])
    }
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
        Some(ItemRef::Provider(name)) => {
            let report = selected_provider_report(app);
            if app.provider_validation_loading.contains(&name) {
                if report.is_empty() {
                    format!("{} Validating {name}…", spinner_glyph())
                } else {
                    format!(
                        "{} Validating {name}…\n\nLast result\n{report}",
                        spinner_glyph()
                    )
                }
            } else if report.is_empty() {
                "Press v to validate this provider.".into()
            } else {
                report
            }
        }
        _ => selected_output(app),
    }
}

fn selected_timeline(app: &App) -> String {
    let report = selected_provider_report(app);
    if app.main_tab == MainTab::Integrations && !report.is_empty() {
        let calls = selected_log(app);
        return if calls.is_empty() {
            report
        } else {
            format!("{report}\n\nrecent calls\n{calls}")
        };
    }
    selected_log(app)
}

fn selected_provider_report(app: &App) -> String {
    let Some(ItemRef::Provider(name)) = app.selected() else {
        return String::new();
    };
    app.provider_reports.get(&name).cloned().unwrap_or_default()
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
                (true, true) if node.session_id.is_none() => {
                    "No agent is assigned to this stage.".into()
                }
                (true, true) => "No activity has been reported for this agent.".into(),
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
                    && let Ok(log) = workflow::read_log_tail(Path::new(path))
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
            .map_or_else(String::new, |session| session_details(app, session)),
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
            .and_then(|run| {
                run.nodes
                    .iter()
                    .find(|node| node.id == node_id)
                    .map(|node| node_details(run, node))
            })
            .unwrap_or_default(),
        Some(ItemRef::Provider(name)) => app
            .providers
            .iter()
            .find(|provider| provider.name == name)
            .map_or_else(String::new, provider_details),
        Some(ItemRef::History) => {
            "Completed workflow runs from archived orchestrators · newest first.".into()
        }
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

fn session_details(app: &App, session: &Session) -> String {
    let placement = session_placement(session);
    let activity = RuntimeActivity::for_session(session)
        .map(|runtime| runtime.label())
        .unwrap_or("inactive");
    let daemon_runtime = app
        .supervisor
        .as_ref()
        .map_or(app.config.lifecycle.runtime_timeout_seconds, |status| {
            status.runtime_timeout_seconds
        });
    let daemon_idle = app
        .supervisor
        .as_ref()
        .map_or(app.config.lifecycle.idle_timeout_seconds, |status| {
            status.idle_timeout_seconds
        });
    let runtime_lease = lease_label(session.runtime_timeout_seconds, daemon_runtime);
    let idle_lease = lease_label(session.idle_timeout_seconds, daemon_idle);
    let supervisor = if app.supervisor.is_some() {
        "running"
    } else {
        "stopped · leases are not enforced"
    };
    let open_readiness = attach_readiness(session, &app.providers).label();
    render_detail_template(
        "session",
        context! { session, placement, activity, runtime_lease, idle_lease, supervisor, open_readiness },
    )
}

fn lease_label(seconds: Option<u64>, default: u64) -> String {
    match seconds {
        Some(0) => "disabled".into(),
        Some(seconds) => format!("{seconds}s"),
        None if default == 0 => "disabled (default)".into(),
        None => format!("{default}s (default)"),
    }
}

fn run_details(run: &WorkflowRun) -> String {
    let phase = run_phase(run);
    render_detail_template("run", context! { run, phase })
}

fn node_details(run: &WorkflowRun, node: &WorkflowNode) -> String {
    let gates = run
        .pending_gates
        .iter()
        .filter(|gate| gate.before == node.id)
        .collect::<Vec<_>>();
    render_detail_template("node", context! { node, gates })
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
    let hints = footer_bindings(app)
        .into_iter()
        .map(|binding| format!("{} {}", binding.keys, binding.short))
        .collect::<Vec<_>>()
        .join("   ");
    let status = app
        .visible_status()
        .map_or_else(|| format!("{focus}   {hints}"), str::to_owned);
    frame.render_widget(Paragraph::new(status).style(dim()), area);
}

fn footer_bindings(app: &App) -> Vec<&'static Binding> {
    BINDINGS
        .iter()
        .filter(|binding| binding.footer && (binding.available)(app))
        .collect()
}

struct Binding {
    id: &'static str,
    keys: &'static str,
    short: &'static str,
    description: &'static str,
    footer: bool,
    available: fn(&App) -> bool,
}

fn anywhere(_: &App) -> bool {
    true
}

fn work_main(app: &App) -> bool {
    app.main_tab == MainTab::Work && app.focus == Focus::Main
}

fn work_tree_main(app: &App) -> bool {
    work_main(app) && app.explorer_view == ExplorerView::Tree
}

fn work_graph_main(app: &App) -> bool {
    work_main(app) && app.explorer_view == ExplorerView::Graph
}

fn integrations_main(app: &App) -> bool {
    app.main_tab == MainTab::Integrations && app.focus == Focus::Main
}

fn provider_available(app: &App) -> bool {
    integrations_main(app) && app.providers.get(app.provider_at).is_some()
}

fn inspector(app: &App) -> bool {
    app.focus == Focus::Inspector && app.dock != Dock::Hidden
}

fn inspector_available(app: &App) -> bool {
    app.focus == Focus::Main && app.hit.inspector.is_some()
}

fn open_available(app: &App) -> bool {
    work_main(app)
        && (app.selected_session().is_some_and(|session| {
            attach_readiness(session, &app.providers) != AttachReadiness::Unavailable
        }) || launch_attach_ready(app))
}

fn launch_attach_ready(app: &App) -> bool {
    let Some((run_id, node_id)) = app.selected_unassigned_stage() else {
        return false;
    };
    app.launch_ready.contains(&(run_id, node_id))
}

fn drill_available(app: &App) -> bool {
    work_tree_main(app) && matches!(app.selected(), Some(ItemRef::Run(_)))
}

fn node_available(app: &App) -> bool {
    work_main(app) && matches!(app.selected(), Some(ItemRef::Node(_, _)))
}

fn gate_available(app: &App) -> bool {
    work_main(app)
        && app.selected_run_id().is_some_and(|run_id| {
            app.state
                .runs
                .iter()
                .find(|run| run.id == run_id)
                .is_some_and(|run| !run.pending_gates.is_empty())
        })
}

fn cancel_available(app: &App) -> bool {
    work_main(app) && (app.selected_run_id().is_some() || app.selected_session().is_some())
}

fn session_available(app: &App) -> bool {
    app.selected_session().is_some()
}

macro_rules! binding {
    ($id:literal, $keys:literal, $short:literal, $description:literal, $footer:literal, $available:ident) => {
        Binding {
            id: $id,
            keys: $keys,
            short: $short,
            description: $description,
            footer: $footer,
            available: $available,
        }
    };
}

const BINDINGS: &[Binding] = &[
    binding!(
        "line",
        "h/j/k/l",
        "navigate",
        "move in the focused pane",
        true,
        anywhere
    ),
    binding!(
        "page",
        "ctrl+d/u",
        "page",
        "half-page the focused pane",
        true,
        inspector
    ),
    binding!(
        "view",
        "tab",
        "tree/graph",
        "toggle the work tree and workflow graph",
        true,
        work_main
    ),
    binding!(
        "inspect-tabs",
        "tab/shift-tab",
        "inspector tab",
        "change inspector tab",
        true,
        inspector
    ),
    binding!(
        "open",
        "enter",
        "open",
        "attach a live session or launch an active unassigned stage",
        true,
        open_available
    ),
    binding!(
        "provider-validate",
        "enter/v",
        "validate",
        "validate the selected integration",
        true,
        provider_available
    ),
    binding!(
        "display-direction",
        "d",
        "direction",
        "cycle the display direction used to open or launch agents",
        true,
        open_available
    ),
    binding!(
        "drill",
        "g",
        "graph",
        "open the selected run's workflow graph",
        true,
        drill_available
    ),
    binding!(
        "integrations",
        "p",
        "integrations",
        "inspect provider integrations",
        true,
        work_main
    ),
    binding!(
        "return-work",
        "esc",
        "work",
        "return from integrations to work",
        true,
        integrations_main
    ),
    binding!(
        "focus-inspector",
        "ctrl+j/l",
        "inspector",
        "focus the inspector",
        true,
        inspector_available
    ),
    binding!(
        "focus-main",
        "ctrl+k/h",
        "main",
        "focus the main pane",
        true,
        inspector
    ),
    binding!(
        "edit-node",
        "e",
        "edit step",
        "edit the selected workflow step",
        false,
        node_available
    ),
    binding!(
        "delete-node",
        "D",
        "delete step",
        "delete the selected step after confirmation",
        false,
        node_available
    ),
    binding!(
        "mode",
        "m",
        "autonomy",
        "cycle supervised, approval-gated, and autonomous modes",
        false,
        work_main
    ),
    binding!(
        "reduced-motion",
        "M",
        "motion",
        "toggle reduced motion for this workspace",
        false,
        anywhere
    ),
    binding!(
        "gate",
        "a",
        "gate",
        "answer a pending human gate for the selected run",
        true,
        gate_available
    ),
    binding!(
        "cancel",
        "x",
        "stop",
        "stop the selected run after confirmation",
        false,
        cancel_available
    ),
    binding!(
        "viewport",
        "+/-/o",
        "viewport",
        "zoom, reset, or fit the graph",
        true,
        work_graph_main
    ),
    binding!(
        "resize",
        "+/-",
        "resize",
        "resize the inspector",
        false,
        inspector
    ),
    binding!(
        "activity",
        "i",
        "activity",
        "load session activity",
        false,
        session_available
    ),
    binding!(
        "changes",
        "c",
        "changes",
        "load workspace changes",
        false,
        work_main
    ),
    binding!(
        "relayout",
        "R",
        "relayout",
        "tidy and fit the graph",
        false,
        work_graph_main
    ),
    binding!(
        "refresh",
        "r",
        "refresh",
        "refresh state and integrations",
        false,
        anywhere
    ),
    binding!(
        "dock",
        "space i h/j/k/l",
        "dock",
        "move or hide the inspector",
        false,
        anywhere
    ),
    binding!(
        "mouse",
        "click/wheel",
        "select/scroll",
        "select panes and rows, choose tabs, pan the graph, or scroll",
        false,
        anywhere
    ),
    binding!("help", "?", "help", "show every key", true, anywhere),
    binding!("quit", "q/ctrl+c", "quit", "leave Orc", false, anywhere),
];

fn binding_enabled(app: &App, id: &str) -> bool {
    BINDINGS
        .iter()
        .find(|binding| binding.id == id)
        .is_some_and(|binding| (binding.available)(app))
}

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
        LifecycleStatus::Pending => '·',
        LifecycleStatus::Working => '●',
        LifecycleStatus::Terminating => '◌',
        LifecycleStatus::Done => '✓',
        LifecycleStatus::Failed => '×',
        LifecycleStatus::Blocked => '!',
        LifecycleStatus::Waiting | LifecycleStatus::Queued => '○',
        LifecycleStatus::Skipped
        | LifecycleStatus::Archived
        | LifecycleStatus::Disconnected
        | LifecycleStatus::Cancelled => '·',
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
    let loaded_animation = animation::load(&config, None)?;
    daemon::ensure_running(&config)?;
    let (_guard, mut terminal) = TerminalGuard::enter()?;
    let (tx, rx): (Sender<BackgroundResult>, Receiver<BackgroundResult>) = mpsc::channel();
    let mut app = App::loading(config, scope, loaded_animation);
    app.request_refresh(&tx);
    let mut last_tick = Instant::now();
    let mut last_draw = Instant::now();
    let mut dirty = true;
    let mut status_visible = false;
    loop {
        while let Ok(result) = rx.try_recv() {
            app.apply_background(result);
            dirty = true;
        }
        let request_state = (
            app.refresh_inflight,
            app.enrichment_inflight,
            app.provider_refresh_inflight,
            app.activity_loading.len(),
            app.provider_activity_loading.len(),
            app.changes_loading,
        );
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
            if app.changes_view_is_open() {
                app.request_changes(&tx, false);
            }
        }
        let current_request_state = (
            app.refresh_inflight,
            app.enrichment_inflight,
            app.provider_refresh_inflight,
            app.activity_loading.len(),
            app.provider_activity_loading.len(),
            app.changes_loading,
        );
        dirty |= request_state != current_request_state;
        if app
            .resize_at
            .is_some_and(|at| at.elapsed() >= Duration::from_millis(120))
        {
            clamp_flow_viewport(&mut app.flow);
            app.resize_at = None;
            dirty = true;
        }
        let animate = app.needs_animation();
        if animate && last_draw.elapsed() >= Duration::from_millis(90) {
            let elapsed = last_tick.elapsed();
            last_tick = Instant::now();
            app.flow.tick_animation(elapsed);
            let _ = app.flow.tick_auto_pan(elapsed);
            dirty = true;
        } else if !animate {
            last_tick = Instant::now();
        }
        let currently_visible = app.visible_status().is_some();
        dirty |= status_visible != currently_visible;
        if dirty {
            terminal.draw(|frame| render(frame, &mut app))?;
            last_draw = Instant::now();
            status_visible = app.visible_status().is_some();
            dirty = false;
        }
        let mut quit = false;
        let poll_for = if animate {
            Duration::from_millis(50)
        } else {
            Duration::from_millis(250)
        };
        if event::poll(poll_for)? {
            let mut processed = 0;
            loop {
                processed += 1;
                match event::read()? {
                    Event::Key(key) if app.handle_key(key, &tx) => quit = true,
                    Event::Mouse(mouse) => app.handle_mouse(mouse),
                    Event::Resize(_, _) => {
                        app.flow.request_fit_view_with_options(
                            FitViewOptions::default().with_padding(3.0),
                        );
                        app.resize_at = Some(Instant::now());
                    }
                    _ => {}
                }
                dirty = true;
                if quit || processed >= 128 || !event::poll(Duration::ZERO)? {
                    break;
                }
            }
        }
        if quit {
            app.persist_preferences();
            break;
        }
    }
    Ok(())
}

pub fn preview_loading(config: &Config, scope: &Path) -> Result<()> {
    let loaded_animation = animation::load(config, None)?;
    let scope = crate::state::resolve_scope(scope)?;
    let preferences = preferences::read(&scope).unwrap_or_default();
    let reduced_motion = preferences
        .reduced_motion
        .unwrap_or(config.ui.reduced_motion);
    let (_guard, mut terminal) = TerminalGuard::enter()?;
    let started_at = Instant::now();
    loop {
        terminal.draw(|frame| {
            render_loading(
                frame,
                frame.area(),
                started_at,
                &loaded_animation.config,
                reduced_motion,
                loaded_animation.warning.as_deref(),
            );
        })?;
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
            runtime_timeout_seconds: None,
            idle_timeout_seconds: None,
            heartbeat_at: Some(Utc::now()),
            termination_reason: None,
            termination_cause: None,
            termination_attempt_at: None,
            termination_operation_id: None,
            connected_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn app() -> App {
        let mut state = WorkspaceState::empty("/tmp/orc-test".into());
        state.sessions = vec![
            session("root", None, "agent-a"),
            session("native-child", Some("root"), "agent-a"),
            session("harness-child", Some("root"), "agent-b"),
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
            parent_run_id: None,
            definition: None,
            revision: None,
            checkpoint: None,
            mode: crate::domain::RunMode::default(),
            process_id: None,
            execution_nonce: None,
            resume_requested: false,
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
            harness: "agent-a".into(),
            model: None,
            execution: Some("executor-a".into()),
            judge_policy: JudgePolicy::Llm,
            goal: format!("Complete {id}"),
            expected_output: format!("Verified {id}"),
            success_criteria: vec![format!("{id} passes")],
            completion: CompletionTarget::Orchestrator,
            review_by: None,
            session_id: None,
            child_run_id: None,
            status,
            attempt,
            retry_after: None,
            prompt: None,
            input: None,
            output: None,
            activity: Vec::new(),
            tokens: 0,
            cost_usd: 0.0,
            updated_at: Utc::now(),
        }
    }

    fn reviewed_workflow_run() -> WorkflowRun {
        let mut run = workflow_run();
        let mut implement = workflow_node("implement", LifecycleStatus::Done, 1);
        implement.review_by = Some("verify".into());
        let mut verify = workflow_node("verify", LifecycleStatus::Done, 1);
        verify.role = crate::domain::SessionRole::Verifier;
        verify.completion = CompletionTarget::Judge;
        run.nodes = vec![implement, verify];
        run.edges.push(crate::domain::WorkflowEdge {
            from: "implement".into(),
            to: "verify".into(),
            relationship: "reviewed_by".into(),
        });
        run
    }

    fn feedback_workflow_run() -> WorkflowRun {
        let mut run = reviewed_workflow_run();
        run.edges.push(crate::domain::WorkflowEdge {
            from: "verify".into(),
            to: "implement".into(),
            relationship: "feedback".into(),
        });
        run
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
        let ids = BINDINGS
            .iter()
            .map(|binding| binding.id)
            .collect::<BTreeSet<_>>();
        assert_eq!(ids.len(), BINDINGS.len());
    }

    #[test]
    fn footer_bindings_follow_view_focus_and_selection() {
        let mut app = app();
        let tree = footer_bindings(&app)
            .into_iter()
            .map(|binding| binding.id)
            .collect::<BTreeSet<_>>();
        assert!(tree.contains("view"));
        assert!(!tree.contains("open"));
        assert!(!tree.contains("drill"));
        assert!(!tree.contains("provider-validate"));

        app.providers.push(
            serde_yaml::from_str(
                "version: orc.provider/v1\nname: display\nkind: display\ncommand: \"true\"\nactions:\n  terminal.focus: Focus an existing display\n",
            )
            .expect("display provider manifest"),
        );
        app.state.sessions[0]
            .providers
            .push(crate::domain::ProviderBinding {
                provider: "display".into(),
                kind: ProviderKind::Display,
                r#ref: Some("pane:1".into()),
                status: BindingStatus::Active,
                label: "ready".into(),
            });
        app.rebuild(true);
        let ready = footer_bindings(&app)
            .into_iter()
            .map(|binding| binding.id)
            .collect::<BTreeSet<_>>();
        assert!(ready.contains("open"));

        app.explorer_view = ExplorerView::Graph;
        app.rebuild(true);
        let graph = footer_bindings(&app)
            .into_iter()
            .map(|binding| binding.id)
            .collect::<BTreeSet<_>>();
        assert!(graph.contains("viewport"));
        assert!(!graph.contains("drill"));

        app.providers.push(
            serde_yaml::from_str(
                "version: orc.provider/v1\nname: provider\ncommand: \"true\"\nactions:\n  session.bind: inspect\n",
            )
            .expect("provider manifest"),
        );
        app.switch_main_tab(MainTab::Integrations);
        let integrations = footer_bindings(&app)
            .into_iter()
            .map(|binding| binding.id)
            .collect::<BTreeSet<_>>();
        assert!(integrations.contains("provider-validate"));
        assert!(integrations.contains("return-work"));
        assert!(!integrations.contains("view"));
        assert!(!integrations.contains("display-direction"));
        let (tx, _rx) = mpsc::channel();
        app.handle_key(key(KeyCode::Tab, KeyModifiers::NONE), &tx);
        assert_eq!(app.main_tab, MainTab::Integrations);
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
                .contains("researcher · external · agent-a")
        );
        assert!(
            app.tree[2]
                .subtitle
                .contains("researcher · external · agent-b")
        );
    }

    #[test]
    fn detail_templates_render_typed_domain_data() {
        let mut app = app();
        app.supervisor = None;
        let rendered = session_details(&app, &app.state.sessions[0]);
        assert!(rendered.contains("orchestrator · external · agent-a"));
        assert!(rendered.contains("activity      active"));
        assert!(rendered.contains("open          open unavailable"));
        assert!(!rendered.contains("integrations"));
        assert!(rendered.contains("expected output Verified result"));
        assert!(rendered.contains("success criteria\n  - It passes"));
        assert!(rendered.contains("runtime lease 28800s (default)"));
        assert!(rendered.contains("supervisor    stopped · leases are not enforced"));
    }

    #[test]
    fn session_details_use_the_running_supervisors_default_leases() {
        let mut app = app();
        app.supervisor = Some(daemon::Status {
            pid: 1,
            token: "test".into(),
            started_at: Utc::now(),
            last_sweep_at: Some(Utc::now()),
            runtime_timeout_seconds: 600,
            idle_timeout_seconds: 60,
            executable_path: "/tmp/orc".into(),
            executable_identity: "test".into(),
            executable_version: "test".into(),
            config_fingerprint: "test".into(),
        });

        let rendered = session_details(&app, &app.state.sessions[0]);

        assert!(rendered.contains("runtime lease 600s (default)"));
        assert!(rendered.contains("idle lease    60s (default)"));
        assert!(rendered.contains("supervisor    running"));
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
    fn mouse_selects_each_header_tab() {
        let mut app = app();
        let backend = TestBackend::new(140, 40);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| render(frame, &mut app))
            .expect("tree renders");

        let graph = app.hit.graph_tab;
        assert!(graph.width > 0);
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: graph.x,
            row: graph.y,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.explorer_view, ExplorerView::Graph);

        terminal
            .draw(|frame| render(frame, &mut app))
            .expect("graph renders");
        let integrations = app.hit.integrations_tab;
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: integrations.x,
            row: integrations.y,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.main_tab, MainTab::Integrations);

        terminal
            .draw(|frame| render(frame, &mut app))
            .expect("integrations render");
        let tree = app.hit.tree_tab;
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: tree.x,
            row: tree.y,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.main_tab, MainTab::Work);
        assert_eq!(app.explorer_view, ExplorerView::Tree);
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
            if (width, height) == (40, 14) {
                assert!(app.hit.inspector.is_none());
                assert_eq!(app.hit.main.height, height - 2);
            } else {
                assert!(app.hit.inspector.is_some_and(|area| area.height >= 8));
            }
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
    fn tab_opens_the_run_selected_in_the_tree() {
        let mut app = app();
        let mut first = workflow_run();
        first.id = "first".into();
        first.name = "First run".into();
        first
            .nodes
            .push(workflow_node("first-stage", LifecycleStatus::Queued, 0));
        let mut second = workflow_run();
        second.id = "second".into();
        second.name = "Second run".into();
        second
            .nodes
            .push(workflow_node("second-stage", LifecycleStatus::Queued, 0));
        app.state.runs = vec![first, second];
        app.rebuild(true);
        app.tree_at = app
            .tree
            .iter()
            .position(|row| row.id == "run:second")
            .expect("second run row");
        let (tx, _rx) = mpsc::channel();

        app.handle_key(key(KeyCode::Tab, KeyModifiers::NONE), &tx);

        assert_eq!(app.explorer_view, ExplorerView::Graph);
        assert_eq!(app.active_run.as_deref(), Some("second"));
        assert!(app.graph_items.contains_key("node:second:second-stage"));
        assert!(!app.graph_items.contains_key("node:first:first-stage"));
    }

    #[test]
    fn archived_orchestrator_is_not_attachable_from_a_run_root() {
        let mut app = app();
        app.state.sessions[0].status = LifecycleStatus::Archived;
        app.state.runs.push(workflow_run());
        app.active_run = Some("run".into());
        app.explorer_view = ExplorerView::Graph;
        app.rebuild(true);
        app.flow.select_node("run:run");

        assert!(matches!(app.selected(), Some(ItemRef::Run(id)) if id == "run"));
        assert!(app.selected_session().is_none());
    }

    #[test]
    fn graph_orchestrator_controls_the_displayed_run() {
        let mut app = app();
        let mut run = workflow_run();
        run.pending_gates.push(crate::domain::PendingGate {
            id: "ship".into(),
            before: "release".into(),
            reason: "approval required".into(),
            authority: crate::domain::GateAuthority::User,
            recommendation: None,
            created_at: Utc::now(),
        });
        app.state.runs.push(run);
        app.active_run = Some("run".into());
        app.explorer_view = ExplorerView::Graph;
        app.rebuild(true);
        app.flow.select_node("session:root");

        assert!(app.request_gate());
        assert!(matches!(
            app.confirmation,
            Some(Confirmation::Approve { ref run_id, .. }) if run_id == "run"
        ));

        app.confirmation = None;
        app.request_cancel();
        assert!(matches!(
            app.confirmation,
            Some(Confirmation::Cancel { ref run_id }) if run_id == "run"
        ));
    }

    #[test]
    fn tree_orchestrator_stops_its_run_before_pruning_the_session() {
        let mut app = app();
        app.state.runs.push(workflow_run());
        app.rebuild(true);
        app.tree_at = app
            .tree
            .iter()
            .position(|row| row.id == "session:root")
            .expect("orchestrator row");

        app.request_cancel();

        assert!(matches!(
            app.confirmation,
            Some(Confirmation::Cancel { ref run_id }) if run_id == "run"
        ));
    }

    #[test]
    fn changes_cache_does_not_expire_on_a_hidden_timer() {
        let mut app = app();
        app.providers.push(
            serde_yaml::from_str(
                "version: orc.provider/v1\nname: changes\ncommand: \"true\"\nactions:\n  changes.inspect: inspect\n",
            )
            .expect("provider manifest"),
        );
        app.changes_loaded_at = Some(Instant::now() - Duration::from_secs(60));

        assert!(!app.changes_need_loading(false));
        assert!(app.changes_need_loading(true));
    }

    #[test]
    fn changes_only_auto_load_for_the_visible_changes_inspector() {
        let mut app = app();
        app.output_tab = OutputTab::Changes;
        assert!(app.changes_view_is_open());

        app.main_tab = MainTab::Integrations;

        assert!(!app.changes_view_is_open());
    }

    #[test]
    fn provider_validation_is_scoped_to_the_selected_provider() {
        let mut app = app();
        app.providers = ["first", "second"]
            .into_iter()
            .map(|name| {
                serde_yaml::from_str::<Manifest>(&format!(
                    "version: orc.provider/v1\nname: {name}\ncommand: \"true\"\nactions:\n  session.bind: inspect\n"
                ))
                .expect("provider manifest")
            })
            .collect();
        app.main_tab = MainTab::Integrations;
        app.provider_reports
            .insert("first".into(), "first is healthy".into());
        app.provider_reports
            .insert("second".into(), "second is healthy".into());

        app.provider_at = 0;
        assert_eq!(selected_provider_report(&app), "first is healthy");
        app.provider_at = 1;
        assert_eq!(selected_provider_report(&app), "second is healthy");
    }

    #[test]
    fn repeated_provider_action_keys_do_not_start_work() {
        let mut app = app();
        app.providers.push(
            serde_yaml::from_str(
                "version: orc.provider/v1\nname: provider\ncommand: \"true\"\nactions:\n  session.bind: inspect\n",
            )
            .expect("provider manifest"),
        );
        app.main_tab = MainTab::Integrations;
        let (tx, rx) = mpsc::channel();
        let repeat = KeyEvent {
            code: KeyCode::Char('v'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Repeat,
            state: KeyEventState::NONE,
        };

        app.handle_key(repeat, &tx);

        assert!(app.provider_validation_loading.is_empty());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn provider_validation_opens_health_with_progress() {
        let mut app = app();
        app.providers.push(
            serde_yaml::from_str(
                "version: orc.provider/v1\nname: provider\ncommand: \"true\"\nactions:\n  session.bind: inspect\n",
            )
            .expect("provider manifest"),
        );
        app.main_tab = MainTab::Integrations;
        let (tx, _rx) = mpsc::channel();

        app.validate_provider(&tx);

        assert_eq!(app.output_tab, OutputTab::Result);
        assert_eq!(app.focus, Focus::Inspector);
        assert!(selected_result(&app).contains("Validating provider"));
    }

    #[test]
    fn graph_selection_and_viewport_restore_from_preferences() {
        let mut app = app();
        let mut run = workflow_run();
        run.nodes
            .push(workflow_node("implement", LifecycleStatus::Queued, 0));
        app.state.runs.push(run);
        app.active_run = Some("run".into());
        app.preferences.graph_selected_item = Some("node:run:implement".into());
        app.preferences.graph_pan_x = 17.0;
        app.preferences.graph_pan_y = -4.0;
        app.preferences.graph_zoom = 1.25;

        app.rebuild(true);

        assert_eq!(
            app.flow.first_selected_node_id().as_deref(),
            Some("node:run:implement")
        );
        let viewport = app.flow.to_snapshot().viewport;
        assert_eq!((viewport.x, viewport.y, viewport.zoom), (17.0, -4.0, 1.25));
    }

    #[test]
    fn display_direction_cycles_and_rejects_invalid_saved_values() {
        let mut app = app();
        app.preferences.display_direction = "diagonal".into();
        assert_eq!(app.display_direction(), "right");

        app.cycle_display_direction();

        assert_eq!(app.display_direction(), "bottom");
    }

    #[test]
    fn enter_launches_only_an_active_unassigned_stage() {
        let mut app = app();
        let mut run = workflow_run();
        run.nodes
            .push(workflow_node("implement", LifecycleStatus::Queued, 0));
        app.state.runs.push(run);
        app.active_run = Some("run".into());
        app.explorer_view = ExplorerView::Graph;
        app.rebuild(true);
        app.flow.select_node("node:run:implement");

        assert_eq!(
            app.selected_unassigned_stage(),
            Some(("run".into(), "implement".into()))
        );

        app.state.runs[0].nodes[0].status = LifecycleStatus::Done;
        assert_eq!(app.selected_unassigned_stage(), None);
    }

    #[test]
    fn launch_attach_waits_for_the_nodes_managed_session() {
        let mut app = app();
        let mut run = workflow_run();
        let mut node = workflow_node("implement", LifecycleStatus::Working, 1);
        node.session_id = Some("native-child".into());
        run.nodes.push(node);
        app.state.runs.push(run);

        let managed = App::managed_node_session(&app.state, "run", "implement")
            .expect("managed node session");
        assert_eq!(managed.id, "native-child");

        app.state.sessions[1].registration = RegistrationSource::Connected;
        assert!(App::managed_node_session(&app.state, "run", "implement").is_none());
        assert!(App::node_is_active(&app.state, "run", "implement"));
        app.state.runs[0].nodes[0].status = LifecycleStatus::Done;
        assert!(!App::node_is_active(&app.state, "run", "implement"));
    }

    #[test]
    fn enter_is_hidden_and_inert_without_a_complete_attach_chain() {
        let mut app = app();
        let mut run = workflow_run();
        run.nodes
            .push(workflow_node("implement", LifecycleStatus::Queued, 0));
        app.state.runs.push(run);
        app.active_run = Some("run".into());
        app.explorer_view = ExplorerView::Graph;
        app.rebuild(true);
        app.flow.select_node("node:run:implement");
        let (tx, rx) = mpsc::channel();

        assert!(!open_available(&app));
        app.open_selected(&tx);

        assert!(!app.action_inflight);
        assert!(app.status.contains("no ready persistence and display"));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn enter_stays_hidden_when_the_launch_provider_declines_the_selected_harness() {
        let mut app = app();
        let mut run = workflow_run();
        run.nodes
            .push(workflow_node("implement", LifecycleStatus::Queued, 0));
        app.state.runs.push(run);
        app.active_run = Some("run".into());
        app.explorer_view = ExplorerView::Graph;
        app.providers = [
            r#"version: orc.provider/v1
name: harness
kind: harness
command:
  - /bin/sh
  - -c
  - |
      cat >/dev/null
      printf '%s\n' '{"version":"orc.provider/v1","status":"declined","reason":"unknown harness"}'
actions:
  session.launch: Launch a harness
"#,
            "version: orc.provider/v1\nname: persistence\nkind: persistence\ncommand: \"true\"\nactions:\n  session.persist: Preserve a session\n  session.stop: Stop a session\n",
            "version: orc.provider/v1\nname: executor-a\nkind: execution\ncommand: \"true\"\nactions:\n  execution.run: Execute the plan\n",
            "version: orc.provider/v1\nname: display\nkind: display\ncommand: \"true\"\nactions:\n  terminal.open: Open a display\n",
        ]
        .into_iter()
        .map(|manifest| serde_yaml::from_str(manifest).expect("provider manifest"))
        .collect();
        app.config.cache.provider_ttl_ms = 0;
        app.launch_ready = launch_ready_nodes(
            &app.config,
            &app.providers,
            &app.scope,
            &app.state,
            app.display_direction(),
        );
        app.rebuild(true);
        app.flow.select_node("node:run:implement");

        assert!(provider::launch_attach_route_available(
            &app.providers,
            Some("executor-a")
        ));
        assert!(!open_available(&app));
    }

    #[test]
    fn enter_is_advertised_for_an_unassigned_stage_with_a_complete_attach_chain() {
        let mut app = app();
        let mut run = workflow_run();
        run.nodes
            .push(workflow_node("implement", LifecycleStatus::Queued, 0));
        app.state.runs.push(run);
        app.active_run = Some("run".into());
        app.explorer_view = ExplorerView::Graph;
        app.providers = [
            "version: orc.provider/v1\nname: harness\nkind: harness\ncommand: \"true\"\nactions:\n  session.launch: Launch a harness\n",
            "version: orc.provider/v1\nname: persistence\nkind: persistence\ncommand: \"true\"\nactions:\n  session.persist: Preserve a session\n  session.stop: Stop a session\n",
            "version: orc.provider/v1\nname: executor-a\nkind: execution\ncommand: \"true\"\nactions:\n  execution.run: Execute the plan\n",
            "version: orc.provider/v1\nname: display\nkind: display\ncommand: \"true\"\nactions:\n  terminal.open: Open a display\n",
        ]
        .into_iter()
        .map(|manifest| serde_yaml::from_str(manifest).expect("provider manifest"))
        .collect();
        app.rebuild(true);
        app.flow.select_node("node:run:implement");
        app.launch_ready.insert(("run".into(), "implement".into()));

        assert!(open_available(&app));

        app.launch_ready.clear();
        assert!(!open_available(&app));
    }

    #[test]
    fn assigned_session_is_nested_once_below_its_stage() {
        let mut app = app();
        let mut run = workflow_run();
        let mut node = workflow_node("implement", LifecycleStatus::Working, 1);
        node.session_id = Some("native-child".into());
        run.nodes.push(node);
        app.state.runs.push(run);
        app.expanded.extend([
            "session:root".into(),
            "run:run".into(),
            "session:native-child".into(),
        ]);
        app.rebuild(true);

        assert!(app.tree.iter().all(|row| row.id != "session:native-child"));

        app.expanded.insert("node:run:implement".into());
        app.rebuild(true);

        let assigned = app
            .tree
            .iter()
            .filter(|row| row.id == "session:native-child")
            .collect::<Vec<_>>();
        assert_eq!(assigned.len(), 1);
        assert_eq!(assigned[0].depth, 3);
        let stage = app
            .tree
            .iter()
            .position(|row| row.id == "node:run:implement")
            .expect("stage row");
        assert_eq!(app.tree[stage + 1].id, "session:native-child");
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
        assert_eq!(status_glyph(LifecycleStatus::Pending), '·');
        assert_eq!(status_glyph(LifecycleStatus::Skipped), '·');
        assert_eq!(
            RuntimeActivity::for_session(&app().state.sessions[0]),
            Some(RuntimeActivity::Active)
        );
    }

    #[test]
    fn runtime_activity_uses_heartbeat_instead_of_metadata_updates() {
        let mut session = session("heartbeat", None, "agent-a");
        session.updated_at = Utc::now();
        session.heartbeat_at = Some(Utc::now() - chrono::Duration::minutes(10));
        assert_eq!(
            RuntimeActivity::for_session(&session),
            Some(RuntimeActivity::Stalled)
        );

        session.updated_at = Utc::now() - chrono::Duration::minutes(10);
        session.heartbeat_at = Some(Utc::now());
        assert_eq!(
            RuntimeActivity::for_session(&session),
            Some(RuntimeActivity::Active)
        );
    }

    #[test]
    fn archived_sessions_are_not_counted_as_visible_agents() {
        let mut state = app().state;
        let mut archived = session("archived", None, "agent-a");
        archived.status = LifecycleStatus::Archived;
        state.sessions.push(archived);

        assert_eq!(visible_agent_count(&state), 3);
        assert_eq!(tree_rows(&state, &default_expansions(&state), &[]).len(), 3);
        let (_, graph_items) = build_flow(&state, None);
        assert_eq!(
            graph_items
                .values()
                .filter(|item| matches!(item, ItemRef::Session(_)))
                .count(),
            3
        );

        state.sessions[0].status = LifecycleStatus::Archived;
        let mut run = workflow_run();
        run.orchestrator_id = Some("root".into());
        state.runs.push(run);
        let (_, graph_items) = build_flow(&state, Some("run"));
        assert!(
            graph_items
                .values()
                .all(|item| !matches!(item, ItemRef::Session(id) if id == "root"))
        );
        assert!(
            graph_items
                .values()
                .any(|item| matches!(item, ItemRef::Run(id) if id == "run"))
        );
    }

    #[test]
    fn inspector_body_keeps_a_bounded_utf8_tail() {
        let body = (0..2_500)
            .map(|line| format!("line {line} — activity"))
            .collect::<Vec<_>>()
            .join("\n");

        let bounded = bounded_inspector_body(&body);

        assert!(bounded.starts_with("… earlier activity omitted\n"));
        assert!(bounded.contains("line 2499 — activity"));
        assert!(!bounded.contains("line 0 — activity"));
        assert!(bounded.len() <= MAX_INSPECTOR_BYTES + 64);
        assert!(bounded.lines().count() <= MAX_INSPECTOR_LINES + 1);
    }

    #[test]
    fn startup_refresh_expands_active_work() {
        let mut state = app().state;
        state.runs.push(workflow_run());
        let mut app = App::loading(
            Config::default(),
            PathBuf::from("/tmp/orc-test"),
            animation::Loaded {
                config: animation::fallback(),
                source: animation::Source::Packaged,
                warning: None,
            },
        );
        app.apply_background(BackgroundResult::Refresh(Ok((state, None, None))));

        assert_eq!(app.active_run.as_deref(), Some("run"));
        assert_eq!(app.tree[0].title, "root");
        assert_eq!(app.tree[1].title, "Provider migration");
        assert!(app.enrichment_requested);
        assert!(!app.enrichment_inflight);
    }

    #[test]
    fn newest_active_run_is_selected_on_startup() {
        let mut older = workflow_run();
        older.id = "older".into();
        older.updated_at = Utc::now() - chrono::Duration::minutes(1);
        let mut newer = workflow_run();
        newer.id = "newer".into();
        newer.updated_at = Utc::now();
        let mut state = app().state;
        state.runs = vec![older, newer];

        let app = App::new(
            Config::default(),
            PathBuf::from("/tmp/orc-test"),
            state,
            Vec::new(),
        );

        assert_eq!(app.active_run.as_deref(), Some("newer"));
    }

    #[test]
    fn archived_orchestrator_runs_stay_in_collapsed_recent_history() {
        let mut state = app().state;
        state.sessions[0].status = LifecycleStatus::Archived;
        let mut older = workflow_run();
        older.id = "older".into();
        older.name = "Older completed run".into();
        older.status = LifecycleStatus::Done;
        older.updated_at = Utc::now() - chrono::Duration::minutes(2);
        let mut newer = workflow_run();
        newer.id = "newer".into();
        newer.name = "Newer completed run".into();
        newer.status = LifecycleStatus::Done;
        newer.updated_at = Utc::now() - chrono::Duration::minutes(1);
        state.runs = vec![older, newer];

        let expanded = default_expansions(&state);
        let collapsed = tree_rows(&state, &expanded, &[]);
        let history = collapsed
            .iter()
            .find(|row| row.item == ItemRef::History)
            .expect("recent history branch");
        assert_eq!(history.subtitle, "2 completed runs · newest first");
        assert!(collapsed.iter().all(|row| {
            !matches!(&row.item, ItemRef::Run(id) if id == "older" || id == "newer")
        }));

        let mut expanded = expanded;
        expanded.insert("history".into());
        let visible = tree_rows(&state, &expanded, &[]);
        let runs = visible
            .iter()
            .filter_map(|row| match &row.item {
                ItemRef::Run(id) => Some(id.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(runs, ["newer", "older"]);
    }

    #[test]
    fn active_run_with_an_archived_orchestrator_remains_visible() {
        let mut state = app().state;
        state.sessions[0].status = LifecycleStatus::Archived;
        let mut run = workflow_run();
        run.status = LifecycleStatus::Working;
        state.runs = vec![run];

        let rows = tree_rows(&state, &default_expansions(&state), &[]);
        let visible = rows
            .iter()
            .filter(|row| matches!(&row.item, ItemRef::Run(id) if id == "run"))
            .collect::<Vec<_>>();

        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].depth, 0);
        assert!(visible[0].subtitle.contains("orchestrator unavailable"));
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

        app.apply_background(BackgroundResult::Refresh(Ok((state, None, None))));

        assert_eq!(app.active_run.as_deref(), Some("run"));
        assert!(
            app.graph_items
                .values()
                .any(|item| { matches!(item, ItemRef::Session(id) if id == "root") })
        );
    }

    #[test]
    fn saved_active_run_restores_a_completed_graph() {
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
        let preferences = WorkspacePreferences {
            view: "graph".into(),
            active_run: Some("run".into()),
            ..WorkspacePreferences::default()
        };

        app.apply_preferences(preferences);
        app.apply_background(BackgroundResult::Refresh(Ok((state, None, None))));

        assert_eq!(app.active_run.as_deref(), Some("run"));
        assert_eq!(app.explorer_view, ExplorerView::Graph);
        assert!(app.graph_items.contains_key("session:root"));
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
            .insert(0, session("another-root", None, "agent-a"));
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
    fn graph_cards_surface_judges_and_pending_human_gates() {
        let mut state = app().state;
        let mut run = workflow_run();
        let mut node = workflow_node("implement", LifecycleStatus::Waiting, 1);
        node.judge_policy = JudgePolicy::LlmAndHuman;
        run.nodes.push(node);
        run.pending_gates.push(crate::domain::PendingGate {
            id: "approve".into(),
            before: "implement".into(),
            reason: "Review the migration boundary".into(),
            authority: crate::domain::GateAuthority::User,
            recommendation: None,
            created_at: Utc::now(),
        });
        state.runs.push(run);

        let mut app = app();
        app.state = state.clone();
        assert!(app.needs_animation());

        let (flow, _) = build_flow(&state, Some("run"));
        let root = flow
            .nodes()
            .find(|node| node.id == "session:root")
            .expect("orchestrator card");
        let stage = flow
            .nodes()
            .find(|node| node.id == "node:run:implement")
            .expect("stage card");

        assert_eq!(root.content.title, "Provider migration");
        assert_eq!(
            root.content.attention.as_deref(),
            Some("! 1 human gate waiting")
        );
        assert!(stage.content.contract.contains("judge llm+human"));
        assert!(
            stage
                .content
                .attention
                .as_deref()
                .is_some_and(|attention| attention.contains("Review the migration boundary"))
        );
        let details = node_details(&state.runs[0], &state.runs[0].nodes[0]);
        assert!(details.find("execution").unwrap() < details.find("purpose").unwrap());
        assert!(details.contains("needs attention\n  - Review the migration boundary"));
    }

    #[test]
    fn header_and_inspector_surface_gate_attention_and_scroll_state() {
        let mut app = app();
        let mut run = workflow_run();
        let mut node = workflow_node("implement", LifecycleStatus::Waiting, 1);
        node.success_criteria = (0..24).map(|index| format!("criterion {index}")).collect();
        run.nodes.push(node);
        run.pending_gates.push(crate::domain::PendingGate {
            id: "approve".into(),
            before: "implement".into(),
            reason: "Review the migration boundary".into(),
            authority: crate::domain::GateAuthority::User,
            recommendation: None,
            created_at: Utc::now(),
        });
        app.state.runs.push(run);
        app.active_run = Some("run".into());
        app.explorer_view = ExplorerView::Graph;
        app.rebuild(true);
        app.flow.select_node("node:run:implement");
        app.focus = Focus::Inspector;

        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| render(frame, &mut app))
            .expect("attention view renders");
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(rendered.contains("1 gate"));
        assert!(rendered.contains("j/k scroll"));
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
            rebind_current: true,
            result: Ok(stale),
        });
        assert_eq!(app.state.sessions[0].title, "root");
        assert!(!app.enrichment_inflight);
        assert!(app.rebind_current_pending);
        assert!(app.enrichment_due_at > Instant::now());

        app.enrichment_inflight = true;
        app.apply_background(BackgroundResult::Enrichment {
            generation: 2,
            rebind_current: false,
            result: Err("provider unavailable".into()),
        });
        assert!(app.status.contains("provider unavailable"));
        assert!(app.enrichment_due_at > Instant::now());
    }

    #[test]
    fn state_refresh_is_not_blocked_by_provider_work() {
        let mut app = app();
        app.enrichment_inflight = true;
        app.provider_refresh_inflight = true;
        let (tx, _rx) = mpsc::channel();

        app.request_refresh(&tx);

        assert!(app.refresh_inflight);
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
    fn graph_marks_every_parallel_working_node_active() {
        let mut state = app().state;
        let mut run = workflow_run();
        let mut research = workflow_node("research", LifecycleStatus::Working, 1);
        research.session_id = Some("native-child".into());
        let mut implement = workflow_node("implement", LifecycleStatus::Working, 1);
        implement.session_id = Some("harness-child".into());
        run.nodes.push(research);
        run.nodes.push(implement);
        run.current_node = Some("research".into());
        run.status = LifecycleStatus::Working;
        state.runs.push(run);

        let (flow, _) = build_flow(&state, Some("run"));
        for id in ["node:run:research", "node:run:implement"] {
            let node = flow
                .nodes()
                .find(|node| node.id == id)
                .unwrap_or_else(|| panic!("missing {id}"));
            assert!(node.content.active, "{id} is not rendered active");
        }
    }

    #[test]
    fn graph_does_not_animate_a_working_stage_without_an_agent() {
        let mut state = app().state;
        let mut run = workflow_run();
        run.nodes
            .push(workflow_node("research", LifecycleStatus::Working, 1));
        run.status = LifecycleStatus::Working;
        state.runs.push(run);

        let (flow, _) = build_flow(&state, Some("run"));
        let node = flow
            .nodes()
            .find(|node| node.id == "node:run:research")
            .expect("research node");
        assert!(!node.content.active);
        assert!(node.content.subtitle.contains("no agent assigned"));
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
    fn startup_surfaces_an_implicit_animation_fallback_warning() {
        let mut app = app();
        app.boot = BootState::Loading {
            started_at: Instant::now(),
        };
        app.startup_warning = Some("ignored invalid animations.yaml".into());
        let state = app.state.clone();

        app.apply_background(BackgroundResult::Refresh(Ok((state, None, None))));

        assert_eq!(
            app.visible_status(),
            Some("ignored invalid animations.yaml")
        );
    }

    #[test]
    fn reduced_motion_toggle_updates_the_workspace_preference() {
        let mut app = app();
        let (tx, _rx) = mpsc::channel();
        assert_eq!(app.preferences.reduced_motion, None);

        app.handle_key(key(KeyCode::Char('M'), KeyModifiers::SHIFT), &tx);

        assert_eq!(app.preferences.reduced_motion, Some(true));
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
        let animation = animation::fallback();
        for (width, height) in [(120, 40), (31, 16), (31, 11), (30, 9)] {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).expect("test terminal");
            terminal
                .draw(|frame| {
                    render_loading(frame, frame.area(), Instant::now(), &animation, false, None);
                })
                .expect("loading art renders");
        }
        let (full, variant) = animation::select(&animation, 120, 40, false);
        assert_eq!(variant, "full");
        assert_eq!((full.dimensions.width, full.dimensions.height), (31, 15));
    }

    #[test]
    fn loading_preview_surfaces_an_implicit_fallback_warning() {
        let animation = animation::fallback();
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| {
                render_loading(
                    frame,
                    frame.area(),
                    Instant::now(),
                    &animation,
                    false,
                    Some("ignored invalid /tmp/animations.yaml"),
                );
            })
            .expect("loading warning renders");
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(rendered.contains("ignored invalid /tmp/animations.yaml"));
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
            inspector_tabs(Some(&ItemRef::Provider("executor-a".into()))),
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
    fn empty_editor_values_preserve_provider_inheritance() {
        assert_eq!(non_empty(String::new()), None);
        assert_eq!(non_empty("   ".into()), None);
        assert_eq!(non_empty("remote".into()).as_deref(), Some("remote"));
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
        assert_eq!(edge_label("depends_on"), Some("depends".into()));
        assert_eq!(edge_label("spawned"), Some("spawns".into()));
        assert_eq!(edge_label("delegates"), Some("delegates".into()));
        assert_eq!(edge_label("reviewed_by"), Some("reviews".into()));
        assert_eq!(edge_label("feedback"), Some("retry".into()));
        assert_eq!(edge_label("reports"), Some("reports".into()));
        assert_eq!(edge_label("conditional"), Some("condition".into()));
        let passing = edge_label("when tests == pass").expect("passing route");
        let failing = edge_label("when tests == fail").expect("failing route");
        assert_eq!(passing, "if tests=pass");
        assert_eq!(failing, "if tests=fail");
        assert_ne!(passing, failing);
        assert!(
            edge_label("when workflow.input.release_candidate == true")
                .expect("long condition")
                .width()
                <= EDGE_LABEL_WIDTH
        );
        assert!(
            edge_label("provider_defined_relationship")
                .expect("provider relation")
                .width()
                <= EDGE_LABEL_WIDTH
        );
        assert_ne!(
            status_color(LifecycleStatus::Blocked),
            status_color(LifecycleStatus::Working)
        );
        assert_eq!(plain().fg, Some(Color::Reset));
        assert_eq!(dim().fg, Some(Color::Reset));
        assert!(dim().add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn review_relationship_does_not_invent_feedback() {
        let mut state = app().state;
        state.runs.push(reviewed_workflow_run());

        let (flow, _) = build_flow(&state, Some("run"));

        assert!(
            flow.edges()
                .iter()
                .any(|edge| edge.content.relation == "reviewed_by")
        );
        assert!(
            flow.edges()
                .iter()
                .all(|edge| edge.content.relation != "feedback")
        );
    }

    #[test]
    fn declared_feedback_uses_a_distinct_return_lane() {
        let mut state = app().state;
        state.runs.push(feedback_workflow_run());

        let (flow, _) = build_flow(&state, Some("run"));
        let feedback = flow
            .edges()
            .iter()
            .find(|edge| edge.content.relation == "feedback")
            .expect("declared feedback edge");
        assert_eq!(feedback.source, "node:run:verify");
        assert_eq!(feedback.target, "node:run:implement");
        assert_eq!(feedback.source_handle.as_deref(), Some("bottom"));
        assert_eq!(feedback.target_handle.as_deref(), Some("bottom"));

        state.runs[0]
            .edges
            .retain(|edge| edge.relationship != "feedback");
        let (without_feedback, _) = build_flow(&state, Some("run"));
        for node in ["implement", "verify"] {
            assert_eq!(
                flow.node_bounds(&format!("node:run:{node}")),
                without_feedback.node_bounds(&format!("node:run:{node}"))
            );
        }
    }

    #[test]
    fn labeled_edges_have_rendered_clearance() {
        let mut state = app().state;
        state.runs.push(reviewed_workflow_run());

        let (flow, _) = build_flow(&state, Some("run"));
        let implement = flow
            .node_bounds("node:run:implement")
            .expect("implement node");
        let verify = flow.node_bounds("node:run:verify").expect("verify node");

        assert!(verify.x() - implement.right() >= LABELED_EDGE_RANK_SPACING);
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
        assert_eq!(reports.source_handle.as_deref(), Some("top"));
        assert_eq!(reports.target_handle.as_deref(), Some("bottom"));
        assert!(!reports.content.active);
        let root = flow.node_bounds("session:root").expect("root node");
        let stage = flow.node_bounds("node:run:implement").expect("stage node");
        assert!(stage.y() - root.bottom() >= AGENT_CARD_HEIGHT);

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
    fn orchestration_lanes_reserve_space_before_stage_cards() {
        let mut state = app().state;
        let mut run = workflow_run();
        run.nodes = ["plan", "implement", "verify"]
            .into_iter()
            .map(|id| workflow_node(id, LifecycleStatus::Queued, 0))
            .collect();
        state.runs.push(run);

        let (flow, _) = build_flow(&state, Some("run"));
        let root = flow.node_bounds("session:root").expect("orchestrator node");
        let first_stage_y = state.runs[0]
            .nodes
            .iter()
            .filter_map(|node| flow.node_bounds(&format!("node:run:{}", node.id)))
            .map(|bounds| bounds.y())
            .reduce(f64::min)
            .expect("stage bounds");
        let edges = orchestration_edges(&state.runs[0], "session:root");

        assert!(first_stage_y - root.bottom() >= orchestration_lane_clearance(&edges));
        assert_eq!(
            flow.edges()
                .iter()
                .filter(|edge| edge.content.relation == "delegates")
                .count(),
            3
        );
        assert_eq!(
            flow.edges()
                .iter()
                .filter(|edge| edge.content.relation == "reports")
                .count(),
            3
        );
    }

    #[test]
    fn graph_renders_complete_edge_labels() {
        for (width, height) in [(120, 32), (160, 40)] {
            let mut app = app();
            app.state.runs.push(reviewed_workflow_run());
            app.active_run = Some("run".into());
            app.explorer_view = ExplorerView::Graph;
            app.rebuild(true);
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).expect("test terminal");
            terminal
                .draw(|frame| render(frame, &mut app))
                .expect("graph renders");
            let rendered = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();

            assert!(rendered.contains("review"));
            assert!(rendered.contains("reports"));
            assert!(!rendered.contains("retry"));
        }
    }

    #[test]
    fn graph_renders_declared_feedback_label() {
        for (width, height) in [(120, 32), (160, 40)] {
            let mut app = app();
            app.state.runs.push(feedback_workflow_run());
            app.active_run = Some("run".into());
            app.explorer_view = ExplorerView::Graph;
            app.rebuild(true);
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).expect("test terminal");
            terminal
                .draw(|frame| render(frame, &mut app))
                .expect("graph renders");
            let rendered = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();

            assert!(rendered.contains("review"));
            assert!(rendered.contains("retry"));
            assert!(rendered.contains("reports"));
        }
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

    #[test]
    fn compact_graph_keeps_root_and_keyboard_selection_visible() {
        let mut app = app();
        let mut run = workflow_run();
        run.nodes
            .push(workflow_node("implement", LifecycleStatus::Queued, 0));
        app.state.runs.push(run);
        app.active_run = Some("run".into());
        app.explorer_view = ExplorerView::Graph;
        app.rebuild(true);
        app.flow.select_node("session:root");
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("compact terminal");
        terminal
            .draw(|frame| render(frame, &mut app))
            .expect("compact graph renders");
        assert!(app.hit.inspector.is_none());
        assert_node_visible(&app.flow, "session:root");

        let (tx, _rx) = mpsc::channel();
        app.handle_key(key(KeyCode::Char('j'), KeyModifiers::NONE), &tx);
        terminal
            .draw(|frame| render(frame, &mut app))
            .expect("selected node renders");
        assert_eq!(
            app.flow.first_selected_node_id().as_deref(),
            Some("node:run:implement")
        );
        assert_node_visible(&app.flow, "node:run:implement");
        assert_node_visible(&app.flow, "session:root");
    }

    fn assert_node_visible(flow: &AgentFlow, id: &str) {
        let (left, top, right, bottom) = flow.node_terminal_rect(id).expect("node rectangle");
        assert!(
            flow.is_in_bounds(left, top),
            "{id} top-left ({left}, {top}) is outside {:?}",
            flow.canvas_area()
        );
        assert!(
            flow.is_in_bounds(right - 1, bottom - 1),
            "{id} bottom-right ({}, {}) is outside {:?}",
            right - 1,
            bottom - 1,
            flow.canvas_area()
        );
    }
}
