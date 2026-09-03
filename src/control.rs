use std::{env, path::Path};

use anyhow::{Context, Result, bail};
use chrono::Utc;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    config::Config,
    domain::{
        ActivityEvent, AgentConfig, CompletionTarget, JudgePolicy, LifecycleStatus,
        RegistrationSource, Session, SessionRole, WorkflowEdge, WorkflowNode, WorkflowRun,
        WorkspaceState,
    },
    provider, state,
};

#[derive(Clone, Debug)]
pub struct Contract {
    pub harness: String,
    pub model: Option<String>,
    pub role: SessionRole,
    pub title: String,
    pub purpose: String,
    pub goal: String,
    pub expected_output: String,
    pub success_criteria: Vec<String>,
    pub completion: CompletionTarget,
    pub review_by: Option<String>,
}

impl Default for Contract {
    fn default() -> Self {
        Self {
            harness: "unknown".into(),
            model: None,
            role: SessionRole::Worker,
            title: "Agent session".into(),
            purpose: "Agent session".into(),
            goal: "Complete the assigned work".into(),
            expected_output: "A verified result".into(),
            success_criteria: Vec::new(),
            completion: CompletionTarget::Orchestrator,
            review_by: None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct SessionLink {
    pub id: Option<String>,
    pub native_id: Option<String>,
    pub parent_id: Option<String>,
    pub run_id: Option<String>,
    pub node_id: Option<String>,
    pub provider_ref: Option<String>,
    pub source: RegistrationSource,
}

#[derive(Clone, Debug)]
pub struct NodeSpec {
    pub id: String,
    pub contract: Contract,
    pub session_id: Option<String>,
    pub status: LifecycleStatus,
    pub attempt: u32,
    pub depends_on: Vec<String>,
    pub execution: Option<String>,
    pub judge_policy: JudgePolicy,
}

#[derive(Clone, Debug)]
pub struct NodeReport {
    pub status: LifecycleStatus,
    pub output: Option<serde_json::Value>,
    pub message: Option<String>,
    pub tokens: Option<u64>,
    pub cost_usd: Option<f64>,
}

pub fn inferred_native_id() -> String {
    [
        "ORC_NATIVE_SESSION_ID",
        "CODEX_THREAD_ID",
        "CODEX_SESSION_ID",
        "CLAUDE_CODE_SESSION_ID",
        "CLAUDE_SESSION_ID",
        "OPENCODE_SESSION_ID",
    ]
    .into_iter()
    .find_map(|name| env::var(name).ok().filter(|value| !value.is_empty()))
    .unwrap_or_else(|| format!("process-{}", std::process::id()))
}

pub fn inferred_session_id(harness: &str, native_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(harness);
    hasher.update(b":\0:");
    hasher.update(native_id);
    format!("{harness}-{}", &hex::encode(hasher.finalize())[..12])
}

pub fn read_workspace(scope: &Path) -> Result<WorkspaceState> {
    let scope = state::resolve_scope(scope)?;
    state::read(&scope)
}

pub fn register(scope: &Path, mut contract: Contract, link: SessionLink) -> Result<Session> {
    let scope = state::resolve_scope(scope)?;
    let native_id = link.native_id.unwrap_or_else(inferred_native_id);
    let base_id = link
        .id
        .or_else(|| env::var("ORC_SESSION_ID").ok())
        .unwrap_or_else(|| inferred_session_id(&contract.harness, &native_id));
    state::update(&scope, |workspace| {
        let current = workspace
            .sessions
            .iter()
            .find(|session| {
                session.status != LifecycleStatus::Archived
                    && (session.id == base_id
                        || (session.harness == contract.harness && session.native_id == native_id))
            })
            .cloned();
        let id = current
            .as_ref()
            .map(|session| session.id.clone())
            .unwrap_or_else(|| {
                if workspace
                    .sessions
                    .iter()
                    .any(|session| session.id == base_id)
                {
                    format!("{base_id}-{}", &Uuid::new_v4().to_string()[..6])
                } else {
                    base_id.clone()
                }
            });
        let explicit_parent = link
            .parent_id
            .clone()
            .or_else(|| env::var("ORC_PARENT_SESSION_ID").ok());
        let active_orchestrator = workspace
            .active_sessions()
            .find(|session| {
                session.role == SessionRole::Orchestrator
                    && current
                        .as_ref()
                        .is_none_or(|current| session.id != current.id)
            })
            .map(|session| session.id.clone());
        if contract.role == SessionRole::Worker
            && explicit_parent.is_none()
            && link.run_id.is_none()
            && link.node_id.is_none()
            && active_orchestrator.is_none()
        {
            contract.role = SessionRole::Orchestrator;
        }
        let parent_id = explicit_parent.or_else(|| {
            (contract.role != SessionRole::Orchestrator)
                .then_some(active_orchestrator)
                .flatten()
        });
        let now = Utc::now();
        let session = Session {
            id,
            native_id: native_id.clone(),
            trace_id: Some(native_id),
            harness: contract.harness,
            model: contract
                .model
                .or_else(|| current.as_ref().and_then(|session| session.model.clone())),
            role: contract.role,
            title: contract.title,
            purpose: contract.purpose,
            goal: contract.goal,
            expected_output: contract.expected_output,
            success_criteria: contract.success_criteria,
            completion: contract.completion,
            review_by: contract.review_by,
            parent_id,
            run_id: link.run_id,
            node_id: link.node_id,
            provider_ref: link
                .provider_ref
                .or_else(|| env::var("ORC_PROVIDER_REF").ok()),
            providers: current
                .as_ref()
                .map(|session| session.providers.clone())
                .unwrap_or_default(),
            directory: scope.display().to_string(),
            registration: link.source,
            status: LifecycleStatus::Working,
            connected_at: current.map(|session| session.connected_at).unwrap_or(now),
            updated_at: now,
        };
        workspace
            .sessions
            .retain(|candidate| candidate.id != session.id);
        workspace.sessions.insert(0, session.clone());
        workspace.active = true;
        Ok(session)
    })
}

pub fn adopt(scope: &Path, mut contract: Contract, native_id: Option<String>) -> Result<Session> {
    contract.role = SessionRole::Orchestrator;
    let scope = state::resolve_scope(scope)?;
    let now = Utc::now();
    state::update(&scope, |workspace| {
        for session in &mut workspace.sessions {
            if session.role == SessionRole::Orchestrator && session.status.active() {
                session.status = LifecycleStatus::Archived;
                session.updated_at = now;
            }
        }
        let native_id = native_id.clone().unwrap_or_else(inferred_native_id);
        let session = Session {
            id: format!(
                "{}-{}",
                inferred_session_id(&contract.harness, &native_id),
                &Uuid::new_v4().to_string()[..6]
            ),
            native_id: native_id.clone(),
            trace_id: Some(native_id),
            harness: contract.harness.clone(),
            model: contract.model.clone(),
            role: SessionRole::Orchestrator,
            title: contract.title.clone(),
            purpose: contract.purpose.clone(),
            goal: contract.goal.clone(),
            expected_output: contract.expected_output.clone(),
            success_criteria: contract.success_criteria.clone(),
            completion: contract.completion,
            review_by: contract.review_by.clone(),
            parent_id: None,
            run_id: None,
            node_id: None,
            provider_ref: None,
            providers: Vec::new(),
            directory: scope.display().to_string(),
            registration: RegistrationSource::Connected,
            status: LifecycleStatus::Working,
            connected_at: now,
            updated_at: now,
        };
        workspace.sessions.insert(0, session.clone());
        Ok(session)
    })
}

pub fn update_session(scope: &Path, id: &str, status: LifecycleStatus) -> Result<Session> {
    let scope = state::resolve_scope(scope)?;
    state::update(&scope, |workspace| {
        let session = workspace
            .sessions
            .iter_mut()
            .find(|session| session.id == id)
            .with_context(|| format!("unknown session: {id}"))?;
        session.status = status;
        session.updated_at = Utc::now();
        Ok(session.clone())
    })
}

pub fn archive(scope: &Path, id: Option<&str>, native_id: Option<&str>) -> Result<Session> {
    let scope = state::resolve_scope(scope)?;
    state::update(&scope, |workspace| {
        let session = workspace
            .sessions
            .iter_mut()
            .filter(|session| session.status != LifecycleStatus::Archived)
            .filter(|session| {
                id.is_some_and(|id| session.id == id)
                    || native_id.is_some_and(|native| session.native_id == native)
            })
            .max_by_key(|session| session.updated_at)
            .context("no matching active session")?;
        session.status = LifecycleStatus::Archived;
        session.updated_at = Utc::now();
        Ok(session.clone())
    })
}

pub fn prune(config: &Config, scope: &Path, id: &str) -> Result<Session> {
    let scope = state::resolve_scope(scope)?;
    let workspace = state::read(&scope)?;
    let session = selected_session(&workspace, id)?.clone();
    if session.status.active() {
        let providers = provider::discover(config)?;
        let request =
            provider::action_request(provider::Action::Stop, &scope, Some(&session), "right");
        let plan = provider::resolve_plan(config, &providers, provider::Action::Stop, request)
            .context("no provider can stop this active agent")?;
        let result = provider::run_plan_with_timeout(&plan, &scope, config.provider_timeout())?;
        if !plan.accepts(result.code) {
            bail!(
                "stop provider exited with {}: {}",
                result.code,
                result.stderr.trim()
            );
        }
    }
    archive(&scope, Some(id), None)
}

pub fn reconcile(config: &Config, scope: &Path) -> Result<WorkspaceState> {
    reconcile_with_current(config, scope, false)
}

pub fn reconcile_with_current(
    config: &Config,
    scope: &Path,
    rebind_current: bool,
) -> Result<WorkspaceState> {
    let scope = state::resolve_scope(scope)?;
    let providers = provider::discover(config)?;
    let snapshot = state::read(&scope)?;
    let enrichments = snapshot
        .sessions
        .iter()
        .filter(|session| session.status != LifecycleStatus::Archived)
        .filter_map(|session| {
            let bindings =
                provider::discover_bindings(config, &providers, &scope, session, rebind_current);
            let (title, goal) = provider::describe(config, &providers, &scope, session);
            (!bindings.is_empty() || title.is_some() || goal.is_some())
                .then(|| (session.id.clone(), bindings, title, goal))
        })
        .collect::<Vec<_>>();
    if enrichments.is_empty() {
        return Ok(snapshot);
    }
    state::update(&scope, |workspace| {
        for (id, bindings, title, goal) in &enrichments {
            let selected = workspace
                .sessions
                .iter_mut()
                .find(|candidate| candidate.id == *id)
                .context("session disappeared")?;
            for binding in bindings {
                selected.providers.retain(|candidate| {
                    candidate.provider != binding.provider || candidate.kind != binding.kind
                });
                selected.providers.push(binding.clone());
            }
            if let Some(title) = title
                && (selected.title == "Agent session" || selected.title == selected.id)
            {
                selected.title.clone_from(title);
            }
            if let Some(goal) = goal
                && selected.goal == "Complete the assigned work"
            {
                selected.goal.clone_from(goal);
            }
        }
        Ok(workspace.clone())
    })
}

pub fn create_run(
    scope: &Path,
    name: String,
    goal: String,
    expected_output: String,
    orchestrator_id: Option<String>,
    harness: Option<String>,
    model: Option<String>,
) -> Result<WorkflowRun> {
    let scope = state::resolve_scope(scope)?;
    state::update(&scope, |workspace| {
        let now = Utc::now();
        let orchestrator_id = orchestrator_id.clone().or_else(|| {
            workspace
                .current_session()
                .map(|session| session.id.clone())
        });
        let run = WorkflowRun {
            id: format!("run-{}", &Uuid::new_v4().to_string()[..12]),
            name: name.clone(),
            goal: goal.clone(),
            expected_output: expected_output.clone(),
            status: LifecycleStatus::Queued,
            orchestrator_id,
            definition: None,
            revision: None,
            checkpoint: None,
            mode: Default::default(),
            process_id: None,
            log_path: None,
            current_node: None,
            tokens: 0,
            cost_usd: 0.0,
            token_burn: Vec::new(),
            pending_gates: Vec::new(),
            approved_gates: Vec::new(),
            agents: harness
                .clone()
                .map(|harness| {
                    vec![AgentConfig {
                        role: SessionRole::Worker,
                        harness,
                        model: model.clone(),
                    }]
                })
                .unwrap_or_default(),
            nodes: Vec::new(),
            edges: Vec::new(),
            created_at: now,
            updated_at: now,
        };
        workspace.runs.insert(0, run.clone());
        Ok(run)
    })
}

pub fn set_run_agent(
    scope: &Path,
    run_id: &str,
    role: SessionRole,
    harness: String,
    model: Option<String>,
) -> Result<WorkflowRun> {
    let scope = state::resolve_scope(scope)?;
    state::update(&scope, |workspace| {
        let run = workspace
            .runs
            .iter_mut()
            .find(|run| run.id == run_id)
            .with_context(|| format!("unknown run: {run_id}"))?;
        run.agents.retain(|agent| agent.role != role);
        run.agents.push(AgentConfig {
            role,
            harness: harness.clone(),
            model: model.clone(),
        });
        run.updated_at = Utc::now();
        Ok(run.clone())
    })
}

pub fn update_run(scope: &Path, run_id: &str, status: LifecycleStatus) -> Result<WorkflowRun> {
    let scope = state::resolve_scope(scope)?;
    state::update(&scope, |workspace| {
        let run = workspace
            .runs
            .iter_mut()
            .find(|run| run.id == run_id)
            .with_context(|| format!("unknown run: {run_id}"))?;
        run.status = status;
        run.updated_at = Utc::now();
        Ok(run.clone())
    })
}

pub fn upsert_node(scope: &Path, run_id: &str, spec: NodeSpec) -> Result<WorkflowNode> {
    let NodeSpec {
        id,
        contract,
        session_id,
        status,
        attempt,
        depends_on,
        execution,
        judge_policy,
    } = spec;
    let scope = state::resolve_scope(scope)?;
    state::update(&scope, |workspace| {
        let run = workspace
            .runs
            .iter_mut()
            .find(|run| run.id == run_id)
            .with_context(|| format!("unknown run: {run_id}"))?;
        let node = WorkflowNode {
            id: id.clone(),
            name: contract.title.clone(),
            purpose: contract.purpose.clone(),
            role: contract.role,
            harness: contract.harness.clone(),
            model: contract.model.clone(),
            execution,
            judge_policy,
            goal: contract.goal.clone(),
            expected_output: contract.expected_output.clone(),
            success_criteria: contract.success_criteria.clone(),
            completion: contract.completion,
            review_by: contract.review_by.clone(),
            session_id: session_id.clone(),
            status,
            attempt,
            prompt: None,
            input: None,
            output: None,
            activity: Vec::new(),
            tokens: 0,
            cost_usd: 0.0,
            updated_at: Utc::now(),
        };
        run.nodes.retain(|candidate| candidate.id != id);
        run.nodes.push(node.clone());
        run.edges
            .retain(|edge| edge.to != id || edge.relationship != "depends_on");
        run.edges
            .extend(depends_on.iter().map(|dependency| WorkflowEdge {
                from: dependency.clone(),
                to: id.clone(),
                relationship: "depends_on".into(),
            }));
        if let Some(review_by) = &node.review_by {
            run.edges.push(WorkflowEdge {
                from: id.clone(),
                to: review_by.clone(),
                relationship: "reviewed_by".into(),
            });
        }
        run.updated_at = Utc::now();
        Ok(node)
    })
}

pub fn update_node(
    scope: &Path,
    run_id: &str,
    node_id: &str,
    status: LifecycleStatus,
) -> Result<WorkflowNode> {
    let scope = state::resolve_scope(scope)?;
    state::update(&scope, |workspace| {
        let run = workspace
            .runs
            .iter_mut()
            .find(|run| run.id == run_id)
            .with_context(|| format!("unknown run: {run_id}"))?;
        let node = run
            .nodes
            .iter_mut()
            .find(|node| node.id == node_id)
            .with_context(|| format!("unknown node: {node_id}"))?;
        node.status = status;
        node.updated_at = Utc::now();
        run.updated_at = Utc::now();
        Ok(node.clone())
    })
}

pub fn report_node(
    scope: &Path,
    run_id: &str,
    node_id: &str,
    report: NodeReport,
) -> Result<WorkflowNode> {
    let scope = state::resolve_scope(scope)?;
    state::update(&scope, |workspace| {
        let run = workspace
            .runs
            .iter_mut()
            .find(|run| run.id == run_id)
            .with_context(|| format!("unknown run: {run_id}"))?;
        let node = run
            .nodes
            .iter_mut()
            .find(|node| node.id == node_id)
            .with_context(|| format!("unknown node: {node_id}"))?;
        node.status = report.status;
        if report.output.is_some() {
            node.output.clone_from(&report.output);
        }
        if let Some(tokens) = report.tokens {
            run.tokens = run
                .tokens
                .saturating_sub(node.tokens)
                .saturating_add(tokens);
            node.tokens = tokens;
        }
        if let Some(cost_usd) = report.cost_usd {
            run.cost_usd = (run.cost_usd - node.cost_usd + cost_usd).max(0.0);
            node.cost_usd = cost_usd;
        }
        node.activity.push(ActivityEvent {
            at: Utc::now(),
            kind: "reported".into(),
            message: report
                .message
                .clone()
                .unwrap_or_else(|| report.status.to_string()),
        });
        node.updated_at = Utc::now();
        run.updated_at = Utc::now();
        Ok(node.clone())
    })
}

pub fn selected_session<'a>(workspace: &'a WorkspaceState, id: &str) -> Result<&'a Session> {
    workspace
        .sessions
        .iter()
        .find(|session| session.id == id)
        .with_context(|| format!("unknown session: {id}"))
}

pub fn attach(
    config: &Config,
    scope: &Path,
    id: &str,
    action: provider::Action,
    direction: &str,
) -> Result<AttachOutcome> {
    let scope = state::resolve_scope(scope)?;
    let workspace = state::read(&scope)?;
    let session = selected_session(&workspace, id)?;
    let providers = provider::discover(config)?;
    let (action, disposition) = if action == provider::Action::Attach
        && session.providers.iter().any(|binding| {
            binding.kind == crate::domain::ProviderKind::Display
                && binding.status == crate::domain::BindingStatus::Active
                && binding
                    .r#ref
                    .as_ref()
                    .is_some_and(|value| !value.is_empty())
        }) {
        (provider::Action::Focus, AttachDisposition::Focused)
    } else {
        (action, AttachDisposition::Launched)
    };
    let has_persistent_process = session.providers.iter().any(|binding| {
        binding.kind == crate::domain::ProviderKind::Persistence
            && binding.status == crate::domain::BindingStatus::Active
            && binding
                .r#ref
                .as_ref()
                .is_some_and(|value| !value.is_empty())
    });
    if action == provider::Action::Attach && session.status.active() && !has_persistent_process {
        anyhow::bail!(
            "{} is active, but no display can focus it and no persistent process can reattach it; inspect it or stop it before resuming",
            session.title
        );
    }
    let request = provider::action_request(action, &scope, Some(session), direction);
    let plan = provider::resolve_plan(config, &providers, action, request)?;
    let code = provider::execute_plan(&plan, &scope, false)?;
    Ok(AttachOutcome { code, disposition })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttachDisposition {
    Focused,
    Launched,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AttachOutcome {
    pub code: i32,
    pub disposition: AttachDisposition,
}

pub fn launch(
    config: &Config,
    scope: &Path,
    harness: String,
    model: Option<String>,
    managed: Option<String>,
    args: Vec<String>,
) -> Result<i32> {
    let scope = state::resolve_scope(scope)?;
    let parent = state::read(&scope)?
        .current_session()
        .map(|session| session.id.clone());
    let native_id = Uuid::new_v4().to_string();
    let session = register(
        &scope,
        Contract {
            harness: harness.clone(),
            model: model.clone(),
            role: if parent.is_some() {
                SessionRole::Worker
            } else {
                SessionRole::Orchestrator
            },
            title: harness.clone(),
            purpose: parent.as_ref().map_or_else(
                || format!("{harness} orchestrator"),
                |_| format!("Child {harness} session"),
            ),
            goal: format!("Run {harness}"),
            ..Contract::default()
        },
        SessionLink {
            native_id: Some(native_id.clone()),
            parent_id: parent,
            provider_ref: managed.clone(),
            source: RegistrationSource::Managed,
            ..SessionLink::default()
        },
    )?;
    let mut command = vec![harness];
    command.extend(args);
    let code = if let Some(managed_id) = managed {
        let providers = provider::discover(config)?;
        let request = serde_json::json!({
            "version": "orc.provider/v1", "action": "launch", "scope": scope, "session": session,
            "command": command, "managedId": managed_id,
        });
        let plan = provider::resolve_plan(config, &providers, provider::Action::Launch, request)?;
        provider::execute_plan(&plan, &scope, true)?
    } else {
        let mut child = std::process::Command::new(&command[0]);
        child
            .args(&command[1..])
            .current_dir(&scope)
            .env("ORC_SCOPE", &scope)
            .env("ORC_SESSION_ID", &session.id)
            .env("ORC_NATIVE_SESSION_ID", &native_id);
        if let Some(model) = &model {
            child.env("ORC_MODEL", model);
        }
        child.status()?.code().unwrap_or(1)
    };
    update_session(
        &scope,
        &session.id,
        if code == 0 {
            LifecycleStatus::Done
        } else {
            LifecycleStatus::Failed
        },
    )?;
    Ok(code)
}

pub fn require_id(id: Option<String>) -> Result<String> {
    id.or_else(|| env::var("ORC_SESSION_ID").ok())
        .context("a session id or ORC_SESSION_ID is required")
}

pub fn ensure_active_context(scope: &Path) -> Result<(WorkspaceState, Session)> {
    let state = read_workspace(scope)?;
    let session = state
        .current_session()
        .cloned()
        .context("Orc requires a registered session in an active scope")?;
    if !state.active {
        bail!("Orc scope is idle");
    }
    Ok((state, session))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inferred_id_is_short_and_stable() {
        let id = inferred_session_id("codex", "abc");
        assert_eq!(id, inferred_session_id("codex", "abc"));
        assert_eq!(id.len(), 18);
    }
}
