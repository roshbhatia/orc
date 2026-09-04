use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::{fd::AsRawFd, unix::process::CommandExt};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use minijinja::Environment;
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    config::{self, Config},
    control::{self, Contract, SessionLink},
    daemon,
    domain::{
        ActivityEvent, CompletionTarget, JudgePolicy, LifecycleStatus, PendingGate,
        RegistrationSource, RunMode, Session, SessionRole, WorkflowEdge, WorkflowNode, WorkflowRun,
        WorkspaceState,
    },
    provider::{self, Action, CommandPlan},
    state,
};

pub use crate::domain::GateAuthority;

#[cfg(test)]
use crate::preferences;

const MAX_WORKFLOW_LOG_BYTES: u64 = 1024 * 1024;
const MAX_RETRY_ATTEMPTS: u32 = 100;

#[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    #[default]
    Supervised,
    #[serde(alias = "manual")]
    ApprovalGated,
    #[serde(alias = "full_auto")]
    Autonomous,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApprovalActor {
    User,
    Orchestrator,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
pub struct ApprovalGate {
    pub id: String,
    pub before: String,
    pub reason: String,
    #[serde(default)]
    pub authority: GateAuthority,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, Serialize)]
#[serde(default)]
pub struct ApprovalPolicy {
    pub mode: ApprovalMode,
    pub gates: Vec<ApprovalGate>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextMode {
    Accumulate,
    LastOnly,
    #[default]
    Explicit,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureMode {
    #[default]
    FailFast,
    ContinueOnError,
    AllOrNothing,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    #[default]
    Agent,
    Script,
    Set,
    Wait,
    Workflow,
    Terminate,
    HumanGate,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, Serialize)]
#[serde(default)]
pub struct Runtime {
    pub harness: Option<String>,
    pub model: Option<String>,
    pub execution: Option<String>,
    pub reasoning: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
pub struct Route {
    pub to: String,
    pub when: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, Serialize)]
#[serde(default)]
pub struct RetryPolicy {
    pub attempts: u32,
    pub backoff_seconds: u64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(default)]
pub struct Step {
    pub name: String,
    pub r#type: StepKind,
    pub role: SessionRole,
    pub purpose: String,
    pub goal: String,
    pub expected_output: String,
    pub success_criteria: Vec<String>,
    pub completion: CompletionTarget,
    pub judge_policy: JudgePolicy,
    pub review_by: Option<String>,
    pub runtime: Runtime,
    pub prompt: Option<String>,
    pub command: Vec<String>,
    pub value: Option<serde_json::Value>,
    pub duration: Option<String>,
    pub workflow: Option<String>,
    pub input_mapping: BTreeMap<String, String>,
    #[serde(rename = "maxDepth", alias = "max_depth")]
    pub max_depth: Option<usize>,
    pub depends_on: Vec<String>,
    pub routes: Vec<Route>,
    pub retry: RetryPolicy,
    #[serde(rename = "timeoutSeconds", alias = "timeout_seconds")]
    pub timeout_seconds: Option<u64>,
    #[serde(rename = "idleTimeoutSeconds", alias = "idle_timeout_seconds")]
    pub idle_timeout_seconds: Option<u64>,
    pub requires_approval: bool,
}

impl Default for Step {
    fn default() -> Self {
        Self {
            name: String::new(),
            r#type: StepKind::Agent,
            role: SessionRole::Worker,
            purpose: String::new(),
            goal: String::new(),
            expected_output: "A verified result".into(),
            success_criteria: Vec::new(),
            completion: CompletionTarget::Orchestrator,
            judge_policy: JudgePolicy::Llm,
            review_by: None,
            runtime: Runtime::default(),
            prompt: None,
            command: Vec::new(),
            value: None,
            duration: None,
            workflow: None,
            input_mapping: BTreeMap::new(),
            max_depth: None,
            depends_on: Vec::new(),
            routes: Vec::new(),
            retry: RetryPolicy::default(),
            timeout_seconds: None,
            idle_timeout_seconds: None,
            requires_approval: false,
        }
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
pub struct ForEach {
    pub source: String,
    #[serde(default = "default_item")]
    pub r#as: String,
}

fn default_item() -> String {
    "item".into()
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, Serialize)]
#[serde(default)]
pub struct ParallelGroup {
    pub name: String,
    pub description: String,
    pub agents: Vec<String>,
    pub for_each: Option<ForEach>,
    pub agent: Option<String>,
    pub max_concurrent: Option<usize>,
    pub failure_mode: FailureMode,
    pub routes: Vec<Route>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(default)]
pub struct Limits {
    pub max_iterations: u32,
    pub timeout_seconds: Option<u64>,
    pub budget_usd: Option<f64>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_iterations: 50,
            timeout_seconds: None,
            budget_usd: None,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, Serialize)]
#[serde(default)]
pub struct WorkflowDefaults {
    pub runtime: Runtime,
    pub context: ContextMode,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(default)]
pub struct Definition {
    pub version: String,
    pub name: String,
    pub description: String,
    pub goal: String,
    pub expected_output: String,
    pub entry_point: String,
    pub approval: ApprovalPolicy,
    pub defaults: WorkflowDefaults,
    pub limits: Limits,
    pub input: BTreeMap<String, serde_json::Value>,
    pub steps: Vec<Step>,
    pub parallel: Vec<ParallelGroup>,
}

impl Default for Definition {
    fn default() -> Self {
        Self {
            version: "orc.workflow/v1".into(),
            name: String::new(),
            description: String::new(),
            goal: String::new(),
            expected_output: "A verified result".into(),
            entry_point: String::new(),
            approval: ApprovalPolicy::default(),
            defaults: WorkflowDefaults::default(),
            limits: Limits::default(),
            input: BTreeMap::new(),
            steps: Vec::new(),
            parallel: Vec::new(),
        }
    }
}

pub fn schema() -> serde_json::Value {
    serde_json::to_value(schema_for!(Definition)).expect("workflow schema serializes")
}

pub fn load(path: &Path) -> Result<Definition> {
    let definition: Definition = serde_yaml::from_str(
        &fs::read_to_string(path).with_context(|| format!("read workflow {}", path.display()))?,
    )
    .with_context(|| format!("parse workflow {}", path.display()))?;
    validate(&definition, path.parent().unwrap_or(Path::new(".")))?;
    Ok(definition)
}

pub fn validate(definition: &Definition, base: &Path) -> Result<()> {
    if definition.version != "orc.workflow/v1" {
        bail!("version must be orc.workflow/v1");
    }
    if definition.name.trim().is_empty() || definition.goal.trim().is_empty() {
        bail!("name and goal are required");
    }
    validate_name(&definition.name)?;
    if definition.limits.max_iterations == 0 || definition.limits.max_iterations > 500 {
        bail!("limits.max_iterations must be between 1 and 500");
    }
    if definition.limits.timeout_seconds == Some(0) {
        bail!("limits.timeout_seconds must be positive when set");
    }
    if definition
        .limits
        .budget_usd
        .is_some_and(|budget| !budget.is_finite() || budget < 0.0)
    {
        bail!("limits.budget_usd must be a finite non-negative number");
    }
    if definition.defaults.context != ContextMode::Explicit {
        bail!("defaults.context must be explicit until implicit context modes are supported");
    }
    let step_names: BTreeSet<_> = definition
        .steps
        .iter()
        .map(|step| step.name.as_str())
        .collect();
    let group_names: BTreeSet<_> = definition
        .parallel
        .iter()
        .map(|group| group.name.as_str())
        .collect();
    if step_names.len() != definition.steps.len() || group_names.len() != definition.parallel.len()
    {
        bail!("step and parallel group names must be unique");
    }
    if let Some(name) = step_names.intersection(&group_names).next() {
        bail!("step and parallel group names overlap: {name}");
    }
    if !step_names.contains(definition.entry_point.as_str())
        && !group_names.contains(definition.entry_point.as_str())
    {
        bail!("entry_point does not exist: {}", definition.entry_point);
    }
    let all: BTreeSet<_> = step_names.union(&group_names).copied().collect();
    for step in &definition.steps {
        if step.name.is_empty() {
            bail!("a step has no name");
        }
        if step.role == SessionRole::Orchestrator {
            bail!(
                "step {} cannot use the orchestrator role; the orchestrator owns the workflow",
                step.name
            );
        }
        if step.retry.attempts > MAX_RETRY_ATTEMPTS {
            bail!(
                "step {} retry attempts exceed the limit of {MAX_RETRY_ATTEMPTS}",
                step.name
            );
        }
        if !step.input_mapping.is_empty() {
            bail!(
                "step {} uses input_mapping, which is not supported yet",
                step.name
            );
        }
        if matches!(step.r#type, StepKind::Agent)
            && step
                .runtime
                .harness
                .as_ref()
                .or(definition.defaults.runtime.harness.as_ref())
                .is_none()
        {
            bail!("agent {} needs a harness", step.name);
        }
        if matches!(step.r#type, StepKind::Script) && step.command.is_empty() {
            bail!("script {} needs command", step.name);
        }
        if matches!(step.r#type, StepKind::Workflow) {
            if step.max_depth == Some(0) {
                bail!("workflow step {} max_depth must be positive", step.name);
            }
            let reference = step
                .workflow
                .as_deref()
                .context("workflow step needs workflow")?;
            if reference.starts_with('.') && !base.join(reference).exists() {
                bail!("sub-workflow does not exist: {reference}");
            }
        }
        for dependency in &step.depends_on {
            if !all.contains(dependency.as_str()) {
                bail!("{} depends on unknown step {dependency}", step.name);
            }
        }
        if let Some(reviewer) = step.review_by.as_deref() {
            if reviewer == step.name {
                bail!("step {} cannot review itself", step.name);
            }
            let reviewer_step = definition
                .steps
                .iter()
                .find(|candidate| candidate.name == reviewer)
                .with_context(|| format!("{} names unknown reviewer {reviewer}", step.name))?;
            if !matches!(
                reviewer_step.role,
                SessionRole::Critic | SessionRole::Judge | SessionRole::Verifier
            ) {
                bail!("reviewer {reviewer} must use the critic, judge, or verifier role");
            }
        } else if step.completion == CompletionTarget::Judge {
            bail!(
                "step {} reports to a judge but does not name review_by",
                step.name
            );
        }
        for route in &step.routes {
            if route.to != "$end" && route.to != "self" && !all.contains(route.to.as_str()) {
                bail!("{} routes to unknown step {}", step.name, route.to);
            }
            validate_route_condition(route, &step.name)?;
        }
    }
    let mut grouped_steps = BTreeSet::new();
    for group in &definition.parallel {
        if group.for_each.is_some() || group.agent.is_some() {
            bail!(
                "parallel {} uses dynamic for_each, which is not supported yet",
                group.name
            );
        }
        if group.agents.is_empty() {
            bail!(
                "parallel {} must define at least one static agent",
                group.name
            );
        }
        if group.max_concurrent == Some(0) {
            bail!("parallel {} max_concurrent must be positive", group.name);
        }
        for agent in &group.agents {
            if !step_names.contains(agent.as_str()) {
                bail!("parallel {} references unknown step {agent}", group.name);
            }
            if !grouped_steps.insert(agent.as_str()) {
                bail!("step {agent} belongs to more than one parallel group");
            }
            if definition
                .steps
                .iter()
                .find(|step| step.name == *agent)
                .is_some_and(|step| step.depends_on.contains(&group.name))
            {
                bail!("parallel member {agent} cannot depend on its own group");
            }
        }
        for route in &group.routes {
            if route.to != "$end" && route.to != "self" && !all.contains(route.to.as_str()) {
                bail!(
                    "parallel {} routes to unknown step {}",
                    group.name,
                    route.to
                );
            }
            validate_route_condition(route, &group.name)?;
        }
    }
    if let Some(entry) = definition
        .steps
        .iter()
        .find(|step| step.name == definition.entry_point)
        && !effective_dependencies(definition, entry).is_empty()
    {
        bail!("entry point {} cannot have dependencies", entry.name);
    }
    if let Some(group) = definition
        .parallel
        .iter()
        .find(|group| group.name == definition.entry_point)
        && let Some(member) = group.agents.iter().find(|name| {
            definition
                .steps
                .iter()
                .find(|step| step.name == name.as_str())
                .is_some_and(|step| !step.depends_on.is_empty())
        })
    {
        bail!("entry parallel member {member} cannot have dependencies");
    }
    for step in definition
        .steps
        .iter()
        .filter(|step| grouped_steps.contains(step.name.as_str()))
    {
        if !step.routes.is_empty() {
            bail!("parallel member {} must use its group's routes", step.name);
        }
    }
    if grouped_steps.contains(definition.entry_point.as_str()) {
        bail!(
            "entry point {} is a parallel member; use its group name instead",
            definition.entry_point
        );
    }
    if let Some((source, target)) = definition
        .steps
        .iter()
        .flat_map(|step| {
            step.routes
                .iter()
                .map(move |route| (step.name.as_str(), route.to.as_str()))
        })
        .chain(definition.parallel.iter().flat_map(|group| {
            group
                .routes
                .iter()
                .map(move |route| (group.name.as_str(), route.to.as_str()))
        }))
        .find(|(_, target)| grouped_steps.contains(target))
    {
        bail!("{source} routes to parallel member {target}; use its group name instead");
    }
    for gate in &definition.approval.gates {
        if !all.contains(gate.before.as_str()) {
            bail!("gate {} references unknown step {}", gate.id, gate.before);
        }
    }
    Ok(())
}

fn route_template(condition: &str) -> String {
    if condition.contains("{{") || condition.contains("{%") {
        condition.to_owned()
    } else {
        format!("{{{{ {condition} }}}}")
    }
}

fn validate_route_condition(route: &Route, source: &str) -> Result<()> {
    let Some(condition) = route.when.as_deref() else {
        return Ok(());
    };
    let template = route_template(condition);
    let mut environment = Environment::new();
    environment
        .add_template("route", &template)
        .with_context(|| format!("compile route condition for {source}"))?;
    Ok(())
}

pub fn repository(config: &Config) -> Result<PathBuf> {
    let repository = config.workflows.repository.clone();
    fs::create_dir_all(&repository)?;
    if !repository.join(".git").exists() {
        run_git(&repository, ["init", "--quiet"])?;
        fs::write(
            repository.join("README.md"),
            r#"# Orc workflows

Versioned workflow definitions managed by Orc.
"#,
        )?;
        run_git(&repository, ["add", "README.md"])?;
        run_git(
            &repository,
            [
                "-c",
                "user.name=Orc",
                "-c",
                "user.email=orc@localhost",
                "commit",
                "--quiet",
                "-m",
                "chore: initialize workflow catalog",
            ],
        )?;
    }
    Ok(repository)
}

fn run_git<const N: usize>(repository: &Path, args: [&str; N]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repository)
        .output()
        .context("run git")?;
    if !output.status.success() {
        bail!(
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub fn scope_directory(config: &Config, scope: &Path) -> Result<PathBuf> {
    let repository = repository(config)?;
    let scope = state::resolve_scope(scope)?;
    let directory = repository.join(state::scope_key(&scope));
    fs::create_dir_all(&directory)?;
    Ok(directory)
}

pub fn path(config: &Config, scope: &Path, name: &str) -> Result<PathBuf> {
    validate_name(name)?;
    Ok(scope_directory(config, scope)?.join(format!("{name}.yaml")))
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || !name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        bail!("workflow name must contain only letters, numbers, '-' or '_'");
    }
    Ok(())
}

pub fn import(config: &Config, scope: &Path, source: &Path) -> Result<PathBuf> {
    let definition = load(source)?;
    save(config, scope, &definition)
}

pub fn save(config: &Config, scope: &Path, definition: &Definition) -> Result<PathBuf> {
    let directory = scope_directory(config, scope)?;
    validate(definition, &directory)?;
    let target = path(config, scope, &definition.name)?;
    fs::write(&target, serde_yaml::to_string(&definition)?)?;
    if config.workflows.auto_commit {
        commit(config, &format!("feat: save {} workflow", definition.name))?;
    }
    Ok(target)
}

fn definition_revision(definition: &Definition) -> Result<String> {
    let encoded = serde_json::to_vec(definition).context("serialize workflow revision")?;
    Ok(format!("sha256:{}", hex::encode(Sha256::digest(encoded))))
}

fn materialized_definition_path(scope: &Path, run_id: &str) -> PathBuf {
    config::state_home()
        .join("orc/runs")
        .join(state::scope_key(scope))
        .join(format!("{run_id}.yaml"))
}

fn materialize_definition_snapshot(
    scope: &Path,
    run_id: &str,
    definition: &Definition,
) -> Result<PathBuf> {
    let path = materialized_definition_path(scope, run_id);
    fs::create_dir_all(path.parent().context("run definition path has no parent")?)?;
    fs::write(&path, serde_yaml::to_string(definition)?)
        .with_context(|| format!("write materialized workflow {}", path.display()))?;
    Ok(path)
}

fn pin_relative_workflow_references(definition: &mut Definition, source: &Path) -> Result<()> {
    let base = source.parent().unwrap_or(Path::new("."));
    for step in definition
        .steps
        .iter_mut()
        .filter(|step| matches!(step.r#type, StepKind::Workflow))
    {
        let Some(reference) = step.workflow.as_deref() else {
            continue;
        };
        if reference.starts_with('.') {
            step.workflow = Some(
                fs::canonicalize(base.join(reference))
                    .with_context(|| format!("resolve sub-workflow {reference}"))?
                    .display()
                    .to_string(),
            );
        }
    }
    Ok(())
}

fn commit_run_definition(config: &Config, path: &Path, definition: &Definition) -> Result<()> {
    if config.workflows.auto_commit
        && fs::canonicalize(&config.workflows.repository)
            .is_ok_and(|repository| path.starts_with(repository))
    {
        commit(
            config,
            &format!("feat: update {} workflow", definition.name),
        )?;
    }
    Ok(())
}

fn update_run_definition<T>(
    config: &Config,
    scope: &Path,
    run_id: &str,
    path: &Path,
    previous_revision: &str,
    definition: &Definition,
    transform: impl FnOnce(&mut WorkflowRun) -> Result<T>,
) -> Result<T> {
    validate(definition, path.parent().unwrap_or(Path::new(".")))?;
    let encoded = serde_yaml::to_string(definition)?;
    let revision = definition_revision(definition)?;
    let result = state::update(scope, |workspace| {
        let run = workspace
            .runs
            .iter_mut()
            .find(|run| run.id == run_id)
            .with_context(|| format!("unknown run: {run_id}"))?;
        if run.revision.as_deref() != Some(previous_revision) {
            bail!("workflow run changed concurrently; reload it before editing");
        }
        let result = transform(run)?;
        fs::write(path, encoded).with_context(|| format!("write workflow {}", path.display()))?;
        apply_definition_revision(run, &revision);
        run.updated_at = Utc::now();
        Ok(result)
    })?;
    commit_run_definition(config, path, definition)?;
    Ok(result)
}

fn apply_definition_revision(run: &mut WorkflowRun, revision: &str) -> bool {
    if run.revision.as_deref() == Some(revision) {
        return false;
    }
    run.revision = Some(revision.to_owned());
    run.pending_gates.clear();
    run.approved_gates.clear();
    run.updated_at = Utc::now();
    true
}

fn require_unstarted_node<'a>(
    run: &'a WorkflowRun,
    node_id: &str,
    operation: &str,
) -> Result<&'a WorkflowNode> {
    let node = run
        .nodes
        .iter()
        .find(|node| node.id == node_id)
        .with_context(|| format!("unknown node: {node_id}"))?;
    if node.attempt != 0
        || node.session_id.is_some()
        || !matches!(
            node.status,
            LifecycleStatus::Pending | LifecycleStatus::Queued | LifecycleStatus::Waiting
        )
    {
        bail!(
            "cannot {operation} node {node_id} after it has started; restart-from-node is not implemented"
        );
    }
    Ok(node)
}

fn reset_unstarted_nodes(run: &mut WorkflowRun, node_ids: &BTreeSet<String>) -> Result<()> {
    for node_id in node_ids {
        require_unstarted_node(run, node_id, "change")?;
    }
    for node in run
        .nodes
        .iter_mut()
        .filter(|node| node_ids.contains(&node.id))
    {
        if node.status == LifecycleStatus::Waiting {
            node.status = LifecycleStatus::Queued;
        }
        node.retry_after = None;
        node.updated_at = Utc::now();
        node.record_activity("contract", "unstarted stage contract changed");
    }
    run.pending_gates.clear();
    run.approved_gates.clear();
    if run
        .current_node
        .as_ref()
        .is_some_and(|node_id| node_ids.contains(node_id))
    {
        run.current_node = None;
    }
    Ok(())
}

fn downstream_nodes(definition: &Definition, root: &str) -> BTreeSet<String> {
    let mut result = BTreeSet::from([root.to_owned()]);
    loop {
        let discovered = definition
            .steps
            .iter()
            .filter(|step| {
                step.depends_on.iter().any(|dependency| {
                    result.contains(dependency)
                        || definition.parallel.iter().any(|group| {
                            group.name == *dependency
                                && group.agents.iter().any(|member| result.contains(member))
                        })
                }) || definition.steps.iter().any(|source| {
                    result.contains(&source.name)
                        && source.review_by.as_deref() == Some(step.name.as_str())
                }) || definition.steps.iter().any(|source| {
                    result.contains(&source.name)
                        && source.routes.iter().any(|route| route.to == step.name)
                })
            })
            .map(|step| step.name.clone())
            .collect::<Vec<_>>();
        let before = result.len();
        result.extend(discovered);
        if result.len() == before {
            return result;
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct NodeEdit {
    pub goal: Option<String>,
    pub expected_output: Option<String>,
    pub success_criteria: Option<Vec<String>>,
    pub harness: Option<String>,
    pub model: Option<String>,
    pub execution: Option<String>,
    pub judge_policy: Option<JudgePolicy>,
}

fn run_definition(scope: &Path, run_id: &str) -> Result<(WorkflowRun, PathBuf, Definition)> {
    let scope = state::resolve_scope(scope)?;
    let run = state::read(&scope)?
        .runs
        .into_iter()
        .find(|run| run.id == run_id)
        .with_context(|| format!("unknown run: {run_id}"))?;
    let path = run
        .definition
        .as_deref()
        .map(PathBuf::from)
        .context("this run has no versioned workflow definition")?;
    let definition = load(&path)?;
    let revision = definition_revision(&definition)?;
    if run.revision.as_deref() != Some(revision.as_str()) {
        bail!(
            "workflow run definition changed outside Orc; restore revision {} or create a new run",
            run.revision.as_deref().unwrap_or("unknown")
        );
    }
    Ok((run, path, definition))
}

pub fn edit_run_node(
    config: &Config,
    scope: &Path,
    run_id: &str,
    node_id: &str,
    edit: NodeEdit,
) -> Result<WorkflowNode> {
    let (original_run, definition_path, mut definition) = run_definition(scope, run_id)?;
    let previous_revision = original_run
        .revision
        .as_deref()
        .context("run has no revision")?;
    let affected = downstream_nodes(&definition, node_id);
    let step = definition
        .steps
        .iter_mut()
        .find(|step| step.name == node_id)
        .with_context(|| format!("unknown node: {node_id}"))?;
    if let Some(value) = edit.goal {
        step.goal = value;
    }
    if let Some(value) = edit.expected_output {
        step.expected_output = value;
    }
    if let Some(value) = edit.success_criteria {
        step.success_criteria = value;
    }
    if let Some(value) = edit.harness {
        step.runtime.harness = Some(value);
    }
    if let Some(value) = edit.model {
        step.runtime.model = Some(value);
    }
    if let Some(value) = edit.execution {
        step.runtime.execution = Some(value);
    }
    if let Some(value) = edit.judge_policy {
        step.judge_policy = value;
    }
    let step = definition
        .steps
        .iter()
        .find(|step| step.name == node_id)
        .expect("edited step remains")
        .clone();
    let scope = state::resolve_scope(scope)?;
    update_run_definition(
        config,
        &scope,
        run_id,
        &definition_path,
        previous_revision,
        &definition,
        |run| {
            reset_unstarted_nodes(run, &affected)?;
            let node = run
                .nodes
                .iter_mut()
                .find(|node| node.id == node_id)
                .with_context(|| format!("unknown node: {node_id}"))?;
            node.goal.clone_from(&step.goal);
            node.expected_output.clone_from(&step.expected_output);
            node.success_criteria.clone_from(&step.success_criteria);
            if let Some(value) = &step.runtime.harness {
                node.harness.clone_from(value);
            }
            node.model.clone_from(&step.runtime.model);
            node.execution.clone_from(&step.runtime.execution);
            node.judge_policy = step.judge_policy;
            node.updated_at = Utc::now();
            Ok(node.clone())
        },
    )
}

pub fn delete_run_node(config: &Config, scope: &Path, run_id: &str, node_id: &str) -> Result<()> {
    let (original_run, definition_path, mut definition) = run_definition(scope, run_id)?;
    let previous_revision = original_run
        .revision
        .as_deref()
        .context("run has no revision")?;
    if !definition.steps.iter().any(|step| step.name == node_id) {
        bail!("unknown node: {node_id}");
    }
    let affected = downstream_nodes(&definition, node_id);
    definition.steps.retain(|step| step.name != node_id);
    for step in &mut definition.steps {
        step.depends_on.retain(|dependency| dependency != node_id);
        step.routes.retain(|route| route.to != node_id);
        if step.review_by.as_deref() == Some(node_id) {
            step.review_by = None;
        }
    }
    definition.parallel.retain_mut(|group| {
        group.agents.retain(|agent| agent != node_id);
        if group.agent.as_deref() == Some(node_id) {
            group.agent = None;
        }
        !group.agents.is_empty() || group.agent.is_some()
    });
    definition
        .approval
        .gates
        .retain(|gate| gate.before != node_id);
    if definition.entry_point == node_id {
        definition.entry_point = definition
            .steps
            .first()
            .map(|step| step.name.clone())
            .unwrap_or_default();
    }
    let scope = state::resolve_scope(scope)?;
    update_run_definition(
        config,
        &scope,
        run_id,
        &definition_path,
        previous_revision,
        &definition,
        |run| {
            reset_unstarted_nodes(run, &affected)?;
            run.nodes.retain(|node| node.id != node_id);
            run.edges
                .retain(|edge| edge.from != node_id && edge.to != node_id);
            Ok(())
        },
    )
}

pub fn set_run_dependency(
    config: &Config,
    scope: &Path,
    run_id: &str,
    node_id: &str,
    dependency: &str,
    present: bool,
) -> Result<()> {
    if node_id == dependency {
        bail!("a node cannot depend on itself");
    }
    let (original_run, definition_path, mut definition) = run_definition(scope, run_id)?;
    let previous_revision = original_run
        .revision
        .as_deref()
        .context("run has no revision")?;
    if !definition.steps.iter().any(|step| step.name == dependency) {
        bail!("unknown dependency: {dependency}");
    }
    let step = definition
        .steps
        .iter_mut()
        .find(|step| step.name == node_id)
        .with_context(|| format!("unknown node: {node_id}"))?;
    step.depends_on.retain(|candidate| candidate != dependency);
    if present {
        step.depends_on.push(dependency.into());
    }
    let _ = plan(config, scope, &definition)?;
    let affected = downstream_nodes(&definition, node_id);
    let scope = state::resolve_scope(scope)?;
    update_run_definition(
        config,
        &scope,
        run_id,
        &definition_path,
        previous_revision,
        &definition,
        |run| {
            reset_unstarted_nodes(run, &affected)?;
            run.edges
                .retain(|edge| !(edge.to == node_id && edge.relationship == "depends_on"));
            run.edges.extend(
                definition
                    .steps
                    .iter()
                    .find(|step| step.name == node_id)
                    .into_iter()
                    .flat_map(|step| step.depends_on.iter())
                    .map(|from| WorkflowEdge {
                        from: from.clone(),
                        to: node_id.into(),
                        relationship: "depends_on".into(),
                    }),
            );
            Ok(())
        },
    )
}

pub fn init(config: &Config, scope: &Path, name: &str, harness: Option<&str>) -> Result<PathBuf> {
    let scope = state::resolve_scope(scope)?;
    let harness = harness.map(str::to_owned).or_else(|| {
        state::read(&scope).ok().and_then(|workspace| {
            workspace
                .current_session()
                .map(|session| session.harness.clone())
        })
    });
    if harness.is_none() {
        bail!("workflow init needs --harness outside an active Orc session");
    }
    let target = path(config, &scope, name)?;
    if target.exists() {
        bail!("workflow already exists: {name}");
    }
    let mut definition = Definition {
        name: name.into(),
        description: format!("{name} workflow"),
        goal: "Describe the workflow goal".into(),
        entry_point: "plan".into(),
        ..Definition::default()
    };
    definition.defaults.runtime.harness = harness;
    definition.steps.push(Step {
        name: "plan".into(),
        role: SessionRole::Planner,
        purpose: "Turn the goal into an executable plan".into(),
        goal: "Produce a verified implementation plan".into(),
        requires_approval: true,
        ..Step::default()
    });
    fs::write(&target, serde_yaml::to_string(&definition)?)?;
    if config.workflows.auto_commit {
        commit(config, &format!("feat: add {name} workflow"))?;
    }
    Ok(target)
}

pub fn list(config: &Config, scope: &Path) -> Result<Vec<PathBuf>> {
    let directory = scope_directory(config, scope)?;
    let mut paths: Vec<_> = fs::read_dir(directory)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("yaml"))
        .collect();
    paths.sort();
    Ok(paths)
}

pub fn commit(config: &Config, message: &str) -> Result<()> {
    let repository = repository(config)?;
    run_git(&repository, ["add", "."])?;
    let dirty = run_git(&repository, ["status", "--porcelain"])?;
    if dirty.trim().is_empty() {
        return Ok(());
    }
    run_git(
        &repository,
        [
            "-c",
            "user.name=Orc",
            "-c",
            "user.email=orc@localhost",
            "commit",
            "--quiet",
            "-m",
            message,
        ],
    )?;
    Ok(())
}

pub fn history(config: &Config, scope: &Path, name: &str) -> Result<String> {
    let repository = repository(config)?;
    let target = path(config, scope, name)?;
    let relative = target
        .strip_prefix(&repository)?
        .to_string_lossy()
        .to_string();
    run_git(
        &repository,
        ["log", "--oneline", "--follow", "--", &relative],
    )
}

pub fn search(config: &Config, query: &str) -> Result<String> {
    let repository = repository(config)?;
    run_git(&repository, ["grep", "-n", "-i", "--", query])
}

#[derive(Clone, Debug, Serialize)]
pub struct Plan {
    pub name: String,
    pub revision: Option<String>,
    pub approval: ApprovalMode,
    pub gates: Vec<ApprovalGate>,
    pub waves: Vec<Vec<String>>,
    pub steps: Vec<PlannedStep>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PlannedStep {
    pub name: String,
    pub kind: StepKind,
    pub harness: Option<String>,
    pub model: Option<String>,
    pub execution: Option<String>,
    pub judge_policy: JudgePolicy,
    pub approval_required: bool,
    pub depends_on: Vec<String>,
}

fn approval_required(definition: &Definition, step: &Step) -> bool {
    let explicitly_gated = definition
        .approval
        .gates
        .iter()
        .any(|gate| gate.before == step.name);
    matches!(step.r#type, StepKind::HumanGate)
        || match definition.approval.mode {
            ApprovalMode::Autonomous => false,
            ApprovalMode::ApprovalGated => step.requires_approval || explicitly_gated,
            ApprovalMode::Supervised => true,
        }
}

pub fn plan(_config: &Config, _scope: &Path, definition: &Definition) -> Result<Plan> {
    let revision = Some(definition_revision(definition)?);
    let steps = definition
        .steps
        .iter()
        .map(|step| PlannedStep {
            name: step.name.clone(),
            kind: step.r#type.clone(),
            harness: step
                .runtime
                .harness
                .clone()
                .or_else(|| definition.defaults.runtime.harness.clone()),
            model: step
                .runtime
                .model
                .clone()
                .or_else(|| definition.defaults.runtime.model.clone()),
            execution: step
                .runtime
                .execution
                .clone()
                .or_else(|| definition.defaults.runtime.execution.clone()),
            judge_policy: step.judge_policy,
            approval_required: approval_required(definition, step),
            depends_on: effective_dependencies(definition, step),
        })
        .collect::<Vec<_>>();
    let mut unresolved: BTreeSet<_> = steps.iter().map(|step| step.name.clone()).collect();
    let mut resolved = BTreeSet::new();
    let mut waves = Vec::new();
    while !unresolved.is_empty() {
        for group in &definition.parallel {
            if group.agents.iter().all(|member| resolved.contains(member)) {
                resolved.insert(group.name.clone());
            }
        }
        let wave: Vec<_> = unresolved
            .iter()
            .filter(|name| {
                steps
                    .iter()
                    .find(|step| &step.name == *name)
                    .is_some_and(|step| {
                        step.depends_on
                            .iter()
                            .all(|dependency| resolved.contains(dependency))
                    })
            })
            .cloned()
            .collect();
        if wave.is_empty() {
            break;
        }
        for name in &wave {
            unresolved.remove(name);
            resolved.insert(name.clone());
        }
        waves.push(wave);
    }
    if !unresolved.is_empty() {
        bail!(
            "workflow contains a dependency cycle involving: {}",
            unresolved.into_iter().collect::<Vec<_>>().join(", ")
        );
    }
    Ok(Plan {
        name: definition.name.clone(),
        revision,
        approval: definition.approval.mode.clone(),
        gates: definition.approval.gates.clone(),
        waves,
        steps,
    })
}

pub fn materialize(
    config: &Config,
    scope: &Path,
    definition_path: &Path,
    mode: RunMode,
) -> Result<WorkflowRun> {
    materialize_with_parent(config, scope, definition_path, mode, None, None)
}

fn materialize_with_parent(
    config: &Config,
    scope: &Path,
    definition_path: &Path,
    mode: RunMode,
    parent_run_id: Option<&str>,
    parent_node_id: Option<&str>,
) -> Result<WorkflowRun> {
    let scope = state::resolve_scope(scope)?;
    let definition_path = fs::canonicalize(definition_path)
        .with_context(|| format!("resolve workflow definition {}", definition_path.display()))?;
    let mut definition = load(&definition_path)?;
    pin_relative_workflow_references(&mut definition, &definition_path)?;
    let planned = plan(config, &scope, &definition)?;
    let snapshot = state::read(&scope)?;
    let orchestrator = snapshot
        .current_session()
        .context("start a registered orchestrator before starting a workflow")?;
    require_orchestrator(orchestrator)?;
    let now = Utc::now();
    let run_id = format!("run-{}", &Uuid::new_v4().to_string()[..12]);
    let materialized_definition = materialize_definition_snapshot(&scope, &run_id, &definition)?;
    let entry_members = definition
        .parallel
        .iter()
        .find(|group| group.name == definition.entry_point)
        .map(|group| group.agents.iter().cloned().collect::<BTreeSet<_>>())
        .unwrap_or_default();
    let nodes = definition
        .steps
        .iter()
        .map(|step| WorkflowNode {
            id: step.name.clone(),
            name: step.name.clone(),
            purpose: step.purpose.clone(),
            role: step.role,
            harness: step
                .runtime
                .harness
                .clone()
                .or_else(|| definition.defaults.runtime.harness.clone())
                .unwrap_or_else(|| "control".into()),
            model: step
                .runtime
                .model
                .clone()
                .or_else(|| definition.defaults.runtime.model.clone()),
            goal: step.goal.clone(),
            expected_output: step.expected_output.clone(),
            success_criteria: step.success_criteria.clone(),
            completion: step.completion,
            review_by: step.review_by.clone(),
            execution: step
                .runtime
                .execution
                .clone()
                .or_else(|| definition.defaults.runtime.execution.clone()),
            judge_policy: step.judge_policy,
            session_id: None,
            child_run_id: None,
            status: if step.name == definition.entry_point || entry_members.contains(&step.name) {
                LifecycleStatus::Queued
            } else {
                LifecycleStatus::Pending
            },
            attempt: 0,
            retry_after: None,
            prompt: step.prompt.clone(),
            input: None,
            output: None,
            activity: vec![ActivityEvent {
                at: now,
                kind: "planned".into(),
                message: format!("{} entered the execution graph", step.name),
            }],
            tokens: 0,
            cost_usd: 0.0,
            updated_at: now,
        })
        .collect::<Vec<_>>();
    let mut edges = Vec::new();
    for step in &definition.steps {
        edges.extend(step.depends_on.iter().map(|dependency| WorkflowEdge {
            from: dependency.clone(),
            to: step.name.clone(),
            relationship: "depends_on".into(),
        }));
        if let Some(reviewer) = &step.review_by {
            edges.push(WorkflowEdge {
                from: step.name.clone(),
                to: reviewer.clone(),
                relationship: "reviewed_by".into(),
            });
        }
        edges.extend(
            step.routes
                .iter()
                .filter(|route| route.to != "$end" && route.to != "self")
                .map(|route| WorkflowEdge {
                    from: step.name.clone(),
                    to: route.to.clone(),
                    relationship: route_relationship(&definition, step, route),
                }),
        );
    }
    let mut agents = Vec::new();
    for node in nodes.iter().filter(|node| {
        definition
            .steps
            .iter()
            .find(|step| step.name == node.id)
            .is_some_and(|step| matches!(step.r#type, StepKind::Agent))
    }) {
        if !agents.iter().any(|agent: &crate::domain::AgentConfig| {
            agent.role == node.role && agent.harness == node.harness && agent.model == node.model
        }) {
            agents.push(crate::domain::AgentConfig {
                role: node.role,
                harness: node.harness.clone(),
                model: node.model.clone(),
            });
        }
    }
    let run = WorkflowRun {
        id: run_id,
        name: definition.name,
        goal: definition.goal,
        expected_output: definition.expected_output,
        status: LifecycleStatus::Queued,
        orchestrator_id: Some(orchestrator.id.clone()),
        parent_run_id: parent_run_id.map(str::to_owned),
        definition: Some(materialized_definition.display().to_string()),
        revision: planned.revision,
        checkpoint: None,
        mode,
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
        agents,
        nodes,
        edges,
        created_at: now,
        updated_at: now,
    };
    state::update(&scope, |workspace| {
        let active_orchestrator = workspace.sessions.iter().any(|session| {
            session.id == orchestrator.id
                && session.role == SessionRole::Orchestrator
                && session.status.active()
                && session.status != LifecycleStatus::Terminating
        });
        if !active_orchestrator {
            bail!(
                "orchestrator is not accepting new work: {}",
                orchestrator.id
            );
        }
        if let (Some(parent_run_id), Some(parent_node_id)) = (parent_run_id, parent_node_id) {
            let parent = workspace
                .runs
                .iter_mut()
                .find(|candidate| candidate.id == parent_run_id)
                .with_context(|| format!("unknown parent run: {parent_run_id}"))?;
            if !parent.status.active() || parent.status == LifecycleStatus::Terminating {
                bail!("parent run is not accepting child workflows: {parent_run_id}");
            }
            let node = parent
                .nodes
                .iter_mut()
                .find(|candidate| candidate.id == parent_node_id)
                .with_context(|| format!("unknown parent node: {parent_node_id}"))?;
            node.child_run_id = Some(run.id.clone());
            node.updated_at = now;
            parent.updated_at = now;
        }
        workspace.runs.insert(0, run.clone());
        Ok(run)
    })
}

fn require_orchestrator(session: &Session) -> Result<()> {
    if session.role != SessionRole::Orchestrator {
        bail!("only an orchestrator can start a workflow");
    }
    Ok(())
}

fn required_gate(definition: &Definition, step: &Step, run: &WorkflowRun) -> Option<PendingGate> {
    let explicit = definition
        .approval
        .gates
        .iter()
        .find(|gate| gate.before == step.name);
    if !approval_required(definition, step) {
        return None;
    }
    let id = explicit
        .map(|gate| gate.id.clone())
        .unwrap_or_else(|| format!("{}:{}", definition.name, step.name));
    if run
        .approved_gates
        .contains(&gate_approval_key(&id, run.revision.as_deref()))
    {
        return None;
    }
    Some(PendingGate {
        id,
        before: step.name.clone(),
        reason: explicit.map_or_else(
            || format!("Approve {} before execution", step.name),
            |gate| gate.reason.clone(),
        ),
        authority: explicit.map_or(GateAuthority::User, |gate| gate.authority),
        recommendation: None,
        created_at: Utc::now(),
    })
}

fn gate_approval_key(id: &str, revision: Option<&str>) -> String {
    revision.map_or_else(|| id.to_owned(), |revision| format!("{revision}:{id}"))
}

fn dependency_succeeded(run: &WorkflowRun, dependency: &str) -> bool {
    run.nodes
        .iter()
        .find(|node| node.id == dependency)
        .is_some_and(|node| node.status == LifecycleStatus::Done)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GroupOutcome {
    Dormant,
    Running,
    Succeeded,
    Failed,
}

fn group_outcome(run: &WorkflowRun, group: &ParallelGroup) -> GroupOutcome {
    let statuses = group
        .agents
        .iter()
        .filter_map(|name| run.nodes.iter().find(|node| node.id == *name))
        .map(|node| node.status)
        .collect::<Vec<_>>();
    if statuses
        .iter()
        .all(|status| *status == LifecycleStatus::Pending)
    {
        return GroupOutcome::Dormant;
    }
    let failed = statuses
        .iter()
        .filter(|status| **status == LifecycleStatus::Failed)
        .count();
    let terminal = statuses
        .iter()
        .all(|status| matches!(status, LifecycleStatus::Done | LifecycleStatus::Failed));
    match group.failure_mode {
        FailureMode::FailFast if failed > 0 => GroupOutcome::Failed,
        FailureMode::ContinueOnError if terminal => GroupOutcome::Succeeded,
        FailureMode::AllOrNothing if terminal && failed > 0 => GroupOutcome::Failed,
        FailureMode::AllOrNothing if terminal => GroupOutcome::Succeeded,
        FailureMode::FailFast if terminal => GroupOutcome::Succeeded,
        _ => GroupOutcome::Running,
    }
}

fn definition_dependency_succeeded(
    definition: &Definition,
    run: &WorkflowRun,
    dependency: &str,
) -> bool {
    definition
        .parallel
        .iter()
        .find(|group| group.name == dependency)
        .map_or_else(
            || dependency_succeeded(run, dependency),
            |group| group_outcome(run, group) == GroupOutcome::Succeeded,
        )
}

fn runtime_dependencies_done(definition: &Definition, run: &WorkflowRun, step: &Step) -> bool {
    effective_dependencies(definition, step)
        .iter()
        .all(|dependency| definition_dependency_succeeded(definition, run, dependency))
}

fn effective_dependencies(definition: &Definition, step: &Step) -> Vec<String> {
    let mut dependencies = step.depends_on.clone();
    dependencies.extend(
        definition
            .steps
            .iter()
            .filter(|candidate| candidate.review_by.as_deref() == Some(step.name.as_str()))
            .map(|candidate| candidate.name.clone()),
    );
    dependencies.sort();
    dependencies.dedup();
    dependencies
}

fn route_context(definition: &Definition, run: &WorkflowRun, source: &str) -> serde_json::Value {
    let mut context = serde_json::Map::new();
    context.insert(
        "workflow".into(),
        json!({
            "input": definition.input,
            "goal": definition.goal,
        }),
    );
    context.insert(
        "context".into(),
        json!({ "iteration": iteration_count(run) }),
    );
    for node in &run.nodes {
        context.insert(
            node.id.clone(),
            json!({
                "output": node.output,
                "status": node.status.to_string(),
            }),
        );
    }
    for group in &definition.parallel {
        let outputs = group
            .agents
            .iter()
            .filter_map(|name| {
                run.nodes
                    .iter()
                    .find(|node| node.id == *name && node.status == LifecycleStatus::Done)
                    .map(|node| (name.clone(), node.output.clone().unwrap_or_default()))
            })
            .collect::<serde_json::Map<_, _>>();
        let errors = group
            .agents
            .iter()
            .filter_map(|name| {
                run.nodes
                    .iter()
                    .find(|node| node.id == *name && node.status == LifecycleStatus::Failed)
                    .map(|node| {
                        (
                            name.clone(),
                            json!({
                                "status": "failed",
                                "message": node.activity.last().map(|event| &event.message),
                            }),
                        )
                    })
            })
            .collect::<serde_json::Map<_, _>>();
        context.insert(
            group.name.clone(),
            json!({ "outputs": outputs, "errors": errors }),
        );
    }
    if let Some(node) = run.nodes.iter().find(|node| node.id == source) {
        context.insert("output".into(), node.output.clone().unwrap_or_default());
        context.insert("status".into(), json!(node.status.to_string()));
    } else if let Some(group) = context.get(source).cloned() {
        context.insert("output".into(), group);
        context.insert("status".into(), json!("done"));
    }
    serde_json::Value::Object(context)
}

fn route_matches(route: &Route, context: &serde_json::Value) -> Result<bool> {
    let Some(condition) = route.when.as_deref() else {
        return Ok(true);
    };
    let template = route_template(condition);
    let mut environment = Environment::new();
    environment.add_template("route", &template)?;
    let rendered = environment.get_template("route")?.render(context)?;
    match rendered.trim().to_ascii_lowercase().as_str() {
        "true" | "1" => Ok(true),
        "false" | "0" | "" | "none" | "null" => Ok(false),
        value => bail!("route condition must render a boolean, got {value:?}"),
    }
}

fn selected_route<'a>(
    routes: &'a [Route],
    context: &serde_json::Value,
) -> Result<Option<&'a Route>> {
    for route in routes {
        if route_matches(route, context)? {
            return Ok(Some(route));
        }
    }
    Ok(None)
}

fn is_review_feedback(definition: &Definition, reviewer: &str, target: &str) -> bool {
    definition
        .steps
        .iter()
        .any(|step| step.name == target && step.review_by.as_deref() == Some(reviewer))
}

fn route_relationship(definition: &Definition, source: &Step, route: &Route) -> String {
    if is_review_feedback(definition, &source.name, &route.to) {
        "feedback".into()
    } else {
        route
            .when
            .as_ref()
            .map_or_else(|| "routes".into(), |when| format!("when {when}"))
    }
}

fn iteration_count(run: &WorkflowRun) -> u32 {
    run.nodes
        .iter()
        .map(|node| node.attempt)
        .fold(0, u32::saturating_add)
}

fn routed_marker(source: &str, generation: u32) -> String {
    format!("route:{source}:{generation}")
}

fn has_route_marker(run: &WorkflowRun, marker: &str) -> bool {
    run.nodes
        .iter()
        .any(|node| node.activity.iter().any(|event| event.kind == marker))
}

fn record_route(run: &mut WorkflowRun, source: &str, marker: &str, target: Option<&str>) {
    let index = run
        .nodes
        .iter()
        .position(|node| node.id == source)
        .or_else(|| (!run.nodes.is_empty()).then_some(0));
    if let Some(node) = index.and_then(|index| run.nodes.get_mut(index)) {
        node.record_activity(
            marker,
            target.map_or_else(
                || "no route matched".into(),
                |target| format!("routed to {target}"),
            ),
        );
    }
}

fn activate_target(run: &mut WorkflowRun, definition: &Definition, target: &str) {
    let names = definition
        .parallel
        .iter()
        .find(|group| group.name == target)
        .map(|group| group.agents.clone())
        .unwrap_or_else(|| vec![target.to_owned()]);
    for node in run.nodes.iter_mut().filter(|node| names.contains(&node.id)) {
        if !matches!(
            node.status,
            LifecycleStatus::Working | LifecycleStatus::Waiting | LifecycleStatus::Terminating
        ) {
            node.status = LifecycleStatus::Queued;
            node.retry_after = None;
            node.updated_at = Utc::now();
        }
    }
}

fn prepare_feedback_review(
    run: &mut WorkflowRun,
    definition: &Definition,
    reviewer: &str,
    target: &str,
) {
    if !is_review_feedback(definition, reviewer, target) {
        return;
    }
    if let Some(node) = run
        .nodes
        .iter_mut()
        .find(|node| node.id == reviewer && node.status == LifecycleStatus::Done)
    {
        node.status = LifecycleStatus::Pending;
        node.retry_after = None;
        node.updated_at = Utc::now();
        node.record_activity(
            "feedback",
            format!("requested another review after {target}"),
        );
    }
}

fn route_targets(definition: &Definition) -> BTreeSet<&str> {
    definition
        .steps
        .iter()
        .flat_map(|step| &step.routes)
        .chain(definition.parallel.iter().flat_map(|group| &group.routes))
        .filter_map(|route| {
            (!matches!(route.to.as_str(), "$end" | "self")).then_some(route.to.as_str())
        })
        .collect()
}

fn advance_state(run: &mut WorkflowRun, definition: &Definition) -> Result<bool> {
    let snapshot = run.clone();
    let mut decisions = Vec::new();
    for step in &definition.steps {
        let Some(node) = snapshot.nodes.iter().find(|node| node.id == step.name) else {
            continue;
        };
        if node.status != LifecycleStatus::Done || step.routes.is_empty() {
            continue;
        }
        let marker = routed_marker(&step.name, node.attempt);
        if has_route_marker(&snapshot, &marker) {
            continue;
        }
        let context = route_context(definition, &snapshot, &step.name);
        decisions.push((
            step.name.clone(),
            marker,
            selected_route(&step.routes, &context)?.map(|route| route.to.clone()),
        ));
    }
    for group in &definition.parallel {
        if group.routes.is_empty() || group_outcome(&snapshot, group) != GroupOutcome::Succeeded {
            continue;
        }
        let generation = group
            .agents
            .iter()
            .filter_map(|name| snapshot.nodes.iter().find(|node| node.id == *name))
            .map(|node| node.attempt)
            .fold(0, u32::saturating_add);
        let marker = routed_marker(&group.name, generation);
        if has_route_marker(&snapshot, &marker) {
            continue;
        }
        let context = route_context(definition, &snapshot, &group.name);
        decisions.push((
            group.name.clone(),
            marker,
            selected_route(&group.routes, &context)?.map(|route| route.to.clone()),
        ));
    }

    let mut ended = false;
    for (source, marker, target) in decisions {
        record_route(run, &source, &marker, target.as_deref());
        match target.as_deref() {
            Some("$end") => {
                ended = true;
                break;
            }
            Some("self") => activate_target(run, definition, &source),
            Some(target) => {
                activate_target(run, definition, target);
                prepare_feedback_review(run, definition, &source, target);
            }
            None => {}
        }
    }
    if ended {
        for node in &mut run.nodes {
            if matches!(
                node.status,
                LifecycleStatus::Pending | LifecycleStatus::Queued | LifecycleStatus::Waiting
            ) {
                node.status = LifecycleStatus::Skipped;
                node.retry_after = None;
                node.updated_at = Utc::now();
                node.record_activity("skipped", "workflow routed to $end");
            }
        }
        return Ok(true);
    }

    let routed_targets = route_targets(definition);
    let group_members = definition
        .parallel
        .iter()
        .flat_map(|group| group.agents.iter().map(String::as_str))
        .collect::<BTreeSet<_>>();
    loop {
        let activatable = definition
            .steps
            .iter()
            .filter(|step| {
                !effective_dependencies(definition, step).is_empty()
                    && !routed_targets.contains(step.name.as_str())
                    && !group_members.contains(step.name.as_str())
                    && run
                        .nodes
                        .iter()
                        .find(|node| node.id == step.name)
                        .is_some_and(|node| node.status == LifecycleStatus::Pending)
                    && runtime_dependencies_done(definition, run, step)
            })
            .map(|step| step.name.clone())
            .collect::<Vec<_>>();
        if activatable.is_empty() {
            break;
        }
        for target in activatable {
            activate_target(run, definition, &target);
        }
    }
    Ok(false)
}

fn fatal_failure(definition: &Definition, run: &WorkflowRun) -> bool {
    run.nodes.iter().any(|node| {
        if node.status != LifecycleStatus::Failed {
            return false;
        }
        definition
            .parallel
            .iter()
            .find(|group| group.agents.contains(&node.id))
            .is_none_or(|group| group_outcome(run, group) == GroupOutcome::Failed)
    })
}

fn workflow_complete(definition: &Definition, run: &WorkflowRun) -> bool {
    !fatal_failure(definition, run)
        && run.nodes.iter().all(|node| {
            matches!(
                node.status,
                LifecycleStatus::Done | LifecycleStatus::Failed | LifecycleStatus::Skipped
            )
        })
}

fn workflow_deadline(definition: &Definition, run: &WorkflowRun) -> Option<DateTime<Utc>> {
    definition.limits.timeout_seconds.and_then(|seconds| {
        chrono::Duration::try_seconds(i64::try_from(seconds).ok()?)
            .map(|limit| run.created_at + limit)
    })
}

fn limit_violation(definition: &Definition, run: &WorkflowRun) -> Option<String> {
    if workflow_deadline(definition, run).is_some_and(|deadline| Utc::now() >= deadline) {
        return Some(format!(
            "workflow timeout of {}s exceeded",
            definition.limits.timeout_seconds.unwrap_or_default()
        ));
    }
    if definition
        .limits
        .budget_usd
        .is_some_and(|budget| run.cost_usd > budget)
    {
        return Some(format!(
            "workflow budget of ${:.4} exceeded by ${:.4}",
            definition.limits.budget_usd.unwrap_or_default(),
            run.cost_usd
        ));
    }
    None
}

fn fail_run_with_reason(
    scope: &Path,
    run_id: &str,
    kind: &str,
    reason: &str,
) -> Result<WorkflowRun> {
    state::update(scope, |workspace| {
        let run = workspace
            .runs
            .iter_mut()
            .find(|run| run.id == run_id)
            .with_context(|| format!("unknown run: {run_id}"))?;
        if run.status == LifecycleStatus::Terminating {
            return Ok(run.clone());
        }
        for node in &mut run.nodes {
            if node.status.active() {
                node.status = LifecycleStatus::Skipped;
                node.retry_after = None;
                node.updated_at = Utc::now();
                node.record_activity(kind, reason);
            }
        }
        run.status = LifecycleStatus::Failed;
        run.current_node = None;
        run.process_id = None;
        run.execution_nonce = None;
        run.pending_gates.clear();
        run.updated_at = Utc::now();
        Ok(run.clone())
    })
}

fn ready_steps(definition: &Definition, run: &WorkflowRun) -> Vec<Step> {
    let mut group_counts = BTreeMap::<&str, usize>::new();
    definition
        .steps
        .iter()
        .filter(|step| {
            run.nodes
                .iter()
                .find(|node| node.id == step.name)
                .is_some_and(|node| {
                    matches!(
                        node.status,
                        LifecycleStatus::Queued | LifecycleStatus::Waiting
                    ) && node
                        .retry_after
                        .is_none_or(|retry_after| retry_after <= Utc::now())
                })
                && runtime_dependencies_done(definition, run, step)
        })
        .filter(|step| {
            let Some(group) = definition
                .parallel
                .iter()
                .find(|group| group.agents.contains(&step.name))
            else {
                return true;
            };
            let count = group_counts.entry(group.name.as_str()).or_default();
            if group.max_concurrent.is_some_and(|limit| *count >= limit) {
                return false;
            }
            *count += 1;
            true
        })
        .cloned()
        .collect()
}

fn parse_duration(value: Option<&str>) -> Result<Duration> {
    let value = value.context("wait step needs duration")?;
    let (number, scale) = if let Some(number) = value.strip_suffix("ms") {
        (number, 1)
    } else if let Some(number) = value.strip_suffix('s') {
        (number, 1_000)
    } else if let Some(number) = value.strip_suffix('m') {
        (number, 60_000)
    } else {
        bail!("duration must end in ms, s, or m");
    };
    let milliseconds = number
        .parse::<u64>()?
        .checked_mul(scale)
        .context("duration is too large")?;
    Ok(Duration::from_millis(milliseconds))
}

fn wait_for_duration(
    scope: &Path,
    run_id: &str,
    duration: Duration,
    workflow_deadline: Option<DateTime<Utc>>,
) -> Result<()> {
    let deadline = Instant::now()
        .checked_add(duration)
        .context("wait duration is too large")?;
    loop {
        let run = find_run(scope, run_id)?;
        if run.status == LifecycleStatus::Terminating {
            bail!("workflow wait cancelled");
        }
        if workflow_deadline.is_some_and(|deadline| Utc::now() >= deadline) {
            bail!("workflow timeout exceeded");
        }
        let now = Instant::now();
        if now >= deadline {
            return Ok(());
        }
        thread::sleep((deadline - now).min(Duration::from_millis(50)));
    }
}

struct StepOutcome {
    status: LifecycleStatus,
    output: serde_json::Value,
    session_id: Option<String>,
    summary: String,
}

fn run_cancelled(
    scope: &Path,
    run_id: &str,
    workflow_deadline: Option<DateTime<Utc>>,
) -> Result<bool> {
    Ok(
        find_run(scope, run_id)?.status == LifecycleStatus::Terminating
            || workflow_deadline.is_some_and(|deadline| Utc::now() >= deadline),
    )
}

const EXECUTION_LEASE_ENV: &str = "ORC_EXECUTION_LEASE";
const EXECUTION_RECOVERY_ENV: &str = "ORC_EXECUTION_RECOVERY";
const DISPLAY_DIRECTION_ENV: &str = "ORC_DISPLAY_DIRECTION";
const DEFAULT_DISPLAY_DIRECTION: &str = "right";

fn validate_display_direction(direction: &str) -> Result<()> {
    if matches!(direction, "right" | "left" | "top" | "bottom") {
        return Ok(());
    }
    bail!("display direction must be right, left, top, or bottom")
}

fn display_direction_path(scope: &Path, run_id: &str) -> PathBuf {
    crate::config::state_home()
        .join("orc/run-preferences")
        .join(state::scope_key(scope))
        .join(format!("{}.direction", state::scope_key(Path::new(run_id))))
}

fn write_display_direction(scope: &Path, run_id: &str, direction: &str) -> Result<()> {
    validate_display_direction(direction)?;
    let path = display_direction_path(scope, run_id);
    fs::create_dir_all(
        path.parent()
            .context("display direction has no directory")?,
    )?;
    fs::write(&path, direction)
        .with_context(|| format!("write display direction {}", path.display()))
}

fn read_display_direction(scope: &Path, run_id: &str) -> Result<String> {
    let path = display_direction_path(scope, run_id);
    let direction = match fs::read_to_string(&path) {
        Ok(direction) => direction,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DEFAULT_DISPLAY_DIRECTION.into());
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("read display direction {}", path.display()));
        }
    };
    validate_display_direction(&direction)?;
    Ok(direction)
}

fn execution_display_direction(scope: &Path, run_id: &str) -> Result<String> {
    match std::env::var(DISPLAY_DIRECTION_ENV) {
        Ok(direction) => {
            validate_display_direction(&direction)?;
            Ok(direction)
        }
        Err(std::env::VarError::NotPresent) => read_display_direction(scope, run_id),
        Err(error) => Err(error).context("read workflow display direction"),
    }
}

struct ExecutionLease {
    path: PathBuf,
    nonce: String,
    armed: bool,
    #[cfg(unix)]
    _identity: fs::File,
}

struct ExecutionLeaseGuard {
    #[cfg(unix)]
    file: fs::File,
}

impl ExecutionLeaseGuard {
    fn try_acquire(path: &Path) -> Result<Option<Self>> {
        let directory = path.parent().context("execution lease has no directory")?;
        fs::create_dir_all(directory).context("create execution lease directory")?;
        #[cfg(unix)]
        {
            let guard_path = path.with_file_name(format!(
                ".{}.guard",
                path.file_name().unwrap_or_default().to_string_lossy()
            ));
            let file = fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(guard_path)
                .context("open execution lease guard")?;
            unsafe extern "C" {
                fn flock(fd: i32, operation: i32) -> i32;
            }
            const LOCK_EX: i32 = 2;
            const LOCK_NB: i32 = 4;
            if unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } == 0 {
                return Ok(Some(Self { file }));
            }
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::WouldBlock {
                return Ok(None);
            }
            Err(error).context("acquire execution lease guard")
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            Ok(Some(Self {}))
        }
    }

    fn acquire(path: &Path, timeout: Duration) -> Result<Self> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .context("execution lease guard timeout is too large")?;
        loop {
            if let Some(guard) = Self::try_acquire(path)? {
                return Ok(guard);
            }
            if Instant::now() >= deadline {
                bail!("timed out acquiring the execution lease guard");
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for ExecutionLeaseGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            unsafe extern "C" {
                fn flock(fd: i32, operation: i32) -> i32;
            }
            const LOCK_UN: i32 = 8;
            let _ = unsafe { flock(self.file.as_raw_fd(), LOCK_UN) };
        }
    }
}

impl ExecutionLease {
    fn acquire(scope: &Path, run_id: &str) -> Result<Option<Self>> {
        Self::acquire_at(&execution_lease_path(scope, run_id))
    }

    fn acquire_at(path: &Path) -> Result<Option<Self>> {
        let directory = path.parent().context("execution lease has no directory")?;
        fs::create_dir_all(directory).context("create execution lease directory")?;
        let inherited = std::env::var(EXECUTION_LEASE_ENV).ok();

        let attempts = if inherited.is_some() { 200 } else { 3 };
        for _ in 0..attempts {
            let Some(_guard) = ExecutionLeaseGuard::try_acquire(path)? else {
                thread::sleep(Duration::from_millis(10));
                continue;
            };
            if let Some(record) = read_execution_lease(path)? {
                if inherited.as_deref() == Some(record.nonce.as_str()) {
                    let Some(identity) = try_acquire_execution_identity(path)? else {
                        drop(_guard);
                        thread::sleep(Duration::from_millis(10));
                        continue;
                    };
                    let lease = Self {
                        path: path.to_owned(),
                        nonce: record.nonce,
                        armed: true,
                        #[cfg(unix)]
                        _identity: identity,
                    };
                    lease.write_owner(std::process::id())?;
                    return Ok(Some(lease));
                }
                if execution_identity_active(path)? {
                    return Ok(None);
                }
                remove_execution_lease(path, &record.nonce)?;
                continue;
            }

            let Some(identity) = try_acquire_execution_identity(path)? else {
                return Ok(None);
            };
            let lease = Self {
                path: path.to_owned(),
                nonce: Uuid::new_v4().to_string(),
                armed: true,
                #[cfg(unix)]
                _identity: identity,
            };
            if lease.install(std::process::id())? {
                return Ok(Some(lease));
            }
        }
        Ok(None)
    }

    fn install(&self, process_id: u32) -> Result<bool> {
        let temporary = self
            .path
            .with_file_name(format!(".{}.lease", Uuid::new_v4()));
        let result = (|| {
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)
                .context("create execution lease candidate")?;
            writeln!(file, "{process_id} {}", self.nonce).context("write execution lease")?;
            file.sync_all().context("flush execution lease")?;
            match fs::hard_link(&temporary, &self.path) {
                Ok(()) => Ok(true),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
                Err(error) => Err(error).context("install execution lease"),
            }
        })();
        let _ = fs::remove_file(temporary);
        result
    }

    fn write_owner(&self, process_id: u32) -> Result<()> {
        let current = read_execution_lease(&self.path)?
            .context("execution lease disappeared during ownership transfer")?;
        if current.nonce != self.nonce {
            bail!("execution lease changed during ownership transfer");
        }
        let temporary = self
            .path
            .with_file_name(format!(".{}.lease", Uuid::new_v4()));
        fs::write(&temporary, format!("{process_id} {}\n", self.nonce))
            .context("write transferred execution lease")?;
        fs::rename(&temporary, &self.path).context("transfer execution lease ownership")
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

fn execution_identity_path(path: &Path) -> PathBuf {
    path.with_extension("identity")
}

fn try_acquire_execution_identity(path: &Path) -> Result<Option<fs::File>> {
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(execution_identity_path(path))
        .context("open execution identity lock")?;
    #[cfg(unix)]
    {
        unsafe extern "C" {
            fn flock(fd: i32, operation: i32) -> i32;
        }
        const LOCK_EX: i32 = 2;
        const LOCK_NB: i32 = 4;
        if unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } == 0 {
            return Ok(Some(file));
        }
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::WouldBlock {
            return Ok(None);
        }
        Err(error).context("acquire execution identity lock")
    }
    #[cfg(not(unix))]
    Ok(Some(file))
}

fn execution_identity_active(path: &Path) -> Result<bool> {
    Ok(try_acquire_execution_identity(path)?.is_none())
}

impl Drop for ExecutionLease {
    fn drop(&mut self) {
        if self.armed {
            let _ = remove_execution_lease(&self.path, &self.nonce);
        }
    }
}

struct ExecutionLeaseRecord {
    process_id: u32,
    nonce: String,
}

fn execution_lease_path(scope: &Path, run_id: &str) -> PathBuf {
    crate::config::state_home()
        .join("orc/leases")
        .join(state::scope_key(scope))
        .join(format!("{}.lease", state::scope_key(Path::new(run_id))))
}

fn active_process_directory(scope: &Path, run_id: &str) -> PathBuf {
    crate::config::state_home()
        .join("orc/processes")
        .join(state::scope_key(scope))
        .join(state::scope_key(Path::new(run_id)))
}

fn clear_process_records(directory: &Path) -> Result<()> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("read command tracker directory"),
    };
    for entry in entries {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) == Some("process") {
            if tracked_process_active(&path)? {
                bail!(
                    "tracked command is still active for {}; cancel the run before resuming",
                    directory.display()
                );
            }
            fs::remove_file(path).context("remove stale command tracker")?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn tracked_process_active(path: &Path) -> Result<bool> {
    let file = match fs::OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error).with_context(|| format!("open command tracker {}", path.display()));
        }
    };
    unsafe extern "C" {
        fn flock(fd: i32, operation: i32) -> i32;
    }
    const LOCK_EX: i32 = 2;
    const LOCK_NB: i32 = 4;
    if unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } == 0 {
        return Ok(false);
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::WouldBlock {
        return Ok(true);
    }
    Err(error).with_context(|| format!("lock command tracker {}", path.display()))
}

#[cfg(not(unix))]
fn tracked_process_active(_path: &Path) -> Result<bool> {
    Ok(false)
}

fn wait_for_tracker_release(path: &Path, timeout: Duration) -> Result<()> {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if !tracked_process_active(path)? {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(25));
    }
    if tracked_process_active(path)? {
        bail!("tracked command monitor did not exit: {}", path.display());
    }
    Ok(())
}

