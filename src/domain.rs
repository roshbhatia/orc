use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

fn parse_enum<T: DeserializeOwned>(value: &str, name: &str) -> Result<T, String> {
    serde_json::from_value(serde_json::Value::String(value.to_owned()))
        .map_err(|_| format!("unknown {name}: {value}"))
}

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
        parse_enum(value, "role")
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LifecycleStatus {
    Pending,
    Queued,
    #[default]
    Working,
    Terminating,
    Waiting,
    Blocked,
    Failed,
    Done,
    Cancelled,
    Disconnected,
    Archived,
    Skipped,
}

impl LifecycleStatus {
    pub fn active(self) -> bool {
        !matches!(
            self,
            Self::Done
                | Self::Failed
                | Self::Cancelled
                | Self::Disconnected
                | Self::Archived
                | Self::Skipped
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
        parse_enum(value, "status")
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CompletionTarget {
    #[default]
    Orchestrator,
    Judge,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum JudgePolicy {
    #[default]
    #[serde(rename = "llm")]
    Llm,
    #[serde(rename = "human")]
    Human,
    #[serde(rename = "llm+human")]
    LlmAndHuman,
}

impl std::fmt::Display for JudgePolicy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = serde_json::to_value(self).expect("judge policy serializes");
        write!(
            formatter,
            "{}",
            value.as_str().expect("judge policy is a string")
        )
    }
}

impl std::str::FromStr for JudgePolicy {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        parse_enum(value, "judge policy")
    }
}

impl std::fmt::Display for CompletionTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", format!("{self:?}").to_lowercase())
    }
}

impl std::str::FromStr for CompletionTarget {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        parse_enum(value, "completion target")
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
    #[serde(default)]
    pub runtime_timeout_seconds: Option<u64>,
    #[serde(default)]
    pub idle_timeout_seconds: Option<u64>,
    #[serde(default)]
    pub heartbeat_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub termination_reason: Option<String>,
    #[serde(default)]
    pub termination_cause: Option<String>,
    #[serde(default)]
    pub termination_attempt_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub termination_operation_id: Option<String>,
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
        parse_enum(value, "registration source")
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
    #[serde(default)]
    pub execution: Option<String>,
    #[serde(default)]
    pub judge_policy: JudgePolicy,
    pub goal: String,
    pub expected_output: String,
    #[serde(default)]
    pub success_criteria: Vec<String>,
    pub completion: CompletionTarget,
    pub review_by: Option<String>,
    pub session_id: Option<String>,
    #[serde(default)]
    pub child_run_id: Option<String>,
    pub status: LifecycleStatus,
    pub attempt: u32,
    #[serde(default)]
    pub retry_after: Option<DateTime<Utc>>,
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

const MAX_ACTIVITY_EVENTS: usize = 256;
const MAX_ACTIVITY_MESSAGE_BYTES: usize = 4096;

impl WorkflowNode {
    pub fn record_activity(&mut self, kind: impl Into<String>, message: impl Into<String>) {
        let mut message = message.into();
        if message.len() > MAX_ACTIVITY_MESSAGE_BYTES {
            let mut end = MAX_ACTIVITY_MESSAGE_BYTES - 3;
            while !message.is_char_boundary(end) {
                end -= 1;
            }
            message.truncate(end);
            message.push_str("...");
        }
        self.activity.push(ActivityEvent {
            at: Utc::now(),
            kind: kind.into(),
            message,
        });
        self.compact_activity();
    }

    pub fn compact_activity(&mut self) {
        let excess = self.activity.len().saturating_sub(MAX_ACTIVITY_EVENTS);
        if excess > 0 {
            self.activity.drain(..excess);
        }
        for event in &mut self.activity {
            if event.message.len() > MAX_ACTIVITY_MESSAGE_BYTES {
                let mut end = MAX_ACTIVITY_MESSAGE_BYTES - 3;
                while !event.message.is_char_boundary(end) {
                    end -= 1;
                }
                event.message.truncate(end);
                event.message.push_str("...");
            }
        }
    }
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
    pub parent_run_id: Option<String>,
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
    pub execution_nonce: Option<String>,
    #[serde(default)]
    pub resume_requested: bool,
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
            schema_version: "orc.state/v4".into(),
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
        self.current_session_for(requested.as_deref())
    }

    fn current_session_for(&self, requested: Option<&str>) -> Option<&Session> {
        if let Some(id) = requested {
            return self.sessions.iter().find(|session| {
                session.id == id
                    && session.status.active()
                    && session.status != LifecycleStatus::Terminating
            });
        }
        self.active_sessions()
            .filter(|session| {
                session.role == SessionRole::Orchestrator
                    && session.status != LifecycleStatus::Terminating
            })
            .max_by_key(|session| session.updated_at)
            .or_else(|| {
                self.active_sessions()
                    .filter(|session| {
                        session.parent_id.is_none()
                            && session.status != LifecycleStatus::Terminating
                    })
                    .max_by_key(|session| session.updated_at)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, role: SessionRole, status: LifecycleStatus, updated: i64) -> Session {
        let at = DateTime::from_timestamp(updated, 0).expect("valid timestamp");
        Session {
            id: id.into(),
            native_id: id.into(),
            trace_id: None,
            harness: "test".into(),
            model: None,
            role,
            title: id.into(),
            purpose: "test".into(),
            goal: "test".into(),
            expected_output: "test".into(),
            success_criteria: Vec::new(),
            completion: CompletionTarget::Orchestrator,
            review_by: None,
            parent_id: None,
            run_id: None,
            node_id: None,
            provider_ref: None,
            providers: Vec::new(),
            directory: "/tmp".into(),
            registration: RegistrationSource::Connected,
            status,
            runtime_timeout_seconds: None,
            idle_timeout_seconds: None,
            heartbeat_at: Some(at),
            termination_reason: None,
            termination_cause: None,
            termination_attempt_at: None,
            termination_operation_id: None,
            connected_at: at,
            updated_at: at,
        }
    }

    #[test]
    fn explicit_inactive_session_does_not_inherit_orchestrator_authority() {
        let mut workspace = WorkspaceState::empty("/tmp".into());
        workspace.sessions = vec![
            session(
                "archived",
                SessionRole::Orchestrator,
                LifecycleStatus::Archived,
                2,
            ),
            session(
                "active",
                SessionRole::Orchestrator,
                LifecycleStatus::Working,
                1,
            ),
        ];

        assert_eq!(
            workspace
                .current_session_for(Some("archived"))
                .map(|session| session.id.as_str()),
            None
        );
    }

    #[test]
    fn missing_environment_session_does_not_inherit_orchestrator_authority() {
        let mut workspace = WorkspaceState::empty("/tmp".into());
        workspace.sessions = vec![session(
            "active",
            SessionRole::Orchestrator,
            LifecycleStatus::Working,
            1,
        )];

        assert!(workspace.current_session_for(Some("missing")).is_none());
        assert_eq!(
            workspace
                .current_session_for(None)
                .map(|session| session.id.as_str()),
            Some("active")
        );
    }

    #[test]
    fn terminating_session_does_not_accept_current_authority() {
        let mut workspace = WorkspaceState::empty("/tmp".into());
        workspace.sessions = vec![session(
            "terminating",
            SessionRole::Orchestrator,
            LifecycleStatus::Terminating,
            1,
        )];

        assert!(workspace.current_session_for(Some("terminating")).is_none());
        assert!(workspace.current_session_for(None).is_none());
    }
}
