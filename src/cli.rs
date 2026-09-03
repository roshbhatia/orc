use std::{
    env, fs,
    io::{self, Read},
    path::PathBuf,
    process::Command,
};

use anyhow::{Context, Result, bail};
use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{Shell, generate};
use clap_complete_nushell::Nushell;

use crate::{
    VERSION,
    config::{self, Config},
    control::{self, Contract, SessionLink},
    domain::{CompletionTarget, JudgePolicy, LifecycleStatus, RegistrationSource, SessionRole},
    mcp,
    preferences::{self, AutonomyMode},
    provider::{self, Action},
    state, tui, workflow,
};

#[derive(Parser)]
#[command(name = "orc", version = VERSION, about = "Local control plane for agent harnesses")]
pub struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    #[command(about = "Open the control plane")]
    Tui(TuiArgs),
    #[command(about = "Show workspace status")]
    Status(OutputArgs),
    #[command(about = "Show or change this workspace's autonomy mode")]
    Mode {
        mode: Option<AutonomyMode>,
        #[command(flatten)]
        scope: ScopeArgs,
    },
    #[command(about = "List registered sessions")]
    List(OutputArgs),
    #[command(about = "Register the current session")]
    Connect(RegisterArgs),
    #[command(about = "Manage sessions")]
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },
    #[command(about = "Manage workflow runs")]
    Run {
        #[command(subcommand)]
        command: RunCommand,
    },
    #[command(about = "Manage workflow nodes")]
    Node {
        #[command(subcommand)]
        command: NodeCommand,
    },
    #[command(about = "Manage provider manifests")]
    Provider {
        #[command(subcommand)]
        command: ProviderCommand,
    },
    #[command(about = "Manage versioned workflow definitions")]
    Workflow {
        #[command(subcommand)]
        command: WorkflowCommand,
    },
    #[command(about = "Launch a managed harness")]
    Launch(LaunchArgs),
    #[command(about = "Attach through a provider chain")]
    Attach(AttachArgs),
    #[command(about = "Inspect through a provider chain")]
    Inspect(AttachArgs),
    #[command(about = "Disconnect a registered session")]
    Disconnect {
        id: Option<String>,
        #[command(flatten)]
        scope: ScopeArgs,
    },
    #[command(about = "Send mid-run guidance")]
    Guide(GuideArgs),
    #[command(about = "Run the MCP server")]
    Mcp,
    #[command(about = "Print the prompt session marker")]
    Prompt(ScopeArgs),
    #[command(about = "Generate shell completions")]
    Completion { shell: CompletionTargetShell },
    #[command(about = "Print generated JSON schemas")]
    Schema { schema: SchemaTarget },
    #[command(hide = true)]
    Generate {
        #[arg(long, default_value = ".")]
        root: PathBuf,
        #[arg(long)]
        check: bool,
    },
}

#[derive(Args)]
struct TuiArgs {
    #[command(flatten)]
    scope: ScopeArgs,
    #[arg(long, hide = true)]
    loading_preview: bool,
}

#[derive(Clone, Args)]
struct ScopeArgs {
    #[arg(long, env = "ORC_SCOPE", default_value = ".")]
    scope: PathBuf,
}

#[derive(Args)]
struct OutputArgs {
    #[command(flatten)]
    scope: ScopeArgs,
    #[arg(long)]
    json: bool,
}

#[derive(Clone, Args)]
struct ContractArgs {
    #[arg(long, default_value = "unknown")]
    harness: String,
    #[arg(long)]
    model: Option<String>,
    #[arg(long, default_value = "worker")]
    role: SessionRole,
    #[arg(long, default_value = "Agent session")]
    title: String,
    #[arg(long, default_value = "Agent session")]
    purpose: String,
    #[arg(long, default_value = "Complete the assigned work")]
    goal: String,
    #[arg(long = "expected-output", default_value = "A verified result")]
    expected_output: String,
    #[arg(long = "success")]
    success_criteria: Vec<String>,
    #[arg(long, default_value = "orchestrator")]
    completion: CompletionTarget,
    #[arg(long = "review-by")]
    review_by: Option<String>,
}

