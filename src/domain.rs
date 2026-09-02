use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionRole {
    Orchestrator,
    Planner,
    Researcher,
    Implementer,
    Critic,
    Judge,
    Verifier,
    Operator,
    Generalist,
    #[default]
    Worker,
}

impl std::fmt::Display for SessionRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", format!("{self:?}").to_lowercase())
    }
}

impl std::str::FromStr for SessionRole {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(&format!("\"{value}\"")).map_err(|_| format!("unknown role: {value}"))
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LifecycleStatus {
    Queued,
    #[default]
    Working,
    Waiting,
    Blocked,
    Failed,
    Done,
    Cancelled,
    Disconnected,
    Archived,
}

impl LifecycleStatus {
    pub fn active(self) -> bool {
        !matches!(
            self,
            Self::Done | Self::Failed | Self::Cancelled | Self::Disconnected | Self::Archived
        )
    }
}

impl std::fmt::Display for LifecycleStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", format!("{self:?}").to_lowercase())
    }
}

impl std::str::FromStr for LifecycleStatus {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(&format!("\"{value}\""))
            .map_err(|_| format!("unknown status: {value}"))
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CompletionTarget {
    #[default]
    Orchestrator,
    Judge,
}

impl std::fmt::Display for CompletionTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", format!("{self:?}").to_lowercase())
    }
}

impl std::str::FromStr for CompletionTarget {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(&format!("\"{value}\""))
            .map_err(|_| format!("unknown completion target: {value}"))
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    Persistence,
    Display,
    Activity,
    Changes,
    Harness,
    Execution,
    Integration,
}

impl std::fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", format!("{self:?}").to_lowercase())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderBinding {
    pub provider: String,
    pub kind: ProviderKind,
    pub r#ref: Option<String>,
    pub status: BindingStatus,
    pub label: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BindingStatus {
    Active,
    Available,
    Unavailable,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConfig {
    pub role: SessionRole,
    pub harness: String,
    pub model: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: String,
    pub native_id: String,
    pub trace_id: Option<String>,
    pub harness: String,
    pub model: Option<String>,
    pub role: SessionRole,
    pub title: String,
    pub purpose: String,
    pub goal: String,
    pub expected_output: String,
    #[serde(default)]
    pub success_criteria: Vec<String>,
    pub completion: CompletionTarget,
    pub review_by: Option<String>,
    pub parent_id: Option<String>,
    pub run_id: Option<String>,
    pub node_id: Option<String>,
    pub provider_ref: Option<String>,
    #[serde(default)]
    pub providers: Vec<ProviderBinding>,
    pub directory: String,
    pub registration: RegistrationSource,
    pub status: LifecycleStatus,
    pub connected_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RegistrationSource {
    #[default]
    Connected,
    Hook,
    Managed,
}

impl std::str::FromStr for RegistrationSource {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(&format!("\"{value}\""))
            .map_err(|_| format!("unknown registration source: {value}"))
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowNode {
    pub id: String,
    pub name: String,
    pub purpose: String,
    pub role: SessionRole,
    pub harness: String,
    pub model: Option<String>,
    pub goal: String,
    pub expected_output: String,
    #[serde(default)]
    pub success_criteria: Vec<String>,
    pub completion: CompletionTarget,
    pub review_by: Option<String>,
    pub session_id: Option<String>,
    pub status: LifecycleStatus,
    pub attempt: u32,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub input: Option<serde_json::Value>,
    #[serde(default)]
    pub output: Option<serde_json::Value>,
    #[serde(default)]
    pub activity: Vec<ActivityEvent>,
    #[serde(default)]
    pub tokens: u64,
    #[serde(default)]
    pub cost_usd: f64,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityEvent {
    pub at: DateTime<Utc>,
    pub kind: String,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunMode {
    #[default]
    Foreground,
    Background,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingGate {
    pub id: String,
    pub before: String,
    pub reason: String,
    pub recommendation: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowEdge {
    pub from: String,
    pub to: String,
    pub relationship: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRun {
    pub id: String,
    pub name: String,
    pub goal: String,
    pub expected_output: String,
    pub status: LifecycleStatus,
    pub orchestrator_id: Option<String>,
    #[serde(default)]
    pub definition: Option<String>,
    #[serde(default)]
    pub revision: Option<String>,
    #[serde(default)]
    pub checkpoint: Option<String>,
    #[serde(default)]
    pub mode: RunMode,
    #[serde(default)]
    pub process_id: Option<u32>,
    #[serde(default)]
    pub log_path: Option<String>,
    #[serde(default)]
    pub current_node: Option<String>,
    #[serde(default)]
    pub tokens: u64,
    #[serde(default)]
    pub cost_usd: f64,
    #[serde(default)]
    pub token_burn: Vec<u64>,
    #[serde(default)]
    pub pending_gates: Vec<PendingGate>,
    #[serde(default)]
    pub approved_gates: Vec<String>,
    #[serde(default)]
    pub agents: Vec<AgentConfig>,
    #[serde(default)]
    pub nodes: Vec<WorkflowNode>,
    #[serde(default)]
    pub edges: Vec<WorkflowEdge>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceState {
    pub schema_version: String,
    pub scope: String,
    pub active: bool,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub sessions: Vec<Session>,
    #[serde(default)]
    pub runs: Vec<WorkflowRun>,
}

impl WorkspaceState {
    pub fn empty(scope: String) -> Self {
        Self {
            schema_version: "orc.state/v3".into(),
            scope,
            active: false,
            updated_at: DateTime::UNIX_EPOCH,
            sessions: Vec::new(),
            runs: Vec::new(),
        }
    }

    pub fn active_sessions(&self) -> impl Iterator<Item = &Session> {
        self.sessions
            .iter()
            .filter(|session| session.status.active())
    }

    pub fn current_session(&self) -> Option<&Session> {
        let requested = std::env::var("ORC_SESSION_ID").ok();
        requested
            .as_deref()
            .and_then(|id| self.sessions.iter().find(|session| session.id == id))
            .or_else(|| {
                self.active_sessions()
                    .filter(|session| session.role == SessionRole::Orchestrator)
                    .max_by_key(|session| session.updated_at)
            })
    }
}
