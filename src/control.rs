use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};

use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    config::Config,
    daemon,
    domain::{
        AgentConfig, BindingStatus, CompletionTarget, JudgePolicy, LifecycleStatus,
        LifecycleSubject, ProviderBinding, ProviderKind, RegistrationSource, ReportedOutput,
        Session, SessionOutputReceipt, SessionOutputStatus, SessionRole, WorkflowEdge,
        WorkflowNode, WorkflowRun, WorkspaceState,
    },
    preferences, provider, state, workflow,
};

const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const CURRENT_BIND_PROVIDER_TIMEOUT_MS: u64 = 1_500;

#[derive(Debug, Error)]
#[error("no matching active session")]
pub(crate) struct NoMatchingActiveSession;

fn require_transition(
    subject: LifecycleSubject,
    current: LifecycleStatus,
    next: LifecycleStatus,
) -> Result<()> {
    if current.can_transition_to(subject, next) {
        Ok(())
    } else {
        bail!("invalid {subject} lifecycle transition: {current} -> {next}")
    }
}

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
    pub runtime_timeout_seconds: Option<u64>,
    pub idle_timeout_seconds: Option<u64>,
    pub source: RegistrationSource,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SessionLease {
    pub runtime_timeout_seconds: Option<u64>,
    pub idle_timeout_seconds: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateNativeSession {
    pub harness: String,
    pub native_id: String,
    pub canonical_id: String,
    pub duplicate_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorReport {
    pub scope: String,
    pub repaired: bool,
    pub duplicates: Vec<DuplicateNativeSession>,
    pub run_issues: Vec<workflow::RunRecoveryIssue>,
    pub repaired_runs: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct NodeSpec {
    pub id: String,
    pub contract: Contract,
    pub session_id: Option<String>,
    pub status: Option<LifecycleStatus>,
    pub attempt: Option<u32>,
    pub depends_on: Vec<String>,
    pub execution: Option<String>,
    pub judge_policy: Option<JudgePolicy>,
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
    env::var("ORC_NATIVE_SESSION_ID")
        .ok()
        .filter(|value| !value.is_empty())
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
    crate::control_plane::project_workspace(state::read(&scope)?)
}

pub fn doctor(config: &Config, scope: &Path, repair: bool) -> Result<DoctorReport> {
    let scope = state::resolve_scope(scope)?;
    let mut duplicates = if repair {
        state::update(&scope, |workspace| {
            repair_duplicate_native_sessions(workspace, true)
        })?
    } else {
        let mut workspace = state::read(&scope)?;
        repair_duplicate_native_sessions(&mut workspace, false)?
    };
    let autonomy = preferences::read(&scope)?.autonomy;
    let workspace = state::read(&scope)?;
    let initial_issues = run_recovery_issues(&scope, &workspace, autonomy)?;
    let mut repaired_runs = Vec::new();
    if repair {
        for issue in &initial_issues {
            if matches!(
                workflow::recover_inconsistent_run(
                    config,
                    &scope,
                    &issue.run_id,
                    autonomy,
                    Utc::now(),
                )?,
                workflow::RunRecoveryOutcome::Blocked
                    | workflow::RunRecoveryOutcome::Restarted
                    | workflow::RunRecoveryOutcome::Reconciled
            ) {
                repaired_runs.push(issue.run_id.clone());
            }
        }
    }
    let workspace = state::read(&scope)?;
    let run_issues = run_recovery_issues(&scope, &workspace, autonomy)?;
    duplicates.repaired |= !repaired_runs.is_empty();
    duplicates.run_issues = run_issues;
    duplicates.repaired_runs = repaired_runs;
    Ok(duplicates)
}

fn run_recovery_issues(
    scope: &Path,
    workspace: &WorkspaceState,
    autonomy: preferences::AutonomyMode,
) -> Result<Vec<workflow::RunRecoveryIssue>> {
    Ok(workspace
        .runs
        .iter()
        .map(|run| workflow::inspect_run_recovery(scope, workspace, run, autonomy))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect())
}

fn repair_duplicate_native_sessions(
    workspace: &mut WorkspaceState,
    repair: bool,
) -> Result<DoctorReport> {
    let mut groups = BTreeMap::<(String, String), Vec<usize>>::new();
    for (index, session) in workspace.sessions.iter().enumerate() {
        if session.status.active() && session.status != LifecycleStatus::Terminating {
            groups
                .entry((session.harness.clone(), session.native_id.clone()))
                .or_default()
                .push(index);
        }
    }
    let mut duplicates = Vec::new();
    for ((harness, native_id), indices) in
        groups.into_iter().filter(|(_, indices)| indices.len() > 1)
    {
        let canonical = indices
            .iter()
            .copied()
            .max_by_key(|index| {
                let session = &workspace.sessions[*index];
                (
                    session.updated_at,
                    session.connected_at,
                    session.role == SessionRole::Orchestrator,
                    session.id.clone(),
                )
            })
            .context("duplicate session group has no canonical record")?;
        let canonical_id = workspace.sessions[canonical].id.clone();
        let mut duplicate_ids = indices
            .into_iter()
            .filter(|index| *index != canonical)
            .map(|index| workspace.sessions[index].id.clone())
            .collect::<Vec<_>>();
        duplicate_ids.sort();
        duplicates.push(DuplicateNativeSession {
            harness,
            native_id,
            canonical_id: canonical_id.clone(),
            duplicate_ids: duplicate_ids.clone(),
        });
        if !repair {
            continue;
        }
        let now = Utc::now();
        let forbidden_parents = duplicate_ids
            .iter()
            .chain(std::iter::once(&canonical_id))
            .cloned()
            .collect::<BTreeSet<_>>();
        if parent_chain_reaches_any(
            workspace,
            workspace.sessions[canonical].parent_id.as_deref(),
            &forbidden_parents,
        ) {
            workspace.sessions[canonical].parent_id = None;
            workspace.sessions[canonical].updated_at = now;
        }
        for session in &mut workspace.sessions {
            if duplicate_ids.contains(&session.id) {
                session.status = LifecycleStatus::Archived;
                session.updated_at = now;
            }
        }
        reparent_active_descendants(workspace, &duplicate_ids, &canonical_id);
        for run in workspace.runs.iter_mut().filter(|run| run.status.active()) {
            if run
                .orchestrator_id
                .as_ref()
                .is_some_and(|id| duplicate_ids.contains(id))
            {
                run.orchestrator_id = Some(canonical_id.clone());
                run.updated_at = now;
            }
            for node in &mut run.nodes {
                if node
                    .session_id
                    .as_ref()
                    .is_some_and(|id| duplicate_ids.contains(id))
                {
                    node.session_id = Some(canonical_id.clone());
                    node.updated_at = now;
                    run.updated_at = now;
                }
            }
        }
    }
    duplicates.sort_by(|left, right| {
        (&left.harness, &left.native_id).cmp(&(&right.harness, &right.native_id))
    });
    Ok(DoctorReport {
        scope: workspace.scope.clone(),
        repaired: repair && !duplicates.is_empty(),
        duplicates,
        run_issues: Vec::new(),
        repaired_runs: Vec::new(),
    })
}

fn parent_chain_reaches_any<'a>(
    workspace: &'a WorkspaceState,
    mut parent_id: Option<&'a str>,
    targets: &BTreeSet<String>,
) -> bool {
    let mut visited = BTreeSet::new();
    while let Some(id) = parent_id {
        if targets.contains(id) || !visited.insert(id) {
            return true;
        }
        parent_id = workspace
            .sessions
            .iter()
            .find(|session| session.id == id)
            .and_then(|session| session.parent_id.as_deref());
    }
    false
}

pub fn register(scope: &Path, mut contract: Contract, link: SessionLink) -> Result<Session> {
    let environment_id = env::var("ORC_SESSION_ID").ok();
    register_for_caller(scope, &mut contract, link, environment_id.as_deref(), None)
}

pub fn register_managed(
    config: &Config,
    scope: &Path,
    contract: Contract,
    link: SessionLink,
) -> Result<Session> {
    register_managed_with_execution(config, scope, contract, link, None)
}

pub(crate) fn register_managed_with_execution(
    config: &Config,
    scope: &Path,
    mut contract: Contract,
    mut link: SessionLink,
    execution_provider: Option<&str>,
) -> Result<Session> {
    let providers = provider::discover(config)?;
    let scope = state::resolve_scope(scope)?;
    let lifecycle_bindings = provider::launch_lifecycle_bindings(
        config,
        &providers,
        &scope,
        "pending",
        execution_provider,
    )?;
    let environment_id = env::var("ORC_SESSION_ID").ok();
    link.source = RegistrationSource::Managed;
    register_for_caller(
        &scope,
        &mut contract,
        link,
        environment_id.as_deref(),
        Some(lifecycle_bindings),
    )
}

fn register_for_caller(
    scope: &Path,
    contract: &mut Contract,
    link: SessionLink,
    environment_id: Option<&str>,
    lifecycle_bindings: Option<Vec<ProviderBinding>>,
) -> Result<Session> {
    let scope = state::resolve_scope(scope)?;
    let native_id = link.native_id.unwrap_or_else(inferred_native_id);
    let explicit_id = link.id;
    state::update(&scope, |workspace| {
        let inferred_id = inferred_session_id(&contract.harness, &native_id);
        let caller = environment_id
            .map(|caller_id| {
                workspace
                    .current_session_for(Some(caller_id))
                    .or_else(|| {
                        workspace.sessions.iter().find(|session| {
                            session.id == caller_id
                                && session.registration == RegistrationSource::Managed
                                && session.status == LifecycleStatus::Disconnected
                        })
                    })
                    .cloned()
                    .with_context(|| format!("inactive or unknown Orc session: {caller_id}"))
            })
            .transpose()?;
        let refreshes_caller = caller.as_ref().is_some_and(|caller| {
            (explicit_id.as_deref() == Some(caller.id.as_str())
                && caller.harness == contract.harness)
                || caller.has_native_identity(&contract.harness, &native_id)
        });
        let base_id = registration_base_id(
            workspace,
            explicit_id.as_deref(),
            refreshes_caller.then_some(environment_id).flatten(),
            &inferred_id,
        );
        let explicit_match = explicit_id.as_deref().and_then(|id| {
            workspace
                .sessions
                .iter()
                .find(|session| session.status != LifecycleStatus::Archived && session.id == id)
        });
        if let Some(explicit_match) = explicit_match
            && explicit_match.harness != contract.harness
        {
            bail!(
                "session id {} belongs to harness {}, not {}",
                explicit_match.id,
                explicit_match.harness,
                contract.harness
            );
        }
        let native_match = workspace
            .sessions
            .iter()
            .filter(|session| {
                session.status != LifecycleStatus::Archived
                    && session.has_native_identity(&contract.harness, &native_id)
            })
            .max_by_key(|session| session.updated_at);
        if let (Some(explicit_match), Some(native_match)) = (explicit_match, native_match)
            && explicit_match.id != native_match.id
        {
            bail!(
                "session id {} and native session {} identify different Orc sessions",
                explicit_match.id,
                native_id
            );
        }
        let current = explicit_match
            .or(native_match)
            .or_else(|| {
                workspace.sessions.iter().find(|session| {
                    session.status != LifecycleStatus::Archived && session.id == base_id
                })
            })
            .cloned();
        if let Some(existing) = current.as_ref()
            && explicit_id.as_deref() == Some(existing.id.as_str())
            && existing.native_id != native_id
            && existing.registration != RegistrationSource::Managed
            && !refreshes_caller
        {
            bail!(
                "session id {} already belongs to another native session",
                existing.id
            );
        }
        if let Some(caller) = caller.as_ref()
            && caller.role != SessionRole::Orchestrator
            && !refreshes_caller
        {
            bail!("a managed child can only refresh its own Orc registration");
        }
        let reconnects_native = current
            .as_ref()
            .is_some_and(|session| session.has_native_identity(&contract.harness, &native_id));
        let governed = current.as_ref().filter(|session| {
            session.registration == RegistrationSource::Managed
                || refreshes_caller
                || reconnects_native
        });
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
                    && session.status != LifecycleStatus::Terminating
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
            && !workspace.sessions.iter().any(|session| {
                session.role == SessionRole::Orchestrator
                    && session.status != LifecycleStatus::Archived
                    && current
                        .as_ref()
                        .is_none_or(|current| session.id != current.id)
            })
        {
            contract.role = SessionRole::Orchestrator;
        }
        let role = governed.map_or(contract.role, |session| session.role);
        if role == SessionRole::Orchestrator
            && let Some(existing) = workspace.sessions.iter().find(|session| {
                session.role == SessionRole::Orchestrator
                    && session.status != LifecycleStatus::Archived
                    && current
                        .as_ref()
                        .is_none_or(|current| session.id != current.id)
            })
        {
            bail!(
                "workspace already has orchestrator {}; use `orc session adopt` to replace it",
                existing.id
            );
        }
        let requested_run_id = governed
            .and_then(|session| session.run_id.clone())
            .or_else(|| link.run_id.clone());
        let run_orchestrator = requested_run_id.as_deref().and_then(|run_id| {
            workspace
                .runs
                .iter()
                .find(|run| run.id == run_id)
                .and_then(|run| run.orchestrator_id.clone())
        });
        let explicit_parent = explicit_parent
            .map(|parent_id| {
                if workspace.sessions.iter().any(|session| {
                    session.id == parent_id
                        && session.status.active()
                        && session.status != LifecycleStatus::Terminating
                }) {
                    return Ok(parent_id);
                }
                let archived_orchestrator = workspace.sessions.iter().any(|session| {
                    session.id == parent_id
                        && session.role == SessionRole::Orchestrator
                        && session.status == LifecycleStatus::Archived
                });
                if archived_orchestrator {
                    return run_orchestrator
                        .clone()
                        .or_else(|| active_orchestrator.clone())
                        .context("the replaced orchestrator has no active successor");
                }
                bail!("inactive or unknown parent Orc session: {parent_id}")
            })
            .transpose()?;
        if let (Some(parent_id), Some(orchestrator_id)) =
            (explicit_parent.as_deref(), run_orchestrator.as_deref())
            && parent_id != orchestrator_id
            && !workspace.sessions.iter().any(|session| {
                session.id == parent_id
                    && session.run_id.as_deref() == requested_run_id.as_deref()
                    && session.status.active()
                    && session.status != LifecycleStatus::Terminating
            })
        {
            bail!(
                "parent session {parent_id} does not own workflow run {}",
                requested_run_id.as_deref().unwrap_or_default()
            );
        }
        let parent_id = governed
            .and_then(|session| session.parent_id.clone())
            .or_else(|| {
                explicit_parent.or(run_orchestrator).or_else(|| {
                    (role != SessionRole::Orchestrator)
                        .then_some(active_orchestrator)
                        .flatten()
                })
            });
        let now = Utc::now();
        let session_native_id =
            governed.map_or_else(|| native_id.clone(), |session| session.native_id.clone());
        let mut initial_bindings = lifecycle_bindings.clone().unwrap_or_default();
        for binding in &mut initial_bindings {
            binding.r#ref = Some(id.clone());
        }
        let session_providers = current
            .as_ref()
            .map(|session| session.providers.clone())
            .filter(|bindings| !bindings.is_empty())
            .unwrap_or(initial_bindings);
        let mut provider_revisions = current
            .as_ref()
            .map(|session| session.provider_revisions.clone())
            .unwrap_or_default();
        bump_changed_provider_revisions(
            &mut provider_revisions,
            current
                .as_ref()
                .map(|session| session.providers.as_slice())
                .unwrap_or_default(),
            &session_providers,
        );
        let registration = governed.map_or(link.source, |session| session.registration);
        if registration == RegistrationSource::Managed && session_providers.is_empty() {
            bail!(
                "managed session registration requires an unambiguous lifecycle owner; use Orc's managed launch path"
            );
        }
        let mut session = Session {
            id,
            native_id: session_native_id.clone(),
            trace_id: governed
                .and_then(|session| session.trace_id.clone())
                .or(Some(session_native_id)),
            harness: governed.map_or_else(
                || contract.harness.clone(),
                |session| session.harness.clone(),
            ),
            model: governed.map_or_else(
                || {
                    contract
                        .model
                        .clone()
                        .or_else(|| current.as_ref().and_then(|session| session.model.clone()))
                },
                |session| session.model.clone(),
            ),
            role,
            title: governed.map_or_else(|| contract.title.clone(), |session| session.title.clone()),
            purpose: governed.map_or_else(
                || contract.purpose.clone(),
                |session| session.purpose.clone(),
            ),
            goal: governed.map_or_else(|| contract.goal.clone(), |session| session.goal.clone()),
            expected_output: governed.map_or_else(
                || contract.expected_output.clone(),
                |session| session.expected_output.clone(),
            ),
            reported_output: current
                .as_ref()
                .and_then(|session| session.reported_output.clone()),
            success_criteria: governed.map_or_else(
                || contract.success_criteria.clone(),
                |session| session.success_criteria.clone(),
            ),
            completion: governed.map_or(contract.completion, |session| session.completion),
            review_by: governed
                .and_then(|session| session.review_by.clone())
                .or(contract.review_by.clone()),
            parent_id,
            run_id: governed
                .and_then(|session| session.run_id.clone())
                .or(link.run_id),
            node_id: governed
                .and_then(|session| session.node_id.clone())
                .or(link.node_id),
            provider_ref: governed
                .map(|session| session.provider_ref.clone())
                .unwrap_or_else(|| {
                    link.provider_ref
                        .or_else(|| env::var("ORC_PROVIDER_REF").ok())
                }),
            providers: session_providers,
            provider_revisions,
            directory: scope.display().to_string(),
            registration,
            status: governed.map_or(LifecycleStatus::Working, |session| session.status),
            runtime_timeout_seconds: governed.map_or(link.runtime_timeout_seconds, |session| {
                session.runtime_timeout_seconds
            }),
            idle_timeout_seconds: governed.map_or(link.idle_timeout_seconds, |session| {
                session.idle_timeout_seconds
            }),
            heartbeat_at: Some(now),
            termination_reason: governed.and_then(|session| session.termination_reason.clone()),
            termination_cause: governed.and_then(|session| session.termination_cause.clone()),
            termination_attempt_at: governed.and_then(|session| session.termination_attempt_at),
            termination_operation_id: governed
                .and_then(|session| session.termination_operation_id.clone()),
            connected_at: current
                .as_ref()
                .map(|session| session.connected_at)
                .unwrap_or(now),
            updated_at: now,
        };
        if session.status == LifecycleStatus::Disconnected {
            session.status = LifecycleStatus::Working;
            session.termination_reason = None;
            session.termination_cause = None;
            session.termination_attempt_at = None;
            session.termination_operation_id = None;
        }
        let mut duplicate_ids = workspace
            .sessions
            .iter_mut()
            .filter(|candidate| {
                candidate.id != session.id
                    && candidate.has_native_identity(&session.harness, &session.native_id)
                    && candidate.status != LifecycleStatus::Archived
                    && candidate.status != LifecycleStatus::Terminating
            })
            .map(|candidate| {
                candidate.status = LifecycleStatus::Archived;
                candidate.updated_at = now;
                candidate.id.clone()
            })
            .collect::<Vec<_>>();
        if session.role == SessionRole::Orchestrator {
            duplicate_ids.extend(
                workspace
                    .sessions
                    .iter()
                    .filter(|candidate| {
                        candidate.role == SessionRole::Orchestrator
                            && candidate.status == LifecycleStatus::Archived
                            && candidate.id != session.id
                    })
                    .map(|candidate| candidate.id.clone()),
            );
            duplicate_ids.sort();
            duplicate_ids.dedup();
        }
        reparent_active_descendants(workspace, &duplicate_ids, &session.id);
        for run in &mut workspace.runs {
            if run.status.active()
                && run
                    .orchestrator_id
                    .as_ref()
                    .is_some_and(|id| duplicate_ids.contains(id))
            {
                run.orchestrator_id = Some(session.id.clone());
                run.updated_at = now;
            }
        }
        workspace
            .sessions
            .retain(|candidate| candidate.id != session.id);
        workspace.sessions.insert(0, session.clone());
        workspace.active = true;
        Ok(session)
    })
}

fn registration_base_id(
    workspace: &WorkspaceState,
    explicit_id: Option<&str>,
    environment_id: Option<&str>,
    inferred_id: &str,
) -> String {
    explicit_id
        .map(str::to_owned)
        .or_else(|| {
            environment_id
                .filter(|id| {
                    !workspace.sessions.iter().any(|session| {
                        session.id == *id && session.status == LifecycleStatus::Archived
                    })
                })
                .map(str::to_owned)
        })
        .unwrap_or_else(|| inferred_id.to_owned())
}

pub fn adopt(scope: &Path, mut contract: Contract, native_id: Option<String>) -> Result<Session> {
    contract.role = SessionRole::Orchestrator;
    let scope = state::resolve_scope(scope)?;
    let now = Utc::now();
    state::update(&scope, |workspace| {
        let native_id = native_id.clone().unwrap_or_else(inferred_native_id);
        if let Some(terminating) = workspace.sessions.iter().find(|session| {
            session.role == SessionRole::Orchestrator
                && session.status == LifecycleStatus::Terminating
        }) {
            bail!(
                "orchestrator transition is already in progress: {}",
                terminating.id
            );
        }
        if let Some(owner) = workspace.sessions.iter().find(|session| {
            session.has_native_identity(&contract.harness, &native_id)
                && session.role != SessionRole::Orchestrator
                && session.status != LifecycleStatus::Archived
        }) {
            bail!(
                "native session {} already belongs to {}; choose another session or archive it first",
                native_id,
                owner.id
            );
        }
        let mut replaced_orchestrators = workspace
            .sessions
            .iter()
            .filter(|session| session.role == SessionRole::Orchestrator)
            .map(|session| session.id.clone())
            .collect::<Vec<_>>();
        replaced_orchestrators.sort();
        replaced_orchestrators.dedup();
        for session in &mut workspace.sessions {
            if session.role == SessionRole::Orchestrator
                && session.status != LifecycleStatus::Terminating
                && session.status != LifecycleStatus::Archived
            {
                session.status = LifecycleStatus::Archived;
                session.updated_at = now;
            }
        }
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
            reported_output: None,
            success_criteria: contract.success_criteria.clone(),
            completion: contract.completion,
            review_by: contract.review_by.clone(),
            parent_id: None,
            run_id: None,
            node_id: None,
            provider_ref: None,
            providers: Vec::new(),
            provider_revisions: BTreeMap::new(),
            directory: scope.display().to_string(),
            registration: RegistrationSource::Connected,
            status: LifecycleStatus::Working,
            runtime_timeout_seconds: None,
            idle_timeout_seconds: None,
            heartbeat_at: Some(now),
            termination_reason: None,
            termination_cause: None,
            termination_attempt_at: None,
            termination_operation_id: None,
            connected_at: now,
            updated_at: now,
        };
        reparent_active_descendants(workspace, &replaced_orchestrators, &session.id);
        for run in &mut workspace.runs {
            if run.status.active()
                && run
                    .orchestrator_id
                    .as_ref()
                    .is_some_and(|id| replaced_orchestrators.contains(id))
            {
                run.orchestrator_id = Some(session.id.clone());
                run.updated_at = now;
            }
        }
        workspace.sessions.insert(0, session.clone());
        workspace.active = true;
        Ok(session)
    })
}

fn reparent_active_descendants(
    workspace: &mut WorkspaceState,
    replaced_orchestrators: &[String],
    new_orchestrator: &str,
) {
    for session in &mut workspace.sessions {
        if (session.status.active()
            || (session.registration == RegistrationSource::Managed
                && session.status == LifecycleStatus::Disconnected))
            && session.status != LifecycleStatus::Terminating
            && session
                .parent_id
                .as_ref()
                .is_some_and(|id| replaced_orchestrators.contains(id))
        {
            session.parent_id = Some(new_orchestrator.to_owned());
            session.updated_at = Utc::now();
        }
    }
}

pub fn update_session(scope: &Path, id: &str, status: LifecycleStatus) -> Result<Session> {
    if matches!(
        status,
        LifecycleStatus::Cancelled | LifecycleStatus::Terminating
    ) {
        bail!("session cancellation must use the provider termination operation");
    }
    let scope = state::resolve_scope(scope)?;
    state::update(&scope, |workspace| {
        let session = workspace
            .sessions
            .iter_mut()
            .find(|session| session.id == id)
            .with_context(|| format!("unknown session: {id}"))?;
        if session
            .termination_reason
            .as_deref()
            .is_some_and(|reason| reason.starts_with("termination pending "))
        {
            bail!("session termination is in progress: {id}");
        }
        if session.status == LifecycleStatus::Cancelled && session.termination_reason.is_some() {
            bail!("session was terminated and must be registered as a new session: {id}");
        }
        require_transition(LifecycleSubject::Session, session.status, status)?;
        session.status = status;
        let now = Utc::now();
        if status.active() {
            session.termination_reason = None;
            session.termination_cause = None;
            session.termination_attempt_at = None;
            session.termination_operation_id = None;
        }
        session.updated_at = now;
        Ok(session.clone())
    })
}

pub fn keepalive(scope: &Path, id: &str) -> Result<Session> {
    let scope = state::resolve_scope(scope)?;
    state::update(&scope, |workspace| {
        let session = workspace
            .sessions
            .iter_mut()
            .find(|session| session.id == id)
            .with_context(|| format!("unknown session: {id}"))?;
        if !session.status.active() {
            bail!("session is not active: {id}");
        }
        if session
            .termination_reason
            .as_deref()
            .is_some_and(|reason| reason.starts_with("termination pending "))
        {
            bail!("session termination is in progress: {id}");
        }
        if session.registration != RegistrationSource::Managed {
            bail!("only managed sessions have renewable leases");
        }
        let now = Utc::now();
        session.heartbeat_at = Some(now);
        session.updated_at = now;
        Ok(session.clone())
    })
}

pub fn terminate(config: &Config, scope: &Path, id: &str, reason: &str) -> Result<Session> {
    let scope = state::resolve_scope(scope)?;
    terminate_resolved(config, &scope, id, reason, None, None)
}

pub fn terminate_expired(
    config: &Config,
    scope: &Path,
    id: &str,
    reason: &str,
    observed_heartbeat: Option<Option<chrono::DateTime<Utc>>>,
    observed_claim: Option<&str>,
) -> Result<Session> {
    let scope = state::resolve_scope(scope)?;
    terminate_resolved(
        config,
        &scope,
        id,
        reason,
        observed_heartbeat,
        observed_claim,
    )
}

pub(crate) fn terminate_expired_persisted_scope(
    config: &Config,
    scope: &Path,
    id: &str,
    reason: &str,
    observed_heartbeat: Option<Option<chrono::DateTime<Utc>>>,
    observed_claim: Option<&str>,
) -> Result<Session> {
    if !scope.is_absolute() {
        bail!(
            "persisted workspace scope must be absolute: {}",
            scope.display()
        );
    }
    terminate_resolved(
        config,
        scope,
        id,
        reason,
        observed_heartbeat,
        observed_claim,
    )
}

fn terminate_resolved(
    config: &Config,
    scope: &Path,
    id: &str,
    reason: &str,
    expected_heartbeat: Option<Option<chrono::DateTime<Utc>>>,
    expected_claim: Option<&str>,
) -> Result<Session> {
    let snapshot = state::read(scope)?;
    let session = selected_session(&snapshot, id)?.clone();
    if !terminable(&session) {
        return Ok(session);
    }
    let providers = provider::discover(config)?;
    let bindings = provider::discover_bindings(config, &providers, scope, &session, false);
    if !bindings.is_empty() {
        state::update(scope, |workspace| {
            let selected = workspace
                .sessions
                .iter_mut()
                .find(|candidate| candidate.id == id)
                .with_context(|| format!("unknown session: {id}"))?;
            let reconciled = reconcile_termination_bindings(&session.providers, &bindings);
            let merged = merge_observed_bindings(&session, selected, &reconciled);
            if replace_provider_bindings(selected, merged) {
                touch_session(selected);
            }
            Ok(())
        })?;
    }
    let session = selected_session(&state::read(scope)?, id)?.clone();
    let operation_id =
        termination_operation_id(expected_claim, session.termination_operation_id.as_deref());
    let termination_cause =
        persisted_termination_cause(session.termination_cause.as_deref(), reason);
    let claim = format!("termination pending {operation_id}");
    let Some(_termination_guard) = TerminationGuard::acquire(scope, id, config.provider_timeout())?
    else {
        let message = "another managed-session termination owns this lock shard";
        state::update(scope, |workspace| {
            let selected = workspace
                .sessions
                .iter_mut()
                .find(|candidate| candidate.id == id)
                .with_context(|| format!("unknown session: {id}"))?;
            let pending = selected
                .termination_reason
                .as_deref()
                .is_some_and(|reason| reason.starts_with("termination pending "));
            if terminable(selected) && !pending {
                let now = Utc::now();
                selected.termination_reason = Some(format!("termination failed: {message}"));
                selected.termination_cause = Some(termination_cause.clone());
                selected.termination_attempt_at = Some(now);
                selected.updated_at = now;
            }
            Ok(())
        })?;
        bail!(message);
    };
    let claimed_status = state::update(scope, |workspace| {
        let selected = workspace
            .sessions
            .iter_mut()
            .find(|candidate| candidate.id == id)
            .with_context(|| format!("unknown session: {id}"))?;
        if !terminable(selected) {
            return Ok(None);
        }
        let pending_claim = selected
            .termination_reason
            .as_deref()
            .filter(|reason| reason.starts_with("termination pending "));
        if pending_claim.is_some() && pending_claim != expected_claim {
            return Ok(None);
        }
        if let Some(expected) = expected_heartbeat
            && selected.heartbeat_at != expected
        {
            return Ok(None);
        }
        let now = Utc::now();
        let claimed_status = selected.status;
        selected.status = LifecycleStatus::Terminating;
        selected.termination_reason = Some(claim.clone());
        selected.termination_cause = Some(termination_cause.clone());
        selected.termination_attempt_at = Some(now);
        selected.termination_operation_id = Some(operation_id.clone());
        selected.updated_at = now;
        Ok(Some(claimed_status))
    })?;
    let Some(claimed_status) = claimed_status else {
        return selected_session(&state::read(scope)?, id).cloned();
    };
    let result = (|| {
        let actions = [
            (
                provider::Action::Stop,
                provider::Capability::SessionStop,
                "session stop",
            ),
            (
                provider::Action::Cancel,
                provider::Capability::ExecutionCancel,
                "execution cancel",
            ),
        ]
        .into_iter()
        .filter(|(_, capability, _)| lifecycle_action_owned(&providers, &session, *capability))
        .collect::<Vec<_>>();
        if actions.is_empty() {
            bail!("no provider can stop or cancel this active agent");
        }
        let mut failures = Vec::new();
        for (action, _, label) in actions {
            let outcome = (|| {
                let mut request = provider::action_request(action, scope, Some(&session), "right");
                request["operationId"] = serde_json::Value::String(operation_id.clone());
                let plan = provider::resolve_plan(config, &providers, action, request)?;
                let result =
                    provider::run_plan_with_timeout(&plan, scope, config.provider_timeout())?;
                if !plan.accepts(result.code) {
                    bail!(
                        "provider exited with {}: {}",
                        result.code,
                        result.stderr.trim()
                    );
                }
                Ok(())
            })();
            if let Err(error) = outcome {
                failures.push(format!("{label}: {error:#}"));
            }
        }
        if !failures.is_empty() {
            bail!("session termination failed: {}", failures.join("; "));
        }
        Ok(())
    })();
    if let Err(error) = result {
        state::update(scope, |workspace| {
            let selected = workspace
                .sessions
                .iter_mut()
                .find(|candidate| candidate.id == id)
                .with_context(|| format!("unknown session: {id}"))?;
            if selected.termination_reason.as_deref() == Some(claim.as_str()) {
                selected.status = claimed_status;
                selected.termination_reason = Some(format!("termination failed: {error:#}"));
                selected.updated_at = Utc::now();
            }
            Ok(())
        })?;
        return Err(error);
    }
    state::update(scope, |workspace| {
        let session = workspace
            .sessions
            .iter_mut()
            .find(|session| session.id == id)
            .with_context(|| format!("unknown session: {id}"))?;
        if session.termination_reason.as_deref() == Some(claim.as_str()) {
            session.status = LifecycleStatus::Cancelled;
            session.termination_reason = Some(termination_cause.clone());
            session.updated_at = Utc::now();
        }
        Ok(session.clone())
    })
}

fn lifecycle_action_owned(
    providers: &[provider::Manifest],
    session: &Session,
    capability: provider::Capability,
) -> bool {
    session.providers.iter().any(|binding| {
        binding.status == BindingStatus::Active
            && binding
                .r#ref
                .as_deref()
                .is_some_and(|reference| !reference.is_empty())
            && providers
                .iter()
                .find(|candidate| candidate.name == binding.provider)
                .is_some_and(|provider| provider.supports(capability))
    })
}

fn termination_operation_id(expected_claim: Option<&str>, persisted: Option<&str>) -> String {
    expected_claim
        .and_then(|claim| claim.strip_prefix("termination pending "))
        .or(persisted)
        .map(str::to_owned)
        .unwrap_or_else(|| Uuid::new_v4().to_string())
}

fn persisted_termination_cause(existing: Option<&str>, reason: &str) -> String {
    match existing {
        Some("idle timeout exceeded") if reason == "runtime timeout exceeded" => reason.to_owned(),
        Some(existing) => existing.to_owned(),
        None => reason.to_owned(),
    }
}

struct TerminationGuard {
    file: File,
    path: PathBuf,
}

impl TerminationGuard {
    fn acquire(
        scope: &Path,
        session_id: &str,
        timeout: std::time::Duration,
    ) -> Result<Option<Self>> {
        let path = termination_lock_path(scope, session_id);
        let directory = path.parent().context("termination lock has no directory")?;
        fs::create_dir_all(directory).context("create termination lock directory")?;
        #[cfg(unix)]
        {
            let deadline = std::time::Instant::now()
                .checked_add(timeout)
                .context("termination lock timeout exceeds the platform clock range")?;
            loop {
                if let Some(_gate) = TerminationGateGuard::try_acquire(&path)? {
                    let file = open_termination_lock(&path)?;
                    if try_lock(&file)? {
                        return Ok(Some(Self { file, path }));
                    }
                }
                if std::time::Instant::now() >= deadline {
                    return Ok(None);
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        #[cfg(not(unix))]
        Ok(Some(Self {
            file: open_termination_lock(&path)?,
            path,
        }))
    }
}

impl Drop for TerminationGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            if let Ok(_gate) = TerminationGateGuard::acquire(&self.path) {
                let _ = fs::remove_file(&self.path);
                unlock(&self.file);
                return;
            }
            unlock(&self.file);
        }
        #[cfg(not(unix))]
        let _ = fs::remove_file(&self.path);
    }
}

pub fn termination_claim_active(scope: &Path, session_id: &str, claim: &str) -> Result<bool> {
    if !claim.starts_with("termination pending ") {
        return Ok(false);
    }
    let path = termination_lock_path(scope, session_id);
    #[cfg(unix)]
    {
        let _gate = TerminationGateGuard::acquire(&path)?;
        let file = match OpenOptions::new().read(true).write(true).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error).context("inspect termination lock"),
        };
        if !try_lock(&file)? {
            return Ok(true);
        }
        fs::remove_file(&path).context("remove released termination lock")?;
        unlock(&file);
    }
    #[cfg(not(unix))]
    return Ok(path.exists());
    #[cfg(unix)]
    Ok(false)
}