fn read_execution_lease(path: &Path) -> Result<Option<ExecutionLeaseRecord>> {
    let source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("read execution lease"),
    };
    let mut fields = source.split_whitespace();
    let process_id = fields.next().and_then(|value| value.parse().ok());
    let nonce = fields.next().map(str::to_owned);
    match (process_id, nonce, fields.next()) {
        (Some(process_id), Some(nonce), None) => {
            Ok(Some(ExecutionLeaseRecord { process_id, nonce }))
        }
        _ => bail!("invalid execution lease: {}", path.display()),
    }
}

fn remove_execution_lease(path: &Path, nonce: &str) -> Result<()> {
    let Some(current) = read_execution_lease(path)? else {
        return Ok(());
    };
    if current.nonce != nonce {
        return Ok(());
    }
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("remove execution lease"),
    }
}

fn process_is_live(process_id: u32) -> bool {
    if process_id == std::process::id() {
        return true;
    }
    signal_process(process_id, 0).is_ok()
}

struct StepExecution<'a> {
    config: &'a Config,
    providers: &'a [provider::Manifest],
    scope: &'a Path,
    run: &'a WorkflowRun,
    definition_path: &'a Path,
    definition: &'a Definition,
    tracker_directory: &'a Path,
    workflow_deadline: Option<DateTime<Utc>>,
    display_direction: &'a str,
}