impl From<ContractArgs> for Contract {
    fn from(value: ContractArgs) -> Self {
        Self {
            harness: value.harness,
            model: value.model,
            role: value.role,
            title: value.title,
            purpose: value.purpose,
            goal: value.goal,
            expected_output: value.expected_output,
            success_criteria: value.success_criteria,
            completion: value.completion,
            review_by: value.review_by,
        }
    }
}

#[derive(Args)]
struct RegisterArgs {
    #[command(flatten)]
    scope: ScopeArgs,
    #[command(flatten)]
    contract: ContractArgs,
    #[arg(long)]
    id: Option<String>,
    #[arg(long = "native-id")]
    native_id: Option<String>,
    #[arg(long = "parent")]
    parent_id: Option<String>,
    #[arg(long = "run")]
    run_id: Option<String>,
    #[arg(long = "node")]
    node_id: Option<String>,
    #[arg(long = "provider-ref")]
    provider_ref: Option<String>,
    #[arg(long = "source", default_value = "connected")]
    source: RegistrationSource,
    #[arg(long = "hook-input")]
    hook_input: bool,
    #[arg(long)]
    quiet: bool,
}

#[derive(Subcommand)]
enum SessionCommand {
    Register(RegisterArgs),
    Adopt {
        #[command(flatten)]
        scope: ScopeArgs,
        #[command(flatten)]
        contract: ContractArgs,
        #[arg(long = "native-id")]
        native_id: Option<String>,
    },
    Archive {
        id: Option<String>,
        #[command(flatten)]
        scope: ScopeArgs,
        #[arg(long = "native-id")]
        native_id: Option<String>,
        #[arg(long = "hook-input")]
        hook_input: bool,
        #[arg(long)]
        quiet: bool,
    },
    #[command(about = "Stop an active agent through its provider, then archive it")]
    Prune {
        id: String,
        #[command(flatten)]
        scope: ScopeArgs,
    },
    Current(OutputArgs),
    List(OutputArgs),
    Update {
        id: String,
        #[command(flatten)]
        scope: ScopeArgs,
        #[arg(long)]
        status: LifecycleStatus,
    },
}

#[derive(Subcommand)]
enum RunCommand {
    Create {
        #[command(flatten)]
        scope: ScopeArgs,
        #[arg(long)]
        name: String,
        #[arg(long)]
        goal: String,
        #[arg(long = "expected-output")]
        expected_output: String,
        #[arg(long = "orchestrator")]
        orchestrator_id: Option<String>,
        #[arg(long)]
        harness: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        json: bool,
    },
    Agent {
        id: String,
        #[command(flatten)]
        scope: ScopeArgs,
        #[arg(long)]
        role: SessionRole,
        #[arg(long)]
        harness: String,
        #[arg(long)]
        model: Option<String>,
    },
    List(OutputArgs),
    Show {
        id: String,
        #[command(flatten)]
        scope: ScopeArgs,
        #[arg(long)]
        json: bool,
    },
    Update {
        id: String,
        #[command(flatten)]
        scope: ScopeArgs,
        #[arg(long)]
        status: LifecycleStatus,
    },
    #[command(hide = true)]
    Execute {
        id: String,
        #[command(flatten)]
        scope: ScopeArgs,
    },
    Resume {
        id: String,
        #[command(flatten)]
        scope: ScopeArgs,
    },
    Approve {
        id: String,
        #[command(flatten)]
        scope: ScopeArgs,
        #[arg(long)]
        gate: Option<String>,
        #[arg(long)]
        no_resume: bool,
    },
    Cancel {
        id: String,
        #[command(flatten)]
        scope: ScopeArgs,
    },
}