fn termination_lock_path(scope: &Path, session_id: &str) -> PathBuf {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    #[cfg(unix)]
    hasher.update(scope.as_os_str().as_bytes());
    #[cfg(not(unix))]
    hasher.update(scope.as_os_str().to_string_lossy().as_bytes());
    hasher.update([0]);
    hasher.update(session_id.as_bytes());
    let name = hex::encode(hasher.finalize());
    crate::config::state_home()
        .join("orc/terminations")
        .join(format!("{name}.lock"))
}

fn open_termination_lock(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .context("open termination lock")
}

#[cfg(unix)]
struct TerminationGateGuard {
    file: File,
}

#[cfg(unix)]
impl TerminationGateGuard {
    fn acquire(path: &Path) -> Result<Self> {
        let file = open_termination_gate_lock(path)?;
        lock(&file)?;
        Ok(Self { file })
    }

    fn try_acquire(path: &Path) -> Result<Option<Self>> {
        let file = open_termination_gate_lock(path)?;
        try_lock(&file).map(|locked| locked.then_some(Self { file }))
    }
}

#[cfg(unix)]
impl Drop for TerminationGateGuard {
    fn drop(&mut self) {
        unlock(&self.file);
    }
}

#[cfg(unix)]
fn open_termination_gate_lock(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path.with_extension("guard"))
        .context("open termination lock cleanup guard")
}

#[cfg(unix)]
fn lock(file: &File) -> Result<()> {
    unsafe extern "C" {
        fn flock(fd: i32, operation: i32) -> i32;
    }
    const LOCK_EX: i32 = 2;
    if unsafe { flock(file.as_raw_fd(), LOCK_EX) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error()).context("acquire termination lock cleanup guard")
    }
}

#[cfg(unix)]
fn try_lock(file: &File) -> Result<bool> {
    unsafe extern "C" {
        fn flock(fd: i32, operation: i32) -> i32;
    }
    const LOCK_EX: i32 = 2;
    const LOCK_NB: i32 = 4;
    if unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::WouldBlock {
        Ok(false)
    } else {
        Err(error).context("acquire termination lock")
    }
}

#[cfg(unix)]
fn unlock(file: &File) {
    unsafe extern "C" {
        fn flock(fd: i32, operation: i32) -> i32;
    }
    const LOCK_UN: i32 = 8;
    let _ = unsafe { flock(file.as_raw_fd(), LOCK_UN) };
}

pub(crate) fn terminable(session: &Session) -> bool {
    session.status.active()
        || (session.registration == RegistrationSource::Managed
            && matches!(
                session.status,
                LifecycleStatus::Disconnected | LifecycleStatus::Failed
            ))
}

pub(crate) fn has_active_session(
    scope: &Path,
    id: Option<&str>,
    native_identity: Option<(&str, &str)>,
) -> Result<bool> {
    Ok(read_workspace(scope)?.sessions.iter().any(|session| {
        session.status.active()
            && if let Some(id) = id {
                session.id == id
            } else {
                native_identity
                    .is_some_and(|(harness, native)| session.has_native_identity(harness, native))
            }
    }))
}

pub fn archive(
    scope: &Path,
    id: Option<&str>,
    native_identity: Option<(&str, &str)>,
) -> Result<Session> {
    let scope = state::resolve_scope(scope)?;
    state::update(&scope, |workspace| {
        let session = workspace
            .sessions
            .iter_mut()
            .filter(|session| session.status != LifecycleStatus::Archived)
            .filter(|session| {
                if let Some(id) = id {
                    session.id == id
                } else {
                    native_identity.is_some_and(|(harness, native)| {
                        session.has_native_identity(harness, native)
                    })
                }
            })
            .max_by_key(|session| session.updated_at)
            .ok_or(NoMatchingActiveSession)?;
        if session.status == LifecycleStatus::Terminating {
            bail!("session termination is in progress: {}", session.id);
        }
        session.status = LifecycleStatus::Archived;
        session.updated_at = Utc::now();
        Ok(session.clone())
    })
}

pub fn prune(config: &Config, scope: &Path, id: &str) -> Result<Session> {
    let scope = state::resolve_scope(scope)?;
    let terminated = terminate(config, &scope, id, "pruned by operator")?;
    if terminable(&terminated) {
        bail!("session is still active after its termination attempt: {id}");
    }
    archive(&scope, Some(id), None)
}

pub fn reconcile(config: &Config, scope: &Path) -> Result<WorkspaceState> {
    reconcile_with_current(config, scope, false)
}

pub fn bind_current_session(config: &Config, scope: &Path, id: &str) -> Result<WorkspaceState> {
    let scope = state::resolve_scope(scope)?;
    let providers = provider::discover(config)?;
    bind_current_session_with(config, &scope, id, |bounded_config, scope, session| {
        provider::discover_current_bindings(bounded_config, &providers, scope, session)
    })
}

fn bind_current_session_with(
    config: &Config,
    scope: &Path,
    id: &str,
    discover_current: impl FnOnce(&Config, &Path, &Session) -> Vec<ProviderBinding>,
) -> Result<WorkspaceState> {
    let snapshot = state::read(scope)?;
    let session = selected_session(&snapshot, id)?.clone();
    let bounded_config = current_bind_config(config);
    let bindings = discover_current(&bounded_config, scope, &session);
    if bindings.is_empty() {
        return Ok(snapshot);
    }
    state::update(scope, |workspace| {
        let selected = workspace
            .sessions
            .iter_mut()
            .find(|candidate| candidate.id == id)
            .with_context(|| format!("unknown session: {id}"))?;
        apply_observed_enrichment(selected, &session, &bindings, None, None);
        Ok(workspace.clone())
    })
}

fn current_bind_config(config: &Config) -> Config {
    let mut bounded = config.clone();
    bounded.providers.timeout_ms = bounded
        .providers
        .timeout_ms
        .min(CURRENT_BIND_PROVIDER_TIMEOUT_MS);
    bounded
}