struct AgentLaunchRequest<'a> {
    scope: &'a Path,
    session: &'a Session,
    harness: &'a str,
    prompt: &'a str,
    native_id: &'a str,
    run: &'a WorkflowRun,
    step: &'a Step,
    direction: &'a str,
}

fn agent_launch_request(request: AgentLaunchRequest<'_>) -> serde_json::Value {
    json!({
        "version": "orc.provider/v1",
        "action": "launch",
        "scope": request.scope,
        "direction": request.direction,
        "session": request.session,
        "command": [request.harness, request.prompt],
        "prompt": request.prompt,
        "environment": {
            "ORC_SCOPE": request.scope,
            "ORC_SESSION_ID": request.session.id,
            "ORC_NATIVE_SESSION_ID": request.native_id,
            "ORC_PARENT_SESSION_ID": request.run.orchestrator_id,
            "ORC_RUN_ID": request.run.id,
            "ORC_NODE_ID": request.step.name,
        },
        "providers": {},
    })
}

fn execute_step(context: &StepExecution<'_>, step: &Step) -> Result<StepOutcome> {
    let config = context.config;
    let providers = context.providers;
    let scope = context.scope;
    let run = context.run;
    let definition_path = context.definition_path;
    let definition = context.definition;
    let tracker_directory = context.tracker_directory;
    match step.r#type {
        StepKind::Agent => {
            let harness = step
                .runtime
                .harness
                .clone()
                .or_else(|| definition.defaults.runtime.harness.clone())
                .context("agent has no harness")?;
            let model = step
                .runtime
                .model
                .clone()
                .or_else(|| definition.defaults.runtime.model.clone());
            let prompt = step.prompt.clone().unwrap_or_else(|| {
                format!(
                    r#"Goal: {}
Expected output: {}
Success criteria:
{}"#,
                    step.goal,
                    step.expected_output,
                    step.success_criteria
                        .iter()
                        .map(|criterion| format!("- {criterion}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            });
            let native_id = Uuid::new_v4().to_string();
            let session = control::register_managed(
                config,
                scope,
                Contract {
                    harness: harness.clone(),
                    model,
                    role: step.role,
                    title: step.name.clone(),
                    purpose: step.purpose.clone(),
                    goal: step.goal.clone(),
                    expected_output: step.expected_output.clone(),
                    success_criteria: step.success_criteria.clone(),
                    completion: step.completion,
                    review_by: step.review_by.clone(),
                },
                SessionLink {
                    native_id: Some(native_id.clone()),
                    parent_id: run.orchestrator_id.clone(),
                    run_id: Some(run.id.clone()),
                    node_id: Some(step.name.clone()),
                    runtime_timeout_seconds: step.timeout_seconds,
                    idle_timeout_seconds: step.idle_timeout_seconds,
                    source: RegistrationSource::Managed,
                    ..SessionLink::default()
                },
            )?;
            assign_node_session(scope, &run.id, &step.name, &session.id)?;
            let mut request = agent_launch_request(AgentLaunchRequest {
                scope,
                session: &session,
                harness: &harness,
                prompt: &prompt,
                native_id: &native_id,
                run,
                step,
                direction: context.display_direction,
            });
            if let Some(execution) = step.runtime.execution.as_ref().or(definition
                .defaults
                .runtime
                .execution
                .as_ref())
            {
                request["providers"][provider::Capability::ExecutionRun.to_string()] =
                    serde_json::Value::String(execution.clone());
            }
            let launched: Result<_> = (|| {
                daemon::ensure_running(config)?;
                let cancelled = || run_cancelled(scope, &run.id, context.workflow_deadline);
                let plan = provider::resolve_plan_tracked(
                    config,
                    providers,
                    Action::Launch,
                    request,
                    tracker_directory,
                    &cancelled,
                )?;
                let result = provider::run_plan_tracked_cancellable(
                    &plan,
                    scope,
                    Some(tracker_directory),
                    Some(&cancelled),
                )?;
                Ok((plan, result))
            })();
            let (plan, result) = match launched {
                Ok(value) => value,
                Err(error) => {
                    if let Err(cleanup_error) =
                        control::terminate(config, scope, &session.id, "managed launch failed")
                    {
                        control::update_session(scope, &session.id, LifecycleStatus::Failed)?;
                        return Err(error.context(format!(
                            "managed session cleanup failed: {cleanup_error:#}"
                        )));
                    }
                    return Err(error);
                }
            };
            if !plan.accepts(result.code) {
                if let Err(cleanup_error) =
                    control::terminate(config, scope, &session.id, "managed launch failed")
                {
                    control::update_session(scope, &session.id, LifecycleStatus::Failed)?;
                    bail!(
                        "agent exited with {}: {}; managed session cleanup failed: {cleanup_error:#}",
                        result.code,
                        result.stderr.trim()
                    );
                }
                bail!(
                    "agent exited with {}: {}",
                    result.code,
                    result.stderr.trim()
                );
            }
            control::update_session(scope, &session.id, LifecycleStatus::Done)?;
            Ok(StepOutcome {
                status: LifecycleStatus::Done,
                output: json!({ "stdout": result.stdout, "stderr": result.stderr }),
                session_id: Some(session.id),
                summary: "agent completed".into(),
            })
        }
        StepKind::Script => {
            let initial = CommandPlan {
                version: "orc.provider/v1".into(),
                command: step.command.clone(),
                cwd: Some(scope.display().to_string()),
                environment: BTreeMap::new(),
                success_codes: vec![0],
            };
            let mut request = json!({
                "version": "orc.provider/v1",
                "action": "execute",
                "scope": scope,
                "step": step.name,
                "providers": {},
            });
            if let Some(execution) = step.runtime.execution.as_ref().or(definition
                .defaults
                .runtime
                .execution
                .as_ref())
            {
                request["providers"][provider::Capability::ExecutionRun.to_string()] =
                    serde_json::Value::String(execution.clone());
            }
            let cancelled = || run_cancelled(scope, &run.id, context.workflow_deadline);
            let plan = provider::resolve_plan_from_tracked(
                config,
                providers,
                Action::Execute,
                request,
                Some(initial),
                Some(tracker_directory),
                Some(&cancelled),
            )?;
            let result = provider::run_plan_tracked_cancellable(
                &plan,
                scope,
                Some(tracker_directory),
                Some(&cancelled),
            )?;
            if !plan.accepts(result.code) {
                bail!(
                    "command exited with {}: {}",
                    result.code,
                    result.stderr.trim()
                );
            }
            Ok(StepOutcome {
                status: LifecycleStatus::Done,
                output: json!({ "stdout": result.stdout, "stderr": result.stderr }),
                session_id: None,
                summary: "command completed".into(),
            })
        }
        StepKind::Set => Ok(StepOutcome {
            status: LifecycleStatus::Done,
            output: step.value.clone().unwrap_or(serde_json::Value::Null),
            session_id: None,
            summary: "value recorded".into(),
        }),
        StepKind::Wait => {
            wait_for_duration(
                scope,
                &run.id,
                parse_duration(step.duration.as_deref())?,
                context.workflow_deadline,
            )?;
            Ok(StepOutcome {
                status: LifecycleStatus::Done,
                output: json!({ "waited": step.duration }),
                session_id: None,
                summary: "wait completed".into(),
            })
        }
        StepKind::Workflow => {
            let reference = step
                .workflow
                .as_deref()
                .context("workflow reference is missing")?;
            let child_path = if Path::new(reference).is_absolute() {
                PathBuf::from(reference)
            } else {
                definition_path
                    .parent()
                    .unwrap_or(Path::new("."))
                    .join(reference)
            };
            let depth = run_depth(&state::read(scope)?, &run.id)?;
            let depth_limit = step.max_depth.unwrap_or(config.workflows.max_depth);
            if depth >= depth_limit {
                bail!(
                    "sub-workflow depth {} exceeds the configured limit {depth_limit}",
                    depth + 1
                );
            }
            let child = if let Some(child_run_id) = run
                .nodes
                .iter()
                .find(|node| node.id == step.name)
                .and_then(|node| node.child_run_id.as_deref())
            {
                find_run(scope, child_run_id)?
            } else {
                materialize_with_parent(
                    config,
                    scope,
                    &child_path,
                    RunMode::Foreground,
                    Some(&run.id),
                    Some(&step.name),
                )?
            };
            let (mut child, mut executor) =
                spawn_executor_with_direction(scope, &child.id, context.display_direction)?;
            let expected_process = child.process_id;
            let expected_nonce = child.execution_nonce.clone();
            while matches!(
                child.status,
                LifecycleStatus::Queued | LifecycleStatus::Working
            ) {
                if context
                    .workflow_deadline
                    .is_some_and(|deadline| Utc::now() >= deadline)
                {
                    cancel(config, scope, &child.id)?;
                    bail!("workflow timeout exceeded");
                }
                if let Some(executor) = executor.as_mut()
                    && let Some(status) = executor.try_wait()?
                {
                    child = reconcile_executor_exit(
                        config,
                        scope,
                        &child.id,
                        expected_process,
                        expected_nonce.as_deref(),
                        status,
                    )?;
                    if !matches!(
                        child.status,
                        LifecycleStatus::Queued | LifecycleStatus::Working
                    ) {
                        break;
                    }
                }
                thread::sleep(Duration::from_millis(50));
                child = find_run(scope, &child.id)?;
            }
            if let Some(executor) = executor.as_mut() {
                let _ = executor.wait();
            }
            if child.status == LifecycleStatus::Waiting {
                return Ok(StepOutcome {
                    status: LifecycleStatus::Waiting,
                    output: serde_json::to_value(&child)?,
                    session_id: None,
                    summary: format!("sub-workflow {} is waiting", child.name),
                });
            }
            if child.status != LifecycleStatus::Done {
                bail!("sub-workflow {} stopped as {}", child.name, child.status);
            }
            Ok(StepOutcome {
                status: LifecycleStatus::Done,
                output: serde_json::to_value(&child)?,
                session_id: None,
                summary: format!("sub-workflow {} completed", child.name),
            })
        }
        StepKind::HumanGate => Ok(StepOutcome {
            status: LifecycleStatus::Done,
            output: json!({ "approved": true }),
            session_id: None,
            summary: "human gate approved".into(),
        }),
        StepKind::Terminate => Ok(StepOutcome {
            status: LifecycleStatus::Done,
            output: json!({ "terminated": true }),
            session_id: None,
            summary: "workflow termination step completed".into(),
        }),
    }
}

fn assign_node_session(scope: &Path, run_id: &str, node_id: &str, session_id: &str) -> Result<()> {
    state::update(scope, |workspace| {
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
        node.session_id = Some(session_id.to_owned());
        node.updated_at = Utc::now();
        run.updated_at = Utc::now();
        Ok(())
    })
}

#[cfg(test)]
pub fn execute(config: &Config, scope: &Path, run_id: &str) -> Result<WorkflowRun> {
    execute_owned(config, scope, run_id)
}

#[cfg(not(test))]
pub fn execute(config: &Config, scope: &Path, run_id: &str) -> Result<WorkflowRun> {
    if std::env::var_os(EXECUTION_LEASE_ENV).is_some() {
        return execute_owned(config, scope, run_id);
    }
    daemon::ensure_running(config)?;
    let scope = state::resolve_scope(scope)?;
    let (mut run, mut executor) = spawn_executor(&scope, run_id)?;
    let expected_process = run.process_id;
    let expected_nonce = run.execution_nonce.clone();
    while matches!(
        run.status,
        LifecycleStatus::Queued | LifecycleStatus::Working
    ) {
        if let Some(child) = executor.as_mut()
            && let Some(status) = child.try_wait()?
        {
            run = reconcile_executor_exit(
                config,
                &scope,
                run_id,
                expected_process,
                expected_nonce.as_deref(),
                status,
            )?;
            if !matches!(
                run.status,
                LifecycleStatus::Queued | LifecycleStatus::Working
            ) {
                break;
            }
        }
        thread::sleep(Duration::from_millis(50));
        run = find_run(&scope, run_id)?;
    }
    if let Some(child) = executor.as_mut() {
        let _ = child.wait();
    }
    Ok(run)
}

fn execute_owned(config: &Config, scope: &Path, run_id: &str) -> Result<WorkflowRun> {
    let scope = state::resolve_scope(scope)?;
    let display_direction = execution_display_direction(&scope, run_id)?;
    let initial = find_run(&scope, run_id)?;
    if !initial.status.active() || initial.status == LifecycleStatus::Terminating {
        return Ok(initial);
    }
    let Some(_lease) = ExecutionLease::acquire(&scope, run_id)? else {
        return find_run(&scope, run_id);
    };
    isolate_executor_process()?;
    let recovering = std::env::var_os(EXECUTION_RECOVERY_ENV).is_some();
    let tracker_directory = active_process_directory(&scope, run_id);
    if recovering {
        terminate_tracked_processes(&tracker_directory)?;
    } else {
        let _tracker_guard =
            provider::ProcessTrackerGuard::acquire(&tracker_directory, Duration::from_secs(5))?;
        clear_process_records(&tracker_directory)?;
    }
    state::update(&scope, |workspace| {
        let run = workspace
            .runs
            .iter_mut()
            .find(|run| run.id == run_id)
            .with_context(|| format!("unknown run: {run_id}"))?;
        if !run.status.active() || run.status == LifecycleStatus::Terminating {
            return Ok(());
        }
        if recovering {
            block_interrupted_nodes(run);
        }
        run.process_id = Some(std::process::id());
        run.execution_nonce = Some(_lease.nonce.clone());
        run.resume_requested = false;
        run.status = LifecycleStatus::Working;
        run.updated_at = Utc::now();
        Ok(())
    })?;
    let providers = provider::discover(config)?;
    loop {
        let snapshot = state::read(&scope)?;
        let run = snapshot
            .runs
            .iter()
            .find(|run| run.id == run_id)
            .cloned()
            .with_context(|| format!("unknown run: {run_id}"))?;
        if run.status == LifecycleStatus::Terminating {
            return Ok(run);
        }
        if !run.status.active() {
            return Ok(run);
        }
        let definition_path = PathBuf::from(
            run.definition
                .as_deref()
                .context("run has no workflow definition")?,
        );
        let definition = load(&definition_path)?;
        let revision = definition_revision(&definition)?;
        if run.revision.as_deref() != Some(revision.as_str()) {
            bail!(
                "workflow run definition changed outside Orc; restore revision {} or create a new run",
                run.revision.as_deref().unwrap_or("unknown")
            );
        }
        if let Some(reason) = limit_violation(&definition, &run) {
            let failed = fail_run_with_reason(&scope, run_id, "limit", &reason)?;
            wake_parent(config, &scope, &failed)?;
            return Ok(failed);
        }
        let advanced = state::update(&scope, |workspace| {
            let run = workspace
                .runs
                .iter_mut()
                .find(|run| run.id == run_id)
                .with_context(|| format!("unknown run: {run_id}"))?;
            if run.revision.as_deref() != Some(revision.as_str()) {
                return Ok(None);
            }
            let changed = advance_state(run, &definition)?;
            run.updated_at = Utc::now();
            Ok(Some(changed))
        })?;
        if advanced.is_none() {
            continue;
        }
        let run = find_run(&scope, run_id)?;
        if workflow_complete(&definition, &run) {
            let completed = finish_run(&scope, run_id, LifecycleStatus::Done)?;
            wake_parent(config, &scope, &completed)?;
            return Ok(completed);
        }
        if fatal_failure(&definition, &run) {
            let failed = fail_run_with_reason(
                &scope,
                run_id,
                "aborted",
                "workflow stopped after a stage failure",
            )?;
            wake_parent(config, &scope, &failed)?;
            return Ok(failed);
        }
        let iterations = iteration_count(&run);
        if iterations >= definition.limits.max_iterations {
            let reason = format!(
                "workflow iteration limit of {} reached",
                definition.limits.max_iterations
            );
            let failed = fail_run_with_reason(&scope, run_id, "limit", &reason)?;
            wake_parent(config, &scope, &failed)?;
            return Ok(failed);
        }
        if !run.pending_gates.is_empty() {
            return finish_run(&scope, run_id, LifecycleStatus::Waiting);
        }
        if run.nodes.iter().any(|node| {
            node.status == LifecycleStatus::Waiting
                && node.child_run_id.as_ref().is_some_and(|child_id| {
                    snapshot.runs.iter().any(|child| {
                        child.id == *child_id
                            && matches!(
                                child.status,
                                LifecycleStatus::Queued
                                    | LifecycleStatus::Working
                                    | LifecycleStatus::Waiting
                                    | LifecycleStatus::Blocked
                            )
                    })
                })
        }) {
            return finish_run(&scope, run_id, LifecycleStatus::Waiting);
        }
        let remaining = definition.limits.max_iterations.saturating_sub(iterations) as usize;
        let mut ready = ready_steps(&definition, &run);
        ready.truncate(remaining);
        if ready.is_empty() {
            if let Some(retry_after) = run
                .nodes
                .iter()
                .filter(|node| node.status == LifecycleStatus::Queued)
                .filter_map(|node| node.retry_after)
                .min()
                && retry_after > Utc::now()
            {
                let duration = (retry_after - Utc::now()).to_std().unwrap_or_default();
                wait_for_duration(
                    &scope,
                    run_id,
                    duration,
                    workflow_deadline(&definition, &run),
                )?;
                continue;
            }
            let settled = state::update(&scope, |workspace| {
                let run = workspace
                    .runs
                    .iter_mut()
                    .find(|run| run.id == run_id)
                    .with_context(|| format!("unknown run: {run_id}"))?;
                if run.revision.as_deref() != Some(revision.as_str()) {
                    return Ok(None);
                }
                for node in run
                    .nodes
                    .iter_mut()
                    .filter(|node| node.status == LifecycleStatus::Pending)
                {
                    node.status = LifecycleStatus::Skipped;
                    node.updated_at = Utc::now();
                    node.record_activity("skipped", "no active route reaches this stage");
                }
                Ok(Some(run.clone()))
            })?;
            let Some(settled) = settled else {
                continue;
            };
            if workflow_complete(&definition, &settled) {
                let completed = finish_run(&scope, run_id, LifecycleStatus::Done)?;
                wake_parent(config, &scope, &completed)?;
                return Ok(completed);
            }
            let blocked = finish_run(&scope, run_id, LifecycleStatus::Blocked)?;
            wake_parent(config, &scope, &blocked)?;
            return Ok(blocked);
        }
        if let Some(gate) = ready
            .iter()
            .find_map(|step| required_gate(&definition, step, &run))
        {
            let before = gate.before.clone();
            let gate_recorded = state::update(&scope, |workspace| {
                let run = workspace
                    .runs
                    .iter_mut()
                    .find(|run| run.id == run_id)
                    .context("run disappeared")?;
                if run.status == LifecycleStatus::Terminating {
                    return Ok(false);
                }
                if run.revision.as_deref() != Some(revision.as_str()) {
                    return Ok(false);
                }
                run.pending_gates.push(gate.clone());
                run.status = LifecycleStatus::Waiting;
                run.process_id = None;
                run.execution_nonce = None;
                run.current_node = Some(before.clone());
                if let Some(node) = run.nodes.iter_mut().find(|node| node.id == before) {
                    node.status = LifecycleStatus::Waiting;
                    node.updated_at = Utc::now();
                    node.record_activity("gate", gate.reason.clone());
                }
                Ok(true)
            })?;
            if !gate_recorded {
                continue;
            }
            return state::read(&scope)?
                .runs
                .into_iter()
                .find(|run| run.id == run_id)
                .context("run disappeared");
        }
        let names = ready
            .iter()
            .map(|step| step.name.clone())
            .collect::<Vec<_>>();
        let expected_attempts = run
            .nodes
            .iter()
            .filter(|node| names.contains(&node.id))
            .map(|node| (node.id.clone(), node.attempt + 1))
            .collect::<BTreeMap<_, _>>();
        let claimed = state::update(&scope, |workspace| {
            let run = workspace
                .runs
                .iter_mut()
                .find(|run| run.id == run_id)
                .context("run disappeared")?;
            if !run.status.active() || run.status == LifecycleStatus::Terminating {
                return Ok(false);
            }
            if run.revision.as_deref() != Some(revision.as_str())
                || names.iter().any(|name| {
                    run.nodes
                        .iter()
                        .find(|node| node.id == *name)
                        .is_none_or(|node| {
                            node.status != LifecycleStatus::Queued
                                || expected_attempts.get(name) != Some(&(node.attempt + 1))
                        })
                })
            {
                return Ok(false);
            }
            run.status = LifecycleStatus::Working;
            run.current_node = names.first().cloned();
            for node in run.nodes.iter_mut().filter(|node| names.contains(&node.id)) {
                node.status = LifecycleStatus::Working;
                node.attempt += 1;
                node.retry_after = None;
                node.updated_at = Utc::now();
                node.record_activity("started", format!("attempt {} started", node.attempt));
            }
            Ok(true)
        })?;
        if !claimed {
            continue;
        }
        let execution = StepExecution {
            config,
            providers: &providers,
            scope: &scope,
            run: &run,
            definition_path: &definition_path,
            definition: &definition,
            tracker_directory: &tracker_directory,
            workflow_deadline: workflow_deadline(&definition, &run),
            display_direction: &display_direction,
        };
        let wave_done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let results = thread::scope(|thread_scope| {
            let deadline = execution.workflow_deadline;
            let budget = definition.limits.budget_usd;
            if deadline.is_some() || budget.is_some() {
                let wave_done = std::sync::Arc::clone(&wave_done);
                let monitor_scope = execution.scope;
                let monitor_run_id = execution.run.id.as_str();
                let tracker_directory = execution.tracker_directory;
                thread_scope.spawn(move || {
                    while !wave_done.load(std::sync::atomic::Ordering::SeqCst) {
                        let timed_out = deadline.is_some_and(|deadline| Utc::now() >= deadline);
                        let over_budget = budget.is_some_and(|limit| {
                            find_run(monitor_scope, monitor_run_id)
                                .is_ok_and(|run| run.cost_usd > limit)
                        });
                        if timed_out || over_budget {
                            let _ = terminate_tracked_processes(tracker_directory);
                            break;
                        }
                        thread::sleep(Duration::from_millis(25));
                    }
                });
            }
            let handles = ready
                .iter()
                .map(|step| {
                    let name = step.name.clone();
                    thread_scope.spawn(|| (name, execute_step(&execution, step)))
                })
                .collect::<Vec<_>>();
            let results = handles
                .into_iter()
                .map(|handle| handle.join().expect("workflow worker panicked"))
                .collect::<Vec<_>>();
            wave_done.store(true, std::sync::atomic::Ordering::SeqCst);
            results
        });
        state::update(&scope, |workspace| {
            let run = workspace
                .runs
                .iter_mut()
                .find(|run| run.id == run_id)
                .context("run disappeared")?;
            if !run.status.active() || run.status == LifecycleStatus::Terminating {
                return Ok(());
            }
            for (name, result) in &results {
                let step = definition
                    .steps
                    .iter()
                    .find(|step| step.name == *name)
                    .context("workflow step disappeared")?;
                let node = run
                    .nodes
                    .iter_mut()
                    .find(|node| node.id == *name)
                    .context("workflow node disappeared")?;
                if node.status != LifecycleStatus::Working
                    || expected_attempts.get(name) != Some(&node.attempt)
                {
                    continue;
                }
                match result {
                    Ok(outcome) => {
                        node.status = outcome.status;
                        node.retry_after = None;
                        node.output = Some(outcome.output.clone());
                        node.session_id.clone_from(&outcome.session_id);
                        node.record_activity(
                            if outcome.status == LifecycleStatus::Waiting {
                                "waiting"
                            } else {
                                "completed"
                            },
                            outcome.summary.clone(),
                        );
                    }
                    Err(error) if node.attempt <= step.retry.attempts => {
                        node.status = LifecycleStatus::Queued;
                        node.retry_after = Some(
                            Utc::now()
                                + chrono::Duration::from_std(Duration::from_secs(
                                    step.retry.backoff_seconds,
                                ))
                                .context("retry backoff is too large")?,
                        );
                        if matches!(step.r#type, StepKind::Workflow) {
                            node.child_run_id = None;
                        }
                        node.record_activity("retry", format!("{error:#}"));
                    }
                    Err(error) => {
                        node.status = LifecycleStatus::Failed;
                        node.retry_after = None;
                        node.record_activity("failed", format!("{error:#}"));
                    }
                }
                node.updated_at = Utc::now();
            }
            run.updated_at = Utc::now();
            Ok(())
        })?;
    }
}

fn block_interrupted_nodes(run: &mut WorkflowRun) {
    for node in run
        .nodes
        .iter_mut()
        .filter(|node| node.status == LifecycleStatus::Working)
    {
        node.status = LifecycleStatus::Blocked;
        node.updated_at = Utc::now();
        node.record_activity(
            "recovered",
            "executor stopped before it committed the outcome; inspect before retrying",
        );
    }
    run.current_node = None;
}

#[cfg(all(unix, not(test)))]
fn isolate_executor_process() -> Result<()> {
    unsafe extern "C" {
        fn getpgid(pid: i32) -> i32;
        fn setpgid(pid: i32, pgid: i32) -> i32;
    }
    let process_id = std::process::id() as i32;
    if unsafe { getpgid(0) } == process_id {
        return Ok(());
    }
    if unsafe { setpgid(0, 0) } != 0 {
        return Err(std::io::Error::last_os_error())
            .context("isolate workflow executor process group");
    }
    Ok(())
}

#[cfg(any(not(unix), test))]
fn isolate_executor_process() -> Result<()> {
    Ok(())
}

fn find_run(scope: &Path, run_id: &str) -> Result<WorkflowRun> {
    state::read(scope)?
        .runs
        .into_iter()
        .find(|run| run.id == run_id)
        .with_context(|| format!("unknown run: {run_id}"))
}

fn reconcile_executor_exit(
    config: &Config,
    scope: &Path,
    run_id: &str,
    expected_process: Option<u32>,
    expected_nonce: Option<&str>,
    status: std::process::ExitStatus,
) -> Result<WorkflowRun> {
    let (run, claimed) = state::update(scope, |workspace| {
        let run = workspace
            .runs
            .iter_mut()
            .find(|run| run.id == run_id)
            .with_context(|| format!("unknown run: {run_id}"))?;
        if run.process_id != expected_process
            || run.execution_nonce.as_deref() != expected_nonce
            || !matches!(
                run.status,
                LifecycleStatus::Queued | LifecycleStatus::Working
            )
        {
            return Ok((run.clone(), false));
        }
        let message = format!("workflow executor exited before committing state: {status}");
        if let Some(node) = run
            .current_node
            .as_ref()
            .and_then(|id| run.nodes.iter_mut().find(|node| &node.id == id))
        {
            node.status = LifecycleStatus::Failed;
            node.updated_at = Utc::now();
            node.record_activity("failed", message);
        }
        run.status = LifecycleStatus::Failed;
        run.process_id = None;
        run.execution_nonce = None;
        run.updated_at = Utc::now();
        Ok((run.clone(), true))
    })?;
    if claimed {
        let directory = active_process_directory(scope, run_id);
        terminate_tracked_processes(&directory)?;
        wake_parent(config, scope, &run)?;
    }
    Ok(run)
}

fn finish_run(scope: &Path, run_id: &str, status: LifecycleStatus) -> Result<WorkflowRun> {
    state::update(scope, |workspace| {
        let run = workspace
            .runs
            .iter_mut()
            .find(|run| run.id == run_id)
            .with_context(|| format!("unknown run: {run_id}"))?;
        if run.status == LifecycleStatus::Terminating {
            return Ok(run.clone());
        }
        if !run.status.active() {
            bail!("run cannot finish while {}", run.status);
        }
        run.status = status;
        run.current_node = None;
        run.process_id = None;
        run.execution_nonce = None;
        if !status.active() {
            run.resume_requested = false;
        }
        run.updated_at = Utc::now();
        Ok(run.clone())
    })
}

fn wake_parent(config: &Config, scope: &Path, child: &WorkflowRun) -> Result<()> {
    let Some(parent_run_id) = child.parent_run_id.as_deref() else {
        return Ok(());
    };
    state::update(scope, |workspace| {
        let parent = workspace
            .runs
            .iter_mut()
            .find(|run| run.id == parent_run_id)
            .with_context(|| format!("unknown parent run: {parent_run_id}"))?;
        if parent.status.active() && parent.status != LifecycleStatus::Terminating {
            parent.resume_requested = true;
            parent.updated_at = Utc::now();
        }
        Ok(())
    })?;
    spawn(config, scope, parent_run_id)?;
    Ok(())
}

pub fn approve(
    config: &Config,
    scope: &Path,
    run_id: &str,
    gate_id: Option<&str>,
    resume: bool,
) -> Result<WorkflowRun> {
    approve_as(config, scope, run_id, gate_id, resume, ApprovalActor::User)
}

pub fn approve_as(
    config: &Config,
    scope: &Path,
    run_id: &str,
    gate_id: Option<&str>,
    resume: bool,
    actor: ApprovalActor,
) -> Result<WorkflowRun> {
    let scope = state::resolve_scope(scope)?;
    let initial = find_run(&scope, run_id)?;
    if initial.status == LifecycleStatus::Terminating {
        return Ok(initial);
    }
    let (_, _, _definition) = run_definition(&scope, run_id)?;
    let (run, completed) = state::update(&scope, |workspace| {
        let run = workspace
            .runs
            .iter_mut()
            .find(|run| run.id == run_id)
            .with_context(|| format!("unknown run: {run_id}"))?;
        if run.status == LifecycleStatus::Terminating {
            return Ok((run.clone(), false));
        }
        if !run.status.active() {
            bail!("run cannot approve a gate while {}", run.status);
        }
        let index = run
            .pending_gates
            .iter()
            .position(|gate| gate_id.is_none_or(|id| gate.id == id))
            .context("run has no matching pending gate")?;
        let gate = run.pending_gates[index].clone();
        let approval_key = gate_approval_key(&gate.id, run.revision.as_deref());
        let orchestrator_key = format!("{approval_key}:orchestrator");
        let completed = match (gate.authority, actor) {
            (GateAuthority::User, ApprovalActor::Orchestrator) => {
                bail!("gate {} requires user approval", gate.id)
            }
            (GateAuthority::Orchestrator, ApprovalActor::User) => {
                bail!("gate {} requires orchestrator approval", gate.id)
            }
            (GateAuthority::OrchestratorThenUser, ApprovalActor::Orchestrator) => {
                if !run.approved_gates.contains(&orchestrator_key) {
                    run.approved_gates.push(orchestrator_key);
                }
                if let Some(node) = run.nodes.iter_mut().find(|node| node.id == gate.before) {
                    node.record_activity(
                        "approved",
                        "orchestrator approved; user approval remains",
                    );
                }
                run.updated_at = Utc::now();
                false
            }
            (GateAuthority::OrchestratorThenUser, ApprovalActor::User) => {
                if !run.approved_gates.contains(&orchestrator_key) {
                    bail!(
                        "gate {} requires orchestrator approval before user approval",
                        gate.id
                    );
                }
                run.approved_gates.retain(|key| key != &orchestrator_key);
                true
            }
            _ => true,
        };
        if !completed {
            return Ok((run.clone(), false));
        }
        run.pending_gates.remove(index);
        run.approved_gates.push(approval_key);
        if let Some(node) = run.nodes.iter_mut().find(|node| node.id == gate.before) {
            node.status = LifecycleStatus::Queued;
            node.record_activity("approved", "gate approved");
        }
        run.status = LifecycleStatus::Queued;
        run.updated_at = Utc::now();
        Ok((run.clone(), true))
    })?;
    if resume && completed {
        execute(config, &scope, run_id)
    } else {
        Ok(run)
    }
}

pub fn cancel(config: &Config, scope: &Path, run_id: &str) -> Result<WorkflowRun> {
    let scope = state::resolve_scope(scope)?;
    let initial = find_run(&scope, run_id)?;
    if !initial.status.active() {
        return Ok(initial);
    }
    daemon::ensure_running(config)?;
    let run_ids = state::update(&scope, |workspace| {
        let run_ids = run_family(workspace, run_id)?;
        let now = Utc::now();
        for run in workspace
            .runs
            .iter_mut()
            .filter(|run| run_ids.contains(&run.id) && run.status.active())
        {
            run.status = LifecycleStatus::Terminating;
            run.resume_requested = false;
            for node in run.nodes.iter_mut().filter(|node| node.status.active()) {
                node.status = LifecycleStatus::Terminating;
                node.updated_at = now;
            }
            run.updated_at = now;
        }
        Ok(run_ids)
    })?;
    let guards = run_ids
        .iter()
        .map(|id| {
            ExecutionLeaseGuard::acquire(&execution_lease_path(&scope, id), Duration::from_secs(2))
                .with_context(|| format!("workflow executor ownership is changing: {id}"))
        })
        .collect::<Result<Vec<_>>>()?;
    for id in &run_ids {
        terminate_run_executor(&scope, id, &execution_lease_path(&scope, id))?;
    }

    let linked_sessions = state::read(&scope)?
        .sessions
        .into_iter()
        .filter(|session| {
            session
                .run_id
                .as_ref()
                .is_some_and(|id| run_ids.contains(id))
                && session.registration == RegistrationSource::Managed
                && session.role != SessionRole::Orchestrator
                && matches!(
                    session.status,
                    LifecycleStatus::Queued
                        | LifecycleStatus::Working
                        | LifecycleStatus::Waiting
                        | LifecycleStatus::Blocked
                        | LifecycleStatus::Failed
                        | LifecycleStatus::Disconnected
                        | LifecycleStatus::Terminating
                )
        })
        .map(|session| session.id)
        .collect::<Vec<_>>();
    for session_id in linked_sessions {
        let stopped = control::terminate(config, &scope, &session_id, "workflow cancelled")
            .with_context(|| format!("stop managed session {session_id}"))?;
        if stopped.status != LifecycleStatus::Cancelled {
            bail!("managed session {session_id} is still terminating; retry cancellation");
        }
    }
    let cancelled = state::update(&scope, |workspace| {
        for session in workspace.sessions.iter_mut().filter(|session| {
            session
                .run_id
                .as_ref()
                .is_some_and(|id| run_ids.contains(id))
                && session.registration != RegistrationSource::Managed
                && session.status.active()
        }) {
            session.status = LifecycleStatus::Disconnected;
            session.termination_reason =
                Some("workflow cancelled; unmanaged session left running".into());
            session.updated_at = Utc::now();
        }
        let now = Utc::now();
        for run in workspace
            .runs
            .iter_mut()
            .filter(|run| run_ids.contains(&run.id) && run.status.active())
        {
            for node in run.nodes.iter_mut().filter(|node| node.status.active()) {
                node.status = LifecycleStatus::Cancelled;
                node.updated_at = now;
                node.record_activity("cancelled", "run cancelled");
            }
            run.status = LifecycleStatus::Cancelled;
            run.current_node = None;
            run.process_id = None;
            run.execution_nonce = None;
            run.resume_requested = false;
            run.pending_gates.clear();
            run.updated_at = now;
        }
        workspace
            .runs
            .iter()
            .find(|run| run.id == run_id)
            .cloned()
            .with_context(|| format!("unknown run: {run_id}"))
    })?;
    drop(guards);
    wake_parent(config, &scope, &cancelled)?;
    Ok(cancelled)
}

fn run_family(workspace: &WorkspaceState, root_id: &str) -> Result<BTreeSet<String>> {
    if !workspace.runs.iter().any(|run| run.id == root_id) {
        bail!("unknown run: {root_id}");
    }
    let mut family = BTreeSet::from([root_id.to_owned()]);
    loop {
        let children = workspace
            .runs
            .iter()
            .filter(|run| {
                run.parent_run_id
                    .as_ref()
                    .is_some_and(|parent| family.contains(parent))
            })
            .map(|run| run.id.clone())
            .collect::<Vec<_>>();
        let before = family.len();
        family.extend(children);
        if family.len() == before {
            return Ok(family);
        }
    }
}

fn run_depth(workspace: &WorkspaceState, run_id: &str) -> Result<usize> {
    let mut depth = 0;
    let mut current = run_id;
    let mut visited = BTreeSet::new();
    loop {
        if !visited.insert(current.to_owned()) {
            bail!("workflow run lineage contains a cycle at {current}");
        }
        let run = workspace
            .runs
            .iter()
            .find(|run| run.id == current)
            .with_context(|| format!("unknown run: {current}"))?;
        let Some(parent) = run.parent_run_id.as_deref() else {
            return Ok(depth);
        };
        depth += 1;
        current = parent;
    }
}

fn terminate_run_executor(scope: &Path, run_id: &str, lease_path: &Path) -> Result<()> {
    let current = find_run(scope, run_id)?;
    let process_directory = active_process_directory(scope, run_id);
    let lease = read_execution_lease(lease_path)?;
    if execution_identity_active(lease_path)? {
        match (current.process_id, current.execution_nonce.as_deref()) {
            (Some(process_id), Some(nonce)) => {
                let record = lease
                    .as_ref()
                    .context("workflow executor lease is missing")?;
                if record.process_id != process_id || record.nonce != nonce {
                    bail!("workflow executor identity changed; refusing cancellation");
                }
            }
            (None, None) => {}
            _ => bail!("workflow executor state has an incomplete identity"),
        }
    }
    terminate_tracked_processes(&process_directory)?;
    if execution_identity_active(lease_path)? {
        wait_for_execution_identity_release(lease_path, Duration::from_secs(2))?;
    }
    if let Some(lease) = lease {
        remove_execution_lease(lease_path, &lease.nonce)?;
    }
    Ok(())
}

fn terminate_tracked_processes(directory: &Path) -> Result<()> {
    let paths = {
        let _guard = provider::ProcessTrackerGuard::acquire(directory, Duration::from_secs(5))?;
        signal_tracked_processes(directory)?
    };
    for path in paths {
        if wait_for_tracker_release(&path, Duration::from_secs(2)).is_err() {
            {
                let _guard =
                    provider::ProcessTrackerGuard::acquire(directory, Duration::from_secs(5))?;
                if let Some(target) = tracked_process_target(&path)?
                    && process_target_is_live(target)
                {
                    signal_target(target, 9).context("kill tracked command process group")?;
                }
            }
            wait_for_tracker_release(&path, Duration::from_secs(2))?;
        }
        let _ = fs::remove_file(path);
    }
    Ok(())
}

fn signal_tracked_processes(directory: &Path) -> Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).context("read command tracker directory"),
    };
    let mut paths = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("process") {
            continue;
        }
        if !tracked_process_active(&path)? {
            let _ = fs::remove_file(path);
            continue;
        }
        let Some(target) = tracked_process_target(&path)? else {
            continue;
        };
        if process_target_is_live(target) {
            signal_target(target, 15).context("stop tracked command process group")?;
        }
        paths.push(path);
    }
    Ok(paths)
}