#[derive(Subcommand)]
enum NodeCommand {
    Upsert {
        id: String,
        #[command(flatten)]
        scope: ScopeArgs,
        #[arg(long = "run")]
        run_id: String,
        #[command(flatten)]
        contract: Box<ContractArgs>,
        #[arg(long = "session")]
        session_id: Option<String>,
        #[arg(long, default_value = "queued")]
        status: LifecycleStatus,
        #[arg(long, default_value_t = 0)]
        attempt: u32,
        #[arg(long = "depends-on")]
        depends_on: Vec<String>,
        #[arg(long)]
        execution: Option<String>,
        #[arg(long = "judge-policy", default_value = "llm")]
        judge_policy: JudgePolicy,
    },
    Update {
        id: String,
        #[command(flatten)]
        scope: ScopeArgs,
        #[arg(long = "run")]
        run_id: String,
        #[arg(long)]
        status: LifecycleStatus,
    },
    Edit {
        id: String,
        #[command(flatten)]
        scope: ScopeArgs,
        #[arg(long = "run")]
        run_id: String,
        #[arg(long)]
        goal: Option<String>,
        #[arg(long = "expected-output")]
        expected_output: Option<String>,
        #[arg(long = "success")]
        success_criteria: Option<Vec<String>>,
        #[arg(long)]
        harness: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        execution: Option<String>,
        #[arg(long = "judge-policy")]
        judge_policy: Option<JudgePolicy>,
    },
    Delete {
        id: String,
        #[command(flatten)]
        scope: ScopeArgs,
        #[arg(long = "run")]
        run_id: String,
    },
    Dependency {
        id: String,
        #[command(flatten)]
        scope: ScopeArgs,
        #[arg(long = "run")]
        run_id: String,
        #[arg(long)]
        on: String,
        #[arg(long)]
        remove: bool,
    },
}

#[derive(Subcommand)]
enum ProviderCommand {
    List(OutputArgs),
    Validate {
        name: Option<String>,
        #[command(flatten)]
        scope: ScopeArgs,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum WorkflowCommand {
    Init {
        name: String,
        #[command(flatten)]
        scope: ScopeArgs,
    },
    Import {
        path: PathBuf,
        #[command(flatten)]
        scope: ScopeArgs,
    },
    List {
        #[command(flatten)]
        scope: ScopeArgs,
    },
    Show {
        name: String,
        #[command(flatten)]
        scope: ScopeArgs,
    },
    Edit {
        name: String,
        #[command(flatten)]
        scope: ScopeArgs,
    },
    Validate {
        workflow: PathBuf,
    },
    Plan {
        workflow: PathBuf,
        #[command(flatten)]
        scope: ScopeArgs,
        #[arg(long)]
        json: bool,
    },
    Start {
        workflow: PathBuf,
        #[command(flatten)]
        scope: ScopeArgs,
        #[arg(long)]
        background: bool,
        #[arg(long)]
        json: bool,
    },
    History {
        name: String,
        #[command(flatten)]
        scope: ScopeArgs,
    },
    Search {
        query: String,
    },
    Path {
        name: String,
        #[command(flatten)]
        scope: ScopeArgs,
    },
}

#[derive(Args)]
struct LaunchArgs {
    harness: String,
    #[command(flatten)]
    scope: ScopeArgs,
    #[arg(long)]
    model: Option<String>,
    #[arg(long)]
    managed: Option<String>,
    #[arg(last = true)]
    args: Vec<String>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum SplitDirection {
    Right,
    Left,
    Top,
    Bottom,
}
impl std::fmt::Display for SplitDirection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Right => "right",
            Self::Left => "left",
            Self::Top => "top",
            Self::Bottom => "bottom",
        })
    }
}

#[derive(Args)]
struct AttachArgs {
    id: String,
    #[command(flatten)]
    scope: ScopeArgs,
    #[arg(long, default_value = "right")]
    direction: SplitDirection,
}

#[derive(Args)]
struct GuideArgs {
    id: String,
    #[command(flatten)]
    scope: ScopeArgs,
    #[arg(long)]
    text: String,
}

#[derive(Clone, Copy, ValueEnum)]
enum CompletionTargetShell {
    Bash,
    Zsh,
    Fish,
    Nu,
}