pub fn reconcile_with_current(
    config: &Config,
    scope: &Path,
    rebind_current: bool,
) -> Result<WorkspaceState> {
    let scope = state::resolve_scope(scope)?;
    let providers = provider::discover(config)?;
    let snapshot = state::read(&scope)?;
    let selection = if rebind_current {
        current_rebind_selection(
            &snapshot,
            env::var("ORC_SESSION_ID").ok().as_deref(),
            env::var("ORC_HARNESS").ok().as_deref(),
            env::var("ORC_NATIVE_SESSION_ID").ok().as_deref(),
        )
    } else {
        ReconcileSelection::All
    };
    if selection == ReconcileSelection::IdentityMismatch {
        return Ok(snapshot);
    }
    let enrichments = snapshot
        .sessions
        .iter()
        .filter(|session| session.status != LifecycleStatus::Archived)
        .filter(|session| {
            matches!(&selection, ReconcileSelection::All)
                || matches!(&selection, ReconcileSelection::One(id) if session.id == *id)
        })
        .map(|session| {
            let bindings =
                provider::discover_bindings(config, &providers, &scope, session, rebind_current);
            let (title, goal) = provider::describe(config, &providers, &scope, session);
            (session.id.clone(), bindings, title, goal)
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
            let observed = snapshot
                .sessions
                .iter()
                .find(|candidate| candidate.id == *id)
                .context("observed session disappeared")?;
            apply_observed_enrichment(
                selected,
                observed,
                bindings,
                title.as_deref(),
                goal.as_deref(),
            );
        }
        Ok(workspace.clone())
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ReconcileSelection {
    All,
    One(String),
    IdentityMismatch,
}

fn current_rebind_selection(
    snapshot: &WorkspaceState,
    session_id: Option<&str>,
    harness: Option<&str>,
    native_id: Option<&str>,
) -> ReconcileSelection {
    if session_id.is_none() && harness.is_none() && native_id.is_none() {
        return ReconcileSelection::All;
    }
    if let Some(session_id) = session_id {
        let Some(session) = snapshot.current_session_for(Some(session_id)) else {
            return ReconcileSelection::IdentityMismatch;
        };
        if harness.is_some_and(|harness| harness != session.harness)
            || native_id.is_some_and(|native_id| native_id != session.native_id)
        {
            return ReconcileSelection::IdentityMismatch;
        }
        return ReconcileSelection::One(session.id.clone());
    }
    let (Some(harness), Some(native_id)) = (harness, native_id) else {
        return ReconcileSelection::IdentityMismatch;
    };
    snapshot
        .active_sessions()
        .filter(|session| {
            session.has_native_identity(harness, native_id)
                && session.status != LifecycleStatus::Terminating
        })
        .max_by_key(|session| session.updated_at)
        .map_or(ReconcileSelection::IdentityMismatch, |session| {
            ReconcileSelection::One(session.id.clone())
        })
}

#[cfg(test)]
fn apply_enrichment(
    session: &mut Session,
    bindings: &[crate::domain::ProviderBinding],
    title: Option<&str>,
    goal: Option<&str>,
) {
    let reconciled_bindings = reconcile_provider_bindings(&session.providers, bindings);
    apply_enrichment_fields(session, reconciled_bindings, title, goal);
}

fn apply_observed_enrichment(
    session: &mut Session,
    observed: &Session,
    bindings: &[crate::domain::ProviderBinding],
    title: Option<&str>,
    goal: Option<&str>,
) {
    let reconciled = reconcile_provider_bindings(&observed.providers, bindings);
    let merged = merge_observed_bindings(observed, session, &reconciled);
    apply_enrichment_fields(session, merged, title, goal);
}

fn reconcile_provider_bindings(
    previous: &[ProviderBinding],
    bindings: &[ProviderBinding],
) -> Vec<ProviderBinding> {
    let mut reconciled = bindings.to_vec();
    for previous in previous
        .iter()
        .filter(|binding| provider::is_launch_ownership(binding))
    {
        let conclusive = bindings.iter().any(|binding| {
            same_binding_slot(binding, previous)
                && matches!(
                    binding.status,
                    BindingStatus::Active | BindingStatus::Unavailable
                )
        });
        if !conclusive {
            reconciled.retain(|binding| !same_binding_slot(binding, previous));
            reconciled.push(previous.clone());
        }
    }
    reconciled
}

fn reconcile_termination_bindings(
    previous: &[ProviderBinding],
    bindings: &[ProviderBinding],
) -> Vec<ProviderBinding> {
    let mut reconciled = previous.to_vec();
    for binding in bindings {
        let existing = reconciled
            .iter()
            .position(|candidate| same_binding_slot(candidate, binding));
        if binding.status == BindingStatus::Active || existing.is_none() {
            if let Some(index) = existing {
                reconciled[index] = binding.clone();
            } else {
                reconciled.push(binding.clone());
            }
        }
    }
    reconciled
}

fn merge_observed_bindings(
    observed: &Session,
    current: &Session,
    reconciled: &[ProviderBinding],
) -> Vec<ProviderBinding> {
    let mut merged = Vec::with_capacity(current.providers.len().max(reconciled.len()));
    for candidate in reconciled {
        let latest = current
            .providers
            .iter()
            .find(|binding| same_binding_slot(binding, candidate));
        if binding_revision(current, candidate) == binding_revision(observed, candidate) {
            merged.push(candidate.clone());
        } else if let Some(latest) = latest {
            merged.push(latest.clone());
        }
    }
    for latest in &current.providers {
        if merged
            .iter()
            .any(|binding| same_binding_slot(binding, latest))
        {
            continue;
        }
        if binding_revision(current, latest) != binding_revision(observed, latest) {
            merged.push(latest.clone());
        }
    }
    merged
}

fn same_binding_slot(left: &ProviderBinding, right: &ProviderBinding) -> bool {
    left.provider == right.provider && left.kind == right.kind
}

fn binding_revision_key(provider: &str, kind: ProviderKind) -> String {
    format!("{kind}:{provider}")
}

fn binding_revision(session: &Session, binding: &ProviderBinding) -> u64 {
    session
        .provider_revisions
        .get(&binding_revision_key(&binding.provider, binding.kind))
        .copied()
        .unwrap_or_default()
}

fn bump_changed_provider_revisions(
    revisions: &mut BTreeMap<String, u64>,
    previous: &[ProviderBinding],
    current: &[ProviderBinding],
) {
    let slots = previous
        .iter()
        .chain(current)
        .map(|binding| binding_revision_key(&binding.provider, binding.kind))
        .collect::<BTreeSet<_>>();
    for slot in slots {
        let before = previous
            .iter()
            .find(|binding| binding_revision_key(&binding.provider, binding.kind) == slot);
        let after = current
            .iter()
            .find(|binding| binding_revision_key(&binding.provider, binding.kind) == slot);
        if before != after {
            let revision = revisions.entry(slot).or_default();
            *revision = revision.saturating_add(1);
        }
    }
}

fn replace_provider_bindings(session: &mut Session, bindings: Vec<ProviderBinding>) -> bool {
    let previous = std::mem::replace(&mut session.providers, bindings);
    if session.providers == previous {
        return false;
    }
    bump_changed_provider_revisions(
        &mut session.provider_revisions,
        &previous,
        &session.providers,
    );
    true
}

fn apply_enrichment_fields(
    session: &mut Session,
    reconciled_bindings: Vec<ProviderBinding>,
    title: Option<&str>,
    goal: Option<&str>,
) {
    let liveness = reconciled_liveness(&session.providers, &reconciled_bindings);
    replace_provider_bindings(session, reconciled_bindings);
    if let Some(is_live) = liveness
        && session.status != LifecycleStatus::Terminating
    {
        if is_live && session.status == LifecycleStatus::Disconnected {
            session.status = LifecycleStatus::Working;
        } else if !is_live && session.status.active() {
            session.status = LifecycleStatus::Disconnected;
        }
        touch_session(session);
    }
    if let Some(title) = title
        && (session.title == "Agent session" || session.title == session.id)
    {
        session.title = title.to_owned();
    }
    if let Some(goal) = goal
        && session.goal == "Complete the assigned work"
    {
        session.goal = goal.to_owned();
    }
}

fn touch_session(session: &mut Session) {
    let now = Utc::now();
    session.updated_at = if now > session.updated_at {
        now
    } else {
        session.updated_at + chrono::TimeDelta::nanoseconds(1)
    };
}

fn reconciled_liveness(
    previous: &[crate::domain::ProviderBinding],
    current: &[crate::domain::ProviderBinding],
) -> Option<bool> {
    if current.iter().any(is_live_binding) {
        return Some(true);
    }
    let previously_live = previous
        .iter()
        .filter(|binding| is_live_binding(binding))
        .collect::<Vec<_>>();
    if previously_live.is_empty() {
        return None;
    }
    previously_live
        .iter()
        .all(|previous| {
            current.iter().any(|binding| {
                binding.provider == previous.provider
                    && binding.kind == previous.kind
                    && binding.status == BindingStatus::Unavailable
            })
        })
        .then_some(false)
}

fn is_live_binding(binding: &crate::domain::ProviderBinding) -> bool {
    matches!(
        binding.kind,
        crate::domain::ProviderKind::Persistence
            | crate::domain::ProviderKind::Display
            | crate::domain::ProviderKind::Execution
    ) && binding.status == crate::domain::BindingStatus::Active
        && binding
            .r#ref
            .as_deref()
            .is_some_and(|reference| !reference.is_empty())
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
        if let Some(orchestrator_id) = orchestrator_id.as_deref() {
            let orchestrator = workspace
                .sessions
                .iter()
                .find(|session| session.id == orchestrator_id)
                .with_context(|| format!("unknown orchestrator session: {orchestrator_id}"))?;
            if orchestrator.role != SessionRole::Orchestrator
                || !orchestrator.status.active()
                || orchestrator.status == LifecycleStatus::Terminating
            {
                bail!("orchestrator is not accepting new work: {orchestrator_id}");
            }
        }
        let run = WorkflowRun {
            id: format!("run-{}", &Uuid::new_v4().to_string()[..12]),
            name: name.clone(),
            goal: goal.clone(),
            expected_output: expected_output.clone(),
            status: LifecycleStatus::Queued,
            orchestrator_id,
            parent_run_id: None,
            definition: None,
            revision: None,
            checkpoint: None,
            mode: Default::default(),
            process_id: None,
            execution_nonce: None,
            recovery: Default::default(),
            resume_requested: false,
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
        if run.status == LifecycleStatus::Terminating {
            return Ok(run.clone());
        }
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
    if status == LifecycleStatus::Terminating || !status.active() {
        bail!("terminal run state must be set by a workflow lifecycle operation");
    }
    let scope = state::resolve_scope(scope)?;
    state::update(&scope, |workspace| {
        let run = workspace
            .runs
            .iter_mut()
            .find(|run| run.id == run_id)
            .with_context(|| format!("unknown run: {run_id}"))?;
        if run.status == LifecycleStatus::Terminating && status != LifecycleStatus::Terminating {
            return Ok(run.clone());
        }
        require_transition(LifecycleSubject::Run, run.status, status)?;
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
        if run.status == LifecycleStatus::Terminating || !run.status.active() {
            bail!("run is not mutable while {}", run.status);
        }
        let current = run
            .nodes
            .iter()
            .find(|candidate| candidate.id == id)
            .cloned();
        let status = status
            .or_else(|| current.as_ref().map(|node| node.status))
            .unwrap_or(LifecycleStatus::Queued);
        let attempt = attempt
            .or_else(|| current.as_ref().map(|node| node.attempt))
            .unwrap_or(0);
        let judge_policy = judge_policy
            .or_else(|| current.as_ref().map(|node| node.judge_policy))
            .unwrap_or_default();
        if !status.valid_for(LifecycleSubject::Node) {
            bail!("invalid node lifecycle state: {status}");
        }
        if let Some(current) = &current {
            require_transition(LifecycleSubject::Node, current.status, status)?;
        }
        let node = WorkflowNode {
            id: id.clone(),
            name: contract.title.clone(),
            purpose: contract.purpose.clone(),
            role: contract.role,
            harness: contract.harness.clone(),
            model: contract
                .model
                .clone()
                .or_else(|| current.as_ref().and_then(|node| node.model.clone())),
            execution: execution
                .clone()
                .or_else(|| current.as_ref().and_then(|node| node.execution.clone())),
            judge_policy,
            goal: contract.goal.clone(),
            expected_output: contract.expected_output.clone(),
            success_criteria: contract.success_criteria.clone(),
            completion: contract.completion,
            review_by: contract
                .review_by
                .clone()
                .or_else(|| current.as_ref().and_then(|node| node.review_by.clone())),
            session_id: session_id
                .clone()
                .or_else(|| current.as_ref().and_then(|node| node.session_id.clone())),
            child_run_id: current.as_ref().and_then(|node| node.child_run_id.clone()),
            status,
            attempt,
            retry_after: current.as_ref().and_then(|node| node.retry_after),
            prompt: current.as_ref().and_then(|node| node.prompt.clone()),
            input: current.as_ref().and_then(|node| node.input.clone()),
            output: current.as_ref().and_then(|node| node.output.clone()),
            activity: current
                .as_ref()
                .map_or_else(Vec::new, |node| node.activity.clone()),
            tokens: current.as_ref().map_or(0, |node| node.tokens),
            cost_usd: current.as_ref().map_or(0.0, |node| node.cost_usd),
            updated_at: Utc::now(),
        };
        run.nodes.retain(|candidate| candidate.id != id);
        run.nodes.push(node.clone());
        run.edges.retain(|edge| {
            (edge.to != id || edge.relationship != "depends_on")
                && (edge.from != id || edge.relationship != "reviewed_by")
        });
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

pub fn adopt_node(
    scope: &Path,
    run_id: &str,
    node_id: &str,
    session_id: &str,
) -> Result<WorkflowNode> {
    let scope = state::resolve_scope(scope)?;
    state::update(&scope, |workspace| {
        let now = Utc::now();
        let session = workspace
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .cloned()
            .with_context(|| format!("unknown session: {session_id}"))?;
        if !session.status.active() || session.status == LifecycleStatus::Terminating {
            bail!("session is not available for adoption: {session_id}");
        }

        let run_index = workspace
            .runs
            .iter()
            .position(|run| run.id == run_id)
            .with_context(|| format!("unknown run: {run_id}"))?;
        let run = &workspace.runs[run_index];
        if !run.status.active() || run.status == LifecycleStatus::Terminating {
            bail!("run is not mutable while {}", run.status);
        }
        let node_index = run
            .nodes
            .iter()
            .position(|node| node.id == node_id)
            .with_context(|| format!("unknown node: {node_id}"))?;
        let orchestrator_direct = session.role == SessionRole::Orchestrator
            && run.orchestrator_id.as_deref() == Some(session_id);
        if session.role == SessionRole::Orchestrator && !orchestrator_direct {
            bail!("orchestrator {session_id} does not own workflow run {run_id}");
        }
        if !orchestrator_direct {
            if let Some(assigned_run) = session.run_id.as_deref()
                && assigned_run != run_id
            {
                bail!("session {session_id} belongs to workflow run {assigned_run}");
            }
            if let Some(assigned_node) = session.node_id.as_deref()
                && assigned_node != node_id
            {
                bail!("session {session_id} belongs to workflow node {assigned_node}");
            }
        }
        if let Some(owner) = workspace.sessions.iter().find(|candidate| {
            candidate.id != session_id
                && candidate.run_id.as_deref() == Some(run_id)
                && candidate.node_id.as_deref() == Some(node_id)
                && candidate.status.active()
                && candidate.status != LifecycleStatus::Terminating
        }) {
            bail!(
                "workflow node {node_id} is owned by active session {}",
                owner.id
            );
        }

        let previous_id = run.nodes[node_index].session_id.clone();
        let session_needs_lineage =
            !orchestrator_direct && (session.run_id.is_none() || session.node_id.is_none());
        if previous_id.as_deref() == Some(session_id) && !session_needs_lineage {
            return Ok(run.nodes[node_index].clone());
        }
        if let Some(previous_id) = previous_id.as_deref()
            && previous_id != session_id
            && workspace.sessions.iter().any(|candidate| {
                candidate.id == previous_id
                    && candidate.status.active()
                    && candidate.status != LifecycleStatus::Terminating
            })
        {
            bail!("workflow node {node_id} is owned by active session {previous_id}");
        }
        if !orchestrator_direct
            && workspace.runs.iter().any(|candidate_run| {
                candidate_run.status.active()
                    && candidate_run.nodes.iter().any(|candidate_node| {
                        candidate_node.session_id.as_deref() == Some(session_id)
                            && (candidate_run.id != run_id || candidate_node.id != node_id)
                            && candidate_node.status.active()
                    })
            })
        {
            bail!("active worker session {session_id} already owns another workflow node");
        }

        let run = &mut workspace.runs[run_index];
        let node = &mut run.nodes[node_index];
        node.session_id = Some(session_id.to_owned());
        node.record_activity(
            "adopted",
            match previous_id.as_deref() {
                None => format!("session {session_id} adopted this node"),
                Some(previous) if previous == session_id => {
                    format!("session {session_id} adopted its existing node assignment")
                }
                Some(previous) => {
                    format!("session {session_id} replaced inactive session {previous}")
                }
            },
        );
        node.updated_at = now;
        run.updated_at = now;

        if !orchestrator_direct {
            let session = workspace
                .sessions
                .iter_mut()
                .find(|candidate| candidate.id == session_id)
                .context("adopted session disappeared")?;
            if session.run_id.is_none() {
                session.run_id = Some(run_id.to_owned());
            }
            if session.node_id.is_none() {
                session.node_id = Some(node_id.to_owned());
            }
            session.updated_at = now;
        }
        Ok(node.clone())
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
        if !run.status.active() {
            bail!("run is not mutable while {}", run.status);
        }
        let node = run
            .nodes
            .iter_mut()
            .find(|node| node.id == node_id)
            .with_context(|| format!("unknown node: {node_id}"))?;
        if run.status == LifecycleStatus::Terminating || node.status == LifecycleStatus::Terminating
        {
            return Ok(node.clone());
        }
        require_transition(LifecycleSubject::Node, node.status, status)?;
        node.status = status;
        node.updated_at = Utc::now();
        run.updated_at = Utc::now();
        Ok(node.clone())
    })
}

pub fn report_session_output(
    scope: &Path,
    session_id: &str,
    output: serde_json::Value,
) -> Result<SessionOutputReceipt> {
    let input_bytes = serde_json::to_vec(&output)?.len();
    if input_bytes > MAX_OUTPUT_BYTES {
        bail!("session output exceeds {MAX_OUTPUT_BYTES} bytes");
    }
    let scope = state::resolve_scope(scope)?;
    state::update(&scope, |workspace| {
        if !workspace.active {
            bail!("Orc scope is idle");
        }
        let session = workspace
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
            .with_context(|| format!("unknown session: {session_id}"))?;
        if session.role != SessionRole::Orchestrator
            || !session.status.active()
            || session.status == LifecycleStatus::Terminating
        {
            bail!("session is not authorized to report orchestrator output: {session_id}");
        }
        session.reported_output = Some(ReportedOutput { value: output });
        session.updated_at = Utc::now();
        Ok(SessionOutputReceipt {
            status: SessionOutputStatus::Reported,
            input_bytes,
            updated_at: session
                .updated_at
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        })
    })
}

pub fn report_node(
    scope: &Path,
    run_id: &str,
    node_id: &str,
    reporting_session_id: Option<&str>,
    report: NodeReport,
) -> Result<WorkflowNode> {
    let scope = state::resolve_scope(scope)?;
    if let Some(output) = report.output.as_ref()
        && serde_json::to_vec(output)?.len() > MAX_OUTPUT_BYTES
    {
        bail!("workflow node output exceeds {MAX_OUTPUT_BYTES} bytes");
    }
    state::update(&scope, |workspace| {
        let reporting = if let Some(session_id) = reporting_session_id {
            let reporting = workspace
                .sessions
                .iter()
                .find(|session| session.id == session_id)
                .cloned()
                .with_context(|| format!("unknown reporting session: {session_id}"))?;
            if !reporting.status.active() || reporting.status == LifecycleStatus::Terminating {
                bail!("reporting session is not authorized: {session_id}");
            }
            Some(reporting)
        } else {
            None
        };
        let active_lineage_owner = reporting_session_id.and_then(|session_id| {
            workspace
                .sessions
                .iter()
                .find(|candidate| {
                    candidate.id != session_id
                        && candidate.run_id.as_deref() == Some(run_id)
                        && candidate.node_id.as_deref() == Some(node_id)
                        && candidate.status.active()
                        && candidate.status != LifecycleStatus::Terminating
                })
                .map(|candidate| candidate.id.clone())
        });
        let run = workspace
            .runs
            .iter_mut()
            .find(|run| run.id == run_id)
            .with_context(|| format!("unknown run: {run_id}"))?;
        if !run.status.active() {
            bail!("run is not mutable while {}", run.status);
        }
        let orchestrator_direct = reporting.as_ref().is_some_and(|session| {
            session.role == SessionRole::Orchestrator
                && run.orchestrator_id.as_deref() == Some(session.id.as_str())
        });
        if let Some(reporting) = &reporting
            && reporting.role == SessionRole::Orchestrator
            && !orchestrator_direct
        {
            bail!(
                "orchestrator {} does not own workflow run {run_id}",
                reporting.id
            );
        }
        let node = run
            .nodes
            .iter_mut()
            .find(|node| node.id == node_id)
            .with_context(|| format!("unknown node: {node_id}"))?;
        if run.status == LifecycleStatus::Terminating || node.status == LifecycleStatus::Terminating
        {
            return Ok(node.clone());
        }
        if let Some(session_id) = reporting_session_id {
            match node.session_id.as_deref() {
                Some(assigned) if assigned != session_id => {
                    bail!("workflow node {node_id} is owned by session {assigned}")
                }
                None if orchestrator_direct => {
                    if let Some(owner) = active_lineage_owner {
                        bail!("workflow node {node_id} is owned by active session {owner}");
                    }
                    node.session_id = Some(session_id.to_owned());
                    node.record_activity(
                        "adopted",
                        format!("orchestrator session {session_id} adopted this node"),
                    );
                }
                None => bail!("a worker can report only its assigned workflow node"),
                Some(_) => {}
            }
        }
        if reporting_session_id.is_some()
            && !matches!(
                report.status,
                LifecycleStatus::Working
                    | LifecycleStatus::Waiting
                    | LifecycleStatus::Blocked
                    | LifecycleStatus::Failed
                    | LifecycleStatus::Done
            )
        {
            bail!(
                "a worker cannot report the {} lifecycle state",
                report.status
            );
        }
        require_transition(LifecycleSubject::Node, node.status, report.status)?;
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
        node.record_activity(
            "reported",
            report
                .message
                .clone()
                .unwrap_or_else(|| report.status.to_string()),
        );
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
    attach_with_output(config, scope, id, action, direction, true)
}

pub fn attach_quiet(
    config: &Config,
    scope: &Path,
    id: &str,
    action: provider::Action,
    direction: &str,
) -> Result<AttachOutcome> {
    attach_with_output(config, scope, id, action, direction, false)
}

fn attach_with_output(
    config: &Config,
    scope: &Path,
    id: &str,
    action: provider::Action,
    direction: &str,
    print_output: bool,
) -> Result<AttachOutcome> {
    let scope = state::resolve_scope(scope)?;
    let workspace = state::read(&scope)?;
    let session = selected_session(&workspace, id)?.clone();
    let providers = provider::discover(config)?;
    let prefer_focus = action == provider::Action::Attach
        && session.providers.iter().any(|binding| {
            binding.kind == crate::domain::ProviderKind::Display
                && binding.status == crate::domain::BindingStatus::Active
                && binding
                    .r#ref
                    .as_ref()
                    .is_some_and(|value| !value.is_empty())
        });
    execute_attach_with(action, prefer_focus, |selected_action| {
        let request = provider::action_request(selected_action, &scope, Some(&session), direction);
        let plan = provider::resolve_plan(config, &providers, selected_action, request)?;
        let result = provider::run_plan(&plan, &scope)?;
        let (accepted, receipt, stdout) = interpret_plan_result(&providers, &plan, &result)?;
        if let Some(binding) = receipt {
            persist_provider_binding(&scope, &session.id, binding)?;
        }
        if print_output && let Some(stdout) = stdout {
            print!("{stdout}");
        }
        if print_output {
            eprint!("{}", result.stderr);
        }
        Ok((result.code, accepted))
    })
}

fn interpret_plan_result<'a>(
    providers: &[provider::Manifest],
    plan: &provider::CommandPlan,
    result: &'a provider::CommandResult,
) -> Result<(bool, Option<ProviderBinding>, Option<&'a str>)> {
    let accepted = plan.accepts(result.code);
    let receipt = accepted
        .then(|| provider::parse_plan_binding_receipt(providers, plan, &result.stdout))
        .transpose()?
        .flatten();
    let stdout = plan.receipt.is_none().then_some(result.stdout.as_str());
    Ok((accepted, receipt, stdout))
}

fn persist_provider_binding(
    scope: &Path,
    session_id: &str,
    binding: ProviderBinding,
) -> Result<()> {
    state::update(scope, |workspace| {
        let session = workspace
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
            .with_context(|| format!("session disappeared while attaching: {session_id}"))?;
        let previous = session.providers.clone();
        if let Some(existing) = session
            .providers
            .iter_mut()
            .find(|existing| existing.provider == binding.provider && existing.kind == binding.kind)
        {
            *existing = binding;
        } else {
            session.providers.push(binding);
        }
        bump_changed_provider_revisions(
            &mut session.provider_revisions,
            &previous,
            &session.providers,
        );
        touch_session(session);
        Ok(())
    })
}

fn execute_attach_with(
    action: provider::Action,
    prefer_focus: bool,
    mut execute: impl FnMut(provider::Action) -> Result<(i32, bool)>,
) -> Result<AttachOutcome> {
    if action != provider::Action::Attach {
        let (code, accepted) = execute(action)?;
        return Ok(AttachOutcome {
            code,
            accepted,
            disposition: AttachDisposition::Launched,
        });
    }

    let focus_failure = if prefer_focus {
        match execute(provider::Action::Focus) {
            Ok((code, true)) => {
                return Ok(AttachOutcome {
                    code,
                    accepted: true,
                    disposition: AttachDisposition::Focused,
                });
            }
            Ok((code, false)) => Some(format!("focus exited with {code}")),
            Err(error) => return Err(error.context("focus failed without opening a duplicate")),
        }
    } else {
        None
    };

    execute(provider::Action::Attach)
        .map(|(code, accepted)| AttachOutcome {
            code,
            accepted,
            disposition: AttachDisposition::Launched,
        })
        .with_context(|| {
            focus_failure.map_or_else(
                || "attach failed".to_owned(),
                |failure| format!("attach fallback failed after {failure}"),
            )
        })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttachDisposition {
    Focused,
    Launched,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AttachOutcome {
    pub code: i32,
    pub accepted: bool,
    pub disposition: AttachDisposition,
}

pub fn launch(
    config: &Config,
    scope: &Path,
    harness: String,
    model: Option<String>,
    managed: Option<String>,
    lease: SessionLease,
    args: Vec<String>,
) -> Result<i32> {
    if managed.is_none()
        && (lease.runtime_timeout_seconds.is_some() || lease.idle_timeout_seconds.is_some())
    {
        bail!("session timeouts require --managed and a lifecycle provider");
    }
    let scope = state::resolve_scope(scope)?;
    let parent = state::read(&scope)?
        .current_session()
        .map(|session| session.id.clone());
    let native_id = Uuid::new_v4().to_string();
    let contract = Contract {
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
    };
    let link = SessionLink {
        native_id: Some(native_id.clone()),
        parent_id: parent,
        provider_ref: managed.clone(),
        runtime_timeout_seconds: lease.runtime_timeout_seconds,
        idle_timeout_seconds: lease.idle_timeout_seconds,
        source: if managed.is_some() {
            RegistrationSource::Managed
        } else {
            RegistrationSource::Connected
        },
        ..SessionLink::default()
    };
    let session = if managed.is_some() {
        register_managed(config, &scope, contract, link)?
    } else {
        register(&scope, contract, link)?
    };
    let supervised = managed.is_some();
    let mut command = vec![harness];
    command.extend(args);
    let result = (|| -> Result<i32> {
        if supervised {
            daemon::ensure_running(config)?;
        }
        let code = if let Some(managed_id) = managed {
            let providers = provider::discover(config)?;
            let request = serde_json::json!({
                "version": "orc.provider/v1", "action": "launch", "scope": scope, "session": session,
                "command": command, "managedId": managed_id,
            });
            let plan =
                provider::resolve_plan(config, &providers, provider::Action::Launch, request)?;
            let launch_guard =
                TerminationGuard::acquire(&scope, &session.id, config.provider_timeout())?
                    .context("managed session launch is changing lifecycle ownership")?;
            let current = selected_session(&state::read(&scope)?, &session.id)?.clone();
            if !current.status.active() || current.status == LifecycleStatus::Terminating {
                bail!(
                    "managed session was cancelled before launch: {}",
                    session.id
                );
            }
            provider::execute_inherited_plan_after_spawn(&plan, &scope, launch_guard, |child| {
                wait_for_managed_launch(config, &providers, &scope, &session.id, child)
            })?
        } else {
            let mut child = std::process::Command::new(&command[0]);
            child
                .args(&command[1..])
                .current_dir(&scope)
                .env("ORC_SCOPE", &scope)
                .env("ORC_SESSION_ID", &session.id)
                .env("ORC_HARNESS", &session.harness)
                .env("ORC_NATIVE_SESSION_ID", &native_id);
            if let Some(model) = &model {
                child.env("ORC_MODEL", model);
            }
            child.status()?.code().unwrap_or(1)
        };
        Ok(code)
    })();
    let code = match result {
        Ok(code) => code,
        Err(error) => {
            if let Err(cleanup_error) =
                terminate(config, &scope, &session.id, "managed launch failed")
            {
                update_session(&scope, &session.id, LifecycleStatus::Failed)?;
                return Err(
                    error.context(format!("managed session cleanup failed: {cleanup_error:#}"))
                );
            }
            return Err(error);
        }
    };
    if code != 0 && supervised {
        if let Err(cleanup_error) = terminate(config, &scope, &session.id, "managed launch failed")
        {
            update_session(&scope, &session.id, LifecycleStatus::Failed)?;
            bail!("managed session cleanup failed after exit {code}: {cleanup_error:#}");
        }
    } else if supervised {
        finalize_managed_launch(config, &scope, &session.id)?;
    } else {
        update_session(
            &scope,
            &session.id,
            if code == 0 {
                LifecycleStatus::Done
            } else {
                LifecycleStatus::Failed
            },
        )?;
    }
    Ok(code)
}

fn finalize_managed_launch(config: &Config, scope: &Path, session_id: &str) -> Result<()> {
    let _guard = TerminationGuard::acquire(scope, session_id, config.provider_timeout())?
        .context("managed session completion is changing lifecycle ownership")?;
    let session = selected_session(&state::read(scope)?, session_id)?.clone();
    if !session.status.active() || session.status == LifecycleStatus::Terminating {
        return Ok(());
    }
    let providers = provider::discover(config)?;
    let bindings = provider::discover_bindings(config, &providers, scope, &session, false);
    state::update(scope, |workspace| {
        let selected = workspace
            .sessions
            .iter_mut()
            .find(|candidate| candidate.id == session_id)
            .with_context(|| format!("unknown session: {session_id}"))?;
        if managed_finalization_allowed(selected.status) {
            apply_observed_enrichment(selected, &session, &bindings, None, None);
            let still_active = has_active_runtime_binding(&selected.providers);
            if !still_active {
                selected.status = LifecycleStatus::Done;
                let previous = selected.providers.clone();
                selected
                    .providers
                    .retain(|binding| !provider::is_launch_ownership(binding));
                bump_changed_provider_revisions(
                    &mut selected.provider_revisions,
                    &previous,
                    &selected.providers,
                );
                selected.updated_at = Utc::now();
            }
        }
        Ok(())
    })
}

fn managed_finalization_allowed(status: LifecycleStatus) -> bool {
    !matches!(
        status,
        LifecycleStatus::Terminating
            | LifecycleStatus::Failed
            | LifecycleStatus::Done
            | LifecycleStatus::Cancelled
            | LifecycleStatus::Archived
            | LifecycleStatus::Skipped
    )
}

fn wait_for_managed_launch(
    config: &Config,
    providers: &[provider::Manifest],
    scope: &Path,
    session_id: &str,
    child: &mut std::process::Child,
) -> Result<()> {
    let deadline = std::time::Instant::now()
        .checked_add(config.provider_timeout())
        .context("provider timeout exceeds the platform clock range")?;
    loop {
        let session = selected_session(&state::read(scope)?, session_id)?.clone();
        if !session.status.active() || session.status == LifecycleStatus::Terminating {
            bail!("managed session was cancelled during launch: {session_id}");
        }
        let bindings = provider::discover_bindings(config, providers, scope, &session, false);
        if has_active_runtime_binding(&bindings) {
            let ready = state::update(scope, |workspace| {
                let selected = workspace
                    .sessions
                    .iter_mut()
                    .find(|candidate| candidate.id == session_id)
                    .with_context(|| format!("unknown session: {session_id}"))?;
                Ok(apply_observed_runtime_readiness(
                    selected, &session, &bindings,
                ))
            })?;
            if ready {
                return Ok(());
            }
        }
        if let Some(status) = child.try_wait()?
            && !status.success()
        {
            bail!("managed launch exited before becoming ready: {status}");
        }
        if std::time::Instant::now() >= deadline {
            bail!("managed session did not become ready before the provider timeout");
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

fn has_active_runtime_binding(bindings: &[ProviderBinding]) -> bool {
    bindings.iter().any(|binding| {
        !provider::is_launch_ownership(binding)
            && binding.status == BindingStatus::Active
            && matches!(
                binding.kind,
                ProviderKind::Persistence | ProviderKind::Execution
            )
            && binding
                .r#ref
                .as_deref()
                .is_some_and(|reference| !reference.is_empty())
    })
}

fn apply_observed_runtime_readiness(
    session: &mut Session,
    observed: &Session,
    bindings: &[ProviderBinding],
) -> bool {
    apply_observed_enrichment(session, observed, bindings, None, None);
    has_active_runtime_binding(&session.providers)
}

pub fn require_id(id: Option<String>) -> Result<String> {
    id.or_else(|| env::var("ORC_SESSION_ID").ok())
        .context("a session id or ORC_SESSION_ID is required")
}

pub fn ensure_active_context(scope: &Path) -> Result<(WorkspaceState, Session)> {
    let session_id = env::var("ORC_SESSION_ID").ok();
    let state = read_workspace(scope)?;
    let session = state
        .current_session_for(session_id.as_deref())
        .cloned()
        .context("Orc requires a registered session in an active scope")?;
    if !state.active {
        bail!("Orc scope is idle");
    }
    Ok((state, session))
}

pub fn ensure_active_context_for(
    scope: &Path,
    session_id: &str,
) -> Result<(WorkspaceState, Session)> {
    let state = read_workspace(scope)?;
    let session = state
        .current_session_for(Some(session_id))
        .cloned()
        .with_context(|| format!("inactive or unknown Orc session: {session_id}"))?;
    if !state.active {
        bail!("Orc scope is idle");
    }
    Ok((state, session))
}

pub fn require_supervisor_control(scope: &Path) -> Result<()> {
    if env::var_os("ORC_SESSION_ID").is_none() {
        return Ok(());
    }
    let (_, current) = ensure_active_context(scope)?;
    if current.role != SessionRole::Orchestrator {
        bail!("only the orchestrator or an external operator can change managed lifecycle state");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{
        domain::{BindingStatus, ProviderBinding, ProviderKind},
        test_support::render_fixture,
    };
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    const LAUNCH_RACE_PROVIDER: &str = r#"#!/bin/sh
request=$(cat)
case "$request" in
  *session.launch*)
    : > '{{ resolving }}'
    while [ ! -e '{{ release }}' ]; do sleep 0.01; done
    cat <<'JSON'
{"version":"orc.provider/v1","command":["touch","{{ launched }}"]}
JSON
    ;;
  *session.stop*)
    cat <<'JSON'
{"version":"orc.provider/v1","command":["true"]}
JSON
    ;;
  *) printf '%s\n' 'null' ;;
esac
"#;

    const READY_LAUNCHER: &str = r#"#!/bin/sh
: > '{{ spawned }}'
while [ ! -e '{{ release }}' ]; do sleep 0.01; done
: > '{{ resource }}'
"#;

    const READY_PROVIDER: &str = r#"#!/bin/sh
request=$(cat)
case "$request" in
  *session.launch*)
    cat <<'JSON'
{"version":"orc.provider/v1","command":["{{ launcher }}"]}
JSON
    ;;
  *execution.run*)
    cat <<'JSON'
{"version":"orc.provider/v1","command":["{{ launcher }}"]}
JSON
    ;;
  *session.bind*)
    if [ -e '{{ resource }}' ]; then
      cat <<'JSON'
{"version":"orc.provider/v1","binding": {"kind":"persistence","status":"active","ref":"ready"}}
JSON
    else
      cat <<'JSON'
{"version":"orc.provider/v1","status":"declined","reason":"not ready"}
JSON
    fi
    ;;
  *session.stop*)
    cat <<'JSON'
{"version":"orc.provider/v1","command":["sh","-c","rm -f '{{ resource }}' && touch '{{ stopped }}'"]}
JSON
    ;;
  *) printf '%s\n' 'null' ;;
esac
"#;

    const CURRENT_BIND_PROVIDER: &str = r#"#!/bin/sh
cat > '{{ request }}'
printf '%s\n' '{"version":"orc.provider/v1","binding":{"kind":"display","status":"active","ref":"pane-7"}}'
"#;

    const CURRENT_BIND_MANIFEST: &str = r#"version: orc.provider/v1
name: display
kind: display
command: {{ command }}
actions:
  session.bind: Bind the current terminal
"#;

    const ATTACH_PROVIDER: &str = r#"#!/bin/sh
request=$(cat)
case "$request" in
  *session.attach*) printf '%s\n' '{"version":"orc.provider/v1","command":["true"]}' ;;
  *) printf '%s\n' 'null' ;;
esac
"#;

    const DISPLAY_RECEIPT_PROVIDER: &str = r#"#!/bin/sh
case "${1:-}" in
  open)
    printf 'open\n' >> "$2"
    printf '%s\n' '{"version":"orc.provider/v1","binding":{"kind":"display","status":"active","ref":"pane-7","label":"test pane"}}'
    exit 0
    ;;
  focus)
    printf 'focus\n' >> "$2"
    exit 0
    ;;
esac
request=$(cat)
case "$request" in
  *terminal.open*)
    jq -n --arg command "$0" --arg marker '{{ opened }}' '{version:"orc.provider/v1",command:[$command,"open",$marker],receipt:{type:"providerBinding",provider:"display"}}'
    ;;
  *terminal.focus*)
    jq -n --arg command "$0" --arg marker '{{ focused }}' '{version:"orc.provider/v1",command:[$command,"focus",$marker]}'
    ;;
  *) printf '%s\n' 'null' ;;
esac
"#;

    const RACING_DISPLAY_RECEIPT_PROVIDER: &str = r#"#!/bin/sh
case "${1:-}" in
  open)
    printf 'open\n' >> "$2"
    printf '%s\n' '{"version":"orc.provider/v1","binding":{"kind":"display","status":"active","ref":"pane-7","label":"test pane"}}'
    exit 0
    ;;
  focus)
    printf 'focus\n' >> "$2"
    exit 0
    ;;
esac
request=$(cat)
case "$request" in
  *session.bind*)
    : > '{{ resolving }}'
    while [ ! -e '{{ release }}' ]; do sleep 0.01; done
    printf '%s\n' '{"version":"orc.provider/v1","binding":{"kind":"display","status":"available","label":"test pane"}}'
    ;;
  *terminal.open*)
    jq -n --arg command "$0" --arg marker '{{ opened }}' '{version:"orc.provider/v1",command:[$command,"open",$marker],receipt:{type:"providerBinding",provider:"display"}}'
    ;;
  *terminal.focus*)
    jq -n --arg command "$0" --arg marker '{{ focused }}' '{version:"orc.provider/v1",command:[$command,"focus",$marker]}'
    ;;
  *) printf '%s\n' 'null' ;;
esac
"#;

    const ATTACH_PROVIDER_MANIFEST: &str = r#"version: orc.provider/v1
name: harness
kind: persistence
command: {{ command }}
actions:
  session.attach: Resume the harness session
"#;

    const DISPLAY_RECEIPT_MANIFEST: &str = r#"version: orc.provider/v1
name: display
kind: display
command: {{ command }}
actions:
  terminal.open: Open a terminal pane
  terminal.focus: Focus a terminal pane
"#;

    const RACING_DISPLAY_RECEIPT_MANIFEST: &str = r#"version: orc.provider/v1
name: display
kind: display
command: {{ command }}
actions:
  session.bind: Inspect a terminal binding
  terminal.open: Open a terminal pane
  terminal.focus: Focus a terminal pane
"#;

    const MISSING_SCOPE_STOP_PROVIDER: &str = r#"#!/bin/sh
request=$(cat)
pwd > '{{ invoked }}'
case "$request" in
  *session.stop*)
    cat <<'JSON'
{"version":"orc.provider/v1","command":["sh","-c","pwd > '{{ executed }}'"],"cwd":"{{ missing }}"}
JSON
    ;;
  *) printf '%s\n' 'null' ;;
esac
"#;

    const COMPOSED_TERMINATION_PROVIDER: &str = r#"#!/bin/sh