fn tracked_process_target(path: &Path) -> Result<Option<i32>> {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let bytes = loop {
        let bytes = fs::read(path).context("read command tracker")?;
        if bytes.len() == 4 {
            break bytes;
        }
        if !tracked_process_active(path)? {
            let _ = fs::remove_file(path);
            return Ok(None);
        }
        if std::time::Instant::now() >= deadline {
            bail!("active command tracker is incomplete: {}", path.display());
        }
        thread::sleep(Duration::from_millis(10));
    };
    let process_id = u32::from_ne_bytes(bytes.try_into().expect("tracker length checked"));
    if process_id == 0 || process_id > i32::MAX as u32 {
        bail!("invalid tracked process id {process_id}");
    }
    Ok(Some(-(process_id as i32)))
}

fn terminate_executor(process_id: u32) -> Result<()> {
    let target = process_group_target(process_id).unwrap_or(process_id as i32);
    if signal_target(target, 15).is_err() && process_is_live(process_id) {
        bail!("could not stop workflow executor {process_id}");
    }
    Ok(())
}

fn wait_for_execution_identity_release(path: &Path, timeout: Duration) -> Result<()> {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if !execution_identity_active(path)? {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(25));
    }
    if execution_identity_active(path)? {
        bail!("workflow executor did not release its identity after cancellation");
    }
    Ok(())
}