#[derive(Clone, Copy, ValueEnum)]
enum SchemaTarget {
    Config,
    Provider,
    Workflow,
    State,
}

fn hook_context(input: bool) -> Result<serde_json::Value> {
    if !input {
        return Ok(serde_json::json!({}));
    }
    let mut source = String::new();
    io::stdin().read_to_string(&mut source)?;
    if source.trim().is_empty() {
        Ok(serde_json::json!({}))
    } else {
        Ok(serde_json::from_str(&source)?)
    }
}

fn register(config: &Config, mut args: RegisterArgs) -> Result<Option<String>> {
    let hook = hook_context(args.hook_input)?;
    if let Some(directory) = hook
        .get("cwd")
        .or_else(|| hook.get("directory"))
        .and_then(serde_json::Value::as_str)
    {
        args.scope.scope = directory.into();
    }
    if let Some(native) = hook
        .get("session_id")
        .or_else(|| hook.get("thread_id"))
        .and_then(serde_json::Value::as_str)
    {
        args.native_id = Some(native.into());
    }
    if let Some(goal) = hook
        .get("goal")
        .or_else(|| hook.get("prompt"))
        .and_then(serde_json::Value::as_str)
    {
        args.contract.goal = goal.into();
    }
    if let Some(title) = hook
        .get("title")
        .or_else(|| hook.get("summary"))
        .and_then(serde_json::Value::as_str)
    {
        args.contract.title = title.into();
    }
    if args.hook_input && env::var_os("ORC_SCOPE").is_none() {
        let scope = state::resolve_scope(&args.scope.scope)?;
        if !state::read(&scope)?.active {
            return Ok(None);
        }
    }
    let session = control::register(
        &args.scope.scope,
        args.contract.into(),
        SessionLink {
            id: args.id,
            native_id: args.native_id,
            parent_id: args.parent_id,
            run_id: args.run_id,
            node_id: args.node_id,
            provider_ref: args.provider_ref,
            source: args.source,
        },
    )?;
    let _ = control::reconcile_with_current(config, &args.scope.scope, true);
    Ok((!args.quiet).then_some(session.id))
}