request=$(cat)
case "$request" in
  *session.stop*) capability=session.stop ;;
  *execution.cancel*) capability=execution.cancel ;;
  *) exit 2 ;;
esac
cat <<JSON
{"version":"orc.provider/v1","command":[{{ executable_json }},"$capability"],"successCodes":[0]}
JSON
"#;

    const PREFLIGHT_STOP_PROVIDER: &str = r#"#!/bin/sh
request=$(cat)
case "$request" in
  *session.stop*)
    cat <<'JSON'
{"version":"orc.provider/v1","command":["sh","-c","touch '{{ stopped }}'"]}
JSON
    ;;
  *) printf '%s\n' 'null' ;;
esac
"#;

    const RESOURCE_ONLY_CANCEL_PROVIDER: &str = r#"#!/bin/sh
request=$(cat)
case "$request" in
  *execution.cancel*'"resource"'*)
    printf '%s\n' '{"version":"orc.provider/v1","command":["true"]}'
    ;;
  *execution.cancel*)
    printf '%s\n' '{"version":"orc.provider/v1","status":"declined","reason":"resource required"}'
    ;;
  *) printf '%s\n' 'null' ;;
esac
"#;

    const PREFLIGHT_ONLY_CANCEL_PROVIDER: &str = r#"#!/bin/sh
request=$(cat)
case "$request" in
  *execution.cancel*'"preflight":true'*)
    printf '%s\n' '{"version":"orc.provider/v1","command":["true"]}'
    ;;
  *execution.cancel*)
    printf '%s\n' '{"version":"orc.provider/v1","status":"declined","reason":"owned execution disappeared"}'
    ;;
  *) printf '%s\n' 'null' ;;
esac
"#;

    const TERMINATION_RECORDER: &str = r#"#!/bin/sh
printf '%s\n' "$1" >> '{{ log }}'
"#;

    const TERMINATION_PROVIDER_MANIFEST: &str = r#"version: orc.provider/v1
name: {{ name }}
kind: {{ kind }}
command: {{ command }}
actions:
  {{ capability }}: Terminate owned work
"#;

    const EXECUTION_OWNER_MANIFEST: &str = r#"version: orc.provider/v1
name: {{ name }}
kind: execution
command: {{ command }}
actions:
  execution.run: Run work
  execution.cancel: Cancel owned work
"#;

    const PROVIDER_MANIFEST: &str = r#"version: orc.provider/v1