#[cfg(unix)]
fn process_target_is_live(target: i32) -> bool {
    signal_target(target, 0).is_ok()
}

#[cfg(not(unix))]
fn process_target_is_live(_target: i32) -> bool {
    false
}

#[cfg(unix)]
fn process_group_target(process_id: u32) -> Option<i32> {
    if process_id > i32::MAX as u32 {
        return None;
    }
    unsafe extern "C" {
        fn getpgid(pid: i32) -> i32;
    }
    let process_group = unsafe { getpgid(process_id as i32) };
    if process_group == process_id as i32 {
        return Some(-process_group);
    }
    let orphaned_group = -(process_id as i32);
    process_target_is_live(orphaned_group).then_some(orphaned_group)
}

#[cfg(not(unix))]
fn process_group_target(_process_id: u32) -> Option<i32> {
    None
}

#[cfg(unix)]
fn signal_process(process_id: u32, signal: i32) -> Result<()> {
    if process_id == 0 || process_id > i32::MAX as u32 {
        bail!("invalid process id {process_id}");
    }
    signal_target(process_id as i32, signal)
}

#[cfg(not(unix))]
fn signal_process(_process_id: u32, _signal: i32) -> Result<()> {
    bail!("process signals are unavailable on this platform")
}

#[cfg(unix)]
fn signal_target(target: i32, signal: i32) -> Result<()> {
    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    if unsafe { kill(target, signal) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error()).context("signal workflow executor")
    }
}

#[cfg(not(unix))]
fn signal_target(_target: i32, _signal: i32) -> Result<()> {
    bail!("process signals are unavailable on this platform")
}

pub fn set_process(
    scope: &Path,
    run_id: &str,
    process_id: u32,
    execution_nonce: &str,
    log_path: Option<&Path>,
) -> Result<WorkflowRun> {
    let scope = state::resolve_scope(scope)?;
    state::update(&scope, |workspace| {
        let run = workspace
            .runs
            .iter_mut()
            .find(|run| run.id == run_id)
            .with_context(|| format!("unknown run: {run_id}"))?;
        if !run.status.active() || run.status == LifecycleStatus::Terminating {
            return Ok(run.clone());
        }
        run.process_id = Some(process_id);
        run.execution_nonce = Some(execution_nonce.to_owned());
        run.log_path = log_path.map(|path| path.display().to_string());
        run.status = LifecycleStatus::Working;
        run.updated_at = Utc::now();
        Ok(run.clone())
    })
}

pub fn fail(
    config: &Config,
    scope: &Path,
    run_id: &str,
    error: &anyhow::Error,
) -> Result<WorkflowRun> {
    let scope = state::resolve_scope(scope)?;
    let failed = state::update(&scope, |workspace| {
        let run = workspace
            .runs
            .iter_mut()
            .find(|run| run.id == run_id)
            .with_context(|| format!("unknown run: {run_id}"))?;
        if !run.status.active() || run.status == LifecycleStatus::Terminating {
            return Ok(run.clone());
        }
        if let Some(node) = run
            .current_node
            .as_ref()
            .and_then(|id| run.nodes.iter_mut().find(|node| &node.id == id))
        {
            node.status = LifecycleStatus::Failed;
            node.record_activity("failed", format!("{error:#}"));
        }
        run.status = LifecycleStatus::Failed;
        run.process_id = None;
        run.execution_nonce = None;
        run.updated_at = Utc::now();
        Ok(run.clone())
    })?;
    wake_parent(config, &scope, &failed)?;
    Ok(failed)
}

pub fn spawn(config: &Config, scope: &Path, run_id: &str) -> Result<WorkflowRun> {
    let scope = state::resolve_scope(scope)?;
    let direction = read_display_direction(&scope, run_id)?;
    spawn_with_direction(config, &scope, run_id, &direction)
}

pub fn spawn_with_direction(
    config: &Config,
    scope: &Path,
    run_id: &str,
    direction: &str,
) -> Result<WorkflowRun> {
    validate_display_direction(direction)?;
    daemon::ensure_running(config)?;
    let scope = state::resolve_scope(scope)?;
    let (run, executor) = spawn_executor_with_direction(&scope, run_id, direction)?;
    if let Some(mut executor) = executor {
        thread::spawn(move || {
            let _ = executor.wait();
        });
    }
    Ok(run)
}

