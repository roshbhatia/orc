use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::Duration,
};

#[cfg(unix)]
use std::os::{fd::AsRawFd, unix::process::CommandExt};

use anyhow::{Context, Result, bail};
use chrono::Utc;
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::{
    config::Config,
    control::{self, Contract, SessionLink},
    domain::{
        ActivityEvent, CompletionTarget, JudgePolicy, LifecycleStatus, PendingGate,
        RegistrationSource, RunMode, SessionRole, WorkflowEdge, WorkflowNode, WorkflowRun,
    },
    preferences::{self, AutonomyMode},
    provider::{self, Action, CommandPlan},
    state,
};

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

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GateAuthority {
    User,
    OrchestratorThenUser,
    Orchestrator,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
pub struct ApprovalGate {
    pub id: String,
    pub before: String,
    pub reason: String,
    #[serde(default = "default_gate_authority")]
    pub authority: GateAuthority,
}

fn default_gate_authority() -> GateAuthority {
    GateAuthority::User
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, Serialize)]
#[serde(default)]
pub struct ApprovalPolicy {
    pub mode: ApprovalMode,
    pub gates: Vec<ApprovalGate>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, Serialize)]
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
    pub max_depth: Option<usize>,
    pub depends_on: Vec<String>,
    pub routes: Vec<Route>,
    pub retry: RetryPolicy,
    pub timeout_seconds: Option<u64>,
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
    if definition.limits.max_iterations == 0 || definition.limits.max_iterations > 500 {
        bail!("limits.max_iterations must be between 1 and 500");
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
        if matches!(step.r#type, StepKind::Agent)
            && step
                .runtime
                .execution
                .as_ref()
                .or(definition.defaults.runtime.execution.as_ref())
                .is_none()
        {
            bail!("agent {} needs an execution provider", step.name);
        }
        if matches!(step.r#type, StepKind::Script) && step.command.is_empty() {
            bail!("script {} needs command", step.name);
        }
        if matches!(step.r#type, StepKind::Workflow) {
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
        for route in &step.routes {
            if route.to != "$end" && route.to != "self" && !all.contains(route.to.as_str()) {
                bail!("{} routes to unknown step {}", step.name, route.to);
            }
        }
    }
    for group in &definition.parallel {
        if group.agents.is_empty() == group.agent.is_none() {
            bail!(
                "parallel {} must define agents or one for_each agent",
                group.name
            );
        }
        for agent in &group.agents {
            if !step_names.contains(agent.as_str()) {
                bail!("parallel {} references unknown step {agent}", group.name);
            }
        }
        if group.for_each.is_some() && group.agent.as_ref().is_none() {
            bail!("parallel {} for_each needs agent", group.name);
        }
    }
    for gate in &definition.approval.gates {
        if !all.contains(gate.before.as_str()) {
            bail!("gate {} references unknown step {}", gate.id, gate.before);
        }
    }
    Ok(())
}

pub fn repository(config: &Config) -> Result<PathBuf> {
    let repository = config.workflows.repository.clone();
    fs::create_dir_all(&repository)?;
    if !repository.join(".git").exists() {
        run_git(&repository, ["init", "--quiet"])?;
        fs::write(
            repository.join("README.md"),
            "# Orc workflows\n\nVersioned workflow definitions managed by Orc.\n",
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
    Ok(scope_directory(config, scope)?.join(format!("{name}.yaml")))
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
    Ok((run, path, definition))
}

pub fn edit_run_node(
    config: &Config,
    scope: &Path,
    run_id: &str,
    node_id: &str,
    edit: NodeEdit,
) -> Result<WorkflowNode> {
    let (_, _, mut definition) = run_definition(scope, run_id)?;
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
    save(config, scope, &definition)?;

    let step = definition
        .steps
        .iter()
        .find(|step| step.name == node_id)
        .expect("edited step remains");
    state::update(&state::resolve_scope(scope)?, |workspace| {
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
        run.updated_at = Utc::now();
        Ok(node.clone())
    })
}

pub fn delete_run_node(config: &Config, scope: &Path, run_id: &str, node_id: &str) -> Result<()> {
    let (_, _, mut definition) = run_definition(scope, run_id)?;
    if !definition.steps.iter().any(|step| step.name == node_id) {
        bail!("unknown node: {node_id}");
    }
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
    save(config, scope, &definition)?;
    state::update(&state::resolve_scope(scope)?, |workspace| {
        let run = workspace
            .runs
            .iter_mut()
            .find(|run| run.id == run_id)
            .with_context(|| format!("unknown run: {run_id}"))?;
        run.nodes.retain(|node| node.id != node_id);
        run.edges
            .retain(|edge| edge.from != node_id && edge.to != node_id);
        run.updated_at = Utc::now();
        Ok(())
    })
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
    let (_, _, mut definition) = run_definition(scope, run_id)?;
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
    save(config, scope, &definition)?;
    state::update(&state::resolve_scope(scope)?, |workspace| {
        let run = workspace
            .runs
            .iter_mut()
            .find(|run| run.id == run_id)
            .with_context(|| format!("unknown run: {run_id}"))?;
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
        run.updated_at = Utc::now();
        Ok(())
    })
}

pub fn init(config: &Config, scope: &Path, name: &str) -> Result<PathBuf> {
    let target = path(config, scope, name)?;
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
    definition.defaults.runtime.harness = Some("codex".into());
    definition.defaults.runtime.execution = Some("local".into());
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

pub fn plan(config: &Config, scope: &Path, definition: &Definition) -> Result<Plan> {
    let revision = config
        .workflows
        .repository
        .join(".git")
        .exists()
        .then(|| {
            run_git(
                &config.workflows.repository,
                ["rev-parse", "--short", "HEAD"],
            )
            .ok()
        })
        .flatten()
        .map(|value| value.trim().to_owned());
    let gates: BTreeSet<_> = definition
        .approval
        .gates
        .iter()
        .map(|gate| gate.before.as_str())
        .collect();
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
            approval_required: match definition.approval.mode {
                ApprovalMode::Autonomous => false,
                ApprovalMode::ApprovalGated => true,
                ApprovalMode::Supervised => {
                    step.requires_approval || gates.contains(step.name.as_str())
                }
            },
            depends_on: step.depends_on.clone(),
        })
        .collect::<Vec<_>>();
    let mut unresolved: BTreeSet<_> = steps.iter().map(|step| step.name.clone()).collect();
    let mut resolved = BTreeSet::new();
    let mut waves = Vec::new();
    while !unresolved.is_empty() {
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
    let _ = scope;
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
    let scope = state::resolve_scope(scope)?;
    let definition_path = fs::canonicalize(definition_path)
        .with_context(|| format!("resolve workflow definition {}", definition_path.display()))?;
    let definition = load(&definition_path)?;
    let planned = plan(config, &scope, &definition)?;
    let snapshot = state::read(&scope)?;
    let orchestrator = snapshot
        .current_session()
        .context("start a registered orchestrator before starting a workflow")?;
    let now = Utc::now();
    let run_id = format!("run-{}", &Uuid::new_v4().to_string()[..12]);
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
            status: LifecycleStatus::Queued,
            attempt: 0,
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
                    relationship: route
                        .when
                        .as_ref()
                        .map_or_else(|| "routes".into(), |when| format!("when {when}")),
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
        definition: Some(definition_path.display().to_string()),
        revision: planned.revision,
        checkpoint: None,
        mode,
        process_id: None,
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
        workspace.runs.insert(0, run.clone());
        Ok(run)
    })
}

fn required_gate(
    definition: &Definition,
    step: &Step,
    run: &WorkflowRun,
    autonomy: AutonomyMode,
) -> Option<PendingGate> {
    let explicit = definition
        .approval
        .gates
        .iter()
        .find(|gate| gate.before == step.name);
    let required = matches!(step.r#type, StepKind::HumanGate)
        || match autonomy {
            AutonomyMode::Supervised => true,
            AutonomyMode::ApprovalGated => step.requires_approval || explicit.is_some(),
            AutonomyMode::Autonomous => false,
        };
    if !required {
        return None;
    }
    let id = explicit
        .map(|gate| gate.id.clone())
        .unwrap_or_else(|| format!("{}:{}", definition.name, step.name));
    if run.approved_gates.contains(&id) {
        return None;
    }
    Some(PendingGate {
        id,
        before: step.name.clone(),
        reason: explicit.map_or_else(
            || format!("Approve {} before execution", step.name),
            |gate| gate.reason.clone(),
        ),
        recommendation: None,
        created_at: Utc::now(),
    })
}

fn dependencies_done(run: &WorkflowRun, step: &Step) -> bool {
    step.depends_on.iter().all(|dependency| {
        run.nodes
            .iter()
            .find(|node| node.id == *dependency)
            .is_some_and(|node| node.status == LifecycleStatus::Done)
    })
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
    Ok(Duration::from_millis(number.parse::<u64>()? * scale))
}

struct StepOutcome {
    output: serde_json::Value,
    session_id: Option<String>,
    summary: String,
}

const EXECUTION_LEASE_ENV: &str = "ORC_EXECUTION_LEASE";

struct ExecutionLease {
    path: PathBuf,
    nonce: String,
    armed: bool,
}

struct ExecutionLeaseGuard {
    #[cfg(unix)]
    file: fs::File,
}

impl ExecutionLeaseGuard {
    fn try_acquire(path: &Path) -> Result<Option<Self>> {
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

        for _ in 0..3 {
            let Some(_guard) = ExecutionLeaseGuard::try_acquire(path)? else {
                thread::sleep(Duration::from_millis(1));
                continue;
            };
            if let Some(record) = read_execution_lease(path)? {
                if inherited.as_deref() == Some(record.nonce.as_str()) {
                    let lease = Self {
                        path: path.to_owned(),
                        nonce: record.nonce,
                        armed: true,
                    };
                    lease.write_owner(std::process::id())?;
                    return Ok(Some(lease));
                }
                if process_is_live(record.process_id) {
                    return Ok(None);
                }
                remove_execution_lease(path, &record.nonce)?;
                continue;
            }

            let lease = Self {
                path: path.to_owned(),
                nonce: Uuid::new_v4().to_string(),
                armed: true,
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

fn execute_step(
    config: &Config,
    providers: &[provider::Manifest],
    scope: &Path,
    run: &WorkflowRun,
    definition_path: &Path,
    definition: &Definition,
    step: &Step,
) -> Result<StepOutcome> {
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
                    "Goal: {}\nExpected output: {}\nSuccess criteria:\n{}",
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
            let session = control::register(
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
                    source: RegistrationSource::Managed,
                    ..SessionLink::default()
                },
            )?;
            let request = json!({
                "version": "orc.provider/v1",
                "action": "launch",
                "scope": scope,
                "session": session,
                "command": [harness, prompt],
                "prompt": prompt,
                "environment": {
                    "ORC_SCOPE": scope,
                    "ORC_SESSION_ID": session.id,
                    "ORC_NATIVE_SESSION_ID": native_id,
                    "ORC_PARENT_SESSION_ID": run.orchestrator_id,
                    "ORC_RUN_ID": run.id,
                    "ORC_NODE_ID": step.name,
                },
            });
            let plan = provider::resolve_plan(config, providers, Action::Launch, request)?;
            let result = provider::run_plan(&plan, scope)?;
            control::update_session(
                scope,
                &session.id,
                if plan.accepts(result.code) {
                    LifecycleStatus::Done
                } else {
                    LifecycleStatus::Failed
                },
            )?;
            if !plan.accepts(result.code) {
                bail!(
                    "agent exited with {}: {}",
                    result.code,
                    result.stderr.trim()
                );
            }
            Ok(StepOutcome {
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
            let request = json!({
                "version": "orc.provider/v1",
                "action": "execute",
                "scope": scope,
                "step": step.name,
            });
            let plan = provider::resolve_plan_from(
                config,
                providers,
                Action::Execute,
                request,
                Some(initial),
            )?;
            let result = provider::run_plan(&plan, scope)?;
            if !plan.accepts(result.code) {
                bail!(
                    "command exited with {}: {}",
                    result.code,
                    result.stderr.trim()
                );
            }
            Ok(StepOutcome {
                output: json!({ "stdout": result.stdout, "stderr": result.stderr }),
                session_id: None,
                summary: "command completed".into(),
            })
        }
        StepKind::Set => Ok(StepOutcome {
            output: step.value.clone().unwrap_or(serde_json::Value::Null),
            session_id: None,
            summary: "value recorded".into(),
        }),
        StepKind::Wait => {
            thread::sleep(parse_duration(step.duration.as_deref())?);
            Ok(StepOutcome {
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
            let child = materialize(config, scope, &child_path, RunMode::Foreground)?;
            let child = execute(config, scope, &child.id)?;
            if child.status != LifecycleStatus::Done {
                bail!("sub-workflow {} stopped as {}", child.name, child.status);
            }
            Ok(StepOutcome {
                output: serde_json::to_value(&child)?,
                session_id: None,
                summary: format!("sub-workflow {} completed", child.name),
            })
        }
        StepKind::HumanGate => Ok(StepOutcome {
            output: json!({ "approved": true }),
            session_id: None,
            summary: "human gate approved".into(),
        }),
        StepKind::Terminate => Ok(StepOutcome {
            output: json!({ "terminated": true }),
            session_id: None,
            summary: "workflow termination step completed".into(),
        }),
    }
}

pub fn execute(config: &Config, scope: &Path, run_id: &str) -> Result<WorkflowRun> {
    let scope = state::resolve_scope(scope)?;
    let initial = find_run(&scope, run_id)?;
    if !initial.status.active() {
        return Ok(initial);
    }
    if initial
        .process_id
        .is_some_and(|process_id| process_id != std::process::id() && process_is_live(process_id))
    {
        return Ok(initial);
    }
    let Some(_lease) = ExecutionLease::acquire(&scope, run_id)? else {
        return find_run(&scope, run_id);
    };
    let autonomy = preferences::read(&scope)?.autonomy;
    state::update(&scope, |workspace| {
        let run = workspace
            .runs
            .iter_mut()
            .find(|run| run.id == run_id)
            .with_context(|| format!("unknown run: {run_id}"))?;
        if !run.status.active() {
            return Ok(());
        }
        run.process_id = Some(std::process::id());
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
        if !run.status.active() {
            return Ok(run);
        }
        let definition_path = PathBuf::from(
            run.definition
                .as_deref()
                .context("run has no workflow definition")?,
        );
        let definition = load(&definition_path)?;
        if run
            .nodes
            .iter()
            .all(|node| node.status == LifecycleStatus::Done)
        {
            return finish_run(&scope, run_id, LifecycleStatus::Done);
        }
        if run
            .nodes
            .iter()
            .any(|node| node.status == LifecycleStatus::Failed)
        {
            return finish_run(&scope, run_id, LifecycleStatus::Failed);
        }
        if !run.pending_gates.is_empty() {
            return finish_run(&scope, run_id, LifecycleStatus::Waiting);
        }
        let ready = definition
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
                        )
                    })
                    && dependencies_done(&run, step)
            })
            .cloned()
            .collect::<Vec<_>>();
        if ready.is_empty() {
            return finish_run(&scope, run_id, LifecycleStatus::Blocked);
        }
        if let Some(gate) = ready
            .iter()
            .find_map(|step| required_gate(&definition, step, &run, autonomy))
        {
            let before = gate.before.clone();
            state::update(&scope, |workspace| {
                let run = workspace
                    .runs
                    .iter_mut()
                    .find(|run| run.id == run_id)
                    .context("run disappeared")?;
                run.pending_gates.push(gate.clone());
                run.status = LifecycleStatus::Waiting;
                run.process_id = None;
                run.current_node = Some(before.clone());
                if let Some(node) = run.nodes.iter_mut().find(|node| node.id == before) {
                    node.status = LifecycleStatus::Waiting;
                    node.updated_at = Utc::now();
                    node.activity.push(ActivityEvent {
                        at: Utc::now(),
                        kind: "gate".into(),
                        message: gate.reason.clone(),
                    });
                }
                Ok(())
            })?;
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
        state::update(&scope, |workspace| {
            let run = workspace
                .runs
                .iter_mut()
                .find(|run| run.id == run_id)
                .context("run disappeared")?;
            if !run.status.active() {
                return Ok(());
            }
            run.status = LifecycleStatus::Working;
            run.current_node = names.first().cloned();
            for node in run.nodes.iter_mut().filter(|node| names.contains(&node.id)) {
                node.status = LifecycleStatus::Working;
                node.attempt += 1;
                node.updated_at = Utc::now();
                node.activity.push(ActivityEvent {
                    at: Utc::now(),
                    kind: "started".into(),
                    message: format!("attempt {} started", node.attempt),
                });
            }
            Ok(())
        })?;
        let results = thread::scope(|thread_scope| {
            let handles = ready
                .iter()
                .map(|step| {
                    let name = step.name.clone();
                    thread_scope.spawn(|| {
                        (
                            name,
                            execute_step(
                                config,
                                &providers,
                                &scope,
                                &run,
                                &definition_path,
                                &definition,
                                step,
                            ),
                        )
                    })
                })
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .map(|handle| handle.join().expect("workflow worker panicked"))
                .collect::<Vec<_>>()
        });
        state::update(&scope, |workspace| {
            let run = workspace
                .runs
                .iter_mut()
                .find(|run| run.id == run_id)
                .context("run disappeared")?;
            if !run.status.active() {
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
                match result {
                    Ok(outcome) => {
                        node.status = LifecycleStatus::Done;
                        node.output = Some(outcome.output.clone());
                        node.session_id.clone_from(&outcome.session_id);
                        node.activity.push(ActivityEvent {
                            at: Utc::now(),
                            kind: "completed".into(),
                            message: outcome.summary.clone(),
                        });
                    }
                    Err(error) if node.attempt <= step.retry.attempts => {
                        node.status = LifecycleStatus::Queued;
                        node.activity.push(ActivityEvent {
                            at: Utc::now(),
                            kind: "retry".into(),
                            message: format!("{error:#}"),
                        });
                    }
                    Err(error) => {
                        node.status = LifecycleStatus::Failed;
                        node.activity.push(ActivityEvent {
                            at: Utc::now(),
                            kind: "failed".into(),
                            message: format!("{error:#}"),
                        });
                    }
                }
                node.updated_at = Utc::now();
            }
            run.updated_at = Utc::now();
            Ok(())
        })?;
    }
}

fn find_run(scope: &Path, run_id: &str) -> Result<WorkflowRun> {
    state::read(scope)?
        .runs
        .into_iter()
        .find(|run| run.id == run_id)
        .with_context(|| format!("unknown run: {run_id}"))
}

fn finish_run(scope: &Path, run_id: &str, status: LifecycleStatus) -> Result<WorkflowRun> {
    state::update(scope, |workspace| {
        let run = workspace
            .runs
            .iter_mut()
            .find(|run| run.id == run_id)
            .with_context(|| format!("unknown run: {run_id}"))?;
        run.status = status;
        run.current_node = None;
        run.process_id = None;
        run.updated_at = Utc::now();
        Ok(run.clone())
    })
}

pub fn approve(
    config: &Config,
    scope: &Path,
    run_id: &str,
    gate_id: Option<&str>,
    resume: bool,
) -> Result<WorkflowRun> {
    let scope = state::resolve_scope(scope)?;
    let run = state::update(&scope, |workspace| {
        let run = workspace
            .runs
            .iter_mut()
            .find(|run| run.id == run_id)
            .with_context(|| format!("unknown run: {run_id}"))?;
        let index = run
            .pending_gates
            .iter()
            .position(|gate| gate_id.is_none_or(|id| gate.id == id))
            .context("run has no matching pending gate")?;
        let gate = run.pending_gates.remove(index);
        run.approved_gates.push(gate.id);
        if let Some(node) = run.nodes.iter_mut().find(|node| node.id == gate.before) {
            node.status = LifecycleStatus::Queued;
            node.activity.push(ActivityEvent {
                at: Utc::now(),
                kind: "approved".into(),
                message: "gate approved".into(),
            });
        }
        run.status = LifecycleStatus::Queued;
        run.updated_at = Utc::now();
        Ok(run.clone())
    })?;
    if resume {
        execute(config, &scope, run_id)
    } else {
        Ok(run)
    }
}

pub fn cancel(scope: &Path, run_id: &str) -> Result<WorkflowRun> {
    let scope = state::resolve_scope(scope)?;
    let process_id = find_run(&scope, run_id)?.process_id;
    if let Some(process_id) = process_id.filter(|id| *id != std::process::id()) {
        terminate_executor(process_id)?;
    }
    state::update(&scope, |workspace| {
        for session in workspace
            .sessions
            .iter_mut()
            .filter(|session| session.run_id.as_deref() == Some(run_id) && session.status.active())
        {
            session.status = LifecycleStatus::Cancelled;
            session.updated_at = Utc::now();
        }
        let run = workspace
            .runs
            .iter_mut()
            .find(|run| run.id == run_id)
            .with_context(|| format!("unknown run: {run_id}"))?;
        for node in run.nodes.iter_mut().filter(|node| node.status.active()) {
            node.status = LifecycleStatus::Cancelled;
            node.updated_at = Utc::now();
            node.activity.push(ActivityEvent {
                at: Utc::now(),
                kind: "cancelled".into(),
                message: "run cancelled".into(),
            });
        }
        run.status = LifecycleStatus::Cancelled;
        run.current_node = None;
        run.process_id = None;
        run.pending_gates.clear();
        run.updated_at = Utc::now();
        Ok(run.clone())
    })
}

fn terminate_executor(process_id: u32) -> Result<()> {
    let target = process_group_target(process_id).unwrap_or(process_id as i32);
    if signal_target(target, 15).is_err() && process_is_live(process_id) {
        bail!("could not stop workflow executor {process_id}");
    }
    Ok(())
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
    (process_group == process_id as i32).then_some(-process_group)
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
    log_path: Option<&Path>,
) -> Result<WorkflowRun> {
    let scope = state::resolve_scope(scope)?;
    state::update(&scope, |workspace| {
        let run = workspace
            .runs
            .iter_mut()
            .find(|run| run.id == run_id)
            .with_context(|| format!("unknown run: {run_id}"))?;
        if !run.status.active() {
            return Ok(run.clone());
        }
        run.process_id = Some(process_id);
        run.log_path = log_path.map(|path| path.display().to_string());
        run.status = LifecycleStatus::Working;
        run.updated_at = Utc::now();
        Ok(run.clone())
    })
}

pub fn fail(scope: &Path, run_id: &str, error: &anyhow::Error) -> Result<WorkflowRun> {
    let scope = state::resolve_scope(scope)?;
    state::update(&scope, |workspace| {
        let run = workspace
            .runs
            .iter_mut()
            .find(|run| run.id == run_id)
            .with_context(|| format!("unknown run: {run_id}"))?;
        if let Some(node) = run
            .current_node
            .as_ref()
            .and_then(|id| run.nodes.iter_mut().find(|node| &node.id == id))
        {
            node.status = LifecycleStatus::Failed;
            node.activity.push(ActivityEvent {
                at: Utc::now(),
                kind: "failed".into(),
                message: format!("{error:#}"),
            });
        }
        run.status = LifecycleStatus::Failed;
        run.process_id = None;
        run.updated_at = Utc::now();
        Ok(run.clone())
    })
}

pub fn spawn(scope: &Path, run_id: &str) -> Result<WorkflowRun> {
    let scope = state::resolve_scope(scope)?;
    let initial = find_run(&scope, run_id)?;
    if !initial.status.active() {
        return Ok(initial);
    }
    if initial.process_id.is_some_and(process_is_live) {
        return Ok(initial);
    }
    let Some(mut lease) = ExecutionLease::acquire(&scope, run_id)? else {
        return find_run(&scope, run_id);
    };
    let log_directory = crate::config::state_home()
        .join("orc/logs")
        .join(state::scope_key(&scope));
    fs::create_dir_all(&log_directory)?;
    let log_path = log_directory.join(format!("{run_id}.log"));
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
        .env(EXECUTION_LEASE_ENV, &lease.nonce);
    #[cfg(unix)]
    command.process_group(0);
    let child = command
        .spawn()
        .context("start background workflow executor")?;
    if let Err(error) = lease.write_owner(child.id()) {
        let _ = terminate_executor(child.id());
        return Err(error);
    }
    let run = set_process(&scope, run_id, child.id(), Some(&log_path));
    if run.is_err() {
        let _ = terminate_executor(child.id());
    } else {
        lease.disarm();
    }
    run
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier, atomic::AtomicUsize, atomic::Ordering};

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
        preferences::write(
            &scope,
            &preferences::WorkspacePreferences {
                autonomy: AutonomyMode::Autonomous,
                ..preferences::WorkspacePreferences::default()
            },
        )
        .expect("write autonomous preferences");
        let definition = Definition {
            name: "concurrent".into(),
            goal: "execute one wave once".into(),
            entry_point: "wait".into(),
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

    fn remove_fixture_state(scope: &Path, run_id: &str) {
        let _ = fs::remove_file(state::path(scope));
        let _ = fs::remove_file(preferences::path(scope));
        let _ = fs::remove_file(execution_lease_path(scope, run_id));
    }

    #[test]
    fn plan_groups_independent_steps() {
        let definition = Definition {
            name: "x".into(),
            goal: "x".into(),
            entry_point: "a".into(),
            defaults: WorkflowDefaults {
                runtime: Runtime {
                    harness: Some("codex".into()),
                    execution: Some("local".into()),
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
    fn plan_rejects_dependency_cycles() {
        let definition = Definition {
            name: "cycle".into(),
            goal: "reject cycles".into(),
            entry_point: "a".into(),
            defaults: WorkflowDefaults {
                runtime: Runtime {
                    harness: Some("codex".into()),
                    execution: Some("local".into()),
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
    fn cancel_reconciles_nodes_and_linked_sessions() {
        let (_directory, _config, scope, run) = workflow_fixture("1s");
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
                run_id: Some(run.id.clone()),
                node_id: Some("wait".into()),
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
                recommendation: None,
                created_at: Utc::now(),
            });
            Ok(())
        })
        .expect("mark work active");

        let cancelled = cancel(&scope, &run.id).expect("cancel workflow");
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
        remove_fixture_state(&scope, &run.id);
    }
}