name: {{ name }}
kind: persistence
command: {{ command }}
actions:
{% for action in actions %}  {{ action.capability }}: {{ action.description }}
{% endfor %}
"#;

    fn session(id: &str, role: SessionRole, status: LifecycleStatus) -> Session {
        Session {
            id: id.into(),
            native_id: id.into(),
            trace_id: None,
            harness: "test".into(),
            model: None,
            role,
            title: "Agent session".into(),
            purpose: "test".into(),
            goal: "Complete the assigned work".into(),
            expected_output: "test".into(),
            reported_output: None,
            success_criteria: Vec::new(),
            completion: CompletionTarget::Orchestrator,
            review_by: None,
            parent_id: None,
            run_id: None,
            node_id: None,
            provider_ref: None,
            providers: Vec::new(),
            provider_revisions: BTreeMap::new(),
            directory: "/tmp".into(),
            registration: RegistrationSource::Connected,
            status,
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

    fn binding(provider: &str, kind: ProviderKind, status: BindingStatus) -> ProviderBinding {
        ProviderBinding {
            provider: provider.into(),
            kind,
            r#ref: Some(format!("{provider}-ref")),
            status,
            label: provider.into(),
        }
    }

    fn run_with_assignment(orchestrator_id: &str, session_id: &str) -> WorkflowRun {
        WorkflowRun {
            id: "run".into(),
            name: "Repair duplicates".into(),
            goal: "Keep one canonical session".into(),
            expected_output: "References point at the canonical session".into(),
            status: LifecycleStatus::Working,
            orchestrator_id: Some(orchestrator_id.into()),
            parent_run_id: None,
            definition: None,
            revision: None,
            checkpoint: None,
            mode: crate::domain::RunMode::Foreground,
            process_id: None,
            execution_nonce: None,
            recovery: Default::default(),
            resume_requested: false,
            log_path: None,
            current_node: Some("repair".into()),
            tokens: 0,
            cost_usd: 0.0,
            token_burn: Vec::new(),
            pending_gates: Vec::new(),
            approved_gates: Vec::new(),
            agents: Vec::new(),
            nodes: vec![WorkflowNode {
                id: "repair".into(),
                name: "Repair".into(),
                purpose: "Repair inherited records".into(),
                role: SessionRole::Implementer,
                harness: "test".into(),
                model: None,
                execution: None,
                judge_policy: JudgePolicy::default(),
                goal: "Repair records".into(),
                expected_output: "Canonical references".into(),
                success_criteria: Vec::new(),
                completion: CompletionTarget::Orchestrator,
                review_by: None,
                session_id: Some(session_id.into()),
                child_run_id: None,
                status: LifecycleStatus::Working,
                attempt: 1,
                retry_after: None,
                prompt: None,
                input: None,
                output: None,
                activity: Vec::new(),
                tokens: 0,
                cost_usd: 0.0,
                updated_at: Utc::now(),
            }],
            edges: Vec::new(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn register(scope: &Path, mut contract: Contract, link: SessionLink) -> Result<Session> {
        if link.source == RegistrationSource::Managed {
            return register_for_caller(
                scope,
                &mut contract,
                link,
                None,
                Some(vec![binding(
                    "test-owner",
                    ProviderKind::Persistence,
                    BindingStatus::Active,
                )]),
            );
        }
        super::register(scope, contract, link)
    }

    #[test]
    fn unmanaged_registration_cannot_create_a_managed_session() {
        let directory = tempfile::tempdir().expect("scope");
        let scope = std::fs::canonicalize(directory.path()).expect("canonical scope");

        let error = super::register(
            &scope,
            Contract::default(),
            SessionLink {
                source: RegistrationSource::Managed,
                ..SessionLink::default()
            },
        )
        .expect_err("reject managed registration without lifecycle ownership");

        assert!(error.to_string().contains("lifecycle owner"));
    }

    #[cfg(unix)]
    #[test]
    fn managed_registration_persists_its_lifecycle_owner() {
        let directory = tempfile::tempdir().expect("fixture");
        let scope_directory = directory.path().join("scope");
        let provider_directory = directory.path().join("providers");
        fs::create_dir_all(&scope_directory).expect("scope");
        fs::create_dir_all(&provider_directory).expect("providers");
        let scope = fs::canonicalize(scope_directory).expect("canonical scope");
        let provider = directory.path().join("owner.sh");
        fs::write(
            &provider,
            r#"#!/bin/sh
cat >/dev/null
printf '%s\n' '{"version":"orc.provider/v1","command":["true"]}'
"#,
        )
        .expect("provider command");
        fs::set_permissions(&provider, fs::Permissions::from_mode(0o755))
            .expect("provider executable");
        fs::write(
            provider_directory.join("owner.yaml"),
            render_fixture(
                PROVIDER_MANIFEST,
                serde_json::json!({
                    "name": "owner",
                    "command": provider.display().to_string(),
                    "actions": [
                        {
                            "capability": "session.launch",
                            "description": "Launch a session",
                        },
                        {
                            "capability": "session.stop",
                            "description": "Stop a session",
                        },
                    ],
                }),
            ),
        )
        .expect("provider manifest");
        let config = Config {
            providers: crate::config::ProviderConfig {
                directory: provider_directory,
                ..crate::config::ProviderConfig::default()
            },
            ..Config::default()
        };

        let session = register_managed(
            &config,
            &scope,
            Contract::default(),
            SessionLink {
                native_id: Some("managed-native".into()),
                ..SessionLink::default()
            },
        )
        .expect("managed session");

        assert_eq!(session.registration, RegistrationSource::Managed);
        assert_eq!(session.providers.len(), 1);
        assert_eq!(session.providers[0].provider, "owner");
        assert_eq!(
            session.providers[0].r#ref.as_deref(),
            Some(session.id.as_str())
        );
    }

    #[cfg(unix)]
    #[test]
    fn managed_registration_excludes_resource_only_cancellation() {
        let directory = tempfile::tempdir().expect("fixture");
        let scope_directory = directory.path().join("scope");
        let provider_directory = directory.path().join("providers");
        fs::create_dir_all(&scope_directory).expect("scope");
        fs::create_dir_all(&provider_directory).expect("providers");
        let scope = fs::canonicalize(scope_directory).expect("canonical scope");
        let stopped = directory.path().join("stopped");
        let stop_provider = directory.path().join("stop-provider.sh");
        let cancel_provider = directory.path().join("cancel-provider.sh");
        fs::write(
            &stop_provider,
            render_fixture(
                PREFLIGHT_STOP_PROVIDER,
                serde_json::json!({ "stopped": stopped.display().to_string() }),
            ),
        )
        .expect("stop provider");
        fs::write(&cancel_provider, RESOURCE_ONLY_CANCEL_PROVIDER).expect("cancel provider");
        for provider in [&stop_provider, &cancel_provider] {
            fs::set_permissions(provider, fs::Permissions::from_mode(0o755))
                .expect("provider executable");
        }
        fs::write(
            provider_directory.join("persistence.yaml"),
            render_fixture(
                PROVIDER_MANIFEST,
                serde_json::json!({
                    "name": "persistence",
                    "command": stop_provider.display().to_string(),
                    "actions": [
                        {
                            "capability": "session.persist",
                            "description": "Persist a session",
                        },
                        {
                            "capability": "session.stop",
                            "description": "Stop a session",
                        },
                    ],
                }),
            ),
        )
        .expect("persistence manifest");
        fs::write(
            provider_directory.join("local.yaml"),
            render_fixture(
                EXECUTION_OWNER_MANIFEST,
                serde_json::json!({
                    "name": "local",
                    "command": cancel_provider.display().to_string(),
                }),
            ),
        )
        .expect("local manifest");
        let config = Config {
            providers: crate::config::ProviderConfig {
                directory: provider_directory,
                ..crate::config::ProviderConfig::default()
            },
            ..Config::default()
        };

        let session = register_managed(
            &config,
            &scope,
            Contract::default(),
            SessionLink {
                native_id: Some("managed-native".into()),
                ..SessionLink::default()
            },
        )
        .expect("managed session");

        assert_eq!(session.providers.len(), 1);
        assert_eq!(session.providers[0].provider, "persistence");
        let terminated = terminate(&config, &scope, &session.id, "operator request")
            .expect("terminate through the accepted owner");
        assert_eq!(terminated.status, LifecycleStatus::Cancelled);
        assert!(stopped.exists());
    }

    #[cfg(unix)]
    #[test]
    fn selected_cancellation_owner_failure_preserves_termination_state() {
        let directory = tempfile::tempdir().expect("fixture");
        let scope_directory = directory.path().join("scope");
        let provider_directory = directory.path().join("providers");
        fs::create_dir_all(&scope_directory).expect("scope");
        fs::create_dir_all(&provider_directory).expect("providers");
        let scope = fs::canonicalize(scope_directory).expect("canonical scope");
        let provider = directory.path().join("cancel-provider.sh");
        fs::write(&provider, PREFLIGHT_ONLY_CANCEL_PROVIDER).expect("cancel provider");
        fs::set_permissions(&provider, fs::Permissions::from_mode(0o755))
            .expect("provider executable");
        fs::write(
            provider_directory.join("execution.yaml"),
            render_fixture(
                EXECUTION_OWNER_MANIFEST,
                serde_json::json!({
                    "name": "executor",
                    "command": provider.display().to_string(),
                }),
            ),
        )
        .expect("execution manifest");
        let config = Config {
            providers: crate::config::ProviderConfig {
                directory: provider_directory,
                ..crate::config::ProviderConfig::default()
            },
            ..Config::default()
        };
        let session = register_managed(
            &config,
            &scope,
            Contract::default(),
            SessionLink {
                native_id: Some("managed-native".into()),
                ..SessionLink::default()
            },
        )
        .expect("managed session");

        let error = terminate(&config, &scope, &session.id, "operator request")
            .expect_err("selected owner must remain authoritative");
        assert!(
            error.to_string().contains("owned execution disappeared"),
            "{error:#}"
        );
        let restored = selected_session(&state::read(&scope).expect("workspace"), &session.id)
            .expect("restored session")
            .clone();
        assert_eq!(restored.status, LifecycleStatus::Working);
        assert!(
            restored
                .termination_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("owned execution disappeared"))
        );
        assert_eq!(
            restored.termination_cause.as_deref(),
            Some("operator request")
        );
        assert!(restored.termination_operation_id.is_some());
    }

    #[test]
    fn inferred_id_is_short_and_stable() {
        let id = inferred_session_id("agent", "abc");
        assert_eq!(id, inferred_session_id("agent", "abc"));
        assert_eq!(id.len(), 18);
    }

    #[test]
    fn current_rebind_validates_every_supplied_identity_field() {
        let mut workspace = WorkspaceState::empty("/tmp".into());
        let mut older = session("older", SessionRole::Orchestrator, LifecycleStatus::Working);
        older.native_id = "native".into();
        older.updated_at = Utc::now() - chrono::Duration::minutes(1);
        let mut newer = session("newer", SessionRole::Orchestrator, LifecycleStatus::Working);
        newer.native_id = "native".into();
        newer.harness = "other".into();
        workspace.sessions = vec![older, newer];

        assert_eq!(
            current_rebind_selection(&workspace, Some("older"), None, Some("native")),
            ReconcileSelection::One("older".into())
        );
        assert_eq!(
            current_rebind_selection(&workspace, None, Some("test"), Some("native")),
            ReconcileSelection::One("older".into())
        );
        assert_eq!(
            current_rebind_selection(&workspace, None, None, Some("native")),
            ReconcileSelection::IdentityMismatch
        );
        assert_eq!(
            current_rebind_selection(&workspace, None, Some("other"), Some("native")),
            ReconcileSelection::One("newer".into())
        );
        assert_eq!(
            current_rebind_selection(&workspace, Some("older"), Some("other"), Some("native")),
            ReconcileSelection::IdentityMismatch
        );
        assert_eq!(
            current_rebind_selection(&workspace, Some("older"), Some("test"), Some("different")),
            ReconcileSelection::IdentityMismatch
        );
        assert_eq!(
            current_rebind_selection(&workspace, Some("missing"), None, None),
            ReconcileSelection::IdentityMismatch
        );
    }

    #[test]
    fn empty_current_identity_selects_workspace_enrichment() {
        let workspace = WorkspaceState::empty("/tmp".into());

        assert_eq!(
            current_rebind_selection(&workspace, None, None, None),
            ReconcileSelection::All
        );
    }

    #[cfg(unix)]
    #[test]
    fn current_registration_binding_uses_bounded_timeout_once() {
        let directory = tempfile::tempdir().expect("binding fixture");
        let scope_directory = directory.path().join("scope");
        let provider_directory = directory.path().join("providers");
        fs::create_dir_all(&scope_directory).expect("scope");
        fs::create_dir_all(&provider_directory).expect("providers");
        let scope = fs::canonicalize(scope_directory).expect("canonical scope");
        let provider = directory.path().join("provider.sh");
        let request = directory.path().join("request.json");
        fs::write(
            &provider,
            render_fixture(
                CURRENT_BIND_PROVIDER,
                serde_json::json!({"request": request.display().to_string()}),
            ),
        )
        .expect("provider script");
        fs::set_permissions(&provider, fs::Permissions::from_mode(0o755))
            .expect("provider executable");
        fs::write(
            provider_directory.join("provider.yaml"),
            render_fixture(
                CURRENT_BIND_MANIFEST,
                serde_json::json!({"command": provider.display().to_string()}),
            ),
        )
        .expect("provider manifest");
        let linked = register(
            &scope,
            Contract::default(),
            SessionLink {
                id: Some("current".into()),
                native_id: Some("native-current".into()),
                source: RegistrationSource::Hook,
                ..SessionLink::default()
            },
        )
        .expect("hook session");
        state::update(&scope, |workspace| {
            let mut unrelated = session(
                "unrelated",
                SessionRole::Researcher,
                LifecycleStatus::Working,
            );
            unrelated.title = "Unrelated title".into();
            unrelated.goal = "Unrelated goal".into();
            unrelated.providers = vec![binding(
                "existing",
                ProviderKind::Activity,
                BindingStatus::Active,
            )];
            workspace.sessions.push(unrelated);
            Ok(())
        })
        .expect("record unrelated session");
        let mut config = Config::default();
        config.providers.directory = provider_directory;
        config.providers.timeout_ms = 30_000;
        let providers = provider::discover(&config).expect("display provider");
        let bindings = provider::discover_current_bindings(&config, &providers, &scope, &linked);
        let request: serde_json::Value =
            serde_json::from_slice(&fs::read(request).expect("current binding request"))
                .expect("current binding request json");
        assert_eq!(request["rebindCurrent"], true);
        assert_eq!(request["currentSessionId"], linked.id);
        assert_eq!(request["session"]["id"], linked.id);
        let invoked = std::cell::Cell::new(0);
        let state = bind_current_session_with(
            &config,
            &scope,
            &linked.id,
            |bounded_config, bound_scope, session| {
                invoked.set(invoked.get() + 1);
                assert_eq!(bounded_config.providers.timeout_ms, 1_500);
                assert_eq!(bound_scope, scope);
                assert_eq!(session.id, linked.id);
                bindings
            },
        )
        .expect("bind the just-registered session");

        let current = selected_session(&state, &linked.id).expect("bound session");
        assert_eq!(current.providers.len(), 1);
        assert_eq!(current.providers[0].provider, "display");
        assert_eq!(current.providers[0].kind, ProviderKind::Display);
        assert_eq!(current.providers[0].status, BindingStatus::Active);
        assert_eq!(current.providers[0].r#ref.as_deref(), Some("pane-7"));
        assert_eq!(invoked.get(), 1);
        let unrelated = selected_session(&state, "unrelated").expect("unrelated session");
        assert_eq!(unrelated.title, "Unrelated title");
        assert_eq!(unrelated.goal, "Unrelated goal");
        assert_eq!(unrelated.providers.len(), 1);
        assert_eq!(unrelated.providers[0].provider, "existing");
        let _ = fs::remove_file(state::path(&scope));
    }

    #[test]
    fn doctor_repairs_duplicate_native_sessions_once() {
        let mut workspace = WorkspaceState::empty("/tmp".into());
        let now = Utc::now();
        let mut older = session("older", SessionRole::Orchestrator, LifecycleStatus::Working);
        older.native_id = "native".into();
        older.updated_at = now - chrono::Duration::minutes(1);
        let mut newer = session("newer", SessionRole::Orchestrator, LifecycleStatus::Working);
        newer.native_id = "native".into();
        newer.updated_at = now;
        let mut child = session("child", SessionRole::Implementer, LifecycleStatus::Working);
        child.parent_id = Some("older".into());
        let mut other_harness = session(
            "other-harness",
            SessionRole::Implementer,
            LifecycleStatus::Working,
        );
        other_harness.harness = "other".into();
        other_harness.native_id = "native".into();
        newer.parent_id = Some("child".into());
        workspace.sessions = vec![older, newer, child, other_harness];
        workspace.runs.push(run_with_assignment("older", "older"));

        let audit = repair_duplicate_native_sessions(&mut workspace, false).expect("audit");
        assert!(!audit.repaired);
        assert_eq!(audit.duplicates.len(), 1);
        assert_eq!(audit.duplicates[0].canonical_id, "newer");
        assert_eq!(workspace.sessions[0].status, LifecycleStatus::Working);

        let repaired = repair_duplicate_native_sessions(&mut workspace, true).expect("repair");
        assert!(repaired.repaired);
        assert_eq!(
            workspace
                .sessions
                .iter()
                .find(|session| session.id == "older")
                .expect("older")
                .status,
            LifecycleStatus::Archived
        );
        assert_eq!(
            workspace
                .sessions
                .iter()
                .find(|session| session.id == "child")
                .expect("child")
                .parent_id
                .as_deref(),
            Some("newer")
        );
        assert_eq!(workspace.runs[0].orchestrator_id.as_deref(), Some("newer"));
        assert_eq!(
            workspace
                .sessions
                .iter()
                .find(|session| session.id == "newer")
                .expect("newer")
                .parent_id,
            None
        );
        assert_eq!(
            workspace.runs[0].nodes[0].session_id.as_deref(),
            Some("newer")
        );
        assert_eq!(
            selected_session(&workspace, "other-harness")
                .expect("other harness session")
                .status,
            LifecycleStatus::Working
        );
        let mut cursor = Some("child");
        let mut visited = BTreeSet::new();
        while let Some(id) = cursor {
            assert!(visited.insert(id), "repair created a parent cycle at {id}");
            cursor = workspace
                .sessions
                .iter()
                .find(|session| session.id == id)
                .and_then(|session| session.parent_id.as_deref());
        }

        let clean = repair_duplicate_native_sessions(&mut workspace, true).expect("idempotent");
        assert!(clean.duplicates.is_empty());
        assert!(!clean.repaired);
    }

    #[test]
    fn doctor_reports_and_blocks_orphan_work_without_choosing_a_session() {
        let directory = tempfile::tempdir().expect("scope");
        let scope = std::fs::canonicalize(directory.path()).expect("canonical scope");
        let run = create_run(
            &scope,
            "orphan".into(),
            "repair orphan work".into(),
            "explicit assignment proposal".into(),
            None,
            None,
            None,
        )
        .expect("run");
        upsert_node(
            &scope,
            &run.id,
            NodeSpec {
                id: "work".into(),
                contract: Contract::default(),
                session_id: None,
                status: Some(LifecycleStatus::Working),
                attempt: Some(1),
                depends_on: Vec::new(),
                execution: None,
                judge_policy: None,
            },
        )
        .expect("node");
        update_run(&scope, &run.id, LifecycleStatus::Working).expect("working run");

        let audit = doctor(&Config::default(), &scope, false).expect("doctor audit");
        assert!(!audit.repaired);
        assert_eq!(audit.run_issues.len(), 1);
        assert_eq!(
            audit.run_issues[0].action,
            crate::domain::RunRepairAction::ResolveAssignment
        );
        assert_eq!(
            read_workspace(&scope).expect("unchanged run").runs[0].status,
            LifecycleStatus::Working
        );

        let repaired = doctor(&Config::default(), &scope, true).expect("doctor repair");
        assert!(repaired.repaired);
        assert_eq!(repaired.repaired_runs, vec![run.id.clone()]);
        assert_eq!(repaired.run_issues.len(), 1);
        assert_eq!(repaired.run_issues[0].status, LifecycleStatus::Blocked);
        let repaired_state = read_workspace(&scope).expect("repaired state");
        assert!(repaired_state.sessions.is_empty());
        assert_eq!(repaired_state.runs[0].nodes[0].session_id, None);
        let event_count = repaired_state.runs[0].recovery.events.len();

        let repeated = doctor(&Config::default(), &scope, true).expect("repeat repair");
        assert!(!repeated.repaired);
        assert!(repeated.repaired_runs.is_empty());
        assert_eq!(
            read_workspace(&scope).expect("stable state").runs[0]
                .recovery
                .events
                .len(),
            event_count
        );
        let _ = std::fs::remove_file(state::path(&scope));
    }

    #[test]
    fn keepalive_renews_idle_lease_without_resetting_runtime() {
        let directory = tempfile::tempdir().expect("scope");
        let scope = std::fs::canonicalize(directory.path()).expect("canonical scope");
        let linked = register(
            &scope,
            Contract {
                harness: "original-harness".into(),
                model: Some("original-model".into()),
                role: SessionRole::Worker,
                ..Contract::default()
            },
            SessionLink {
                id: Some(format!("worker-{}", Uuid::new_v4())),
                native_id: Some(Uuid::new_v4().to_string()),
                run_id: Some("run".into()),
                runtime_timeout_seconds: Some(7200),
                idle_timeout_seconds: Some(300),
                source: RegistrationSource::Managed,
                ..SessionLink::default()
            },
        )
        .expect("managed session");
        let original_connected_at = linked.connected_at;
        let old_heartbeat = original_connected_at - chrono::Duration::hours(1);
        state::update(&scope, |workspace| {
            workspace.sessions[0].heartbeat_at = Some(old_heartbeat);
            Ok(())
        })
        .expect("age heartbeat");

        let renewed = keepalive(&scope, &linked.id).expect("renew managed session");

        assert_eq!(renewed.connected_at, original_connected_at);
        assert!(renewed.heartbeat_at.is_some_and(|at| at > old_heartbeat));
        assert_eq!(renewed.runtime_timeout_seconds, Some(7200));
        assert_eq!(renewed.idle_timeout_seconds, Some(300));
        let _ = std::fs::remove_file(state::path(&scope));
    }

    #[test]
    fn keepalive_preserves_failed_termination_identity() {
        let directory = tempfile::tempdir().expect("scope");
        let scope = std::fs::canonicalize(directory.path()).expect("canonical scope");
        let linked = register(
            &scope,
            Contract {
                role: SessionRole::Worker,
                ..Contract::default()
            },
            SessionLink {
                id: Some(format!("worker-{}", Uuid::new_v4())),
                native_id: Some(Uuid::new_v4().to_string()),
                source: RegistrationSource::Managed,
                ..SessionLink::default()
            },
        )
        .expect("managed session");
        let attempt_at = Utc::now() - chrono::Duration::seconds(5);
        state::update(&scope, |workspace| {
            let session = workspace.sessions.first_mut().expect("managed session");
            session.termination_reason = Some("termination failed: unavailable".into());
            session.termination_cause = Some("runtime timeout exceeded".into());
            session.termination_attempt_at = Some(attempt_at);
            session.termination_operation_id = Some("stable-operation".into());
            Ok(())
        })
        .expect("record failed termination");

        let renewed = keepalive(&scope, &linked.id).expect("renew managed session");

        assert_eq!(
            renewed.termination_reason.as_deref(),
            Some("termination failed: unavailable")
        );
        assert_eq!(
            renewed.termination_cause.as_deref(),
            Some("runtime timeout exceeded")
        );
        assert_eq!(renewed.termination_attempt_at, Some(attempt_at));
        assert_eq!(
            renewed.termination_operation_id.as_deref(),
            Some("stable-operation")
        );
        let _ = std::fs::remove_file(state::path(&scope));
    }

    #[test]
    fn managed_orchestrator_can_renew_its_idle_lease() {
        let directory = tempfile::tempdir().expect("scope");
        let scope = std::fs::canonicalize(directory.path()).expect("canonical scope");
        let linked = register(
            &scope,
            Contract {
                role: SessionRole::Orchestrator,
                ..Contract::default()
            },
            SessionLink {
                id: Some(format!("orchestrator-{}", Uuid::new_v4())),
                native_id: Some(Uuid::new_v4().to_string()),
                source: RegistrationSource::Managed,
                ..SessionLink::default()
            },
        )
        .expect("managed orchestrator");

        let renewed = keepalive(&scope, &linked.id).expect("renew orchestrator lease");

        assert!(renewed.heartbeat_at >= linked.heartbeat_at);
        let _ = std::fs::remove_file(state::path(&scope));
    }

    #[test]
    fn archive_and_reconciliation_preserve_a_termination_claim() {
        let directory = tempfile::tempdir().expect("scope");
        let scope = std::fs::canonicalize(directory.path()).expect("canonical scope");
        let linked = register(
            &scope,
            Contract::default(),
            SessionLink {
                id: Some(format!("worker-{}", Uuid::new_v4())),
                native_id: Some(Uuid::new_v4().to_string()),
                source: RegistrationSource::Managed,
                ..SessionLink::default()
            },
        )
        .expect("managed session");
        state::update(&scope, |workspace| {
            let selected = workspace
                .sessions
                .iter_mut()
                .find(|session| session.id == linked.id)
                .expect("session");
            selected.status = LifecycleStatus::Terminating;
            selected.termination_reason = Some("termination pending operation".into());
            selected.providers = vec![binding(
                "persistence",
                ProviderKind::Persistence,
                BindingStatus::Active,
            )];
            Ok(())
        })
        .expect("claim termination");

        archive(&scope, Some(&linked.id), None).expect_err("archive must reject termination");
        state::update(&scope, |workspace| {
            let selected = workspace
                .sessions
                .iter_mut()
                .find(|session| session.id == linked.id)
                .expect("session");
            apply_enrichment(selected, &[], None, None);
            Ok(())
        })
        .expect("reconcile terminating session");

        let workspace = state::read(&scope).expect("workspace");
        let selected = selected_session(&workspace, &linked.id).expect("session");
        assert_eq!(selected.status, LifecycleStatus::Terminating);
        assert_eq!(
            selected.termination_reason.as_deref(),
            Some("termination pending operation")
        );
        let _ = std::fs::remove_file(state::path(&scope));
    }

    #[cfg(unix)]
    #[test]
    fn managed_launch_does_not_start_after_concurrent_termination() {
        let directory = tempfile::tempdir().expect("launch fixture");
        let scope_directory = directory.path().join("scope");
        let provider_directory = directory.path().join("providers");
        fs::create_dir_all(&scope_directory).expect("scope");
        fs::create_dir_all(&provider_directory).expect("providers");
        let scope = fs::canonicalize(scope_directory).expect("canonical scope");
        let provider = directory.path().join("provider.sh");
        let resolving = directory.path().join("resolving");
        let release = directory.path().join("release");
        let launched = directory.path().join("launched");
        fs::write(
            &provider,
            render_fixture(
                LAUNCH_RACE_PROVIDER,
                serde_json::json!({
                    "resolving": resolving.display().to_string(),
                    "release": release.display().to_string(),
                    "launched": launched.display().to_string(),
                }),
            ),
        )
        .expect("provider script");
        fs::set_permissions(&provider, fs::Permissions::from_mode(0o755))
            .expect("provider executable");
        fs::write(
            provider_directory.join("provider.yaml"),
            render_fixture(
                PROVIDER_MANIFEST,
                serde_json::json!({
                    "name": "launch-race",
                    "command": provider.display().to_string(),
                    "actions": [
                        {
                            "capability": "session.launch",
                            "description": "Launch a session",
                        },
                        {
                            "capability": "session.stop",
                            "description": "Stop a session",
                        },
                    ],
                }),
            ),
        )
        .expect("provider manifest");
        let mut config = Config::default();
        config.providers.directory = provider_directory;
        let deadline = std::time::Instant::now() + config.provider_timeout();
        let worker_config = config.clone();
        let worker_scope = scope.clone();
        let worker = std::thread::spawn(move || {
            launch(
                &worker_config,
                &worker_scope,
                "test-harness".into(),
                None,
                Some("persistent".into()),
                SessionLease::default(),
                Vec::new(),
            )
        });
        while !resolving.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "launch resolution did not start"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let session_id = state::read(&scope)
            .expect("workspace")
            .sessions
            .first()
            .expect("managed session")
            .id
            .clone();

        terminate(&config, &scope, &session_id, "test cancellation")
            .expect("terminate managed session");
        fs::write(&release, []).expect("release launch provider");
        worker
            .join()
            .expect("launch thread")
            .expect_err("cancelled launch must fail");

        assert!(!launched.exists());
        let _ = fs::remove_file(state::path(&scope));
    }

    #[cfg(unix)]
    #[test]
    fn managed_launch_is_not_cancellable_before_its_resource_is_ready() {
        let directory = tempfile::tempdir().expect("launch fixture");
        let scope_directory = directory.path().join("scope");
        let provider_directory = directory.path().join("providers");
        fs::create_dir_all(&scope_directory).expect("scope");
        fs::create_dir_all(&provider_directory).expect("providers");
        let scope = fs::canonicalize(scope_directory).expect("canonical scope");
        let provider = directory.path().join("provider.sh");
        let launcher = directory.path().join("launcher.sh");
        let spawned = directory.path().join("spawned");
        let release = directory.path().join("release");
        let resource = directory.path().join("resource");
        let stopped = directory.path().join("stopped");
        fs::write(
            &launcher,
            render_fixture(
                READY_LAUNCHER,
                serde_json::json!({
                    "spawned": spawned.display().to_string(),
                    "release": release.display().to_string(),
                    "resource": resource.display().to_string(),
                }),
            ),
        )
        .expect("launcher script");
        fs::set_permissions(&launcher, fs::Permissions::from_mode(0o755))
            .expect("launcher executable");
        fs::write(
            &provider,
            render_fixture(
                READY_PROVIDER,
                serde_json::json!({
                    "launcher": launcher.display().to_string(),
                    "resource": resource.display().to_string(),
                    "stopped": stopped.display().to_string(),
                }),
            ),
        )
        .expect("provider script");
        fs::set_permissions(&provider, fs::Permissions::from_mode(0o755))
            .expect("provider executable");
        fs::write(
            provider_directory.join("provider.yaml"),
            render_fixture(
                PROVIDER_MANIFEST,
                serde_json::json!({
                    "name": "launch-ready",
                    "command": provider.display().to_string(),
                    "actions": [
                        {
                            "capability": "execution.run",
                            "description": "Execute a launch plan",
                        },
                        {
                            "capability": "session.bind",
                            "description": "Inspect a session",
                        },
                        {
                            "capability": "session.launch",
                            "description": "Launch a session",
                        },
                        {
                            "capability": "session.stop",
                            "description": "Stop a session",
                        },
                    ],
                }),
            ),
        )
        .expect("provider manifest");
        let mut config = Config::default();
        config.providers.directory = provider_directory;
        let deadline = std::time::Instant::now() + config.provider_timeout();
        let worker_config = config.clone();
        let worker_scope = scope.clone();
        let launch_worker = std::thread::spawn(move || {
            launch(
                &worker_config,
                &worker_scope,
                "test-harness".into(),
                None,
                Some("persistent".into()),
                SessionLease::default(),
                Vec::new(),
            )
        });
        while !spawned.exists() {
            if launch_worker.is_finished() {
                let result = launch_worker.join().expect("launch thread");
                panic!("managed launch exited before spawning: {result:?}");
            }
            assert!(
                std::time::Instant::now() < deadline,
                "managed launch command did not spawn"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let session_id = state::read(&scope)
            .expect("workspace")
            .sessions
            .first()
            .expect("managed session")
            .id
            .clone();
        let stop_config = config.clone();
        let stop_scope = scope.clone();
        let stop_id = session_id.clone();
        let stop_worker = std::thread::spawn(move || {
            terminate(&stop_config, &stop_scope, &stop_id, "test cancellation")
        });
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(!stopped.exists(), "stop cannot run before launch readiness");

        fs::write(&release, []).expect("release managed resource");
        let _ = launch_worker.join().expect("launch thread");
        let terminated = stop_worker
            .join()
            .expect("stop thread")
            .expect("terminate managed session");

        assert_eq!(terminated.status, LifecycleStatus::Cancelled);
        assert!(stopped.exists());
        assert!(!resource.exists());
        let _ = fs::remove_file(state::path(&scope));
    }

    #[test]
    fn reregistration_cannot_weaken_a_managed_session_lease() {
        let directory = tempfile::tempdir().expect("scope");
        let scope = std::fs::canonicalize(directory.path()).expect("canonical scope");
        let linked = register(
            &scope,
            Contract {
                harness: "original-harness".into(),
                model: Some("original-model".into()),
                role: SessionRole::Worker,
                ..Contract::default()
            },
            SessionLink {
                id: Some(format!("worker-{}", Uuid::new_v4())),
                native_id: Some(Uuid::new_v4().to_string()),
                run_id: Some("run".into()),
                provider_ref: Some("managed-a".into()),
                runtime_timeout_seconds: Some(3600),
                idle_timeout_seconds: Some(300),
                source: RegistrationSource::Managed,
                ..SessionLink::default()
            },
        )
        .expect("managed session");

        let reregistered = register(
            &scope,
            Contract {
                harness: "original-harness".into(),
                model: Some("replacement-model".into()),
                role: SessionRole::Orchestrator,
                ..Contract::default()
            },
            SessionLink {
                id: Some(linked.id.clone()),
                native_id: Some("native-b".into()),
                provider_ref: Some("managed-b".into()),
                runtime_timeout_seconds: Some(0),
                idle_timeout_seconds: Some(0),
                source: RegistrationSource::Connected,
                ..SessionLink::default()
            },
        )
        .expect("repeat registration");

        assert_eq!(reregistered.role, SessionRole::Worker);
        assert_eq!(reregistered.harness, "original-harness");
        assert_eq!(reregistered.model.as_deref(), Some("original-model"));
        assert_eq!(reregistered.registration, RegistrationSource::Managed);
        assert_eq!(reregistered.runtime_timeout_seconds, Some(3600));
        assert_eq!(reregistered.idle_timeout_seconds, Some(300));
        assert_eq!(reregistered.run_id.as_deref(), Some("run"));
        assert_eq!(reregistered.native_id, linked.native_id);
        assert_eq!(reregistered.provider_ref.as_deref(), Some("managed-a"));
        let _ = std::fs::remove_file(state::path(&scope));
    }

    #[test]
    fn managed_child_cannot_create_an_orchestrator_registration() {
        let directory = tempfile::tempdir().expect("scope");
        let scope = std::fs::canonicalize(directory.path()).expect("canonical scope");
        let orchestrator = register(
            &scope,
            Contract {
                harness: "root-harness".into(),
                role: SessionRole::Orchestrator,
                ..Contract::default()
            },
            SessionLink {
                id: Some("root".into()),
                native_id: Some("root-native".into()),
                source: RegistrationSource::Connected,
                ..SessionLink::default()
            },
        )
        .expect("orchestrator");
        let worker = register(
            &scope,
            Contract {
                harness: "child-harness".into(),
                role: SessionRole::Worker,
                ..Contract::default()
            },
            SessionLink {
                id: Some("worker".into()),
                native_id: Some("worker-native".into()),
                parent_id: Some(orchestrator.id),
                run_id: Some("run".into()),
                source: RegistrationSource::Managed,
                ..SessionLink::default()
            },
        )
        .expect("managed worker");
        let mut forged = Contract {
            harness: "forged-harness".into(),
            role: SessionRole::Orchestrator,
            ..Contract::default()
        };

        let error = register_for_caller(
            &scope,
            &mut forged,
            SessionLink {
                id: Some("forged-root".into()),
                native_id: Some("forged-native".into()),
                source: RegistrationSource::Connected,
                ..SessionLink::default()
            },
            Some(&worker.id),
            None,
        )
        .expect_err("managed child registration must fail");

        assert!(error.to_string().contains("only refresh its own"));
        assert!(
            read_workspace(&scope)
                .expect("workspace")
                .sessions
                .iter()
                .all(|session| session.id != "forged-root")
        );
        let _ = std::fs::remove_file(state::path(&scope));
    }

    #[test]
    fn orchestrator_child_registration_uses_a_distinct_session() {
        let directory = tempfile::tempdir().expect("scope");
        let scope = std::fs::canonicalize(directory.path()).expect("canonical scope");
        let orchestrator = register(
            &scope,
            Contract {
                harness: "root-harness".into(),
                role: SessionRole::Orchestrator,
                ..Contract::default()
            },
            SessionLink {
                id: Some("root".into()),
                native_id: Some("root-native".into()),
                source: RegistrationSource::Connected,
                ..SessionLink::default()
            },
        )
        .expect("orchestrator");
        let mut child_contract = Contract {
            harness: "child-harness".into(),
            role: SessionRole::Implementer,
            ..Contract::default()
        };

        let child = register_for_caller(
            &scope,
            &mut child_contract,
            SessionLink {
                native_id: Some("child-native".into()),
                parent_id: Some(orchestrator.id.clone()),
                run_id: Some("run".into()),
                node_id: Some("implement".into()),
                source: RegistrationSource::Managed,
                ..SessionLink::default()
            },
            Some(&orchestrator.id),
            Some(vec![binding(
                "test-owner",
                ProviderKind::Persistence,
                BindingStatus::Active,
            )]),
        )
        .expect("managed child");

        assert_ne!(child.id, orchestrator.id);
        assert_eq!(child.parent_id.as_deref(), Some(orchestrator.id.as_str()));
        let workspace = read_workspace(&scope).expect("workspace");
        assert!(workspace.sessions.iter().any(|session| {
            session.id == orchestrator.id && session.role == SessionRole::Orchestrator
        }));
        let _ = std::fs::remove_file(state::path(&scope));
    }

    #[test]
    fn connected_worker_cannot_promote_its_own_registration() {
        let directory = tempfile::tempdir().expect("scope");
        let scope = std::fs::canonicalize(directory.path()).expect("canonical scope");
        let root = register(
            &scope,
            Contract {
                role: SessionRole::Orchestrator,
                ..Contract::default()
            },
            SessionLink {
                id: Some("root".into()),
                native_id: Some("root-native".into()),
                ..SessionLink::default()
            },
        )
        .expect("root");
        let worker = register(
            &scope,
            Contract {
                harness: "worker-harness".into(),
                role: SessionRole::Worker,
                ..Contract::default()
            },
            SessionLink {
                id: Some("worker".into()),
                native_id: Some("worker-native".into()),
                parent_id: Some(root.id),
                source: RegistrationSource::Connected,
                ..SessionLink::default()
            },
        )
        .expect("worker");
        let mut forged = Contract {
            harness: "worker-harness".into(),
            role: SessionRole::Orchestrator,
            ..Contract::default()
        };

        let refreshed = register_for_caller(
            &scope,
            &mut forged,
            SessionLink {
                id: Some(worker.id.clone()),
                native_id: Some("other-native".into()),
                source: RegistrationSource::Connected,
                ..SessionLink::default()
            },
            Some(&worker.id),
            None,
        )
        .expect("refresh worker");

        assert_eq!(refreshed.role, SessionRole::Worker);
        assert_eq!(refreshed.harness, "worker-harness");
        assert_eq!(refreshed.native_id, "worker-native");
        let _ = std::fs::remove_file(state::path(&scope));
    }

    #[test]
    fn registering_one_native_session_under_another_harness_requires_adoption() {
        let directory = tempfile::tempdir().expect("scope");
        let scope = std::fs::canonicalize(directory.path()).expect("canonical scope");
        let first = register(
            &scope,
            Contract {
                harness: "codex".into(),
                role: SessionRole::Orchestrator,
                ..Contract::default()
            },
            SessionLink {
                native_id: Some("native-session".into()),
                ..SessionLink::default()
            },
        )
        .expect("first registration");

        let before = read_workspace(&scope).expect("workspace before rejected registration");
        let error = register(
            &scope,
            Contract {
                harness: "claude".into(),
                role: SessionRole::Orchestrator,
                ..Contract::default()
            },
            SessionLink {
                native_id: Some("native-session".into()),
                ..SessionLink::default()
            },
        )
        .expect_err("a different harness must not reuse a native identity");

        let workspace = read_workspace(&scope).expect("workspace");
        assert!(error.to_string().contains("session adopt"), "{error:#}");
        assert_eq!(workspace, before);
        assert_eq!(workspace.sessions[0].id, first.id);
        let _ = std::fs::remove_file(state::path(&scope));
    }

    #[test]
    fn same_harness_reconnect_preserves_authority_and_refreshes_liveness() {
        let directory = tempfile::tempdir().expect("scope");
        let scope = std::fs::canonicalize(directory.path()).expect("canonical scope");
        let root = register(
            &scope,
            Contract {
                harness: "codex".into(),
                role: SessionRole::Orchestrator,
                ..Contract::default()
            },
            SessionLink {
                native_id: Some("root-native".into()),
                ..SessionLink::default()
            },
        )
        .expect("root registration");
        let worker = register(
            &scope,
            Contract {
                harness: "claude".into(),
                model: Some("original-model".into()),
                role: SessionRole::Implementer,
                title: "Implement identity safety".into(),
                purpose: "Prevent cross-harness mutation".into(),
                goal: "Qualify native identities".into(),
                expected_output: "Verified identity invariants".into(),
                ..Contract::default()
            },
            SessionLink {
                native_id: Some("shared-native".into()),
                parent_id: Some(root.id.clone()),
                run_id: Some("identity-run".into()),
                node_id: Some("implement".into()),
                source: RegistrationSource::Hook,
                ..SessionLink::default()
            },
        )
        .expect("worker registration");
        let old_heartbeat = Utc::now() - chrono::Duration::hours(1);
        let existing_binding = binding("activity", ProviderKind::Activity, BindingStatus::Active);
        state::update(&scope, |workspace| {
            let existing = workspace
                .sessions
                .iter_mut()
                .find(|session| session.id == worker.id)
                .context("worker disappeared")?;
            existing.status = LifecycleStatus::Disconnected;
            existing.heartbeat_at = Some(old_heartbeat);
            existing.providers = vec![existing_binding.clone()];
            existing.reported_output = Some(ReportedOutput {
                value: serde_json::json!({"result": "retained"}),
            });
            Ok(())
        })
        .expect("disconnect worker");

        let reconnected = register(
            &scope,
            Contract {
                harness: "claude".into(),
                model: Some("replacement-model".into()),
                role: SessionRole::Judge,
                title: "Replacement title".into(),
                purpose: "Replacement purpose".into(),
                goal: "Replacement goal".into(),
                expected_output: "Replacement output".into(),
                ..Contract::default()
            },
            SessionLink {
                native_id: Some("shared-native".into()),
                source: RegistrationSource::Connected,
                ..SessionLink::default()
            },
        )
        .expect("same-harness reconnect");

        assert_eq!(reconnected.id, worker.id);
        assert_eq!(reconnected.harness, "claude");
        assert_eq!(reconnected.model.as_deref(), Some("original-model"));
        assert_eq!(reconnected.role, SessionRole::Implementer);
        assert_eq!(reconnected.title, "Implement identity safety");
        assert_eq!(reconnected.purpose, "Prevent cross-harness mutation");
        assert_eq!(reconnected.goal, "Qualify native identities");
        assert_eq!(reconnected.expected_output, "Verified identity invariants");
        assert_eq!(reconnected.registration, RegistrationSource::Hook);
        assert_eq!(reconnected.parent_id.as_deref(), Some(root.id.as_str()));
        assert_eq!(reconnected.run_id.as_deref(), Some("identity-run"));
        assert_eq!(reconnected.node_id.as_deref(), Some("implement"));
        assert_eq!(reconnected.providers, vec![existing_binding]);
        assert_eq!(
            reconnected.reported_output,
            Some(ReportedOutput {
                value: serde_json::json!({"result": "retained"}),
            })
        );
        assert_eq!(reconnected.status, LifecycleStatus::Working);
        assert!(
            reconnected
                .heartbeat_at
                .is_some_and(|at| at > old_heartbeat)
        );
        let _ = std::fs::remove_file(state::path(&scope));
    }

    #[test]
    fn explicit_id_cannot_replace_another_native_session() {
        let directory = tempfile::tempdir().expect("scope");
        let scope = std::fs::canonicalize(directory.path()).expect("canonical scope");
        let root = register(
            &scope,
            Contract {
                harness: "root-harness".into(),
                role: SessionRole::Orchestrator,
                ..Contract::default()
            },
            SessionLink {
                id: Some("root".into()),
                native_id: Some("root-native".into()),
                ..SessionLink::default()
            },
        )
        .expect("root registration");

        let error = register(
            &scope,
            Contract {
                harness: "root-harness".into(),
                role: SessionRole::Worker,
                ..Contract::default()
            },
            SessionLink {
                id: Some(root.id.clone()),
                native_id: Some("different-native".into()),
                ..SessionLink::default()
            },
        )
        .expect_err("explicit id collision");

        assert!(error.to_string().contains("another native session"));
        let current = read_workspace(&scope).expect("workspace");
        assert_eq!(current.sessions.len(), 1);
        assert_eq!(current.sessions[0].native_id, "root-native");
        assert_eq!(current.sessions[0].role, SessionRole::Orchestrator);
        let _ = std::fs::remove_file(state::path(&scope));
    }

    #[test]
    fn explicit_id_cannot_register_under_another_harness() {
        let directory = tempfile::tempdir().expect("scope");
        let scope = std::fs::canonicalize(directory.path()).expect("canonical scope");
        let root = register(
            &scope,
            Contract {
                harness: "codex".into(),
                role: SessionRole::Orchestrator,
                ..Contract::default()
            },
            SessionLink {
                id: Some("root".into()),
                native_id: Some("native".into()),
                ..SessionLink::default()
            },
        )
        .expect("root registration");
        let before = read_workspace(&scope).expect("workspace before rejected registration");

        let error = register(
            &scope,
            Contract {
                harness: "claude".into(),
                role: SessionRole::Orchestrator,
                ..Contract::default()
            },
            SessionLink {
                id: Some(root.id),
                native_id: Some("native".into()),
                ..SessionLink::default()
            },
        )
        .expect_err("explicit cross-harness registration must fail");

        assert!(error.to_string().contains("belongs to harness codex"));
        assert_eq!(read_workspace(&scope).expect("unchanged workspace"), before);
        let _ = std::fs::remove_file(state::path(&scope));
    }

    #[test]
    fn cancelled_status_requires_provider_termination() {
        let directory = tempfile::tempdir().expect("scope");
        let scope = std::fs::canonicalize(directory.path()).expect("canonical scope");
        let session = register(
            &scope,
            Contract {
                harness: "codex".into(),
                role: SessionRole::Orchestrator,
                ..Contract::default()
            },
            SessionLink {
                native_id: Some("native-session".into()),
                ..SessionLink::default()
            },
        )
        .expect("registration");

        let error = update_session(&scope, &session.id, LifecycleStatus::Cancelled)
            .expect_err("cancellation must invoke a provider");

        assert!(error.to_string().contains("provider termination"));
        assert_eq!(
            selected_session(&read_workspace(&scope).expect("workspace"), &session.id)
                .expect("session")
                .status,
            LifecycleStatus::Working
        );
        let _ = std::fs::remove_file(state::path(&scope));
    }

    #[test]
    fn orchestrator_output_replaces_one_persisted_value() {
        let directory = tempfile::tempdir().expect("scope");
        let scope = std::fs::canonicalize(directory.path()).expect("canonical scope");
        let session = register(
            &scope,
            Contract {
                role: SessionRole::Orchestrator,
                ..Contract::default()
            },
            SessionLink {
                native_id: Some("root-native".into()),
                ..SessionLink::default()
            },
        )
        .expect("register orchestrator");

        report_session_output(&scope, &session.id, serde_json::json!({"first": true}))
            .expect("report first output");
        let boundary = serde_json::Value::String("x".repeat(MAX_OUTPUT_BYTES - 2));
        assert_eq!(
            serde_json::to_vec(&boundary)
                .expect("serialize boundary output")
                .len(),
            MAX_OUTPUT_BYTES
        );
        report_session_output(&scope, &session.id, boundary).expect("report boundary output");
        let receipt = report_session_output(&scope, &session.id, serde_json::Value::Null)
            .expect("replace output");
        let restored = read_workspace(&scope).expect("workspace");

        assert_eq!(receipt.status, SessionOutputStatus::Reported);
        assert_eq!(receipt.input_bytes, 4);
        assert_eq!(receipt.updated_at.len(), 24);
        assert!(receipt.updated_at.ends_with('Z'));
        assert_eq!(
            serde_json::to_value(&receipt).expect("serialize receipt"),
            serde_json::json!({
                "status": "reported",
                "inputBytes": 4,
                "updatedAt": receipt.updated_at,
            })
        );
        assert_eq!(
            selected_session(&restored, &session.id)
                .expect("persisted session")
                .reported_output,
            Some(ReportedOutput {
                value: serde_json::Value::Null
            })
        );
        assert!(
            selected_session(&restored, &session.id)
                .expect("persisted session")
                .updated_at
                >= session.updated_at
        );
        let _ = std::fs::remove_file(state::path(&scope));
    }

    #[test]
    fn rejected_session_outputs_leave_workspace_unchanged() {
        let directory = tempfile::tempdir().expect("scope");
        let scope = std::fs::canonicalize(directory.path()).expect("canonical scope");
        let root = register(
            &scope,
            Contract {
                role: SessionRole::Orchestrator,
                ..Contract::default()
            },
            SessionLink {
                native_id: Some("root-native".into()),
                ..SessionLink::default()
            },
        )
        .expect("register orchestrator");
        let worker = register(
            &scope,
            Contract::default(),
            SessionLink {
                native_id: Some("worker-native".into()),
                parent_id: Some(root.id.clone()),
                ..SessionLink::default()
            },
        )
        .expect("register worker");

        for (id, output) in [
            (
                worker.id.as_str(),
                serde_json::json!({"unauthorized": true}),
            ),
            (
                root.id.as_str(),
                serde_json::Value::String("x".repeat(MAX_OUTPUT_BYTES)),
            ),
        ] {
            let before = read_workspace(&scope).expect("workspace before rejection");
            report_session_output(&scope, id, output).expect_err("reject report");
            assert_eq!(
                read_workspace(&scope).expect("workspace after rejection"),
                before
            );
        }

        state::update(&scope, |workspace| {
            let root = workspace
                .sessions
                .iter_mut()
                .find(|session| session.id == root.id)
                .expect("root session");
            root.status = LifecycleStatus::Terminating;
            Ok(())
        })
        .expect("mark terminating");
        let before = read_workspace(&scope).expect("terminating workspace");
        report_session_output(&scope, &root.id, serde_json::json!({"late": true}))
            .expect_err("reject terminating report");
        assert_eq!(read_workspace(&scope).expect("unchanged workspace"), before);

        state::update(&scope, |workspace| {
            workspace
                .sessions
                .iter_mut()
                .find(|session| session.id == root.id)
                .expect("root session")
                .status = LifecycleStatus::Archived;
            Ok(())
        })
        .expect("archive root");
        let before = read_workspace(&scope).expect("archived workspace");
        report_session_output(&scope, &root.id, serde_json::json!({"later": true}))
            .expect_err("reject archived report");
        assert_eq!(read_workspace(&scope).expect("unchanged workspace"), before);
        let _ = std::fs::remove_file(state::path(&scope));
    }

    #[test]
    fn idle_workspace_rejects_output_without_rewriting_state() {
        let directory = tempfile::tempdir().expect("scope");
        let scope = std::fs::canonicalize(directory.path()).expect("canonical scope");
        let root = register(
            &scope,
            Contract {
                role: SessionRole::Orchestrator,
                ..Contract::default()
            },
            SessionLink {
                native_id: Some("root-native".into()),
                ..SessionLink::default()
            },
        )
        .expect("register orchestrator");
        let path = state::path(&scope);
        let mut persisted: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).expect("read workspace before making it idle"))
                .expect("parse workspace");
        persisted["active"] = serde_json::Value::Bool(false);
        fs::write(
            &path,
            serde_json::to_vec_pretty(&persisted).expect("serialize idle workspace"),
        )
        .expect("write idle workspace");
        let before = fs::read(&path).expect("read idle workspace");

        let error = report_session_output(&scope, &root.id, serde_json::json!({"late": true}))
            .expect_err("reject idle workspace report");

        assert!(error.to_string().contains("scope is idle"));
        assert_eq!(fs::read(&path).expect("read unchanged workspace"), before);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn same_session_changes_preserve_reported_output() {
        let directory = tempfile::tempdir().expect("scope");
        let scope = std::fs::canonicalize(directory.path()).expect("canonical scope");
        let root = register(
            &scope,
            Contract {
                role: SessionRole::Orchestrator,
                ..Contract::default()
            },
            SessionLink {
                native_id: Some("root-native".into()),
                source: RegistrationSource::Managed,
                ..SessionLink::default()
            },
        )
        .expect("register orchestrator");
        let output = ReportedOutput {
            value: serde_json::json!({"result": "verified"}),
        };
        report_session_output(&scope, &root.id, output.value.clone()).expect("report output");

        let reregistered = register(
            &scope,
            Contract {
                harness: root.harness.clone(),
                role: SessionRole::Orchestrator,
                ..Contract::default()
            },
            SessionLink {
                native_id: Some("root-native".into()),
                source: RegistrationSource::Managed,
                ..SessionLink::default()
            },
        )
        .expect("repeat registration");
        let renewed = keepalive(&scope, &root.id).expect("renew idle lease");
        let waiting = update_session(&scope, &root.id, LifecycleStatus::Waiting)
            .expect("update lifecycle state");

        assert_eq!(reregistered.reported_output, Some(output.clone()));
        assert_eq!(renewed.reported_output, Some(output.clone()));
        assert_eq!(waiting.reported_output, Some(output));
        let _ = std::fs::remove_file(state::path(&scope));
    }

    #[test]
    fn replacing_root_keeps_output_only_on_archived_reporter() {
        let directory = tempfile::tempdir().expect("scope");
        let scope = std::fs::canonicalize(directory.path()).expect("canonical scope");
        let root = register(
            &scope,
            Contract {
                role: SessionRole::Orchestrator,
                ..Contract::default()
            },
            SessionLink {
                native_id: Some("old-native".into()),
                ..SessionLink::default()
            },
        )
        .expect("register orchestrator");
        let output = serde_json::json!({"owner": "old root"});
        report_session_output(&scope, &root.id, output.clone()).expect("report output");

        let replacement = adopt(&scope, Contract::default(), Some("new-native".into()))
            .expect("replace orchestrator");
        let workspace = read_workspace(&scope).expect("workspace");
        let archived = selected_session(&workspace, &root.id).expect("archived reporter");

        assert_eq!(archived.status, LifecycleStatus::Archived);
        assert_eq!(
            archived
                .reported_output
                .as_ref()
                .map(|report| &report.value),
            Some(&output)
        );
        assert_eq!(replacement.reported_output, None);
        let _ = std::fs::remove_file(state::path(&scope));
    }

    #[test]
    fn disconnected_managed_session_can_refresh_its_registration() {
        let directory = tempfile::tempdir().expect("scope");
        let scope = std::fs::canonicalize(directory.path()).expect("canonical scope");
        let root = register(
            &scope,
            Contract {
                harness: "codex".into(),
                role: SessionRole::Orchestrator,
                ..Contract::default()
            },
            SessionLink {
                native_id: Some("root-native".into()),
                ..SessionLink::default()
            },
        )
        .expect("root registration");
        let child = register(
            &scope,
            Contract {
                harness: "pi".into(),
                role: SessionRole::Worker,
                ..Contract::default()
            },
            SessionLink {
                native_id: Some("child-native".into()),
                parent_id: Some(root.id),
                source: RegistrationSource::Managed,
                ..SessionLink::default()
            },
        )
        .expect("child registration");
        update_session(&scope, &child.id, LifecycleStatus::Disconnected)
            .expect("disconnect managed session");
        let mut contract = Contract {
            harness: "pi".into(),
            role: SessionRole::Worker,
            ..Contract::default()
        };

        let refreshed = register_for_caller(
            &scope,
            &mut contract,
            SessionLink {
                native_id: Some("child-native".into()),
                source: RegistrationSource::Managed,
                ..SessionLink::default()
            },
            Some(&child.id),
            None,
        )
        .expect("refresh disconnected child");

        assert_eq!(refreshed.id, child.id);
        assert_eq!(refreshed.status, LifecycleStatus::Working);
        assert!(refreshed.termination_reason.is_none());
        let _ = std::fs::remove_file(state::path(&scope));
    }

    #[test]
    fn registering_a_second_orchestrator_requires_adoption() {
        let directory = tempfile::tempdir().expect("scope");
        let scope = std::fs::canonicalize(directory.path()).expect("canonical scope");
        let first = register(
            &scope,
            Contract {
                harness: "codex".into(),
                role: SessionRole::Orchestrator,
                ..Contract::default()
            },
            SessionLink {
                native_id: Some("native-a".into()),
                ..SessionLink::default()
            },
        )
        .expect("first orchestrator");

        let error = register(
            &scope,
            Contract {
                harness: "claude".into(),
                role: SessionRole::Orchestrator,
                ..Contract::default()
            },
            SessionLink {
                native_id: Some("native-b".into()),
                ..SessionLink::default()
            },
        )
        .expect_err("replacement requires adoption");

        assert!(error.to_string().contains("orc session adopt"));
        let workspace = read_workspace(&scope).expect("workspace");
        assert_eq!(
            workspace
                .active_sessions()
                .filter(|session| session.role == SessionRole::Orchestrator)
                .map(|session| session.id.as_str())
                .collect::<Vec<_>>(),
            vec![first.id.as_str()]
        );
        let _ = std::fs::remove_file(state::path(&scope));
    }

    #[test]
    fn disconnected_orchestrator_still_requires_an_adoption_transition() {
        let directory = tempfile::tempdir().expect("scope");
        let scope = std::fs::canonicalize(directory.path()).expect("canonical scope");
        let first = register(
            &scope,
            Contract {
                harness: "codex".into(),
                role: SessionRole::Orchestrator,
                ..Contract::default()
            },
            SessionLink {
                native_id: Some("native-a".into()),
                ..SessionLink::default()
            },
        )
        .expect("first orchestrator");
        update_session(&scope, &first.id, LifecycleStatus::Disconnected)
            .expect("disconnect orchestrator");

        let error = register(
            &scope,
            Contract {
                harness: "claude".into(),
                role: SessionRole::Orchestrator,
                ..Contract::default()
            },
            SessionLink {
                native_id: Some("native-b".into()),
                ..SessionLink::default()
            },
        )
        .expect_err("disconnected root still owns the workspace");

        assert!(error.to_string().contains("orc session adopt"));
        let workspace = read_workspace(&scope).expect("workspace");
        assert_eq!(
            workspace
                .sessions
                .iter()
                .filter(|session| {
                    session.role == SessionRole::Orchestrator
                        && session.status != LifecycleStatus::Archived
                })
                .count(),
            1
        );
        let _ = std::fs::remove_file(state::path(&scope));
    }

    #[test]
    fn adopted_session_refreshes_through_its_archived_environment_id() {
        let directory = tempfile::tempdir().expect("scope");
        let scope = std::fs::canonicalize(directory.path()).expect("canonical scope");
        let old = register(
            &scope,
            Contract {
                harness: "codex".into(),
                role: SessionRole::Orchestrator,
                ..Contract::default()
            },
            SessionLink {
                native_id: Some("native-session".into()),
                ..SessionLink::default()
            },
        )
        .expect("old incarnation");
        let adopted = adopt(
            &scope,
            Contract {
                harness: "codex".into(),
                ..Contract::default()
            },
            Some("native-session".into()),
        )
        .expect("adopt session");
        state::update(&scope, |workspace| {
            let mut resumable = session(
                "resumable-descendant",
                SessionRole::Researcher,
                LifecycleStatus::Disconnected,
            );
            resumable.registration = RegistrationSource::Managed;
            resumable.parent_id = Some(old.id.clone());
            let mut terminal = session(
                "terminal-descendant",
                SessionRole::Verifier,
                LifecycleStatus::Failed,
            );
            terminal.registration = RegistrationSource::Managed;
            terminal.parent_id = Some(old.id.clone());
            workspace.sessions.extend([resumable, terminal]);
            Ok(())
        })
        .expect("add descendants of the archived root");
        let mut refreshed_contract = Contract {
            harness: "codex".into(),
            role: SessionRole::Orchestrator,
            title: "refreshed".into(),
            ..Contract::default()
        };

        let refreshed = register_for_caller(
            &scope,
            &mut refreshed_contract,
            SessionLink {
                native_id: Some("native-session".into()),
                ..SessionLink::default()
            },
            Some(&old.id),
            None,
        )
        .expect("refresh adopted incarnation");

        assert_eq!(refreshed.id, adopted.id);
        assert_eq!(refreshed.native_id, "native-session");
        let workspace = read_workspace(&scope).expect("workspace");
        assert_eq!(
            workspace
                .active_sessions()
                .filter(|session| session.role == SessionRole::Orchestrator)
                .count(),
            1
        );
        assert_eq!(
            workspace
                .sessions
                .iter()
                .find(|session| session.id == "resumable-descendant")
                .expect("resumable descendant")
                .parent_id
                .as_deref(),
            Some(adopted.id.as_str())
        );
        assert_eq!(
            workspace
                .sessions
                .iter()
                .find(|session| session.id == "terminal-descendant")
                .expect("terminal descendant")
                .parent_id
                .as_deref(),
            Some(old.id.as_str())
        );
        let _ = std::fs::remove_file(state::path(&scope));
    }

    #[test]
    fn adoption_cannot_reuse_an_active_child_native_id() {
        let directory = tempfile::tempdir().expect("scope");
        let scope = std::fs::canonicalize(directory.path()).expect("canonical scope");
        let root = register(
            &scope,
            Contract {
                harness: "codex".into(),
                role: SessionRole::Orchestrator,
                ..Contract::default()
            },
            SessionLink {
                native_id: Some("root-native".into()),
                ..SessionLink::default()
            },
        )
        .expect("root registration");
        let child = register(
            &scope,
            Contract {
                harness: "pi".into(),
                role: SessionRole::Worker,
                ..Contract::default()
            },
            SessionLink {
                native_id: Some("child-native".into()),
                parent_id: Some(root.id.clone()),
                source: RegistrationSource::Managed,
                ..SessionLink::default()
            },
        )
        .expect("child registration");

        let error = adopt(
            &scope,
            Contract {
                harness: "pi".into(),
                ..Contract::default()
            },
            Some(child.native_id.clone()),
        )
        .expect_err("child identity cannot become the orchestrator");

        assert!(error.to_string().contains("already belongs"));
        let workspace = read_workspace(&scope).expect("workspace");
        assert_eq!(
            workspace
                .active_sessions()
                .find(|session| session.role == SessionRole::Orchestrator)
                .expect("orchestrator")
                .id,
            root.id
        );
        assert_eq!(
            workspace
                .active_sessions()
                .find(|session| session.id == child.id)
                .expect("child")
                .parent_id
                .as_deref(),
            Some(root.id.as_str())
        );
        let _ = std::fs::remove_file(state::path(&scope));
    }

    #[test]
    fn stale_expiration_cannot_stop_a_renewed_session() {
        let directory = tempfile::tempdir().expect("scope");
        let scope = std::fs::canonicalize(directory.path()).expect("canonical scope");
        let linked = register(
            &scope,
            Contract {
                role: SessionRole::Worker,
                ..Contract::default()
            },
            SessionLink {
                id: Some(format!("worker-{}", Uuid::new_v4())),
                native_id: Some(Uuid::new_v4().to_string()),
                run_id: Some("run".into()),
                source: RegistrationSource::Managed,
                ..SessionLink::default()
            },
        )
        .expect("managed session");
        let observed_heartbeat = linked.heartbeat_at;
        let renewed = keepalive(&scope, &linked.id).expect("renew lease");
        let mut config = Config::default();
        config.providers.directory = directory.path().join("providers");

        let result = terminate_expired(
            &config,
            &scope,
            &linked.id,
            "idle timeout exceeded",
            Some(observed_heartbeat),
            None,
        )
        .expect("stale expiration is ignored");

        assert_eq!(result.status, LifecycleStatus::Working);
        assert_eq!(result.heartbeat_at, renewed.heartbeat_at);
        let _ = std::fs::remove_file(state::path(&scope));
    }

    #[test]
    fn hard_expiration_is_not_cancelled_by_a_new_heartbeat() {
        let directory = tempfile::tempdir().expect("scope");
        let scope = std::fs::canonicalize(directory.path()).expect("canonical scope");
        let linked = register(
            &scope,
            Contract {
                role: SessionRole::Worker,
                ..Contract::default()
            },
            SessionLink {
                id: Some(format!("worker-{}", Uuid::new_v4())),
                native_id: Some(Uuid::new_v4().to_string()),
                run_id: Some("run".into()),
                source: RegistrationSource::Managed,
                ..SessionLink::default()
            },
        )
        .expect("managed session");
        keepalive(&scope, &linked.id).expect("new heartbeat");
        let mut config = Config::default();
        config.providers.directory = directory.path().join("providers");

        terminate_expired(
            &config,
            &scope,
            &linked.id,
            "runtime timeout exceeded",
            None,
            None,
        )
        .expect_err("hard expiration must attempt termination");

        let session = read_workspace(&scope)
            .expect("workspace")
            .sessions
            .into_iter()
            .find(|session| session.id == linked.id)
            .expect("managed session");
        assert!(
            session
                .termination_reason
                .as_deref()
                .is_some_and(|reason| reason.starts_with("termination failed:"))
        );
        let _ = std::fs::remove_file(state::path(&scope));
    }

    #[test]
    fn failed_provider_stop_releases_the_termination_claim() {
        let directory = tempfile::tempdir().expect("scope");
        let scope = std::fs::canonicalize(directory.path()).expect("canonical scope");
        let linked = register(
            &scope,
            Contract {
                role: SessionRole::Worker,
                ..Contract::default()
            },
            SessionLink {
                id: Some(format!("worker-{}", Uuid::new_v4())),
                native_id: Some(Uuid::new_v4().to_string()),
                run_id: Some("run".into()),
                source: RegistrationSource::Managed,
                ..SessionLink::default()
            },
        )
        .expect("managed session");
        let mut config = Config::default();
        config.providers.directory = directory.path().join("providers");

        terminate(&config, &scope, &linked.id, "idle timeout exceeded")
            .expect_err("missing stop provider");

        let restored = read_workspace(&scope)
            .expect("workspace")
            .sessions
            .into_iter()
            .find(|session| session.id == linked.id)
            .expect("restored session");
        assert_eq!(restored.status, LifecycleStatus::Working);
        assert!(
            restored
                .termination_reason
                .as_deref()
                .is_some_and(|reason| reason.starts_with("termination failed:"))
        );
        assert_eq!(
            restored.termination_cause.as_deref(),
            Some("idle timeout exceeded")
        );
        let _ = std::fs::remove_file(state::path(&scope));
    }

    #[cfg(unix)]
    #[test]
    fn termination_runs_persistence_stop_and_execution_cancel() {
        let directory = tempfile::tempdir().expect("termination fixture");
        let scope_directory = directory.path().join("scope");
        let provider_directory = directory.path().join("providers");
        fs::create_dir_all(&scope_directory).expect("scope");
        fs::create_dir_all(&provider_directory).expect("providers");
        let scope = fs::canonicalize(scope_directory).expect("canonical scope");
        let log = directory.path().join("terminations");
        let recorder = directory.path().join("record-termination.sh");
        fs::write(
            &recorder,
            render_fixture(
                TERMINATION_RECORDER,
                serde_json::json!({ "log": log.display().to_string() }),
            ),
        )
        .expect("termination recorder");
        fs::set_permissions(&recorder, fs::Permissions::from_mode(0o755))
            .expect("recorder executable");
        let provider = directory.path().join("termination-provider.sh");
        fs::write(
            &provider,
            render_fixture(
                COMPOSED_TERMINATION_PROVIDER,
                serde_json::json!({
                    "executable_json": serde_json::to_string(&recorder.display().to_string())
                        .expect("serialize recorder path"),
                }),
            ),
        )
        .expect("termination provider");
        fs::set_permissions(&provider, fs::Permissions::from_mode(0o755))
            .expect("provider executable");
        for (name, kind, capability) in [
            ("persistence", "persistence", "session.stop"),
            ("executor", "execution", "execution.cancel"),
        ] {
            fs::write(
                provider_directory.join(format!("{name}.yaml")),
                render_fixture(
                    TERMINATION_PROVIDER_MANIFEST,
                    serde_json::json!({
                        "name": name,
                        "kind": kind,
                        "command": provider.display().to_string(),
                        "capability": capability,
                    }),
                ),
            )
            .expect("provider manifest");
        }
        let linked = register(
            &scope,
            Contract::default(),
            SessionLink {
                id: Some("managed".into()),
                native_id: Some("managed-native".into()),
                source: RegistrationSource::Managed,
                ..SessionLink::default()
            },
        )
        .expect("managed session");
        state::update(&scope, |workspace| {
            workspace.sessions[0].providers = vec![
                binding(
                    "persistence",
                    ProviderKind::Persistence,
                    BindingStatus::Active,
                ),
                binding("executor", ProviderKind::Execution, BindingStatus::Active),
            ];
            Ok(())
        })
        .expect("record lifecycle owners");
        let mut config = Config::default();
        config.providers.directory = provider_directory;

        let terminated = terminate(&config, &scope, &linked.id, "operator request")
            .expect("terminate composed session");

        assert_eq!(terminated.status, LifecycleStatus::Cancelled);
        assert_eq!(
            fs::read_to_string(log).expect("termination log"),
            "session.stop\nexecution.cancel\n"
        );
        let _ = fs::remove_file(state::path(&scope));
    }

    #[cfg(unix)]
    #[test]
    fn persisted_scope_termination_survives_a_deleted_workspace_directory() {
        let directory = tempfile::tempdir().expect("termination fixture");
        let scope_directory = directory.path().join("scope");
        let provider_directory = directory.path().join("providers");
        fs::create_dir_all(&scope_directory).expect("scope");
        fs::create_dir_all(&provider_directory).expect("providers");
        let scope = fs::canonicalize(&scope_directory).expect("canonical scope");
        let provider = directory.path().join("provider.sh");
        let invoked = directory.path().join("provider-cwd");
        let executed = directory.path().join("plan-cwd");
        fs::write(
            &provider,
            render_fixture(
                MISSING_SCOPE_STOP_PROVIDER,
                serde_json::json!({
                    "invoked": invoked.display().to_string(),
                    "executed": executed.display().to_string(),
                    "missing": scope.display().to_string(),
                }),
            ),
        )
        .expect("provider script");
        fs::set_permissions(&provider, fs::Permissions::from_mode(0o755))
            .expect("provider executable");
        fs::write(
            provider_directory.join("provider.yaml"),
            render_fixture(
                PROVIDER_MANIFEST,
                serde_json::json!({
                    "name": "owner",
                    "command": provider.display().to_string(),
                    "actions": [{
                        "capability": "session.stop",
                        "description": "Stop the owned session",
                    }],
                }),
            ),
        )
        .expect("provider manifest");
        let linked = register(
            &scope,
            Contract::default(),
            SessionLink {
                id: Some("managed".into()),
                native_id: Some("managed-native".into()),
                source: RegistrationSource::Managed,
                ..SessionLink::default()
            },
        )
        .expect("managed session");
        state::update(&scope, |workspace| {
            workspace.sessions[0].providers = vec![binding(
                "owner",
                ProviderKind::Persistence,
                BindingStatus::Active,
            )];
            Ok(())
        })
        .expect("record provider owner");
        fs::remove_dir(&scope).expect("remove workspace directory");
        let mut config = Config::default();
        config.providers.directory = provider_directory;

        let stopped = terminate_expired_persisted_scope(
            &config,
            &scope,
            &linked.id,
            "idle timeout exceeded",
            Some(linked.heartbeat_at),
            None,
        )
        .expect("terminate from persisted state");

        assert_eq!(stopped.status, LifecycleStatus::Cancelled);
        for recorded in [invoked, executed] {
            let cwd = fs::read_to_string(recorded).expect("record stable cwd");
            assert!(Path::new(cwd.trim()).is_dir());
            assert_ne!(Path::new(cwd.trim()), scope);
        }
        let _ = fs::remove_file(state::path(&scope));
    }

    #[test]
    fn lifecycle_update_cannot_override_a_termination_claim() {
        let directory = tempfile::tempdir().expect("scope");
        let scope = std::fs::canonicalize(directory.path()).expect("canonical scope");
        let linked = register(
            &scope,
            Contract {
                role: SessionRole::Worker,
                ..Contract::default()
            },
            SessionLink {
                id: Some(format!("worker-{}", Uuid::new_v4())),
                native_id: Some(Uuid::new_v4().to_string()),
                run_id: Some("run".into()),
                source: RegistrationSource::Managed,
                ..SessionLink::default()
            },
        )
        .expect("managed session");
        state::update(&scope, |workspace| {
            let session = workspace
                .sessions
                .iter_mut()
                .find(|session| session.id == linked.id)
                .expect("registered session");
            session.status = LifecycleStatus::Cancelled;
            session.termination_reason = Some(format!("termination pending {}", Uuid::new_v4()));
            Ok(())
        })
        .expect("claim termination");

        let error = update_session(&scope, &linked.id, LifecycleStatus::Done)
            .expect_err("termination claim must be exclusive");

        assert!(error.to_string().contains("termination is in progress"));
        let _ = std::fs::remove_file(state::path(&scope));
    }

    #[test]
    fn lifecycle_update_cannot_overwrite_a_completed_termination() {
        let directory = tempfile::tempdir().expect("scope");
        let scope = std::fs::canonicalize(directory.path()).expect("canonical scope");
        let linked = register(
            &scope,
            Contract {
                role: SessionRole::Worker,
                ..Contract::default()
            },
            SessionLink {
                id: Some(format!("worker-{}", Uuid::new_v4())),
                native_id: Some(Uuid::new_v4().to_string()),
                source: RegistrationSource::Managed,
                ..SessionLink::default()
            },
        )
        .expect("managed session");
        state::update(&scope, |workspace| {
            let session = workspace
                .sessions
                .iter_mut()
                .find(|session| session.id == linked.id)
                .expect("registered session");
            session.status = LifecycleStatus::Cancelled;
            session.termination_reason = Some("runtime timeout exceeded".into());
            Ok(())
        })
        .expect("complete termination");

        let error = update_session(&scope, &linked.id, LifecycleStatus::Done)
            .expect_err("completed termination must remain terminal");

        assert!(error.to_string().contains("was terminated"));
        let _ = std::fs::remove_file(state::path(&scope));
    }

    #[test]
    fn public_lifecycle_updates_cannot_revive_terminal_work() {
        let directory = tempfile::tempdir().expect("scope");
        let scope = std::fs::canonicalize(directory.path()).expect("canonical scope");
        let orchestrator = register(
            &scope,
            Contract {
                role: SessionRole::Orchestrator,
                ..Contract::default()
            },
            SessionLink {
                native_id: Some("state-machine-root".into()),
                ..SessionLink::default()
            },
        )
        .expect("register orchestrator");
        let run = create_run(
            &scope,
            "state machine".into(),
            "reject invalid transitions".into(),
            "terminal work stays terminal".into(),
            Some(orchestrator.id.clone()),
            None,
            None,
        )
        .expect("create run");
        let node = upsert_node(
            &scope,
            &run.id,
            NodeSpec {
                id: "work".into(),
                contract: Contract::default(),
                session_id: None,
                status: Some(LifecycleStatus::Queued),
                attempt: Some(0),
                depends_on: Vec::new(),
                execution: None,
                judge_policy: Some(JudgePolicy::Llm),
            },
        )
        .expect("create node");

        assert!(
            update_run(&scope, &run.id, LifecycleStatus::Cancelled)
                .expect_err("cancellation needs provider evidence")
                .to_string()
                .contains("workflow lifecycle operation")
        );
        assert_eq!(
            read_workspace(&scope)
                .expect("unchanged workspace")
                .runs
                .into_iter()
                .find(|candidate| candidate.id == run.id)
                .expect("run remains registered")
                .status,
            LifecycleStatus::Queued
        );

        state::update(&scope, |workspace| {
            let selected = workspace
                .runs
                .iter_mut()
                .find(|candidate| candidate.id == run.id)
                .expect("run");
            selected.status = LifecycleStatus::Done;
            Ok(())
        })
        .expect("force terminal run fixture");
        assert!(
            update_node(&scope, &run.id, &node.id, LifecycleStatus::Working)
                .expect_err("terminal parent run")
                .to_string()
                .contains("run is not mutable")
        );
        state::update(&scope, |workspace| {
            let selected = workspace
                .runs
                .iter_mut()
                .find(|candidate| candidate.id == run.id)
                .expect("run");
            selected.status = LifecycleStatus::Queued;
            Ok(())
        })
        .expect("restore active fixture");

        update_session(&scope, &orchestrator.id, LifecycleStatus::Done).expect("finish session");
        update_node(&scope, &run.id, &node.id, LifecycleStatus::Done).expect("finish node");
        state::update(&scope, |workspace| {
            let selected = workspace
                .runs
                .iter_mut()
                .find(|candidate| candidate.id == run.id)
                .expect("run");
            selected.status = LifecycleStatus::Done;
            Ok(())
        })
        .expect("finish run");

        assert!(
            update_session(&scope, &orchestrator.id, LifecycleStatus::Working)
                .expect_err("terminal session")
                .to_string()
                .contains("invalid session lifecycle transition")
        );
        assert!(
            update_run(&scope, &run.id, LifecycleStatus::Working)
                .expect_err("terminal run")
                .to_string()
                .contains("invalid run lifecycle transition")
        );
        assert!(
            update_node(&scope, &run.id, &node.id, LifecycleStatus::Working)
                .expect_err("terminal node")
                .to_string()
                .contains("run is not mutable")
        );
        let _ = std::fs::remove_file(state::path(&scope));
    }

    #[test]
    fn repeated_node_upserts_replace_review_edges() {
        let directory = tempfile::tempdir().expect("scope");
        let scope = std::fs::canonicalize(directory.path()).expect("canonical scope");
        let run = create_run(
            &scope,
            "review edges".into(),
            "replace stale review relationships".into(),
            "one current review edge".into(),
            None,
            None,
            None,
        )
        .expect("create run");
        for reviewer in ["first-reviewer", "second-reviewer"] {
            upsert_node(
                &scope,
                &run.id,
                NodeSpec {
                    id: "work".into(),
                    contract: Contract {
                        review_by: Some(reviewer.into()),
                        ..Contract::default()
                    },
                    session_id: None,
                    status: Some(LifecycleStatus::Queued),
                    attempt: Some(0),
                    depends_on: Vec::new(),
                    execution: None,
                    judge_policy: Some(JudgePolicy::Llm),
                },
            )
            .expect("upsert node");
        }

        let current = read_workspace(&scope).expect("workspace");
        let run = current
            .runs
            .iter()
            .find(|candidate| candidate.id == run.id)
            .expect("run");
        let review_edges = run
            .edges
            .iter()
            .filter(|edge| edge.from == "work" && edge.relationship == "reviewed_by")
            .collect::<Vec<_>>();
        assert_eq!(review_edges.len(), 1);
        assert_eq!(review_edges[0].to, "second-reviewer");
        let _ = std::fs::remove_file(state::path(&scope));
    }

    #[test]
    fn repeated_node_upserts_preserve_runtime_and_omitted_optional_fields() {
        let directory = tempfile::tempdir().expect("scope");
        let scope = std::fs::canonicalize(directory.path()).expect("canonical scope");
        let run = create_run(
            &scope,
            "runtime state".into(),
            "preserve server fields".into(),
            "runtime state survives".into(),
            None,
            None,
            None,
        )
        .expect("create run");
        upsert_node(
            &scope,
            &run.id,
            NodeSpec {
                id: "work".into(),
                contract: Contract {
                    model: Some("model-a".into()),
                    review_by: Some("reviewer".into()),
                    ..Contract::default()
                },
                session_id: Some("session-a".into()),
                status: Some(LifecycleStatus::Queued),
                attempt: Some(1),
                depends_on: Vec::new(),
                execution: Some("local".into()),
                judge_policy: Some(JudgePolicy::Llm),
            },
        )
        .expect("create node");
        let retry_after = Utc::now() + chrono::Duration::seconds(30);
        state::update(&scope, |workspace| {
            let node = &mut workspace.runs[0].nodes[0];
            node.status = LifecycleStatus::Working;
            node.attempt = 3;
            node.judge_policy = JudgePolicy::Human;
            node.child_run_id = Some("child-run".into());
            node.retry_after = Some(retry_after);
            node.prompt = Some("runtime prompt".into());
            node.input = Some(serde_json::json!({"input": true}));
            node.output = Some(serde_json::json!({"output": true}));
            node.record_activity("started", "runtime activity");
            node.tokens = 42;
            node.cost_usd = 1.25;
            Ok(())
        })
        .expect("record runtime state");

        let updated = upsert_node(
            &scope,
            &run.id,
            NodeSpec {
                id: "work".into(),
                contract: Contract {
                    title: "Updated contract".into(),
                    model: None,
                    review_by: None,
                    ..Contract::default()
                },
                session_id: None,
                status: None,
                attempt: None,
                depends_on: Vec::new(),
                execution: None,
                judge_policy: None,
            },
        )
        .expect("update desired fields");

        assert_eq!(updated.name, "Updated contract");
        assert_eq!(updated.status, LifecycleStatus::Working);
        assert_eq!(updated.attempt, 3);
        assert_eq!(updated.judge_policy, JudgePolicy::Human);
        assert_eq!(updated.model.as_deref(), Some("model-a"));
        assert_eq!(updated.review_by.as_deref(), Some("reviewer"));
        assert_eq!(updated.execution.as_deref(), Some("local"));
        assert_eq!(updated.session_id.as_deref(), Some("session-a"));
        assert_eq!(updated.child_run_id.as_deref(), Some("child-run"));
        assert_eq!(updated.retry_after, Some(retry_after));
        assert_eq!(updated.prompt.as_deref(), Some("runtime prompt"));
        assert_eq!(updated.input, Some(serde_json::json!({"input": true})));
        assert_eq!(updated.output, Some(serde_json::json!({"output": true})));
        assert_eq!(updated.activity.len(), 1);
        assert_eq!(updated.tokens, 42);
        assert_eq!(updated.cost_usd, 1.25);
        let _ = std::fs::remove_file(state::path(&scope));
    }

    #[test]
    fn new_node_defaults_desired_runtime_fields() {
        let directory = tempfile::tempdir().expect("scope");
        let scope = std::fs::canonicalize(directory.path()).expect("canonical scope");
        let run = create_run(
            &scope,
            "defaults".into(),
            "default omitted node state".into(),
            "queued node".into(),
            None,
            None,
            None,
        )
        .expect("create run");

        let node = upsert_node(
            &scope,
            &run.id,
            NodeSpec {
                id: "work".into(),
                contract: Contract::default(),
                session_id: None,
                status: None,
                attempt: None,
                depends_on: Vec::new(),
                execution: None,
                judge_policy: None,
            },
        )
        .expect("create node");

        assert_eq!(node.status, LifecycleStatus::Queued);
        assert_eq!(node.attempt, 0);
        assert_eq!(node.judge_policy, JudgePolicy::None);
        let _ = std::fs::remove_file(state::path(&scope));
    }

    #[test]
    fn omitted_node_state_preserves_terminal_values() {
        let directory = tempfile::tempdir().expect("scope");
        let scope = std::fs::canonicalize(directory.path()).expect("canonical scope");
        let run = create_run(
            &scope,
            "terminal".into(),
            "preserve terminal node state".into(),
            "done node remains done".into(),
            None,
            None,
            None,
        )
        .expect("create run");
        upsert_node(
            &scope,
            &run.id,
            NodeSpec {
                id: "work".into(),
                contract: Contract::default(),
                session_id: None,
                status: Some(LifecycleStatus::Done),
                attempt: Some(4),
                depends_on: Vec::new(),
                execution: None,
                judge_policy: Some(JudgePolicy::LlmAndHuman),
            },
        )
        .expect("create terminal node");

        let node = upsert_node(
            &scope,
            &run.id,
            NodeSpec {
                id: "work".into(),
                contract: Contract {
                    title: "Updated terminal contract".into(),
                    ..Contract::default()
                },
                session_id: None,
                status: None,
                attempt: None,
                depends_on: Vec::new(),
                execution: None,
                judge_policy: None,
            },
        )
        .expect("update terminal node contract");

        assert_eq!(node.status, LifecycleStatus::Done);
        assert_eq!(node.attempt, 4);
        assert_eq!(node.judge_policy, JudgePolicy::LlmAndHuman);
        let _ = std::fs::remove_file(state::path(&scope));
    }

    #[test]
    fn run_orchestrator_can_adopt_and_report_direct_work() {
        let directory = tempfile::tempdir().expect("scope");
        let scope = std::fs::canonicalize(directory.path()).expect("canonical scope");
        let orchestrator = register(
            &scope,
            Contract {
                role: SessionRole::Orchestrator,
                ..Contract::default()
            },
            SessionLink {
                native_id: Some("direct-work-root".into()),
                ..SessionLink::default()
            },
        )
        .expect("register orchestrator");
        let run = create_run(
            &scope,
            "direct work".into(),
            "let the orchestrator implement".into(),
            "a reported result".into(),
            Some(orchestrator.id.clone()),
            None,
            None,
        )
        .expect("create run");
        upsert_node(
            &scope,
            &run.id,
            NodeSpec {
                id: "work".into(),
                contract: Contract::default(),
                session_id: None,
                status: Some(LifecycleStatus::Queued),
                attempt: Some(0),
                depends_on: Vec::new(),
                execution: None,
                judge_policy: Some(JudgePolicy::Llm),
            },
        )
        .expect("create node");

        let reported = report_node(
            &scope,
            &run.id,
            "work",
            Some(&orchestrator.id),
            NodeReport {
                status: LifecycleStatus::Working,
                output: Some(serde_json::json!({"progress": "started"})),
                message: Some("working directly".into()),
                tokens: Some(9),
                cost_usd: Some(0.25),
            },
        )
        .expect("orchestrator reports direct work");

        assert_eq!(
            reported.session_id.as_deref(),
            Some(orchestrator.id.as_str())
        );
        assert_eq!(reported.tokens, 9);
        assert_eq!(reported.cost_usd, 0.25);
        assert_eq!(reported.activity[0].kind, "adopted");
        assert_eq!(reported.activity[1].kind, "reported");
        let current = read_workspace(&scope).expect("workspace");
        let current_orchestrator = current
            .sessions
            .iter()
            .find(|session| session.id == orchestrator.id)
            .expect("orchestrator");
        assert!(current_orchestrator.run_id.is_none());
        assert!(current_orchestrator.node_id.is_none());
        let _ = std::fs::remove_file(state::path(&scope));
    }

    #[test]
    fn explicit_node_adoption_allows_the_run_orchestrator() {
        let directory = tempfile::tempdir().expect("scope");
        let scope = std::fs::canonicalize(directory.path()).expect("canonical scope");
        let orchestrator = register(
            &scope,
            Contract {
                role: SessionRole::Orchestrator,
                ..Contract::default()
            },
            SessionLink {
                native_id: Some("explicit-adoption-root".into()),
                ..SessionLink::default()
            },
        )
        .expect("register orchestrator");
        let run = create_run(
            &scope,
            "explicit adoption".into(),
            "claim direct work".into(),
            "node links to the orchestrator".into(),
            Some(orchestrator.id.clone()),
            None,
            None,
        )
        .expect("create run");
        upsert_node(
            &scope,
            &run.id,
            NodeSpec {
                id: "work".into(),
                contract: Contract::default(),
                session_id: None,
                status: Some(LifecycleStatus::Queued),
                attempt: Some(0),
                depends_on: Vec::new(),
                execution: None,
                judge_policy: Some(JudgePolicy::Llm),
            },
        )
        .expect("create node");
        state::update(&scope, |workspace| {
            workspace.runs[0].nodes[0].prompt = Some("preserved prompt".into());
            Ok(())
        })
        .expect("record runtime data");

        let adopted = adopt_node(&scope, &run.id, "work", &orchestrator.id)
            .expect("orchestrator adopts direct work");

        assert_eq!(
            adopted.session_id.as_deref(),
            Some(orchestrator.id.as_str())
        );
        assert_eq!(adopted.prompt.as_deref(), Some("preserved prompt"));
        assert_eq!(adopted.activity.last().expect("activity").kind, "adopted");
        let current = read_workspace(&scope).expect("workspace");
        let current_orchestrator = current
            .sessions
            .iter()
            .find(|session| session.id == orchestrator.id)
            .expect("orchestrator");
        assert!(current_orchestrator.run_id.is_none());
        assert!(current_orchestrator.node_id.is_none());
        let _ = std::fs::remove_file(state::path(&scope));
    }

    #[test]
    fn node_adoption_backfills_missing_session_lineage_once() {
        let directory = tempfile::tempdir().expect("scope");
        let scope = std::fs::canonicalize(directory.path()).expect("canonical scope");
        let run = create_run(
            &scope,
            "backfill".into(),
            "restore session lineage".into(),
            "both sides reference each other".into(),
            None,
            None,
            None,
        )
        .expect("create run");
        upsert_node(
            &scope,
            &run.id,
            NodeSpec {
                id: "work".into(),
                contract: Contract::default(),
                session_id: Some("existing-worker".into()),
                status: Some(LifecycleStatus::Working),
                attempt: Some(1),
                depends_on: Vec::new(),
                execution: None,
                judge_policy: Some(JudgePolicy::Llm),
            },
        )
        .expect("create assigned node");
        state::update(&scope, |workspace| {
            workspace.sessions.push(session(
                "existing-worker",
                SessionRole::Implementer,
                LifecycleStatus::Working,
            ));
            Ok(())
        })
        .expect("record unlinked session");

        let adopted =
            adopt_node(&scope, &run.id, "work", "existing-worker").expect("backfill lineage");
        let repeated =
            adopt_node(&scope, &run.id, "work", "existing-worker").expect("repeat adoption");

        assert_eq!(adopted.activity.len(), 1);
        assert_eq!(repeated.activity.len(), 1);
        let current = read_workspace(&scope).expect("workspace");
        let worker = current
            .sessions
            .iter()
            .find(|session| session.id == "existing-worker")
            .expect("worker");
        assert_eq!(worker.run_id.as_deref(), Some(run.id.as_str()));
        assert_eq!(worker.node_id.as_deref(), Some("work"));
        let _ = std::fs::remove_file(state::path(&scope));
    }

    #[test]
    fn orchestrator_report_rejects_a_node_owned_by_another_session() {
        let directory = tempfile::tempdir().expect("scope");
        let scope = std::fs::canonicalize(directory.path()).expect("canonical scope");
        let orchestrator = register(
            &scope,
            Contract {
                role: SessionRole::Orchestrator,
                ..Contract::default()
            },
            SessionLink {
                native_id: Some("conflict-root".into()),
                ..SessionLink::default()
            },
        )
        .expect("register orchestrator");
        let run = create_run(
            &scope,
            "conflict".into(),
            "protect active work".into(),
            "worker ownership remains".into(),
            Some(orchestrator.id.clone()),
            None,
            None,
        )
        .expect("create run");
        upsert_node(
            &scope,
            &run.id,
            NodeSpec {
                id: "work".into(),
                contract: Contract::default(),
                session_id: Some("worker-a".into()),
                status: Some(LifecycleStatus::Working),
                attempt: Some(1),
                depends_on: Vec::new(),
                execution: None,
                judge_policy: Some(JudgePolicy::Llm),
            },
        )
        .expect("create assigned node");

        let error = report_node(
            &scope,
            &run.id,
            "work",
            Some(&orchestrator.id),
            NodeReport {
                status: LifecycleStatus::Working,
                output: None,
                message: None,
                tokens: None,
                cost_usd: None,
            },
        )
        .expect_err("orchestrator cannot replace a worker by reporting");

        assert!(error.to_string().contains("owned by session worker-a"));
        assert_eq!(
            read_workspace(&scope).expect("workspace").runs[0].nodes[0]
                .session_id
                .as_deref(),
            Some("worker-a")
        );
        state::update(&scope, |workspace| {
            workspace.runs[0].nodes[0].session_id = None;
            workspace.runs[0].orchestrator_id = Some("different-orchestrator".into());
            Ok(())
        })
        .expect("change run owner");
        let error = report_node(
            &scope,
            &run.id,
            "work",
            Some(&orchestrator.id),
            NodeReport {
                status: LifecycleStatus::Working,
                output: None,
                message: None,
                tokens: None,
                cost_usd: None,
            },
        )
        .expect_err("another run orchestrator cannot report");
        assert!(error.to_string().contains("does not own workflow run"));
        let _ = std::fs::remove_file(state::path(&scope));
    }

    #[test]
    fn node_adoption_rejects_active_conflicts_and_preserves_node_data() {
        let directory = tempfile::tempdir().expect("scope");
        let scope = std::fs::canonicalize(directory.path()).expect("canonical scope");
        let run = create_run(
            &scope,
            "adoption".into(),
            "adopt an existing worker".into(),
            "provenance is retained".into(),
            None,
            None,
            None,
        )
        .expect("create run");
        upsert_node(
            &scope,
            &run.id,
            NodeSpec {
                id: "work".into(),
                contract: Contract::default(),
                session_id: Some("old-worker".into()),
                status: Some(LifecycleStatus::Working),
                attempt: Some(1),
                depends_on: Vec::new(),
                execution: None,
                judge_policy: Some(JudgePolicy::Llm),
            },
        )
        .expect("create node");
        state::update(&scope, |workspace| {
            let mut old = session(
                "old-worker",
                SessionRole::Implementer,
                LifecycleStatus::Working,
            );
            old.run_id = Some(run.id.clone());
            old.node_id = Some("work".into());
            let candidate = session(
                "candidate",
                SessionRole::Implementer,
                LifecycleStatus::Working,
            );
            workspace.sessions.extend([old, candidate]);
            workspace.runs[0].nodes[0].output = Some(serde_json::json!({"kept": true}));
            Ok(())
        })
        .expect("record sessions");

        let error = adopt_node(&scope, &run.id, "work", "candidate")
            .expect_err("active owner cannot be displaced");
        assert!(
            error
                .to_string()
                .contains("owned by active session old-worker")
        );

        state::update(&scope, |workspace| {
            workspace
                .sessions
                .iter_mut()
                .find(|session| session.id == "old-worker")
                .expect("old worker")
                .status = LifecycleStatus::Disconnected;
            Ok(())
        })
        .expect("disconnect old worker");
        let adopted =
            adopt_node(&scope, &run.id, "work", "candidate").expect("replace inactive owner");

        assert_eq!(adopted.session_id.as_deref(), Some("candidate"));
        assert_eq!(adopted.output, Some(serde_json::json!({"kept": true})));
        assert_eq!(adopted.activity.last().expect("activity").kind, "adopted");
        let current = read_workspace(&scope).expect("workspace");
        let candidate = current
            .sessions
            .iter()
            .find(|session| session.id == "candidate")
            .expect("candidate");
        assert_eq!(candidate.run_id.as_deref(), Some(run.id.as_str()));
        assert_eq!(candidate.node_id.as_deref(), Some("work"));
        let reported = report_node(
            &scope,
            &run.id,
            "work",
            Some("candidate"),
            NodeReport {
                status: LifecycleStatus::Working,
                output: Some(serde_json::json!({"adopted": true})),
                message: Some("adopted session reporting".into()),
                tokens: None,
                cost_usd: None,
            },
        )
        .expect("adopted connected session reports its node");
        assert_eq!(reported.output, Some(serde_json::json!({"adopted": true})));
        let _ = std::fs::remove_file(state::path(&scope));
    }

    #[test]
    fn node_adoption_validates_run_node_and_session_lineage() {
        let directory = tempfile::tempdir().expect("scope");
        let scope = std::fs::canonicalize(directory.path()).expect("canonical scope");
        let run = create_run(
            &scope,
            "lineage".into(),
            "validate adoption targets".into(),
            "invalid lineage is rejected".into(),
            None,
            None,
            None,
        )
        .expect("create run");
        upsert_node(
            &scope,
            &run.id,
            NodeSpec {
                id: "work".into(),
                contract: Contract::default(),
                session_id: None,
                status: Some(LifecycleStatus::Queued),
                attempt: Some(0),
                depends_on: Vec::new(),
                execution: None,
                judge_policy: Some(JudgePolicy::Llm),
            },
        )
        .expect("create node");
        state::update(&scope, |workspace| {
            let mut wrong_run = session(
                "wrong-run",
                SessionRole::Implementer,
                LifecycleStatus::Working,
            );
            wrong_run.run_id = Some("another-run".into());
            let mut wrong_node = session(
                "wrong-node",
                SessionRole::Implementer,
                LifecycleStatus::Working,
            );
            wrong_node.run_id = Some(run.id.clone());
            wrong_node.node_id = Some("another-node".into());
            workspace.sessions.extend([wrong_run, wrong_node]);
            Ok(())
        })
        .expect("record sessions");

        assert!(
            adopt_node(&scope, &run.id, "work", "missing")
                .expect_err("unknown session")
                .to_string()
                .contains("unknown session")
        );
        assert!(
            adopt_node(&scope, "missing-run", "work", "wrong-run")
                .expect_err("unknown run")
                .to_string()
                .contains("unknown run")
        );
        assert!(
            adopt_node(&scope, &run.id, "missing-node", "wrong-run")
                .expect_err("unknown node")
                .to_string()
                .contains("unknown node")
        );
        assert!(
            adopt_node(&scope, &run.id, "work", "wrong-run")
                .expect_err("wrong run lineage")
                .to_string()
                .contains("belongs to workflow run another-run")
        );
        assert!(
            adopt_node(&scope, &run.id, "work", "wrong-node")
                .expect_err("wrong node lineage")
                .to_string()
                .contains("belongs to workflow node another-node")
        );
        let _ = std::fs::remove_file(state::path(&scope));
    }

    #[test]
    fn interrupted_termination_reuses_its_provider_operation_id() {
        let first = termination_operation_id(None, None);
        let claim = format!("termination pending {first}");

        assert_eq!(termination_operation_id(Some(&claim), None), first);
        assert_eq!(termination_operation_id(None, Some(&first)), first);
    }

    #[test]
    fn termination_claim_lock_distinguishes_live_and_interrupted_work() {
        let directory = tempfile::tempdir().expect("termination scope");
        let scope = directory.path();
        let session_id = "worker";
        let operation_id = Uuid::new_v4().to_string();
        let claim = format!("termination pending {operation_id}");
        let guard = TerminationGuard::acquire(scope, session_id, std::time::Duration::ZERO)
            .expect("termination lock")
            .expect("unclaimed operation");

        assert!(termination_claim_active(scope, session_id, &claim).expect("active claim"));
        drop(guard);
        assert!(!termination_claim_active(scope, session_id, &claim).expect("released claim"));
    }

    #[test]
    fn termination_locks_use_independent_full_digests() {
        let scope = Path::new("/tmp/orc-termination-shards");
        let paths = (0..1024)
            .map(|index| termination_lock_path(scope, &format!("session-{index}")))
            .collect::<std::collections::BTreeSet<_>>();

        assert_eq!(paths.len(), 1024);
        assert!(paths.iter().all(|path| {
            path.file_stem()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.len() == 64)
        }));
    }

    #[test]
    fn termination_lock_is_removed_after_release() {
        let directory = tempfile::tempdir().expect("termination scope");
        let path = termination_lock_path(directory.path(), "worker");
        let guard =
            TerminationGuard::acquire(directory.path(), "worker", std::time::Duration::ZERO)
                .expect("termination lock")
                .expect("unclaimed operation");

        assert!(path.exists());
        drop(guard);
        assert!(!path.exists());
    }

    #[test]
    fn direct_launch_rejects_unenforceable_timeouts() {
        let error = launch(
            &Config::default(),
            Path::new("."),
            "true".into(),
            None,
            None,
            SessionLease {
                runtime_timeout_seconds: Some(1),
                idle_timeout_seconds: None,
            },
            Vec::new(),
        )
        .expect_err("direct launch cannot enforce a lease");

        assert!(error.to_string().contains("require --managed"));
    }

    #[test]
    fn archived_environment_id_is_not_reused_for_registration() {
        let mut workspace = WorkspaceState::empty("/tmp".into());
        workspace.sessions.push(session(
            "stale",
            SessionRole::Orchestrator,
            LifecycleStatus::Archived,
        ));

        assert_eq!(
            registration_base_id(&workspace, None, Some("stale"), "inferred"),
            "inferred"
        );
        assert_eq!(
            registration_base_id(&workspace, Some("stale"), Some("stale"), "inferred"),
            "stale"
        );
    }

    #[test]
    fn adoption_reparents_active_children_of_replaced_orchestrator() {
        let mut workspace = WorkspaceState::empty("/tmp".into());
        let root = session(
            "old-root",
            SessionRole::Orchestrator,
            LifecycleStatus::Working,
        );
        let mut active_child = session(
            "active-child",
            SessionRole::Implementer,
            LifecycleStatus::Working,
        );
        active_child.parent_id = Some(root.id.clone());
        let mut finished_child = session(
            "finished-child",
            SessionRole::Verifier,
            LifecycleStatus::Done,
        );
        finished_child.parent_id = Some(root.id.clone());
        let mut resumable_child = session(
            "resumable-child",
            SessionRole::Researcher,
            LifecycleStatus::Disconnected,
        );
        resumable_child.registration = RegistrationSource::Managed;
        resumable_child.parent_id = Some(root.id.clone());
        let mut failed_child =
            session("failed-child", SessionRole::Critic, LifecycleStatus::Failed);
        failed_child.registration = RegistrationSource::Managed;
        failed_child.parent_id = Some(root.id.clone());
        workspace.sessions = vec![
            root,
            active_child,
            finished_child,
            resumable_child,
            failed_child,
        ];

        reparent_active_descendants(&mut workspace, &["old-root".into()], "new-root");

        assert_eq!(workspace.sessions[1].parent_id.as_deref(), Some("new-root"));
        assert_eq!(workspace.sessions[2].parent_id.as_deref(), Some("old-root"));
        assert_eq!(workspace.sessions[3].parent_id.as_deref(), Some("new-root"));
        assert_eq!(workspace.sessions[4].parent_id.as_deref(), Some("old-root"));
    }

    #[test]
    fn registration_remaps_a_stale_orchestrator_parent_after_adoption() {
        let directory = tempfile::tempdir().expect("scope");
        let scope = std::fs::canonicalize(directory.path()).expect("canonical scope");
        let old = register(
            &scope,
            Contract {
                role: SessionRole::Orchestrator,
                ..Contract::default()
            },
            SessionLink {
                id: Some("old-root".into()),
                native_id: Some("old-native".into()),
                ..SessionLink::default()
            },
        )
        .expect("old root");
        let run = create_run(
            &scope,
            "adoption-race".into(),
            "keep the run attached".into(),
            "a managed child".into(),
            Some(old.id.clone()),
            None,
            None,
        )
        .expect("run");
        let replacement = adopt(
            &scope,
            Contract {
                role: SessionRole::Orchestrator,
                ..Contract::default()
            },
            Some("replacement-native".into()),
        )
        .expect("replacement root");

        let child = register(
            &scope,
            Contract::default(),
            SessionLink {
                native_id: Some("late-child".into()),
                parent_id: Some(old.id),
                run_id: Some(run.id),
                source: RegistrationSource::Managed,
                ..SessionLink::default()
            },
        )
        .expect("late child");

        assert_eq!(child.parent_id.as_deref(), Some(replacement.id.as_str()));
    }

    #[test]
    fn registration_rejects_conflicting_explicit_and_native_identities() {
        let directory = tempfile::tempdir().expect("scope");
        let scope = std::fs::canonicalize(directory.path()).expect("canonical scope");
        let root = register(
            &scope,
            Contract {
                role: SessionRole::Orchestrator,
                ..Contract::default()
            },
            SessionLink {
                id: Some("root".into()),
                native_id: Some("root-native".into()),
                ..SessionLink::default()
            },
        )
        .expect("root");
        let child = register(
            &scope,
            Contract::default(),
            SessionLink {
                id: Some("child".into()),
                native_id: Some("child-native".into()),
                parent_id: Some(root.id.clone()),
                source: RegistrationSource::Managed,
                ..SessionLink::default()
            },
        )
        .expect("child");

        let error = register(
            &scope,
            Contract::default(),
            SessionLink {
                id: Some(root.id.clone()),
                native_id: Some(child.native_id.clone()),
                ..SessionLink::default()
            },
        )
        .expect_err("identities must not be merged");

        assert!(
            error
                .to_string()
                .contains("identify different Orc sessions")
        );
        let workspace = read_workspace(&scope).expect("workspace");
        assert_eq!(workspace.sessions.len(), 2);
        assert!(
            workspace
                .sessions
                .iter()
                .all(|session| session.status != LifecycleStatus::Archived)
        );
        let _ = std::fs::remove_file(state::path(&scope));
    }

    #[test]
    fn adoption_replaces_a_disconnected_orchestrator() {
        let directory = tempfile::tempdir().unwrap();
        let scope_directory = directory.path().join("scope");
        std::fs::create_dir(&scope_directory).unwrap();
        let scope = std::fs::canonicalize(scope_directory).unwrap();
        let old = register(
            &scope,
            Contract {
                role: SessionRole::Orchestrator,
                title: "old root".into(),
                ..Contract::default()
            },
            SessionLink {
                id: Some("old-root".into()),
                native_id: Some("old-native".into()),
                ..SessionLink::default()
            },
        )
        .unwrap();
        let child = register(
            &scope,
            Contract {
                role: SessionRole::Worker,
                title: "child".into(),
                ..Contract::default()
            },
            SessionLink {
                id: Some("child".into()),
                native_id: Some("child-native".into()),
                parent_id: Some(old.id.clone()),
                ..SessionLink::default()
            },
        )
        .unwrap();
        update_session(&scope, &old.id, LifecycleStatus::Disconnected).unwrap();

        let new = adopt(
            &scope,
            Contract {
                title: "new root".into(),
                ..Contract::default()
            },
            Some("new-native".into()),
        )
        .unwrap();
        let workspace = state::read(&scope).unwrap();

        assert_eq!(
            workspace
                .sessions
                .iter()
                .find(|session| session.id == old.id)
                .unwrap()
                .status,
            LifecycleStatus::Archived
        );
        assert_eq!(
            workspace
                .sessions
                .iter()
                .find(|session| session.id == child.id)
                .unwrap()
                .parent_id
                .as_deref(),
            Some(new.id.as_str())
        );
        assert_eq!(
            workspace
                .sessions
                .iter()
                .filter(|session| {
                    session.role == SessionRole::Orchestrator
                        && session.status != LifecycleStatus::Archived
                })
                .count(),
            1
        );
        let _ = std::fs::remove_file(state::path(&scope));
    }

    #[test]
    fn reconciliation_replaces_stale_bindings() {
        let mut selected = session(
            "session",
            SessionRole::Orchestrator,
            LifecycleStatus::Working,
        );
        selected.providers = vec![binding(
            "stale",
            ProviderKind::Display,
            BindingStatus::Active,
        )];
        let current = binding("current", ProviderKind::Persistence, BindingStatus::Active);

        apply_enrichment(
            &mut selected,
            std::slice::from_ref(&current),
            Some("described"),
            Some("specific goal"),
        );

        assert_eq!(selected.providers, vec![current]);
        assert_eq!(selected.title, "described");
        assert_eq!(selected.goal, "specific goal");
    }

    #[test]
    fn reconciliation_preserves_a_binding_changed_after_observation() {
        let mut observed = session(
            "session",
            SessionRole::Orchestrator,
            LifecycleStatus::Working,
        );
        observed.providers = vec![binding(
            "display",
            ProviderKind::Display,
            BindingStatus::Available,
        )];
        let mut selected = observed.clone();
        selected.providers = vec![ProviderBinding {
            provider: "display".into(),
            kind: ProviderKind::Display,
            r#ref: Some("pane-7".into()),
            status: BindingStatus::Active,
            label: "test pane".into(),
        }];
        bump_changed_provider_revisions(
            &mut selected.provider_revisions,
            &observed.providers,
            &selected.providers,
        );

        apply_observed_enrichment(&mut selected, &observed, &observed.providers, None, None);

        assert_eq!(selected.providers.len(), 1);
        assert_eq!(selected.providers[0].status, BindingStatus::Active);
        assert_eq!(selected.providers[0].r#ref.as_deref(), Some("pane-7"));
        assert_eq!(selected.status, LifecycleStatus::Working);
    }

    #[test]
    fn reconciliation_preserves_a_concurrent_binding_removal() {
        let mut observed = session(
            "session",
            SessionRole::Orchestrator,
            LifecycleStatus::Working,
        );
        let available = binding("display", ProviderKind::Display, BindingStatus::Available);
        observed.providers = vec![available.clone()];
        let mut selected = observed.clone();
        selected.providers.clear();
        bump_changed_provider_revisions(
            &mut selected.provider_revisions,
            &observed.providers,
            &selected.providers,
        );

        apply_observed_enrichment(&mut selected, &observed, &[available], None, None);

        assert!(selected.providers.is_empty());
    }

    #[test]
    fn reconciliation_preserves_a_new_binding_when_observation_returns_nothing() {
        let observed = session(
            "session",
            SessionRole::Orchestrator,
            LifecycleStatus::Working,
        );
        let mut selected = observed.clone();
        selected.providers = vec![ProviderBinding {
            provider: "display".into(),
            kind: ProviderKind::Display,
            r#ref: Some("pane-7".into()),
            status: BindingStatus::Active,
            label: "test pane".into(),
        }];
        bump_changed_provider_revisions(
            &mut selected.provider_revisions,
            &observed.providers,
            &selected.providers,
        );

        apply_observed_enrichment(&mut selected, &observed, &[], None, None);

        assert_eq!(selected.providers.len(), 1);
        assert_eq!(selected.providers[0].r#ref.as_deref(), Some("pane-7"));
    }

    #[test]
    fn reconciliation_does_not_revive_a_concurrently_cancelled_session() {
        let observed = session(
            "session",
            SessionRole::Orchestrator,
            LifecycleStatus::Working,
        );
        let mut selected = observed.clone();
        selected.status = LifecycleStatus::Cancelled;
        let live = ProviderBinding {
            provider: "display".into(),
            kind: ProviderKind::Display,
            r#ref: Some("pane-7".into()),
            status: BindingStatus::Active,
            label: "test pane".into(),
        };

        apply_observed_enrichment(&mut selected, &observed, &[live], None, None);

        assert_eq!(selected.status, LifecycleStatus::Cancelled);
    }

    #[test]
    fn reconciliation_revision_guard_closes_identical_value_aba() {
        let mut observed = session(
            "session",
            SessionRole::Orchestrator,
            LifecycleStatus::Working,
        );
        observed.providers = vec![ProviderBinding {
            provider: "display".into(),
            kind: ProviderKind::Display,
            r#ref: Some("pane-7".into()),
            status: BindingStatus::Active,
            label: "test pane".into(),
        }];
        let mut selected = observed.clone();
        let removed = Vec::new();
        bump_changed_provider_revisions(
            &mut selected.provider_revisions,
            &observed.providers,
            &removed,
        );
        bump_changed_provider_revisions(
            &mut selected.provider_revisions,
            &removed,
            &selected.providers,
        );
        let available = binding("display", ProviderKind::Display, BindingStatus::Available);

        apply_observed_enrichment(&mut selected, &observed, &[available], None, None);

        assert_eq!(selected.providers, observed.providers);
        assert_eq!(binding_revision(&selected, &selected.providers[0]), 2);
    }

    #[test]
    fn unrelated_display_change_still_commits_runtime_observation() {
        let mut observed = session("session", SessionRole::Worker, LifecycleStatus::Working);
        observed.providers = vec![
            ProviderBinding {
                provider: "persistence".into(),
                kind: ProviderKind::Persistence,
                r#ref: Some("runtime".into()),
                status: BindingStatus::Active,
                label: "runtime".into(),
            },
            binding("display", ProviderKind::Display, BindingStatus::Available),
        ];
        let mut selected = observed.clone();
        selected.providers[1] = ProviderBinding {
            provider: "display".into(),
            kind: ProviderKind::Display,
            r#ref: Some("pane-7".into()),
            status: BindingStatus::Active,
            label: "test pane".into(),
        };
        bump_changed_provider_revisions(
            &mut selected.provider_revisions,
            &observed.providers,
            &selected.providers,
        );
        let discovered = vec![
            ProviderBinding {
                status: BindingStatus::Unavailable,
                r#ref: None,
                ..observed.providers[0].clone()
            },
            observed.providers[1].clone(),
        ];

        apply_observed_enrichment(&mut selected, &observed, &discovered, None, None);

        assert_eq!(selected.providers[0].status, BindingStatus::Unavailable);
        assert_eq!(selected.providers[0].r#ref, None);
        assert_eq!(selected.providers[1].r#ref.as_deref(), Some("pane-7"));
        assert!(!has_active_runtime_binding(&selected.providers));
    }

    #[test]
    fn termination_discovery_cannot_replace_a_concurrent_display_receipt() {
        let mut observed = session(
            "session",
            SessionRole::Orchestrator,
            LifecycleStatus::Working,
        );
        observed.providers = vec![ProviderBinding {
            provider: "display".into(),
            kind: ProviderKind::Display,
            r#ref: Some("pane-old".into()),
            status: BindingStatus::Active,
            label: "old pane".into(),
        }];
        let mut selected = observed.clone();
        selected.providers[0].r#ref = Some("pane-new".into());
        selected.providers[0].label = "new pane".into();
        bump_changed_provider_revisions(
            &mut selected.provider_revisions,
            &observed.providers,
            &selected.providers,
        );
        let reconciled = reconcile_termination_bindings(&observed.providers, &observed.providers);
        let merged = merge_observed_bindings(&observed, &selected, &reconciled);

        replace_provider_bindings(&mut selected, merged);

        assert_eq!(selected.providers[0].r#ref.as_deref(), Some("pane-new"));
        assert_eq!(selected.providers[0].label, "new pane");
    }

    #[test]
    fn readiness_uses_the_binding_state_committed_after_merge() {
        let mut observed = session("session", SessionRole::Worker, LifecycleStatus::Working);
        observed.providers = vec![ProviderBinding {
            provider: "persistence".into(),
            kind: ProviderKind::Persistence,
            r#ref: Some("live-session".into()),
            status: BindingStatus::Active,
            label: "live session".into(),
        }];
        let mut selected = observed.clone();
        selected.providers[0].status = BindingStatus::Unavailable;
        selected.providers[0].r#ref = None;
        bump_changed_provider_revisions(
            &mut selected.provider_revisions,
            &observed.providers,
            &selected.providers,
        );

        let ready = apply_observed_runtime_readiness(&mut selected, &observed, &observed.providers);

        assert!(!ready);
        assert_eq!(selected.providers[0].status, BindingStatus::Unavailable);
        assert_eq!(selected.providers[0].r#ref, None);
    }

    #[test]
    fn launch_ownership_reservations_are_not_runtime_readiness() {
        let mut reservation = binding(
            "persistence",
            ProviderKind::Persistence,
            BindingStatus::Active,
        );
        reservation.r#ref = Some("reserved-session".into());
        reservation.label = "Launch ownership: persistence".into();
        let mut live = reservation.clone();
        live.label = "live session".into();

        assert!(!has_active_runtime_binding(&[reservation]));
        assert!(has_active_runtime_binding(&[live]));
    }

    #[test]
    fn managed_finalization_retires_only_synthetic_lifecycle_owners() {
        let directory = tempfile::tempdir().expect("fixture");
        let scope_directory = directory.path().join("scope");
        let provider_directory = directory.path().join("providers");
        fs::create_dir_all(&scope_directory).expect("scope");
        fs::create_dir_all(&provider_directory).expect("providers");
        let scope = fs::canonicalize(scope_directory).expect("canonical scope");
        let config = Config {
            providers: crate::config::ProviderConfig {
                directory: provider_directory,
                ..crate::config::ProviderConfig::default()
            },
            ..Config::default()
        };
        let session = register(&scope, Contract::default(), SessionLink::default())
            .expect("registered session");
        state::update(&scope, |workspace| {
            let selected = workspace
                .sessions
                .iter_mut()
                .find(|candidate| candidate.id == session.id)
                .expect("session remains");
            selected.registration = RegistrationSource::Managed;
            selected.providers = [ProviderKind::Persistence, ProviderKind::Execution]
                .into_iter()
                .map(|kind| ProviderBinding {
                    provider: kind.to_string(),
                    kind,
                    r#ref: Some(session.id.clone()),
                    status: BindingStatus::Active,
                    label: format!("Launch ownership: {kind}"),
                })
                .collect();
            let previous = Vec::new();
            bump_changed_provider_revisions(
                &mut selected.provider_revisions,
                &previous,
                &selected.providers,
            );
            Ok(())
        })
        .expect("install lifecycle reservations");

        finalize_managed_launch(&config, &scope, &session.id).expect("finalize managed launch");

        let finalized = selected_session(&state::read(&scope).expect("state"), &session.id)
            .expect("finalized session")
            .clone();
        assert_eq!(finalized.status, LifecycleStatus::Done);
        assert!(finalized.providers.is_empty());
    }

    #[test]
    fn managed_finalization_cannot_overwrite_a_concurrent_failure() {
        assert!(!managed_finalization_allowed(LifecycleStatus::Failed));
        assert!(!managed_finalization_allowed(LifecycleStatus::Cancelled));
        assert!(managed_finalization_allowed(LifecycleStatus::Disconnected));
    }

    #[test]
    fn reconciliation_preserves_launch_ownership_until_binding_is_conclusive() {
        let mut selected = session("session", SessionRole::Worker, LifecycleStatus::Working);
        let mut reservation = binding(
            "persistence-a",
            ProviderKind::Persistence,
            BindingStatus::Active,
        );
        reservation.label = "Launch ownership: persistence-a".into();
        selected.providers = vec![reservation.clone()];
        let mut available = binding(
            "persistence-a",
            ProviderKind::Persistence,
            BindingStatus::Available,
        );
        available.r#ref = None;

        apply_enrichment(&mut selected, &[available], None, None);

        assert_eq!(selected.providers, vec![reservation]);
        assert_eq!(selected.status, LifecycleStatus::Working);
    }

    #[test]
    fn reconciliation_disconnects_a_session_that_loses_its_live_binding() {
        let mut selected = session(
            "session",
            SessionRole::Orchestrator,
            LifecycleStatus::Working,
        );
        selected.providers = vec![binding(
            "persistence-a",
            ProviderKind::Persistence,
            BindingStatus::Active,
        )];
        let unavailable = binding(
            "persistence-a",
            ProviderKind::Persistence,
            BindingStatus::Unavailable,
        );

        apply_enrichment(&mut selected, &[unavailable], None, None);

        assert_eq!(selected.status, LifecycleStatus::Disconnected);
    }

    #[test]
    fn reconciliation_treats_a_missing_live_binding_as_unknown() {
        let mut selected = session(
            "session",
            SessionRole::Orchestrator,
            LifecycleStatus::Working,
        );
        selected.providers = vec![binding(
            "display-a",
            ProviderKind::Display,
            BindingStatus::Active,
        )];

        apply_enrichment(&mut selected, &[], None, None);

        assert_eq!(selected.status, LifecycleStatus::Working);
    }

    #[test]
    fn a_display_does_not_override_missing_persistence_evidence() {
        let mut selected = session(
            "session",
            SessionRole::Orchestrator,
            LifecycleStatus::Working,
        );
        selected.providers = vec![
            binding(
                "persistence-a",
                ProviderKind::Persistence,
                BindingStatus::Active,
            ),
            binding("display-a", ProviderKind::Display, BindingStatus::Active),
        ];
        let remaining_display = binding("display-a", ProviderKind::Display, BindingStatus::Active);

        apply_enrichment(&mut selected, &[remaining_display], None, None);

        assert_eq!(selected.status, LifecycleStatus::Working);
    }

    #[test]
    fn active_display_overrides_unavailable_persistence_evidence() {
        let mut selected = session(
            "session",
            SessionRole::Orchestrator,
            LifecycleStatus::Working,
        );
        selected.providers = vec![
            binding(
                "persistence-a",
                ProviderKind::Persistence,
                BindingStatus::Active,
            ),
            binding("display-a", ProviderKind::Display, BindingStatus::Active),
        ];
        let unavailable = binding(
            "persistence-a",
            ProviderKind::Persistence,
            BindingStatus::Unavailable,
        );
        let display = binding("display-a", ProviderKind::Display, BindingStatus::Active);

        apply_enrichment(&mut selected, &[unavailable, display], None, None);

        assert_eq!(selected.status, LifecycleStatus::Working);
    }

    #[test]
    fn active_execution_overrides_unavailable_persistence_evidence() {
        let mut selected = session(
            "session",
            SessionRole::Implementer,
            LifecycleStatus::Working,
        );
        selected.providers = vec![binding(
            "persistence-a",
            ProviderKind::Persistence,
            BindingStatus::Active,
        )];
        let unavailable = binding(
            "persistence-a",
            ProviderKind::Persistence,
            BindingStatus::Unavailable,
        );
        let execution = binding(
            "execution-a",
            ProviderKind::Execution,
            BindingStatus::Active,
        );

        apply_enrichment(&mut selected, &[unavailable, execution], None, None);

        assert_eq!(selected.status, LifecycleStatus::Working);
    }

    #[test]
    fn reconciliation_requires_all_known_live_bindings_to_be_unavailable() {
        let mut selected = session(
            "session",
            SessionRole::Orchestrator,
            LifecycleStatus::Working,
        );
        selected.providers = vec![
            binding(
                "persistence-a",
                ProviderKind::Persistence,
                BindingStatus::Active,
            ),
            binding("display-a", ProviderKind::Display, BindingStatus::Active),
        ];
        let unavailable = binding(
            "persistence-a",
            ProviderKind::Persistence,
            BindingStatus::Unavailable,
        );

        apply_enrichment(&mut selected, &[unavailable], None, None);

        assert_eq!(selected.status, LifecycleStatus::Working);
    }

    #[test]
    fn reconciliation_revives_a_disconnected_session_with_live_evidence() {
        let mut selected = session(
            "session",
            SessionRole::Orchestrator,
            LifecycleStatus::Disconnected,
        );
        selected.providers = vec![binding(
            "persistence-a",
            ProviderKind::Persistence,
            BindingStatus::Unavailable,
        )];
        let live = binding(
            "persistence-a",
            ProviderKind::Persistence,
            BindingStatus::Active,
        );

        apply_enrichment(&mut selected, &[live], None, None);

        assert_eq!(selected.status, LifecycleStatus::Working);
    }

    #[test]
    fn reconciliation_refreshes_a_session_with_a_live_binding() {
        let mut selected = session(
            "session",
            SessionRole::Orchestrator,
            LifecycleStatus::Working,
        );
        selected.updated_at = chrono::DateTime::UNIX_EPOCH;
        let live = binding(
            "persistence-a",
            ProviderKind::Persistence,
            BindingStatus::Active,
        );
        selected.providers = vec![live.clone()];

        apply_enrichment(&mut selected, &[live], None, None);

        assert_eq!(selected.status, LifecycleStatus::Working);
        assert!(selected.updated_at > chrono::DateTime::UNIX_EPOCH);
    }

    #[test]
    fn reconciliation_does_not_infer_liveness_without_prior_evidence() {
        let mut selected = session(
            "session",
            SessionRole::Orchestrator,
            LifecycleStatus::Working,
        );
        selected.updated_at = chrono::DateTime::UNIX_EPOCH;

        apply_enrichment(&mut selected, &[], None, None);

        assert_eq!(selected.status, LifecycleStatus::Working);
        assert_eq!(selected.updated_at, chrono::DateTime::UNIX_EPOCH);
    }

    #[test]
    fn reconciliation_preserves_terminal_lifecycle_states() {
        let mut selected = session("session", SessionRole::Verifier, LifecycleStatus::Done);
        selected.updated_at = chrono::DateTime::UNIX_EPOCH;
        selected.providers = vec![binding(
            "display-a",
            ProviderKind::Display,
            BindingStatus::Active,
        )];

        apply_enrichment(&mut selected, &[], None, None);

        assert_eq!(selected.status, LifecycleStatus::Done);
        assert_eq!(selected.updated_at, chrono::DateTime::UNIX_EPOCH);
    }

    #[test]
    fn attach_falls_back_after_focus_provider_fails() {
        let mut calls = Vec::new();

        let outcome = execute_attach_with(provider::Action::Attach, true, |action| {
            calls.push(action);
            match action {
                provider::Action::Focus => Ok((1, false)),
                provider::Action::Attach => Ok((0, true)),
                _ => unreachable!("unexpected action"),
            }
        })
        .expect("attach fallback succeeds");

        assert_eq!(
            calls,
            vec![provider::Action::Focus, provider::Action::Attach]
        );
        assert_eq!(outcome.disposition, AttachDisposition::Launched);
        assert_eq!(outcome.code, 0);
        assert!(outcome.accepted);
    }

    #[test]
    fn attach_does_not_fallback_after_successful_focus() {
        let mut calls = Vec::new();

        let outcome = execute_attach_with(provider::Action::Attach, true, |action| {
            calls.push(action);
            Ok((17, true))
        })
        .expect("focus succeeds");

        assert_eq!(calls, vec![provider::Action::Focus]);
        assert_eq!(outcome.disposition, AttachDisposition::Focused);
        assert_eq!(outcome.code, 17);
        assert!(outcome.accepted);
    }

    #[test]
    fn attach_does_not_open_after_ambiguous_focus_ownership() {
        let mut calls = Vec::new();

        let error = execute_attach_with(provider::Action::Attach, true, |action| {
            calls.push(action);
            Err(anyhow::anyhow!("ambiguous active display ownership"))
        })
        .expect_err("ambiguous focus must not open another display");

        assert_eq!(calls, vec![provider::Action::Focus]);
        assert!(
            error
                .to_string()
                .contains("focus failed without opening a duplicate")
        );
    }

    #[cfg(unix)]
    #[test]
    fn disconnected_session_with_an_active_display_receipt_focuses_without_opening_again() {
        let directory = tempfile::tempdir().expect("fixture");
        let scope_directory = directory.path().join("scope");
        let provider_directory = directory.path().join("providers");
        fs::create_dir_all(&scope_directory).expect("scope");
        fs::create_dir_all(&provider_directory).expect("providers");
        let scope = fs::canonicalize(scope_directory).expect("canonical scope");
        let attach_provider = directory.path().join("attach.sh");
        let display_provider = directory.path().join("display.sh");
        let opened = directory.path().join("opened");
        let focused = directory.path().join("focused");
        fs::write(&attach_provider, ATTACH_PROVIDER).expect("attach provider");
        fs::write(
            &display_provider,
            render_fixture(
                DISPLAY_RECEIPT_PROVIDER,
                serde_json::json!({
                    "opened": opened.display().to_string(),
                    "focused": focused.display().to_string(),
                }),
            ),
        )
        .expect("display provider");
        for provider in [&attach_provider, &display_provider] {
            fs::set_permissions(provider, fs::Permissions::from_mode(0o755))
                .expect("provider executable");
        }
        fs::write(
            provider_directory.join("harness.yaml"),
            render_fixture(
                ATTACH_PROVIDER_MANIFEST,
                serde_json::json!({ "command": attach_provider.display().to_string() }),
            ),
        )
        .expect("attach manifest");
        fs::write(
            provider_directory.join("display.yaml"),
            render_fixture(
                DISPLAY_RECEIPT_MANIFEST,
                serde_json::json!({ "command": display_provider.display().to_string() }),
            ),
        )
        .expect("display manifest");
        let config = Config {
            providers: crate::config::ProviderConfig {
                directory: provider_directory,
                ..crate::config::ProviderConfig::default()
            },
            ..Config::default()
        };
        let session = register(&scope, Contract::default(), SessionLink::default())
            .expect("registered session");
        state::update(&scope, |workspace| {
            let session = selected_session(workspace, &session.id)?.clone();
            workspace
                .sessions
                .iter_mut()
                .find(|candidate| candidate.id == session.id)
                .expect("session remains")
                .providers
                .push(ProviderBinding {
                    provider: "display".into(),
                    kind: ProviderKind::Display,
                    r#ref: None,
                    status: BindingStatus::Available,
                    label: "test pane".into(),
                });
            Ok(())
        })
        .expect("available display binding");

        let first = attach_quiet(
            &config,
            &scope,
            &session.id,
            provider::Action::Attach,
            "right",
        )
        .expect("first attach");
        assert_eq!(first.disposition, AttachDisposition::Launched);
        assert_eq!(fs::read_to_string(&opened).expect("open marker"), "open\n");
        let attached = selected_session(&state::read(&scope).expect("state"), &session.id)
            .expect("attached session")
            .clone();
        assert!(attached.providers.iter().any(|binding| {
            binding.provider == "display"
                && binding.kind == ProviderKind::Display
                && binding.status == BindingStatus::Active
                && binding.r#ref.as_deref() == Some("pane-7")
        }));
        state::update(&scope, |workspace| {
            let attached = workspace
                .sessions
                .iter_mut()
                .find(|candidate| candidate.id == session.id)
                .expect("attached session remains");
            attached.status = LifecycleStatus::Disconnected;
            touch_session(attached);
            Ok(())
        })
        .expect("disconnect attached session");

        let second = attach_quiet(
            &config,
            &scope,
            &session.id,
            provider::Action::Attach,
            "right",
        )
        .expect("second attach");
        assert_eq!(second.disposition, AttachDisposition::Focused);
        assert_eq!(fs::read_to_string(&opened).expect("open marker"), "open\n");
        assert_eq!(
            fs::read_to_string(&focused).expect("focus marker"),
            "focus\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_reconciliation_cannot_replace_an_attach_receipt() {
        let directory = tempfile::tempdir().expect("fixture");
        let scope_directory = directory.path().join("scope");
        let provider_directory = directory.path().join("providers");
        fs::create_dir_all(&scope_directory).expect("scope");
        fs::create_dir_all(&provider_directory).expect("providers");
        let scope = fs::canonicalize(scope_directory).expect("canonical scope");
        let attach_provider = directory.path().join("attach.sh");
        let display_provider = directory.path().join("display.sh");
        let resolving = directory.path().join("resolving");
        let release = directory.path().join("release");
        let opened = directory.path().join("opened");
        let focused = directory.path().join("focused");
        fs::write(&attach_provider, ATTACH_PROVIDER).expect("attach provider");
        fs::write(
            &display_provider,
            render_fixture(
                RACING_DISPLAY_RECEIPT_PROVIDER,
                serde_json::json!({
                    "resolving": resolving.display().to_string(),
                    "release": release.display().to_string(),
                    "opened": opened.display().to_string(),
                    "focused": focused.display().to_string(),
                }),
            ),
        )
        .expect("display provider");
        for provider in [&attach_provider, &display_provider] {
            fs::set_permissions(provider, fs::Permissions::from_mode(0o755))
                .expect("provider executable");
        }
        fs::write(
            provider_directory.join("harness.yaml"),
            render_fixture(
                ATTACH_PROVIDER_MANIFEST,
                serde_json::json!({ "command": attach_provider.display().to_string() }),
            ),
        )
        .expect("attach manifest");
        fs::write(
            provider_directory.join("display.yaml"),
            render_fixture(
                RACING_DISPLAY_RECEIPT_MANIFEST,
                serde_json::json!({ "command": display_provider.display().to_string() }),
            ),
        )
        .expect("display manifest");
        let config = Config {
            providers: crate::config::ProviderConfig {
                directory: provider_directory,
                ..crate::config::ProviderConfig::default()
            },
            ..Config::default()
        };
        let session = register(&scope, Contract::default(), SessionLink::default())
            .expect("registered session");

        let reconcile_config = config.clone();
        let reconcile_scope = scope.clone();
        let reconciliation =
            std::thread::spawn(move || reconcile(&reconcile_config, &reconcile_scope));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !resolving.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "reconciliation did not reach the blocked provider"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let first = attach_quiet(
            &config,
            &scope,
            &session.id,
            provider::Action::Attach,
            "right",
        );
        fs::write(&release, []).expect("release reconciliation");
        let reconciled = reconciliation
            .join()
            .expect("reconciliation thread")
            .expect("reconciliation succeeds");
        first.expect("first attach succeeds");
        let attached = selected_session(&reconciled, &session.id).expect("attached session");
        assert!(attached.providers.iter().any(|binding| {
            binding.provider == "display"
                && binding.kind == ProviderKind::Display
                && binding.status == BindingStatus::Active
                && binding.r#ref.as_deref() == Some("pane-7")
        }));

        let second = attach_quiet(
            &config,
            &scope,
            &session.id,
            provider::Action::Attach,
            "right",
        )
        .expect("second attach");
        assert_eq!(second.disposition, AttachDisposition::Focused);
        assert_eq!(fs::read_to_string(&opened).expect("open marker"), "open\n");
        assert_eq!(
            fs::read_to_string(&focused).expect("focus marker"),
            "focus\n"
        );
    }

    #[test]
    fn active_session_without_persistence_uses_provider_neutral_attach() {
        let mut calls = Vec::new();

        let outcome = execute_attach_with(provider::Action::Attach, false, |action| {
            calls.push(action);
            Ok((23, true))
        })
        .expect("attach provider decides whether the session can resume");

        assert_eq!(calls, vec![provider::Action::Attach]);
        assert_eq!(outcome.disposition, AttachDisposition::Launched);
        assert_eq!(outcome.code, 23);
        assert!(outcome.accepted);
    }

    #[test]
    fn attach_outcome_preserves_a_provider_rejection() {
        let outcome = execute_attach_with(provider::Action::Attach, false, |_| Ok((17, false)))
            .expect("provider rejection remains an outcome");

        assert_eq!(outcome.code, 17);
        assert!(!outcome.accepted);
        assert_eq!(outcome.disposition, AttachDisposition::Launched);
    }

    #[test]
    fn rejected_binding_receipt_has_no_binding_or_visible_output() {
        let plan = provider::CommandPlan {
            version: "orc.provider/v1".into(),
            command: vec!["false".into()],
            cwd: None,
            environment: BTreeMap::new(),
            success_codes: vec![0],
            receipt: Some(provider::CommandReceipt::ProviderBinding {
                provider: "display".into(),
            }),
        };
        let result = provider::CommandResult {
            code: 7,
            stdout: "reserved receipt".into(),
            stderr: String::new(),
        };

        let (accepted, binding, stdout) = interpret_plan_result(&[], &plan, &result)
            .expect("rejected command does not parse its receipt");

        assert!(!accepted);
        assert!(binding.is_none());
        assert!(stdout.is_none());
    }

    #[test]
    fn non_attach_outcome_preserves_provider_acceptance() {
        let outcome = execute_attach_with(provider::Action::Inspect, false, |_| Ok((23, true)))
            .expect("provider-defined success remains accepted");

        assert_eq!(outcome.code, 23);
        assert!(outcome.accepted);
        assert_eq!(outcome.disposition, AttachDisposition::Launched);
    }
}