pub(crate) fn executor_active(scope: &Path, run: &WorkflowRun) -> Result<bool> {
    let (Some(process_id), Some(nonce)) = (run.process_id, run.execution_nonce.as_deref()) else {
        return Ok(false);
    };
    let path = execution_lease_path(scope, &run.id);
    if !execution_identity_active(&path)? {
        return Ok(false);
    }
    let Some(record) = read_execution_lease(&path)? else {
        return Ok(false);
    };
    Ok(record.process_id == process_id && record.nonce == nonce)
}

#[cfg(not(test))]
fn spawn_executor(
    scope: &Path,
    run_id: &str,
) -> Result<(WorkflowRun, Option<std::process::Child>)> {
    let scope = state::resolve_scope(scope)?;
    let direction = read_display_direction(&scope, run_id)?;
    spawn_executor_with_direction(&scope, run_id, &direction)
}

fn spawn_executor_with_direction(
    scope: &Path,
    run_id: &str,
    direction: &str,
) -> Result<(WorkflowRun, Option<std::process::Child>)> {
    let scope = state::resolve_scope(scope)?;
    write_display_direction(&scope, run_id, direction)?;
    let initial = find_run(&scope, run_id)?;
    if !initial.status.active() || initial.status == LifecycleStatus::Terminating {
        return Ok((initial, None));
    }
    let Some(mut lease) = ExecutionLease::acquire(&scope, run_id)? else {
        return find_run(&scope, run_id).map(|run| (run, None));
    };
    let log_directory = crate::config::state_home()
        .join("orc/logs")
        .join(state::scope_key(&scope));
    fs::create_dir_all(&log_directory)?;
    let log_path = log_directory.join(format!("{run_id}.log"));
    compact_workflow_log(&log_path)?;
    let stdout = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let stderr = stdout.try_clone()?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args([
            "run",
            "execute",
            run_id,
            "--scope",
            &scope.display().to_string(),
        ])
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .env(EXECUTION_LEASE_ENV, &lease.nonce)
        .env(DISPLAY_DIRECTION_ENV, direction);
    if initial.process_id.is_some() {
        command.env(EXECUTION_RECOVERY_ENV, "1");
    }
    for name in [
        "ORC_SESSION_ID",
        "ORC_NATIVE_SESSION_ID",
        "ORC_PARENT_SESSION_ID",
        "ORC_RUN_ID",
        "ORC_NODE_ID",
        "ORC_PROVIDER_REF",
    ] {
        command.env_remove(name);
    }
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command
        .spawn()
        .context("start background workflow executor")?;
    if let Err(error) = lease.write_owner(child.id()) {
        let _ = terminate_executor(child.id());
        let _ = child.wait();
        return Err(error);
    }
    let run = match set_process(&scope, run_id, child.id(), &lease.nonce, Some(&log_path)) {
        Ok(run)
            if run.status == LifecycleStatus::Working
                && run.process_id == Some(child.id())
                && run.execution_nonce.as_deref() == Some(lease.nonce.as_str()) =>
        {
            lease.disarm();
            return Ok((run, Some(child)));
        }
        Ok(run) => run,
        Err(error) => {
            let _ = terminate_executor(child.id());
            let _ = child.wait();
            return Err(error);
        }
    };
    let _ = terminate_executor(child.id());
    let _ = child.wait();
    Ok((run, None))
}

fn compact_workflow_log(path: &Path) -> Result<()> {
    let length = match fs::metadata(path) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("inspect workflow log"),
    };
    if length <= MAX_WORKFLOW_LOG_BYTES {
        return Ok(());
    }
    let mut source = fs::File::open(path).context("open workflow log")?;
    source
        .seek(SeekFrom::Start(length - MAX_WORKFLOW_LOG_BYTES))
        .context("seek workflow log")?;
    let mut tail = Vec::with_capacity(MAX_WORKFLOW_LOG_BYTES as usize);
    source
        .read_to_end(&mut tail)
        .context("read workflow log tail")?;
    let mut target = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)
        .context("truncate workflow log")?;
    target
        .write_all(b"[earlier workflow output omitted]\n")
        .and_then(|()| target.write_all(&tail))
        .context("compact workflow log")
}

pub(crate) fn read_log_tail(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path).context("open workflow log")?;
    let length = file.metadata().context("inspect workflow log")?.len();
    file.seek(SeekFrom::Start(
        length.saturating_sub(MAX_WORKFLOW_LOG_BYTES),
    ))
    .context("seek workflow log")?;
    let mut bytes = Vec::with_capacity(length.min(MAX_WORKFLOW_LOG_BYTES) as usize);
    file.read_to_end(&mut bytes)
        .context("read workflow log tail")?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::render_fixture;
    use std::sync::{Arc, Barrier, atomic::AtomicUsize, atomic::Ordering};

    const STOP_PROVIDER: &str = r#"#!/bin/sh
if [ "${1:-}" = stop ]; then
  : > "$2"
  exit 0
fi
request=$(cat)
case "$request" in
  *session.bind*)
    printf '%s\n' '{"version":"orc.provider/v1","binding":{"kind":"persistence","status":"active","ref":"stop-test"}}'
    exit 0
    ;;
esac
cat <<'JSON'
{{ plan }}
JSON
"#;

    const STOP_PROVIDER_MANIFEST: &str = r#"version: orc.provider/v1
name: stop-test
command: {{ command }}
actions:
  session.bind: Bind a test session
  session.stop: Stop a test session