fn print_json(value: &impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

pub fn run() -> Result<u8> {
    let cli = Cli::parse();
    let config = config::load()?;
    match cli.command.unwrap_or(Commands::Tui(TuiArgs {
        scope: ScopeArgs {
            scope: env::current_dir()?,
        },
        loading_preview: false,
    })) {
        Commands::Tui(args) => {
            if args.loading_preview {
                tui::preview_loading()?;
            } else {
                tui::run(config, &args.scope.scope)?;
            }
        }
        Commands::Status(args) => {
            let state = control::read_workspace(&args.scope.scope)?;
            if args.json {
                print_json(&state)?;
            } else {
                println!(
                    "{} · {} working · {} sessions · {} runs · {}",
                    if state.active { "active" } else { "idle" },
                    state.active_sessions().count(),
                    state.sessions.len(),
                    state.runs.len(),
                    state.scope
                );
            }
        }
        Commands::Mode { mode, scope } => {
            let scope = state::resolve_scope(&scope.scope)?;
            let mut selected = preferences::read(&scope)?;
            if let Some(mode) = mode {
                selected.autonomy = mode;
                preferences::write(&scope, &selected)?;
            }
            println!("{}", selected.autonomy);
        }
        Commands::List(args) => list_sessions(&args)?,
        Commands::Connect(args) => {
            if let Some(id) = register(&config, args)? {
                println!("{id}");
            }
        }
        Commands::Session { command } => match command {
            SessionCommand::Register(args) => {
                if let Some(id) = register(&config, args)? {
                    println!("{id}");
                }
            }
            SessionCommand::Adopt {
                scope,
                contract,
                native_id,
            } => {
                let session = control::adopt(&scope.scope, contract.into(), native_id)?;
                let _ = control::reconcile_with_current(&config, &scope.scope, true);
                println!("{}", session.id);
            }
            SessionCommand::Archive {
                id,
                scope,
                mut native_id,
                hook_input,
                quiet,
            } => {
                let hook = hook_context(hook_input)?;
                native_id = native_id.or_else(|| {
                    hook.get("session_id")
                        .or_else(|| hook.get("thread_id"))
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                });
                if id.is_none() && native_id.is_none() {
                    return Ok(0);
                }
                let session = control::archive(&scope.scope, id.as_deref(), native_id.as_deref())?;
                if !quiet {
                    println!("{}", session.id);
                }
            }
            SessionCommand::Current(args) => {
                let state = control::read_workspace(&args.scope.scope)?;
                let session = state.current_session().context("no current Orc session")?;
                if args.json {
                    print_json(session)?;
                } else {
                    println!("{}", session.id);
                }
            }
            SessionCommand::Prune { id, scope } => {
                println!("{}", control::prune(&config, &scope.scope, &id)?.id)
            }
            SessionCommand::List(args) => list_sessions(&args)?,
            SessionCommand::Update { id, scope, status } => {
                print_json(&control::update_session(&scope.scope, &id, status)?)?
            }
        },
        Commands::Run { command } => match command {
            RunCommand::Create {
                scope,
                name,
                goal,
                expected_output,
                orchestrator_id,
                harness,
                model,
                json,
            } => {
                let run = control::create_run(
                    &scope.scope,
                    name,
                    goal,
                    expected_output,
                    orchestrator_id,
                    harness,
                    model,
                )?;
                if json {
                    print_json(&run)?;
                } else {
                    println!("{}", run.id);
                }
            }
            RunCommand::Agent {
                id,
                scope,
                role,
                harness,
                model,
            } => print_json(&control::set_run_agent(
                &scope.scope,
                &id,
                role,
                harness,
                model,
            )?)?,
            RunCommand::List(args) => {
                let state = control::read_workspace(&args.scope.scope)?;
                if args.json {
                    print_json(&state.runs)?;
                } else {
                    for run in state.runs {
                        println!("{}\t{}\t{}", run.id, run.status, run.name);
                    }
                }
            }
            RunCommand::Show { id, scope, json } => {
                let state = control::read_workspace(&scope.scope)?;
                let run = state
                    .runs
                    .iter()
                    .find(|run| run.id == id)
                    .with_context(|| format!("unknown run: {id}"))?;
                if json {
                    print_json(run)?;
                } else {
                    println!("{}\n{}\n{} steps", run.name, run.goal, run.nodes.len());
                }
            }
            RunCommand::Update { id, scope, status } => {
                print_json(&control::update_run(&scope.scope, &id, status)?)?
            }
            RunCommand::Execute { id, scope } | RunCommand::Resume { id, scope } => {
                match workflow::execute(&config, &scope.scope, &id) {
                    Ok(run) => println!("{}\t{}", run.id, run.status),
                    Err(error) => {
                        let _ = workflow::fail(&scope.scope, &id, &error);
                        return Err(error);
                    }
                }
            }
            RunCommand::Approve {
                id,
                scope,
                gate,
                no_resume,
            } => print_json(&workflow::approve(
                &config,
                &scope.scope,
                &id,
                gate.as_deref(),
                !no_resume,
            )?)?,
            RunCommand::Cancel { id, scope } => print_json(&workflow::cancel(&scope.scope, &id)?)?,
        },
        Commands::Node { command } => match command {
            NodeCommand::Upsert {
                id,
                scope,
                run_id,
                contract,
                session_id,
                status,
                attempt,
                depends_on,
                execution,
                judge_policy,
            } => print_json(&control::upsert_node(
                &scope.scope,
                &run_id,
                control::NodeSpec {
                    id,
                    contract: (*contract).into(),
                    session_id,
                    status,
                    attempt,
                    depends_on,
                    execution,
                    judge_policy,
                },
            )?)?,
            NodeCommand::Update {
                id,
                scope,
                run_id,
                status,
            } => print_json(&control::update_node(&scope.scope, &run_id, &id, status)?)?,
            NodeCommand::Edit {
                id,
                scope,
                run_id,
                goal,
                expected_output,
                success_criteria,
                harness,
                model,
                execution,
                judge_policy,
            } => print_json(&workflow::edit_run_node(
                &config,
                &scope.scope,
                &run_id,
                &id,
                workflow::NodeEdit {
                    goal,
                    expected_output,
                    success_criteria,
                    harness,
                    model,
                    execution,
                    judge_policy,
                },
            )?)?,
            NodeCommand::Delete { id, scope, run_id } => {
                workflow::delete_run_node(&config, &scope.scope, &run_id, &id)?;
            }
            NodeCommand::Dependency {
                id,
                scope,
                run_id,
                on,
                remove,
            } => {
                workflow::set_run_dependency(&config, &scope.scope, &run_id, &id, &on, !remove)?;
            }
        },
        Commands::Provider { command } => match command {
            ProviderCommand::List(args) => {
                let providers = provider::discover(&config)?;
                if args.json {
                    print_json(&providers)?;
                } else {
                    for item in providers {
                        println!("{}\t{}\t{}", item.name, item.kind, item.description);
                        for capability in item.all_capabilities() {
                            println!("  {capability}");
                        }
                    }
                }
            }
            ProviderCommand::Validate { name, scope, json } => {
                let scope = state::resolve_scope(scope.scope)?;
                let results = provider::validate_all(&config, &scope, name.as_deref())?;
                let ok = results
                    .iter()
                    .all(|result| result.status == provider::CheckStatus::Ok);
                if json {
                    print_json(&results)?;
                } else {
                    for result in results {
                        println!(
                            "{:?} {} · {}",
                            result.status, result.provider.name, result.provider.kind
                        );
                        for check in result.checks {
                            println!("  {:?} {:<12} {}", check.status, check.name, check.message);
                        }
                    }
                }
                if !ok {
                    return Ok(1);
                }
            }
        },
        Commands::Workflow { command } => workflow_command(&config, command)?,
        Commands::Launch(args) => {
            return Ok(control::launch(
                &config,
                &args.scope.scope,
                args.harness,
                args.model,
                args.managed,
                args.args,
            )?
            .clamp(0, 255) as u8);
        }
        Commands::Attach(args) => {
            return Ok(control::attach(
                &config,
                &args.scope.scope,
                &args.id,
                Action::Attach,
                &args.direction.to_string(),
            )?
            .code
            .clamp(0, 255) as u8);
        }
        Commands::Inspect(args) => {
            return Ok(control::attach(
                &config,
                &args.scope.scope,
                &args.id,
                Action::Inspect,
                &args.direction.to_string(),
            )?
            .code
            .clamp(0, 255) as u8);
        }
        Commands::Disconnect { id, scope } => {
            let id = control::require_id(id)?;
            println!(
                "{}",
                control::update_session(&scope.scope, &id, LifecycleStatus::Disconnected)?.id
            );
        }
        Commands::Guide(args) => guide(&config, &args)?,
        Commands::Mcp => mcp::run(config)?,
        Commands::Prompt(scope) => {
            let state = control::read_workspace(&scope.scope)?;
            print!(
                "{}",
                if state.current_session().is_some() {
                    "|⚔|"
                } else {
                    ""
                }
            );
        }
        Commands::Completion { shell } => completion(shell),
        Commands::Schema { schema } => {
            let value = match schema {
                SchemaTarget::Config => config::schema(),
                SchemaTarget::Provider => provider::schema(),
                SchemaTarget::Workflow => workflow::schema(),
                SchemaTarget::State => {
                    serde_json::to_value(schemars::schema_for!(crate::domain::WorkspaceState))?
                }
            };
            print_json(&value)?;
        }
        Commands::Generate { root, check } => generate_artifacts(&root, check)?,
    }
    Ok(0)
}

fn list_sessions(args: &OutputArgs) -> Result<()> {
    let state = control::read_workspace(&args.scope.scope)?;
    if args.json {
        print_json(&state.sessions)?;
    } else {
        for session in state.sessions {
            println!(
                "{}\t{}\t{}\t{}",
                session.id, session.status, session.role, session.title
            );
        }
    }
    Ok(())
}

fn workflow_command(config: &Config, command: WorkflowCommand) -> Result<()> {
    match command {
        WorkflowCommand::Init { name, scope } => {
            println!("{}", workflow::init(config, &scope.scope, &name)?.display())
        }
        WorkflowCommand::Import { path, scope } => println!(
            "{}",
            workflow::import(config, &scope.scope, &path)?.display()
        ),
        WorkflowCommand::List { scope } => {
            for path in workflow::list(config, &scope.scope)? {
                println!(
                    "{}",
                    path.file_stem()
                        .and_then(|value| value.to_str())
                        .unwrap_or_default()
                );
            }
        }
        WorkflowCommand::Show { name, scope } => print!(
            "{}",
            fs::read_to_string(workflow::path(config, &scope.scope, &name)?)?
        ),
        WorkflowCommand::Edit { name, scope } => {
            let path = workflow::path(config, &scope.scope, &name)?;
            let editor = env::var("EDITOR").unwrap_or_else(|_| "vi".into());
            let status = Command::new(editor).arg(&path).status()?;
            if !status.success() {
                bail!("editor exited with {status}");
            }
            workflow::load(&path)?;
            workflow::commit(config, &format!("feat: update {name} workflow"))?;
        }
        WorkflowCommand::Validate { workflow: path } => {
            let definition = workflow::load(&path)?;
            println!(
                "valid · {} · {} steps",
                definition.name,
                definition.steps.len()
            );
        }
        WorkflowCommand::Plan {
            workflow: path,
            scope,
            json,
        } => {
            let definition = workflow::load(&path)?;
            let plan = workflow::plan(config, &scope.scope, &definition)?;
            if json {
                print_json(&plan)?;
            } else {
                println!(
                    "{} · {:?}\nrevision {}",
                    plan.name,
                    plan.approval,
                    plan.revision.as_deref().unwrap_or("uncommitted")
                );
                for (index, wave) in plan.waves.iter().enumerate() {
                    println!("wave {}  {}", index + 1, wave.join(", "));
                }
                for gate in plan.gates {
                    println!("gate {} before {} · {}", gate.id, gate.before, gate.reason);
                }
            }
        }
        WorkflowCommand::Start {
            workflow: requested,
            scope,
            background,
            json,
        } => {
            let definition_path = if requested.exists() {
                requested
            } else {
                let name = requested
                    .to_str()
                    .context("workflow name is not valid UTF-8")?;
                workflow::path(config, &scope.scope, name)?
            };
            let mode = if background {
                crate::domain::RunMode::Background
            } else {
                crate::domain::RunMode::Foreground
            };
            let run = workflow::materialize(config, &scope.scope, &definition_path, mode)?;
            let autonomy = preferences::read(&state::resolve_scope(&scope.scope)?)?.autonomy;
            let run = if autonomy == AutonomyMode::Autonomous {
                if background {
                    workflow::spawn(&scope.scope, &run.id)?
                } else {
                    workflow::execute(config, &scope.scope, &run.id)?
                }
            } else {
                run
            };
            if json {
                print_json(&run)?;
            } else {
                println!("{}\t{}\t{}", run.id, run.status, run.name);
            }
        }
        WorkflowCommand::History { name, scope } => {
            print!("{}", workflow::history(config, &scope.scope, &name)?)
        }
        WorkflowCommand::Search { query } => print!("{}", workflow::search(config, &query)?),
        WorkflowCommand::Path { name, scope } => {
            println!("{}", workflow::path(config, &scope.scope, &name)?.display())
        }
    }
    Ok(())
}

fn guide(config: &Config, args: &GuideArgs) -> Result<()> {
    let scope = state::resolve_scope(&args.scope.scope)?;
    let workspace = state::read(&scope)?;
    let session = control::selected_session(&workspace, &args.id)?;
    let providers = provider::discover(config)?;
    let request = serde_json::json!({ "version": "orc.provider/v1", "action": "guide", "scope": scope, "session": session, "text": args.text });
    let plan = provider::resolve_plan(config, &providers, Action::Guide, request)?;
    let code = provider::execute_plan(&plan, &scope, true)?;
    if code != 0 {
        bail!("guidance provider exited with {code}");
    }
    Ok(())
}

fn completion(shell: CompletionTargetShell) {
    let mut command = Cli::command();
    match shell {
        CompletionTargetShell::Bash => {
            generate(Shell::Bash, &mut command, "orc", &mut io::stdout())
        }
        CompletionTargetShell::Zsh => generate(Shell::Zsh, &mut command, "orc", &mut io::stdout()),
        CompletionTargetShell::Fish => {
            generate(Shell::Fish, &mut command, "orc", &mut io::stdout())
        }
        CompletionTargetShell::Nu => generate(Nushell, &mut command, "orc", &mut io::stdout()),
    }
}

fn command_reference() -> String {
    fn render(command: &clap::Command, path: &str, output: &mut String) {
        if command.is_hide_set() {
            return;
        }
        output.push_str(&format!("### `{path}`\n\n"));
        if let Some(about) = command.get_about() {
            output.push_str(&format!("{about}\n\n"));
        }
        let mut help = command.clone();
        output.push_str("```text\n");
        for line in help.render_help().to_string().lines() {
            output.push_str(line.trim_end());
            output.push('\n');
        }
        output.push_str("```\n\n");
        for subcommand in command.get_subcommands() {
            if subcommand.get_name() == "help" {
                continue;
            }
            render(
                subcommand,
                &format!("{path} {}", subcommand.get_name()),
                output,
            );
        }
    }
    let command = Cli::command();
    let mut output = String::new();
    for subcommand in command.get_subcommands() {
        if subcommand.get_name() != "help" {
            render(
                subcommand,
                &format!("orc {}", subcommand.get_name()),
                &mut output,
            );
        }
    }
    output
}

fn generate_artifacts(root: &std::path::Path, check: bool) -> Result<()> {
    fn update(path: &std::path::Path, content: &str, check: bool) -> Result<()> {
        if check {
            let current = fs::read_to_string(path)
                .with_context(|| format!("read generated artifact {}", path.display()))?;
            if current != content {
                bail!("generated artifact is stale: {}", path.display());
            }
        } else {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(path, content)?;
        }
        Ok(())
    }

    let schemas = [
        ("orc.schema.json", config::schema()),
        ("provider.schema.json", provider::schema()),
        ("workflow.schema.json", workflow::schema()),
        (
            "state.schema.json",
            serde_json::to_value(schemars::schema_for!(crate::domain::WorkspaceState))?,
        ),
    ];
    for (name, schema) in schemas {
        let mut content = serde_json::to_string_pretty(&schema)?;
        content.push('\n');
        update(&root.join("schema").join(name), &content, check)?;
    }

    let readme = root.join("README.md");
    let source = fs::read_to_string(&readme)?;
    let start_marker = "<!-- BEGIN GENERATED:commands -->";
    let end_marker = "<!-- END GENERATED:commands -->";
    let start = source
        .find(start_marker)
        .context("README command marker is missing")?;
    let end = source
        .find(end_marker)
        .context("README command end marker is missing")?;
    let generated = format!("{}\n{}\n{}", start_marker, command_reference(), end_marker);
    let next = format!(
        "{}{}{}",
        &source[..start],
        generated,
        &source[end + end_marker.len()..]
    );
    update(&readme, &next, check)
}

#[cfg(test)]
mod tests {
    use super::SplitDirection;

    #[test]
    fn split_directions_render_without_recursive_formatting() {
        assert_eq!(SplitDirection::Right.to_string(), "right");
        assert_eq!(SplitDirection::Left.to_string(), "left");
        assert_eq!(SplitDirection::Top.to_string(), "top");
        assert_eq!(SplitDirection::Bottom.to_string(), "bottom");
    }
}