"#;

    #[test]
    fn workflow_log_compaction_and_read_are_bounded() {
        let directory = tempfile::tempdir().expect("log directory");
        let path = directory.path().join("run.log");
        let mut contents = vec![b'x'; MAX_WORKFLOW_LOG_BYTES as usize + 128];
        contents.extend_from_slice(b"\nfinal event\n");
        fs::write(&path, contents).expect("workflow log");

        compact_workflow_log(&path).expect("compact workflow log");
        let tail = read_log_tail(&path).expect("read workflow log tail");

        assert!(fs::metadata(&path).expect("workflow log").len() < MAX_WORKFLOW_LOG_BYTES + 64);
        assert!(tail.ends_with("final event\n"));
        assert!(tail.len() <= MAX_WORKFLOW_LOG_BYTES as usize);
    }

    #[test]
    fn workflow_activity_is_bounded_and_utf8_safe() {
        let (_directory, _config, scope, mut run) = workflow_fixture("1ms");
        let node = run.nodes.first_mut().expect("workflow node");
        for attempt in 0..300 {
            node.record_activity("retry", format!("{attempt}:{}", "é".repeat(3000)));
        }

        assert_eq!(node.activity.len(), 256);
        assert!(
            node.activity
                .iter()
                .all(|event| event.message.len() <= 4096)
        );
        assert!(node.activity[0].message.starts_with("44:"));
        remove_fixture_state(&scope, &run.id);
    }

    fn workflow_fixture(duration: &str) -> (tempfile::TempDir, Config, PathBuf, WorkflowRun) {
        let directory = tempfile::tempdir().expect("workflow fixture directory");
        let scope_directory = directory.path().join("scope");
        fs::create_dir_all(&scope_directory).expect("scope directory");
        let scope = fs::canonicalize(scope_directory).expect("canonical scope");
        let provider_directory = directory.path().join("providers");
        fs::create_dir_all(&provider_directory).expect("provider directory");
        let config = Config {
            providers: crate::config::ProviderConfig {
                directory: provider_directory,
                ..crate::config::ProviderConfig::default()
            },
            workflows: crate::config::WorkflowConfig {
                repository: directory.path().join("workflows"),
                auto_commit: false,
                ..crate::config::WorkflowConfig::default()
            },
            ..Config::default()
        };
        control::register(
            &scope,
            Contract {
                harness: "test".into(),
                role: SessionRole::Orchestrator,
                title: "test orchestrator".into(),
                ..Contract::default()
            },
            SessionLink {
                id: Some(format!("orchestrator-{}", Uuid::new_v4())),
                native_id: Some(Uuid::new_v4().to_string()),
                ..SessionLink::default()
            },
        )
        .expect("register orchestrator");
        let definition = Definition {
            name: "concurrent".into(),
            goal: "execute one wave once".into(),
            entry_point: "wait".into(),
            approval: ApprovalPolicy {
                mode: ApprovalMode::Autonomous,
                ..ApprovalPolicy::default()
            },
            steps: vec![Step {
                name: "wait".into(),
                r#type: StepKind::Wait,
                duration: Some(duration.into()),
                ..Step::default()
            }],
            ..Definition::default()
        };
        let definition_path = directory.path().join("workflow.yaml");
        fs::write(
            &definition_path,
            serde_yaml::to_string(&definition).expect("serialize workflow"),
        )
        .expect("write workflow");
        let run = materialize(&config, &scope, &definition_path, RunMode::Foreground)
            .expect("materialize workflow");
        (directory, config, scope, run)
    }

    fn materialize_definition(
        directory: &tempfile::TempDir,
        config: &Config,
        scope: &Path,
        definition: &Definition,
    ) -> WorkflowRun {
        let mut definition = definition.clone();
        definition.approval.mode = ApprovalMode::Autonomous;
        let path = directory.path().join(format!("{}.yaml", definition.name));
        fs::write(
            &path,
            serde_yaml::to_string(&definition).expect("serialize workflow"),
        )
        .expect("write workflow");
        materialize(config, scope, &path, RunMode::Foreground).expect("materialize workflow")
    }

    fn set_step(name: &str) -> Step {
        Step {
            name: name.into(),
            r#type: StepKind::Set,
            value: Some(json!(true)),
            ..Step::default()
        }
    }

    #[test]
    fn entry_point_is_the_only_initially_active_root() {
        let (directory, config, scope, _) = workflow_fixture("1ms");
        let definition = Definition {
            name: "entry-only".into(),
            goal: "run only the reachable branch".into(),
            entry_point: "start".into(),
            steps: vec![
                Step {
                    routes: vec![Route {
                        to: "$end".into(),
                        when: None,
                    }],
                    ..set_step("start")
                },
                set_step("unrelated"),
            ],
            ..Definition::default()
        };
        let run = materialize_definition(&directory, &config, &scope, &definition);

        assert_eq!(run.nodes[0].status, LifecycleStatus::Queued);
        assert_eq!(run.nodes[1].status, LifecycleStatus::Pending);
        let completed = execute(&config, &scope, &run.id).expect("execute entry branch");
        assert_eq!(completed.status, LifecycleStatus::Done);
        assert_eq!(completed.nodes[0].attempt, 1);
        assert_eq!(completed.nodes[1].status, LifecycleStatus::Skipped);
        assert_eq!(completed.nodes[1].attempt, 0);
        remove_fixture_state(&scope, &run.id);
    }

    #[test]
    fn routes_choose_the_first_matching_condition() {
        let (directory, config, scope, _) = workflow_fixture("1ms");
        let definition = Definition {
            name: "ordered-routes".into(),
            goal: "choose one route".into(),
            entry_point: "start".into(),
            steps: vec![
                Step {
                    routes: vec![
                        Route {
                            to: "chosen".into(),
                            when: Some("{{ output }}".into()),
                        },
                        Route {
                            to: "fallback".into(),
                            when: None,
                        },
                    ],
                    ..set_step("start")
                },
                set_step("chosen"),
                set_step("fallback"),
            ],
            ..Definition::default()
        };
        let run = materialize_definition(&directory, &config, &scope, &definition);

        let completed = execute(&config, &scope, &run.id).expect("execute routed workflow");
        assert_eq!(completed.status, LifecycleStatus::Done);
        assert_eq!(completed.nodes[0].attempt, 1);
        assert_eq!(completed.nodes[1].attempt, 1);
        assert_eq!(completed.nodes[2].status, LifecycleStatus::Skipped);
        remove_fixture_state(&scope, &run.id);
    }

    #[test]
    fn self_route_stops_at_the_iteration_limit() {
        let (directory, config, scope, _) = workflow_fixture("1ms");
        let definition = Definition {
            name: "bounded-loop".into(),
            goal: "bound a self route".into(),
            entry_point: "loop".into(),
            limits: Limits {
                max_iterations: 2,
                ..Limits::default()
            },
            steps: vec![Step {
                routes: vec![Route {
                    to: "self".into(),
                    when: None,
                }],
                ..set_step("loop")
            }],
            ..Definition::default()
        };
        let run = materialize_definition(&directory, &config, &scope, &definition);

        let failed = execute(&config, &scope, &run.id).expect("execute bounded loop");
        assert_eq!(failed.status, LifecycleStatus::Failed);
        assert_eq!(failed.nodes[0].attempt, 2);
        assert!(
            failed.nodes[0].activity.iter().any(|event| {
                event.kind == "limit" && event.message.contains("iteration limit")
            })
        );
        remove_fixture_state(&scope, &run.id);
    }

    #[test]
    fn parallel_group_enforces_concurrency_and_continue_on_error() {
        let (directory, config, scope, _) = workflow_fixture("1ms");
        let definition = Definition {
            name: "partial-parallel".into(),
            goal: "accept one successful group member".into(),
            entry_point: "workers".into(),
            steps: vec![
                Step {
                    name: "fails".into(),
                    r#type: StepKind::Wait,
                    duration: None,
                    ..Step::default()
                },
                set_step("succeeds"),
            ],
            parallel: vec![ParallelGroup {
                name: "workers".into(),
                agents: vec!["fails".into(), "succeeds".into()],
                max_concurrent: Some(1),
                failure_mode: FailureMode::ContinueOnError,
                ..ParallelGroup::default()
            }],
            ..Definition::default()
        };
        let run = materialize_definition(&directory, &config, &scope, &definition);

        assert_eq!(ready_steps(&definition, &run).len(), 1);
        let completed = execute(&config, &scope, &run.id).expect("execute partial group");
        assert_eq!(completed.status, LifecycleStatus::Done);
        assert_eq!(completed.nodes[0].status, LifecycleStatus::Failed);
        assert_eq!(completed.nodes[1].status, LifecycleStatus::Done);
        assert_eq!(completed.nodes[0].attempt, 1);
        assert_eq!(completed.nodes[1].attempt, 1);
        remove_fixture_state(&scope, &run.id);
    }

    #[test]
    fn continue_on_error_routes_after_every_member_fails() {
        let (directory, config, scope, _) = workflow_fixture("1ms");
        let definition = Definition {
            name: "failed-parallel".into(),
            goal: "continue after collecting every group error".into(),
            entry_point: "workers".into(),
            steps: vec![
                Step {
                    name: "left".into(),
                    r#type: StepKind::Wait,
                    ..Step::default()
                },
                Step {
                    name: "right".into(),
                    r#type: StepKind::Wait,
                    ..Step::default()
                },
                set_step("finish"),
            ],
            parallel: vec![ParallelGroup {
                name: "workers".into(),
                agents: vec!["left".into(), "right".into()],
                failure_mode: FailureMode::ContinueOnError,
                routes: vec![Route {
                    to: "finish".into(),
                    when: None,
                }],
                ..ParallelGroup::default()
            }],
            ..Definition::default()
        };
        let run = materialize_definition(&directory, &config, &scope, &definition);

        let completed = execute(&config, &scope, &run.id).expect("continue after group errors");

        assert_eq!(completed.status, LifecycleStatus::Done);
        assert_eq!(completed.nodes[0].status, LifecycleStatus::Failed);
        assert_eq!(completed.nodes[1].status, LifecycleStatus::Failed);
        assert_eq!(completed.nodes[2].status, LifecycleStatus::Done);
        remove_fixture_state(&scope, &run.id);
    }

    #[test]
    fn parallel_group_routes_to_a_downstream_stage() {
        let (directory, config, scope, _) = workflow_fixture("1ms");
        let definition = Definition {
            name: "parallel-route".into(),
            goal: "route after the whole group completes".into(),
            entry_point: "workers".into(),
            steps: vec![set_step("left"), set_step("right"), set_step("join")],
            parallel: vec![ParallelGroup {
                name: "workers".into(),
                agents: vec!["left".into(), "right".into()],
                max_concurrent: Some(2),
                routes: vec![
                    Route {
                        to: "join".into(),
                        when: Some("{{ workers.outputs | length == 2 }}".into()),
                    },
                    Route {
                        to: "$end".into(),
                        when: None,
                    },
                ],
                ..ParallelGroup::default()
            }],
            ..Definition::default()
        };
        let run = materialize_definition(&directory, &config, &scope, &definition);

        let completed = execute(&config, &scope, &run.id).expect("execute routed group");
        assert_eq!(completed.status, LifecycleStatus::Done);
        assert!(completed.nodes.iter().all(|node| node.attempt == 1));
        remove_fixture_state(&scope, &run.id);
    }

    #[test]
    fn parallel_failure_modes_stop_at_the_declared_boundary() {
        for (mode, second_attempt) in [(FailureMode::FailFast, 0), (FailureMode::AllOrNothing, 1)] {
            let (directory, config, scope, _) = workflow_fixture("1ms");
            let definition = Definition {
                name: format!("failure-{mode:?}").to_ascii_lowercase(),
                goal: "enforce the group failure mode".into(),
                entry_point: "workers".into(),
                steps: vec![
                    Step {
                        name: "fails".into(),
                        r#type: StepKind::Wait,
                        duration: None,
                        ..Step::default()
                    },
                    set_step("second"),
                ],
                parallel: vec![ParallelGroup {
                    name: "workers".into(),
                    agents: vec!["fails".into(), "second".into()],
                    max_concurrent: Some(1),
                    failure_mode: mode,
                    ..ParallelGroup::default()
                }],
                ..Definition::default()
            };
            let run = materialize_definition(&directory, &config, &scope, &definition);

            let failed = execute(&config, &scope, &run.id).expect("execute failing group");
            assert_eq!(failed.status, LifecycleStatus::Failed);
            assert_eq!(failed.nodes[1].attempt, second_attempt);
            remove_fixture_state(&scope, &run.id);
        }
    }

    #[test]
    fn timeout_and_budget_limits_fail_before_more_work_starts() {
        let (directory, config, scope, _) = workflow_fixture("1ms");
        let definition = Definition {
            name: "budget".into(),
            goal: "stop over budget".into(),
            entry_point: "work".into(),
            limits: Limits {
                budget_usd: Some(1.0),
                ..Limits::default()
            },
            steps: vec![set_step("work")],
            ..Definition::default()
        };
        let run = materialize_definition(&directory, &config, &scope, &definition);
        state::update(&scope, |workspace| {
            workspace
                .runs
                .iter_mut()
                .find(|candidate| candidate.id == run.id)
                .expect("run")
                .cost_usd = 1.01;
            Ok(())
        })
        .expect("record cost");

        let failed = execute(&config, &scope, &run.id).expect("enforce budget");
        assert_eq!(failed.status, LifecycleStatus::Failed);
        assert_eq!(failed.nodes[0].attempt, 0);
        assert!(
            failed.nodes[0]
                .activity
                .iter()
                .any(|event| { event.kind == "limit" && event.message.contains("budget") })
        );
        remove_fixture_state(&scope, &run.id);

        let (directory, config, scope, _) = workflow_fixture("1ms");
        let definition = Definition {
            name: "timeout".into(),
            goal: "stop after deadline".into(),
            entry_point: "wait".into(),
            limits: Limits {
                timeout_seconds: Some(1),
                ..Limits::default()
            },
            steps: vec![Step {
                name: "wait".into(),
                r#type: StepKind::Wait,
                duration: Some("30s".into()),
                ..Step::default()
            }],
            ..Definition::default()
        };
        let run = materialize_definition(&directory, &config, &scope, &definition);
        let started = Instant::now();

        let failed = execute(&config, &scope, &run.id).expect("enforce timeout");
        assert!(started.elapsed() < Duration::from_secs(3));
        assert_eq!(failed.status, LifecycleStatus::Failed);
        remove_fixture_state(&scope, &run.id);
    }

    #[test]
    fn unsupported_dynamic_context_features_fail_validation() {
        let mut definition = Definition {
            name: "unsupported".into(),
            goal: "reject inert workflow fields".into(),
            entry_point: "work".into(),
            steps: vec![set_step("work")],
            ..Definition::default()
        };
        definition.defaults.context = ContextMode::Accumulate;
        assert!(validate(&definition, Path::new(".")).is_err());

        definition.defaults.context = ContextMode::Explicit;
        definition.steps[0]
            .input_mapping
            .insert("source".into(), "target".into());
        assert!(validate(&definition, Path::new(".")).is_err());

        definition.steps[0].input_mapping.clear();
        definition.parallel.push(ParallelGroup {
            name: "dynamic".into(),
            for_each: Some(ForEach {
                source: "items".into(),
                r#as: "item".into(),
            }),
            agent: Some("work".into()),
            ..ParallelGroup::default()
        });
        assert!(validate(&definition, Path::new(".")).is_err());

        let grouped_entry = Definition {
            name: "grouped-entry".into(),
            goal: "reject an ambiguous group entry".into(),
            entry_point: "work".into(),
            steps: vec![set_step("work")],
            parallel: vec![ParallelGroup {
                name: "workers".into(),
                agents: vec!["work".into()],
                ..ParallelGroup::default()
            }],
            ..Definition::default()
        };
        assert!(
            validate(&grouped_entry, Path::new("."))
                .expect_err("reject a group member as the entry point")
                .to_string()
                .contains("use its group name")
        );

        let routed_member = Definition {
            name: "routed-member".into(),
            goal: "reject an ambiguous route target".into(),
            entry_point: "start".into(),
            steps: vec![
                Step {
                    routes: vec![Route {
                        to: "work".into(),
                        when: None,
                    }],
                    ..set_step("start")
                },
                set_step("work"),
            ],
            parallel: vec![ParallelGroup {
                name: "workers".into(),
                agents: vec!["work".into()],
                ..ParallelGroup::default()
            }],
            ..Definition::default()
        };
        assert!(
            validate(&routed_member, Path::new("."))
                .expect_err("reject a route to one group member")
                .to_string()
                .contains("use its group name")
        );
    }

    fn remove_fixture_state(scope: &Path, run_id: &str) {
        let _ = fs::remove_file(materialized_definition_path(scope, run_id));
        let _ = fs::remove_file(state::path(scope));
        let _ = fs::remove_file(preferences::path(scope));
        let _ = fs::remove_file(execution_lease_path(scope, run_id));
        let _ = fs::remove_file(display_direction_path(scope, run_id));
    }

    #[test]
    fn launch_direction_is_persisted_and_sent_to_the_provider() {
        let (_directory, _config, scope, run) = workflow_fixture("1ms");
        let session = state::read(&scope).expect("workspace").sessions[0].clone();
        let step = Step {
            name: "worker".into(),
            ..Step::default()
        };

        write_display_direction(&scope, &run.id, "bottom").expect("save display direction");
        let request = agent_launch_request(AgentLaunchRequest {
            scope: &scope,
            session: &session,
            harness: "codex",
            prompt: "perform the task",
            native_id: "native-worker",
            run: &run,
            step: &step,
            direction: &read_display_direction(&scope, &run.id).expect("display direction"),
        });

        assert_eq!(request["direction"], "bottom");
        assert_eq!(read_display_direction(&scope, &run.id).unwrap(), "bottom");
        assert!(write_display_direction(&scope, &run.id, "diagonal").is_err());
        remove_fixture_state(&scope, &run.id);
    }

    #[test]
    fn external_run_edits_update_the_executed_definition_and_invalidate_gates() {
        let (_directory, config, scope, run) = workflow_fixture("1ms");
        let definition_path = PathBuf::from(run.definition.as_deref().expect("definition path"));
        state::update(&scope, |workspace| {
            let run = workspace
                .runs
                .iter_mut()
                .find(|candidate| candidate.id == run.id)
                .expect("run");
            run.pending_gates.push(PendingGate {
                id: "before-wait".into(),
                before: "wait".into(),
                reason: "review wait".into(),
                authority: GateAuthority::User,
                recommendation: None,
                created_at: Utc::now(),
            });
            run.approved_gates.push("before-wait".into());
            Ok(())
        })
        .expect("seed stale approvals");

        edit_run_node(
            &config,
            &scope,
            &run.id,
            "wait",
            NodeEdit {
                goal: Some("use the edited external definition".into()),
                ..NodeEdit::default()
            },
        )
        .expect("edit external workflow node");

        let definition = load(&definition_path).expect("load executed definition");
        let current = find_run(&scope, &run.id).expect("edited run");
        assert_eq!(
            definition.steps[0].goal,
            "use the edited external definition"
        );
        assert!(!config.workflows.repository.exists());
        assert_ne!(current.revision, run.revision);
        assert!(current.pending_gates.is_empty());
        assert!(current.approved_gates.is_empty());
        remove_fixture_state(&scope, &run.id);
    }

    #[test]
    fn dependency_edits_invalidate_gate_approvals() {
        let (directory, config, scope, fixture_run) = workflow_fixture("1ms");
        let definition = Definition {
            name: "dependency-edit".into(),
            goal: "change a dependency safely".into(),
            entry_point: "first".into(),
            steps: vec![set_step("first"), set_step("second")],
            ..Definition::default()
        };
        let run = materialize_definition(&directory, &config, &scope, &definition);
        state::update(&scope, |workspace| {
            let run = workspace
                .runs
                .iter_mut()
                .find(|candidate| candidate.id == run.id)
                .expect("run");
            run.pending_gates.push(PendingGate {
                id: "before-second".into(),
                before: "second".into(),
                reason: "review dependency".into(),
                authority: GateAuthority::User,
                recommendation: None,
                created_at: Utc::now(),
            });
            run.approved_gates.push("before-second".into());
            Ok(())
        })
        .expect("seed stale approvals");

        set_run_dependency(&config, &scope, &run.id, "second", "first", true)
            .expect("edit dependency");

        let current = find_run(&scope, &run.id).expect("edited run");
        assert_ne!(current.revision, run.revision);
        assert!(current.pending_gates.is_empty());
        assert!(current.approved_gates.is_empty());
        remove_fixture_state(&scope, &run.id);
        remove_fixture_state(&scope, &fixture_run.id);
    }

    #[test]
    fn run_edits_reject_working_and_terminal_nodes() {
        for status in [LifecycleStatus::Working, LifecycleStatus::Done] {
            let (directory, config, scope, fixture_run) = workflow_fixture("1ms");
            let definition = Definition {
                name: format!("locked-{status}"),
                goal: "protect an executed contract".into(),
                entry_point: "first".into(),
                steps: vec![set_step("first"), set_step("second")],
                ..Definition::default()
            };
            let run = materialize_definition(&directory, &config, &scope, &definition);
            state::update(&scope, |workspace| {
                let node = workspace
                    .runs
                    .iter_mut()
                    .find(|candidate| candidate.id == run.id)
                    .expect("run")
                    .nodes
                    .iter_mut()
                    .find(|node| node.id == "second")
                    .expect("second node");
                node.status = status;
                node.attempt = 1;
                Ok(())
            })
            .expect("mark node started");
            let before = fs::read(run.definition.as_deref().expect("definition")).expect("before");

            for error in [
                edit_run_node(
                    &config,
                    &scope,
                    &run.id,
                    "second",
                    NodeEdit {
                        goal: Some("changed".into()),
                        ..NodeEdit::default()
                    },
                )
                .expect_err("started node cannot be edited"),
                delete_run_node(&config, &scope, &run.id, "second")
                    .expect_err("started node cannot be deleted"),
                set_run_dependency(&config, &scope, &run.id, "second", "first", true)
                    .expect_err("started node dependencies cannot change"),
            ] {
                assert!(error.to_string().contains("restart-from-node"));
            }
            assert_eq!(
                fs::read(run.definition.as_deref().expect("definition")).expect("after"),
                before
            );
            remove_fixture_state(&scope, &run.id);
            remove_fixture_state(&scope, &fixture_run.id);
        }
    }

    #[test]
    fn catalog_workflow_edits_do_not_relabel_existing_runs() {
        let (directory, config, scope, run) = workflow_fixture("1ms");
        let catalog_path = directory.path().join("workflow.yaml");
        let mut definition = load(&catalog_path).expect("catalog workflow");
        definition.goal = "edited workflow goal".into();
        fs::write(
            &catalog_path,
            serde_yaml::to_string(&definition).expect("serialize workflow"),
        )
        .expect("write workflow");

        let completed = execute(&config, &scope, &run.id).expect("execute pinned run");

        assert_eq!(completed.status, LifecycleStatus::Done);
        assert_eq!(completed.goal, run.goal);
        assert_eq!(completed.revision, run.revision);
        assert_ne!(completed.goal, definition.goal);
        remove_fixture_state(&scope, &run.id);
    }

    #[test]
    fn stale_gate_cannot_be_approved_after_an_external_workflow_edit() {
        let (_directory, config, scope, run) = workflow_fixture("1ms");
        let definition_path = PathBuf::from(run.definition.as_deref().expect("definition path"));
        state::update(&scope, |workspace| {
            let run = workspace
                .runs
                .iter_mut()
                .find(|candidate| candidate.id == run.id)
                .expect("run");
            run.status = LifecycleStatus::Waiting;
            run.nodes[0].status = LifecycleStatus::Waiting;
            run.pending_gates.push(PendingGate {
                id: "old-gate".into(),
                before: "wait".into(),
                reason: "old workflow approval".into(),
                authority: GateAuthority::User,
                recommendation: None,
                created_at: Utc::now(),
            });
            Ok(())
        })
        .expect("seed stale gate");
        let mut definition = load(&definition_path).expect("workflow");
        definition.goal = "new workflow contract".into();
        fs::write(
            &definition_path,
            serde_yaml::to_string(&definition).expect("serialize workflow"),
        )
        .expect("write workflow");

        let error = approve(&config, &scope, &run.id, Some("old-gate"), false)
            .expect_err("reject an externally changed materialized definition");

        assert!(error.to_string().contains("changed outside Orc"));
        let current = find_run(&scope, &run.id).expect("unchanged run");
        assert_eq!(current.revision, run.revision);
        assert_eq!(current.pending_gates.len(), 1);
        assert!(current.approved_gates.is_empty());
        remove_fixture_state(&scope, &run.id);
    }

    #[test]
    fn plan_groups_independent_steps() {
        let definition = Definition {
            name: "x".into(),
            goal: "x".into(),
            entry_point: "a".into(),
            defaults: WorkflowDefaults {
                runtime: Runtime {
                    harness: Some("agent-a".into()),
                    execution: Some("executor-a".into()),
                    ..Runtime::default()
                },
                ..WorkflowDefaults::default()
            },
            steps: vec![
                Step {
                    name: "a".into(),
                    ..Step::default()
                },
                Step {
                    name: "b".into(),
                    ..Step::default()
                },
                Step {
                    name: "c".into(),
                    depends_on: vec!["a".into(), "b".into()],
                    ..Step::default()
                },
            ],
            ..Definition::default()
        };
        let config = Config::default();
        let planned = plan(&config, Path::new("."), &definition).unwrap();
        assert_eq!(planned.waves[0].len(), 2);
        assert_eq!(planned.waves[1], vec!["c"]);
    }

    #[test]
    fn workflow_stages_cannot_claim_the_orchestrator_role() {
        let definition = Definition {
            name: "invalid-orchestrator-stage".into(),
            goal: "keep the orchestrator above the workflow".into(),
            entry_point: "work".into(),
            steps: vec![Step {
                name: "work".into(),
                r#type: StepKind::Set,
                role: SessionRole::Orchestrator,
                value: Some(json!(true)),
                ..Step::default()
            }],
            ..Definition::default()
        };

        let error = validate(&definition, Path::new("."))
            .expect_err("reject an orchestrator as a workflow stage");
        assert!(error.to_string().contains("orchestrator owns the workflow"));
    }

    #[test]
    fn workflow_reviewers_are_typed_and_must_exist() {
        let definition = |review_by: &str, reviewer_role| Definition {
            name: "review-contract".into(),
            goal: "validate the review relationship".into(),
            entry_point: "implement".into(),
            steps: vec![
                Step {
                    name: "implement".into(),
                    r#type: StepKind::Set,
                    role: SessionRole::Implementer,
                    review_by: Some(review_by.into()),
                    value: Some(json!(true)),
                    ..Step::default()
                },
                Step {
                    name: "review".into(),
                    r#type: StepKind::Set,
                    role: reviewer_role,
                    value: Some(json!(true)),
                    ..Step::default()
                },
            ],
            ..Definition::default()
        };

        let missing = validate(
            &definition("missing", SessionRole::Verifier),
            Path::new("."),
        )
        .expect_err("reject an unknown reviewer");
        assert!(missing.to_string().contains("unknown reviewer missing"));

        let wrong_role = validate(
            &definition("review", SessionRole::Researcher),
            Path::new("."),
        )
        .expect_err("reject a reviewer without a review role");
        assert!(
            wrong_role
                .to_string()
                .contains("critic, judge, or verifier")
        );
    }

    #[test]
    fn review_relationship_schedules_the_reviewer_after_its_subject() {
        let definition = Definition {
            name: "review-order".into(),
            goal: "review completed implementation work".into(),
            entry_point: "implement".into(),
            steps: vec![
                Step {
                    name: "implement".into(),
                    r#type: StepKind::Set,
                    role: SessionRole::Implementer,
                    review_by: Some("verify".into()),
                    value: Some(json!(true)),
                    ..Step::default()
                },
                Step {
                    name: "verify".into(),
                    r#type: StepKind::Set,
                    role: SessionRole::Verifier,
                    value: Some(json!(true)),
                    ..Step::default()
                },
            ],
            ..Definition::default()
        };

        validate(&definition, Path::new(".")).expect("valid review relationship");
        let planned = plan(&Config::default(), Path::new("."), &definition).expect("plan");

        assert_eq!(planned.waves, vec![vec!["implement"], vec!["verify"]]);
        assert_eq!(planned.steps[1].depends_on, vec!["implement"]);
    }

    #[test]
    fn reviewer_feedback_retries_the_subject_before_reviewing_again() {
        let (directory, config, scope, _) = workflow_fixture("1ms");
        let definition = Definition {
            name: "review-feedback".into(),
            goal: "repeat implementation after review feedback".into(),
            entry_point: "implement".into(),
            steps: vec![
                Step {
                    name: "implement".into(),
                    r#type: StepKind::Set,
                    role: SessionRole::Implementer,
                    review_by: Some("verify".into()),
                    value: Some(json!(true)),
                    ..Step::default()
                },
                Step {
                    name: "verify".into(),
                    r#type: StepKind::Set,
                    role: SessionRole::Verifier,
                    routes: vec![Route {
                        to: "implement".into(),
                        when: Some("output == false".into()),
                    }],
                    value: Some(json!(false)),
                    ..Step::default()
                },
            ],
            ..Definition::default()
        };
        let mut run = materialize_definition(&directory, &config, &scope, &definition);

        assert!(run.edges.iter().any(|edge| {
            edge.from == "verify" && edge.to == "implement" && edge.relationship == "feedback"
        }));
        for node in &mut run.nodes {
            node.status = LifecycleStatus::Done;
            node.attempt = 1;
            node.output = Some(json!(node.id != "verify"));
        }

        advance_state(&mut run, &definition).expect("apply review feedback");

        let implement = run
            .nodes
            .iter()
            .find(|node| node.id == "implement")
            .expect("implementation node");
        let verify = run
            .nodes
            .iter()
            .find(|node| node.id == "verify")
            .expect("review node");
        assert_eq!(implement.status, LifecycleStatus::Queued);
        assert_eq!(verify.status, LifecycleStatus::Pending);
        assert!(verify.activity.iter().any(|event| event.kind == "feedback"));
        remove_fixture_state(&scope, &run.id);
    }

    #[test]
    fn plan_exposes_the_effective_approval_policy() {
        let definition = |mode| Definition {
            name: "approval-policy".into(),
            goal: "expose operator intervention points".into(),
            entry_point: "ordinary".into(),
            approval: ApprovalPolicy {
                mode,
                gates: vec![ApprovalGate {
                    id: "before-flagged".into(),
                    before: "flagged".into(),
                    reason: "inspect the result".into(),
                    authority: GateAuthority::User,
                }],
            },
            steps: vec![
                Step {
                    name: "ordinary".into(),
                    r#type: StepKind::Set,
                    value: Some(json!(true)),
                    ..Step::default()
                },
                Step {
                    name: "flagged".into(),
                    r#type: StepKind::Set,
                    requires_approval: true,
                    value: Some(json!(true)),
                    ..Step::default()
                },
                Step {
                    name: "human".into(),
                    r#type: StepKind::HumanGate,
                    ..Step::default()
                },
            ],
            ..Definition::default()
        };
        let approvals = |mode| {
            plan(&Config::default(), Path::new("."), &definition(mode))
                .expect("plan approval policy")
                .steps
                .into_iter()
                .map(|step| step.approval_required)
                .collect::<Vec<_>>()
        };

        assert_eq!(approvals(ApprovalMode::Supervised), vec![true, true, true]);
        assert_eq!(
            approvals(ApprovalMode::ApprovalGated),
            vec![false, true, true]
        );
        assert_eq!(
            approvals(ApprovalMode::Autonomous),
            vec![false, false, true]
        );
    }

    #[test]
    fn execution_uses_the_approval_policy_exposed_by_the_plan() {
        let (_directory, _config, scope, run) = workflow_fixture("1ms");
        for mode in [
            ApprovalMode::Supervised,
            ApprovalMode::ApprovalGated,
            ApprovalMode::Autonomous,
        ] {
            let definition = Definition {
                name: "approval-policy".into(),
                goal: "use one approval policy".into(),
                entry_point: "ordinary".into(),
                approval: ApprovalPolicy {
                    mode,
                    gates: vec![ApprovalGate {
                        id: "before-flagged".into(),
                        before: "flagged".into(),
                        reason: "inspect the result".into(),
                        authority: GateAuthority::User,
                    }],
                },
                steps: vec![
                    set_step("ordinary"),
                    Step {
                        name: "flagged".into(),
                        requires_approval: true,
                        ..set_step("flagged")
                    },
                    Step {
                        name: "human".into(),
                        r#type: StepKind::HumanGate,
                        ..Step::default()
                    },
                ],
                ..Definition::default()
            };
            let planned = plan(&Config::default(), Path::new("."), &definition)
                .expect("plan approval policy");
            let executed = definition
                .steps
                .iter()
                .map(|step| required_gate(&definition, step, &run).is_some())
                .collect::<Vec<_>>();
            assert_eq!(
                planned
                    .steps
                    .iter()
                    .map(|step| step.approval_required)
                    .collect::<Vec<_>>(),
                executed
            );
        }
        remove_fixture_state(&scope, &run.id);
    }

    #[test]
    fn orchestrator_cannot_approve_a_user_gate() {
        let (_directory, config, scope, run) = workflow_fixture("1ms");
        state::update(&scope, |workspace| {
            let run = workspace
                .runs
                .iter_mut()
                .find(|candidate| candidate.id == run.id)
                .expect("run");
            run.status = LifecycleStatus::Waiting;
            run.nodes[0].status = LifecycleStatus::Waiting;
            run.pending_gates.push(PendingGate {
                id: "user-only".into(),
                before: "wait".into(),
                reason: "user decision".into(),
                authority: GateAuthority::User,
                recommendation: None,
                created_at: Utc::now(),
            });
            Ok(())
        })
        .expect("record user gate");

        let error = approve_as(
            &config,
            &scope,
            &run.id,
            Some("user-only"),
            false,
            ApprovalActor::Orchestrator,
        )
        .expect_err("orchestrator must not approve a user gate");

        assert!(error.to_string().contains("requires user approval"));
        let current = find_run(&scope, &run.id).expect("pending run");
        assert_eq!(current.pending_gates[0].authority, GateAuthority::User);
        assert_eq!(current.status, LifecycleStatus::Waiting);
        remove_fixture_state(&scope, &run.id);
    }

    #[test]
    fn dual_authority_gate_requires_orchestrator_then_user() {
        let (_directory, config, scope, run) = workflow_fixture("1ms");
        state::update(&scope, |workspace| {
            let run = workspace
                .runs
                .iter_mut()
                .find(|candidate| candidate.id == run.id)
                .expect("run");
            run.status = LifecycleStatus::Waiting;
            run.nodes[0].status = LifecycleStatus::Waiting;
            run.pending_gates.push(PendingGate {
                id: "dual".into(),
                before: "wait".into(),
                reason: "two approvals".into(),
                authority: GateAuthority::OrchestratorThenUser,
                recommendation: None,
                created_at: Utc::now(),
            });
            Ok(())
        })
        .expect("record dual gate");

        approve_as(
            &config,
            &scope,
            &run.id,
            Some("dual"),
            false,
            ApprovalActor::Orchestrator,
        )
        .expect("orchestrator approval");
        let intermediate = find_run(&scope, &run.id).expect("intermediate run");
        assert_eq!(intermediate.pending_gates.len(), 1);
        assert_eq!(intermediate.status, LifecycleStatus::Waiting);

        let approved =
            approve(&config, &scope, &run.id, Some("dual"), false).expect("user approval");
        assert!(approved.pending_gates.is_empty());
        assert_eq!(approved.status, LifecycleStatus::Queued);
        remove_fixture_state(&scope, &run.id);
    }

    #[test]
    fn workflow_lease_overrides_accept_documented_names() {
        let definition: Definition = serde_yaml::from_str(
            r#"name: lease
goal: test leases
entry_point: work
steps:
  - name: work
    timeoutSeconds: 120
    idleTimeoutSeconds: 30
    maxDepth: 4
"#,
        )
        .expect("parse workflow");

        assert_eq!(definition.steps[0].timeout_seconds, Some(120));
        assert_eq!(definition.steps[0].idle_timeout_seconds, Some(30));
        assert_eq!(definition.steps[0].max_depth, Some(4));
    }

    #[test]
    fn workflow_init_requires_or_inherits_a_harness_and_uses_provider_selection() {
        let directory = tempfile::tempdir().expect("workflow fixture");
        let scope_directory = directory.path().join("scope");
        fs::create_dir_all(&scope_directory).expect("scope directory");
        let scope = fs::canonicalize(scope_directory).expect("canonical scope");
        let mut config = Config::default();
        config.workflows.repository = directory.path().join("catalog");
        config.workflows.auto_commit = false;

        let missing = init(&config, &scope, "missing", None)
            .expect_err("an external caller must choose a harness");
        assert!(missing.to_string().contains("needs --harness"));
        assert!(!config.workflows.repository.exists());

        let path =
            init(&config, &scope, "planned", Some("test-harness")).expect("initialize workflow");
        let definition = load(&path).expect("load initialized workflow");
        validate(&definition, path.parent().expect("workflow parent"))
            .expect("initialized workflow validates");
        assert_eq!(
            definition.defaults.runtime.harness.as_deref(),
            Some("test-harness")
        );
        assert_eq!(definition.defaults.runtime.execution, None);
        let _ = fs::remove_file(state::path(&scope));
    }

    #[test]
    fn interrupted_side_effects_require_an_explicit_retry() {
        let (_directory, _config, scope, mut run) = workflow_fixture("1ms");
        run.status = LifecycleStatus::Working;
        run.current_node = Some(run.nodes[0].id.clone());
        run.nodes[0].status = LifecycleStatus::Working;

        block_interrupted_nodes(&mut run);

        assert_eq!(run.nodes[0].status, LifecycleStatus::Blocked);
        assert_eq!(run.current_node, None);
        assert!(
            run.nodes[0]
                .activity
                .last()
                .is_some_and(|event| event.message.contains("inspect before retrying"))
        );
        remove_fixture_state(&scope, &run.id);
    }

    #[test]
    fn nested_workflow_materialization_links_the_parent_node() {
        let (directory, config, scope, parent) = workflow_fixture("1ms");
        let child_definition = Definition {
            name: "child".into(),
            goal: "complete child".into(),
            entry_point: "done".into(),
            steps: vec![Step {
                name: "done".into(),
                r#type: StepKind::Set,
                value: Some(json!(true)),
                ..Step::default()
            }],
            ..Definition::default()
        };
        let child_path = directory.path().join("child.yaml");
        fs::write(
            &child_path,
            serde_yaml::to_string(&child_definition).expect("serialize child workflow"),
        )
        .expect("write child workflow");

        let child = materialize_with_parent(
            &config,
            &scope,
            &child_path,
            RunMode::Foreground,
            Some(&parent.id),
            Some("wait"),
        )
        .expect("materialize child workflow");
        let stored_parent = find_run(&scope, &parent.id).expect("parent run");

        assert_eq!(child.parent_run_id.as_deref(), Some(parent.id.as_str()));
        assert_eq!(
            stored_parent.nodes[0].child_run_id.as_deref(),
            Some(child.id.as_str())
        );
        remove_fixture_state(&scope, &parent.id);
    }

    #[test]
    fn plan_rejects_dependency_cycles() {
        let definition = Definition {
            name: "cycle".into(),
            goal: "reject cycles".into(),
            entry_point: "a".into(),
            defaults: WorkflowDefaults {
                runtime: Runtime {
                    harness: Some("agent-a".into()),
                    execution: Some("executor-a".into()),
                    ..Runtime::default()
                },
                ..WorkflowDefaults::default()
            },
            steps: vec![
                Step {
                    name: "a".into(),
                    depends_on: vec!["b".into()],
                    ..Step::default()
                },
                Step {
                    name: "b".into(),
                    depends_on: vec!["a".into()],
                    ..Step::default()
                },
            ],
            ..Definition::default()
        };

        let error = plan(&Config::default(), Path::new("."), &definition).unwrap_err();
        assert!(error.to_string().contains("dependency cycle"));
    }

    #[test]
    fn plan_rejects_a_cycle_through_parallel_group_completion() {
        let definition = Definition {
            name: "parallel-cycle".into(),
            goal: "reject cycles through a parallel group".into(),
            entry_point: "start".into(),
            steps: vec![
                set_step("start"),
                Step {
                    name: "member".into(),
                    depends_on: vec!["after".into()],
                    ..set_step("member")
                },
                Step {
                    name: "after".into(),
                    depends_on: vec!["workers".into()],
                    ..set_step("after")
                },
            ],
            parallel: vec![ParallelGroup {
                name: "workers".into(),
                agents: vec!["member".into()],
                ..ParallelGroup::default()
            }],
            ..Definition::default()
        };

        let error = plan(&Config::default(), Path::new("."), &definition)
            .expect_err("group completion must participate in cycle detection");

        assert!(error.to_string().contains("dependency cycle"));
        assert!(error.to_string().contains("member"));
        assert!(error.to_string().contains("after"));
    }

    #[test]
    fn workflow_names_cannot_escape_the_catalog() {
        let error = validate_name("../../config").expect_err("reject path traversal");
        assert!(error.to_string().contains("workflow name"));
    }

    #[test]
    fn worker_session_cannot_own_a_workflow() {
        let (_directory, _config, scope, _run) = workflow_fixture("1ms");
        let mut worker = state::read(&scope).expect("workspace").sessions[0].clone();
        worker.role = SessionRole::Worker;
        let error = require_orchestrator(&worker).expect_err("worker must not own workflow");
        assert!(error.to_string().contains("only an orchestrator"));
    }

    #[test]
    fn concurrent_lease_claims_have_one_owner() {
        let directory = tempfile::tempdir().expect("lease directory");
        let path = Arc::new(directory.path().join("run.lease"));
        let start = Arc::new(Barrier::new(8));
        let release = Arc::new(Barrier::new(9));
        let owners = Arc::new(AtomicUsize::new(0));
        let workers = (0..8)
            .map(|_| {
                let path = Arc::clone(&path);
                let start = Arc::clone(&start);
                let release = Arc::clone(&release);
                let owners = Arc::clone(&owners);
                thread::spawn(move || {
                    start.wait();
                    let lease = ExecutionLease::acquire_at(&path).expect("claim lease");
                    if lease.is_some() {
                        owners.fetch_add(1, Ordering::SeqCst);
                    }
                    release.wait();
                    lease
                })
            })
            .collect::<Vec<_>>();
        release.wait();
        assert_eq!(owners.load(Ordering::SeqCst), 1);
        for worker in workers {
            worker.join().expect("lease worker");
        }
    }

    #[test]
    fn stale_execution_lease_is_reclaimed() {
        let directory = tempfile::tempdir().expect("lease directory");
        let path = directory.path().join("run.lease");
        fs::write(&path, format!("{} stale\n", u32::MAX)).expect("write stale lease");

        let lease = ExecutionLease::acquire_at(&path)
            .expect("reclaim stale lease")
            .expect("lease owner");

        assert_ne!(lease.nonce, "stale");
    }

    #[test]
    fn execution_identity_is_live_only_while_its_lease_is_held() {
        let directory = tempfile::tempdir().expect("lease directory");
        let path = directory.path().join("run.lease");
        let lease = ExecutionLease::acquire_at(&path)
            .expect("acquire execution lease")
            .expect("lease owner");

        assert!(execution_identity_active(&path).expect("inspect active identity"));
        drop(lease);
        let released = (0..50).any(|_| {
            if !execution_identity_active(&path).expect("inspect released identity") {
                true
            } else {
                thread::sleep(Duration::from_millis(10));
                false
            }
        });
        assert!(released);
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_stale_lease_reclaim_has_one_owner() {
        let directory = tempfile::tempdir().expect("lease directory");
        let path = Arc::new(directory.path().join("run.lease"));
        fs::write(path.as_ref(), format!("{} stale\n", u32::MAX)).expect("write stale lease");
        let start = Arc::new(Barrier::new(8));
        let release = Arc::new(Barrier::new(9));
        let owners = Arc::new(AtomicUsize::new(0));
        let workers = (0..8)
            .map(|_| {
                let path = Arc::clone(&path);
                let start = Arc::clone(&start);
                let release = Arc::clone(&release);
                let owners = Arc::clone(&owners);
                thread::spawn(move || {
                    start.wait();
                    let lease = ExecutionLease::acquire_at(path.as_ref()).expect("claim lease");
                    if lease.is_some() {
                        owners.fetch_add(1, Ordering::SeqCst);
                    }
                    release.wait();
                    lease
                })
            })
            .collect::<Vec<_>>();
        release.wait();
        assert_eq!(owners.load(Ordering::SeqCst), 1);
        for worker in workers {
            worker.join().expect("lease worker");
        }
    }

    #[cfg(unix)]
    #[test]
    fn terminate_executor_targets_a_dedicated_process_group() {
        let mut command = Command::new("sleep");
        command.arg("30").process_group(0);
        let mut child = command.spawn().expect("spawn process group leader");
        let target = process_group_target(child.id()).expect("dedicated process group");
        assert_eq!(target, -(child.id() as i32));

        terminate_executor(child.id()).expect("terminate process group");
        let exited = (0..100).any(|_| {
            if child.try_wait().expect("read child status").is_some() {
                true
            } else {
                thread::sleep(Duration::from_millis(10));
                false
            }
        });
        if !exited {
            let _ = child.kill();
        }
        assert!(exited, "process group leader ignored termination");
    }

    #[test]
    fn concurrent_execute_runs_a_wave_once() {
        let (_directory, config, scope, run) = workflow_fixture("150ms");
        let start = Arc::new(Barrier::new(2));
        let workers = (0..2)
            .map(|_| {
                let config = config.clone();
                let scope = scope.clone();
                let run_id = run.id.clone();
                let start = Arc::clone(&start);
                thread::spawn(move || {
                    start.wait();
                    execute(&config, &scope, &run_id)
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker
                .join()
                .expect("execution worker")
                .expect("execute workflow");
        }

        let completed = find_run(&scope, &run.id).expect("completed run");
        assert_eq!(completed.status, LifecycleStatus::Done);
        assert_eq!(completed.nodes[0].status, LifecycleStatus::Done);
        assert_eq!(completed.nodes[0].attempt, 1);
        remove_fixture_state(&scope, &run.id);
    }

    #[test]
    fn cancel_interrupts_a_wait_step() {
        let (_directory, config, scope, run) = workflow_fixture("30s");
        let execution_config = config.clone();
        let execution_scope = scope.clone();
        let run_id = run.id.clone();
        let executor = thread::spawn(move || execute(&execution_config, &execution_scope, &run_id));
        for _ in 0..100 {
            if find_run(&scope, &run.id).expect("active run").nodes[0].status
                == LifecycleStatus::Working
            {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }

        let started = Instant::now();
        let cancelled = cancel(&config, &scope, &run.id).expect("cancel wait");
        executor
            .join()
            .expect("execution thread")
            .expect("execute cancelled wait");

        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(cancelled.status, LifecycleStatus::Cancelled);
        remove_fixture_state(&scope, &run.id);
    }

    #[test]
    fn retry_backoff_delays_the_next_attempt() {
        let (directory, config, scope, fixture_run) = workflow_fixture("1ms");
        let definition = Definition {
            name: "retry-backoff".into(),
            goal: "delay the retry".into(),
            entry_point: "fail".into(),
            steps: vec![Step {
                name: "fail".into(),
                r#type: StepKind::Script,
                command: vec!["sh".into(), "-c".into(), "exit 1".into()],
                retry: RetryPolicy {
                    attempts: 1,
                    backoff_seconds: 1,
                },
                ..Step::default()
            }],
            ..Definition::default()
        };
        let run = materialize_definition(&directory, &config, &scope, &definition);

        let started = Instant::now();
        let failed = execute(&config, &scope, &run.id).expect("execute retries");

        assert!(started.elapsed() >= Duration::from_millis(900));
        assert_eq!(failed.status, LifecycleStatus::Failed);
        assert_eq!(failed.nodes[0].attempt, 2);
        remove_fixture_state(&scope, &run.id);
        remove_fixture_state(&scope, &fixture_run.id);
    }

    #[test]
    fn persisted_retry_deadline_delays_recovered_execution() {
        let (_directory, config, scope, run) = workflow_fixture("1ms");
        state::update(&scope, |workspace| {
            let node = &mut workspace
                .runs
                .iter_mut()
                .find(|candidate| candidate.id == run.id)
                .expect("run")
                .nodes[0];
            node.retry_after = Some(Utc::now() + chrono::Duration::seconds(1));
            Ok(())
        })
        .expect("persist retry deadline");

        let started = Instant::now();
        let completed = execute(&config, &scope, &run.id).expect("resume delayed workflow");

        assert!(started.elapsed() >= Duration::from_millis(900));
        assert_eq!(completed.status, LifecycleStatus::Done);
        assert_eq!(completed.nodes[0].attempt, 1);
        remove_fixture_state(&scope, &run.id);
    }

    #[test]
    fn fatal_parallel_failure_suppresses_retry_backoff() {
        let (directory, config, scope, fixture_run) = workflow_fixture("1ms");
        let failing_step = |name: &str, retry| Step {
            name: name.into(),
            r#type: StepKind::Script,
            command: vec!["sh".into(), "-c".into(), "exit 1".into()],
            retry,
            ..Step::default()
        };
        let definition = Definition {
            name: "fatal-parallel".into(),
            goal: "stop retry backoff after a fatal peer failure".into(),
            entry_point: "workers".into(),
            steps: vec![
                failing_step(
                    "retrying",
                    RetryPolicy {
                        attempts: 1,
                        backoff_seconds: 60,
                    },
                ),
                failing_step("fatal", RetryPolicy::default()),
            ],
            parallel: vec![ParallelGroup {
                name: "workers".into(),
                agents: vec!["retrying".into(), "fatal".into()],
                failure_mode: FailureMode::FailFast,
                ..ParallelGroup::default()
            }],
            ..Definition::default()
        };
        let run = materialize_definition(&directory, &config, &scope, &definition);

        let started = Instant::now();
        let failed = execute(&config, &scope, &run.id).expect("execute parallel failures");

        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(failed.status, LifecycleStatus::Failed);
        remove_fixture_state(&scope, &run.id);
        remove_fixture_state(&scope, &fixture_run.id);
    }

    #[test]
    fn stale_executor_exit_cannot_fail_its_replacement() {
        let (_directory, config, scope, run) = workflow_fixture("1ms");
        state::update(&scope, |workspace| {
            let run = workspace
                .runs
                .iter_mut()
                .find(|candidate| candidate.id == run.id)
                .expect("run");
            run.status = LifecycleStatus::Working;
            run.process_id = Some(200);
            run.execution_nonce = Some("replacement".into());
            Ok(())
        })
        .expect("install replacement identity");
        let status = Command::new("true").status().expect("exit status");

        let current =
            reconcile_executor_exit(&config, &scope, &run.id, Some(100), Some("old"), status)
                .expect("ignore stale watcher");

        assert_eq!(current.status, LifecycleStatus::Working);
        assert_eq!(current.process_id, Some(200));
        assert_eq!(current.execution_nonce.as_deref(), Some("replacement"));
        remove_fixture_state(&scope, &run.id);
    }

    #[test]
    fn termination_claim_cannot_be_overwritten() {
        let (_directory, config, scope, run) = workflow_fixture("1ms");
        state::update(&scope, |workspace| {
            let run = workspace
                .runs
                .iter_mut()
                .find(|candidate| candidate.id == run.id)
                .expect("run");
            run.status = LifecycleStatus::Terminating;
            run.nodes[0].status = LifecycleStatus::Terminating;
            Ok(())
        })
        .expect("claim termination");

        let finished = finish_run(&scope, &run.id, LifecycleStatus::Done).expect("finish run");
        let started = set_process(&scope, &run.id, 42, "nonce", None).expect("set process");
        let updated = state::read(&scope)
            .expect("read terminating run")
            .runs
            .into_iter()
            .find(|candidate| candidate.id == run.id)
            .expect("terminating run");
        let reported = control::report_node(
            &scope,
            &run.id,
            "wait",
            None,
            control::NodeReport {
                status: LifecycleStatus::Done,
                output: None,
                message: None,
                tokens: None,
                cost_usd: None,
            },
        )
        .expect("ignore node report");
        let approved = approve(&config, &scope, &run.id, None, false)
            .expect("ignore approval during termination");

        assert_eq!(finished.status, LifecycleStatus::Terminating);
        assert_eq!(started.status, LifecycleStatus::Terminating);
        assert_eq!(updated.status, LifecycleStatus::Terminating);
        assert_eq!(reported.status, LifecycleStatus::Terminating);
        assert_eq!(approved.status, LifecycleStatus::Terminating);
        assert!(started.process_id.is_none());
        remove_fixture_state(&scope, &run.id);
    }

    #[test]
    fn failed_run_cannot_be_revived_by_a_stale_gate() {
        let (_directory, config, scope, run) = workflow_fixture("1ms");
        state::update(&scope, |workspace| {
            let run = workspace
                .runs
                .iter_mut()
                .find(|candidate| candidate.id == run.id)
                .expect("run");
            run.status = LifecycleStatus::Failed;
            run.pending_gates.push(PendingGate {
                id: "stale-approval".into(),
                before: "wait".into(),
                reason: "stale gate".into(),
                authority: GateAuthority::User,
                recommendation: None,
                created_at: Utc::now(),
            });
            Ok(())
        })
        .expect("create terminal run with stale gate");

        let error = approve(&config, &scope, &run.id, None, false)
            .expect_err("terminal run must not revive");

        assert!(error.to_string().contains("cannot approve"));
        let current = state::read(&scope)
            .expect("read run")
            .runs
            .into_iter()
            .find(|candidate| candidate.id == run.id)
            .expect("failed run");
        assert_eq!(current.status, LifecycleStatus::Failed);
        remove_fixture_state(&scope, &run.id);
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_escalates_when_a_tracked_command_ignores_termination() {
        let directory = tempfile::tempdir().expect("tracker directory");
        let tracker_directory = directory.path().join("trackers");
        let worker_directory = tracker_directory.clone();
        let plan = CommandPlan {
            version: "orc.provider/v1".into(),
            command: vec![
                "sh".into(),
                "-c".into(),
                "trap '' TERM; while :; do sleep 3600; done".into(),
            ],
            cwd: None,
            environment: BTreeMap::new(),
            success_codes: vec![0],
        };
        let worker = thread::spawn(move || {
            provider::run_plan_tracked(&plan, Path::new("."), Some(&worker_directory))
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let tracker_exists = fs::read_dir(&tracker_directory)
                .ok()
                .into_iter()
                .flatten()
                .filter_map(|entry| entry.ok())
                .any(|entry| {
                    entry.path().extension().and_then(|value| value.to_str()) == Some("process")
                });
            if tracker_exists {
                break;
            }
            assert!(Instant::now() < deadline, "tracked command did not start");
            thread::sleep(Duration::from_millis(10));
        }

        let started = Instant::now();
        terminate_tracked_processes(&tracker_directory).expect("terminate tracked process");
        let result = worker.join().expect("tracked command thread");

        assert!(
            result.is_ok(),
            "tracked runner must reconcile after cancellation"
        );
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(
            fs::read_dir(&tracker_directory)
                .expect("tracker directory")
                .filter_map(|entry| entry.ok())
                .filter(|entry| {
                    entry.path().extension().and_then(|value| value.to_str()) == Some("process")
                })
                .count(),
            0
        );
    }

    #[cfg(unix)]
    #[test]
    fn cancel_reconciles_nodes_and_linked_sessions() {
        use std::os::unix::fs::PermissionsExt;

        let (directory, config, scope, run) = workflow_fixture("1s");
        let marker = directory.path().join("managed-session-stopped");
        let provider = directory.path().join("stop-provider.sh");
        let plan = serde_json::to_string(&json!({
            "version": "orc.provider/v1",
            "command": [provider, "stop", marker],
            "cwd": scope,
            "environment": {},
            "successCodes": [0]
        }))
        .expect("stop plan");
        fs::write(
            &provider,
            render_fixture(STOP_PROVIDER, serde_json::json!({ "plan": plan })),
        )
        .expect("write stop provider");
        fs::set_permissions(&provider, fs::Permissions::from_mode(0o755))
            .expect("make stop provider executable");
        fs::write(
            config.providers.directory.join("stop.yaml"),
            render_fixture(
                STOP_PROVIDER_MANIFEST,
                serde_json::json!({ "command": provider.display().to_string() }),
            ),
        )
        .expect("write stop provider manifest");
        let linked = control::register(
            &scope,
            Contract {
                harness: "test".into(),
                role: SessionRole::Worker,
                title: "linked worker".into(),
                ..Contract::default()
            },
            SessionLink {
                id: Some(format!("worker-{}", Uuid::new_v4())),
                native_id: Some(Uuid::new_v4().to_string()),
                parent_id: run.orchestrator_id.clone(),
                run_id: Some(run.id.clone()),
                node_id: Some("wait".into()),
                source: RegistrationSource::Connected,
                ..SessionLink::default()
            },
        )
        .expect("register linked session");
        state::update(&scope, |workspace| {
            let run = workspace
                .runs
                .iter_mut()
                .find(|candidate| candidate.id == run.id)
                .expect("run");
            run.status = LifecycleStatus::Working;
            run.current_node = Some("wait".into());
            run.nodes[0].status = LifecycleStatus::Working;
            run.pending_gates.push(PendingGate {
                id: "approval".into(),
                before: "wait".into(),
                reason: "test".into(),
                authority: GateAuthority::User,
                recommendation: None,
                created_at: Utc::now(),
            });
            let session = workspace
                .sessions
                .iter_mut()
                .find(|session| session.id == linked.id)
                .expect("linked session");
            session.registration = RegistrationSource::Managed;
            session.providers.push(crate::domain::ProviderBinding {
                provider: "stop-test".into(),
                kind: crate::domain::ProviderKind::Persistence,
                r#ref: Some("stop-test".into()),
                status: crate::domain::BindingStatus::Active,
                label: "test stop ownership".into(),
            });
            Ok(())
        })
        .expect("mark work active");

        let cancelled = cancel(&config, &scope, &run.id).expect("cancel workflow");
        let workspace = state::read(&scope).expect("read cancelled workflow");
        let linked = workspace
            .sessions
            .iter()
            .find(|session| session.id == linked.id)
            .expect("linked session");
        assert_eq!(cancelled.status, LifecycleStatus::Cancelled);
        assert_eq!(cancelled.nodes[0].status, LifecycleStatus::Cancelled);
        assert!(cancelled.pending_gates.is_empty());
        assert_eq!(linked.status, LifecycleStatus::Cancelled);
        assert!(marker.exists(), "managed session stop provider must run");
        remove_fixture_state(&scope, &run.id);
    }

    #[test]
    fn cancel_reconciles_descendant_runs() {
        let (_directory, config, scope, run) = workflow_fixture("1s");
        let child_id = format!("run-{}", Uuid::new_v4());
        let failed_child_id = format!("run-{}", Uuid::new_v4());
        state::update(&scope, |workspace| {
            let mut child = workspace
                .runs
                .iter()
                .find(|candidate| candidate.id == run.id)
                .cloned()
                .expect("parent run");
            child.id.clone_from(&child_id);
            child.parent_run_id = Some(run.id.clone());
            child.status = LifecycleStatus::Working;
            workspace.runs.push(child);
            let mut failed_child = workspace
                .runs
                .iter()
                .find(|candidate| candidate.id == run.id)
                .cloned()
                .expect("parent run");
            failed_child.id.clone_from(&failed_child_id);
            failed_child.parent_run_id = Some(run.id.clone());
            failed_child.status = LifecycleStatus::Failed;
            failed_child.nodes[0].status = LifecycleStatus::Failed;
            workspace.runs.push(failed_child);
            Ok(())
        })
        .expect("add child run");

        cancel(&config, &scope, &run.id).expect("cancel run family");

        let workspace = state::read(&scope).expect("read run family");
        assert_eq!(
            workspace
                .runs
                .iter()
                .find(|candidate| candidate.id == child_id)
                .expect("child run")
                .status,
            LifecycleStatus::Cancelled
        );
        let failed_child = workspace
            .runs
            .iter()
            .find(|candidate| candidate.id == failed_child_id)
            .expect("failed child run");
        assert_eq!(failed_child.status, LifecycleStatus::Failed);
        assert_eq!(failed_child.nodes[0].status, LifecycleStatus::Failed);
        remove_fixture_state(&scope, &run.id);
        remove_fixture_state(&scope, &child_id);
        remove_fixture_state(&scope, &failed_child_id);
    }

    #[test]
    fn cancel_preserves_a_completed_run() {
        let (_directory, config, scope, run) = workflow_fixture("1ms");
        let completed = state::update(&scope, |workspace| {
            let run = workspace
                .runs
                .iter_mut()
                .find(|candidate| candidate.id == run.id)
                .expect("run");
            run.status = LifecycleStatus::Done;
            Ok(run.clone())
        })
        .expect("complete run");

        let result = cancel(&config, &scope, &run.id).expect("cancel completed run");

        assert_eq!(result.status, LifecycleStatus::Done);
        assert_eq!(result.updated_at, completed.updated_at);
        remove_fixture_state(&scope, &run.id);
    }

    #[test]
    fn cancel_preserves_a_failed_run() {
        let (_directory, config, scope, run) = workflow_fixture("1ms");
        let failed = state::update(&scope, |workspace| {
            let run = workspace
                .runs
                .iter_mut()
                .find(|candidate| candidate.id == run.id)
                .expect("run");
            run.status = LifecycleStatus::Failed;
            run.nodes[0].status = LifecycleStatus::Failed;
            Ok(run.clone())
        })
        .expect("fail run");

        let result = cancel(&config, &scope, &run.id).expect("cancel failed run");

        assert_eq!(result.status, LifecycleStatus::Failed);
        assert_eq!(result.nodes[0].status, LifecycleStatus::Failed);
        assert_eq!(result.updated_at, failed.updated_at);
        remove_fixture_state(&scope, &run.id);
    }
}
