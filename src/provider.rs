use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Instant,
};

use anyhow::{Context, Result, anyhow, bail};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use wait_timeout::ChildExt;

use crate::{
    config::Config,
    domain::{BindingStatus, ProviderBinding, ProviderKind, Session},
};

#[derive(
    Clone, Copy, Debug, Deserialize, Eq, JsonSchema, Ord, PartialEq, PartialOrd, Serialize,
)]
pub enum Capability {
    #[serde(rename = "changes.inspect")]
    ChangesInspect,
    #[serde(rename = "session.attach")]
    SessionAttach,
    #[serde(rename = "session.bind")]
    SessionBind,
    #[serde(rename = "session.describe")]
    SessionDescribe,
    #[serde(rename = "session.inspect")]
    SessionInspect,
    #[serde(rename = "session.launch")]
    SessionLaunch,
    #[serde(rename = "session.persist")]
    SessionPersist,
    #[serde(rename = "session.stop")]
    SessionStop,
    #[serde(rename = "terminal.open")]
    TerminalOpen,
    #[serde(rename = "execution.run")]
    ExecutionRun,
    #[serde(rename = "execution.cancel")]
    ExecutionCancel,
    #[serde(rename = "execution.status")]
    ExecutionStatus,
    #[serde(rename = "execution.logs")]
    ExecutionLogs,
    #[serde(rename = "session.guide")]
    SessionGuide,
}

impl std::fmt::Display for Capability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = serde_json::to_value(self).expect("capability serializes");
        write!(f, "{}", value.as_str().expect("capability is a string"))
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
pub struct Manifest {
    pub version: String,
    pub name: String,
    #[serde(default = "default_description")]
    pub description: String,
    #[serde(default = "default_kind")]
    pub kind: ProviderKind,
    pub command: String,
    #[serde(default)]
    pub actions: BTreeMap<Capability, String>,
    #[serde(default)]
    pub capabilities: Vec<Capability>,
    #[serde(default)]
    pub requires: Requirements,
    #[serde(default)]
    pub priority: i64,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Requirements {
    pub commands: Vec<String>,
    pub environment: Vec<String>,
    pub paths: Vec<PathBuf>,
}

fn default_description() -> String {
    "Orc provider".into()
}
fn default_kind() -> ProviderKind {
    ProviderKind::Integration
}

impl Manifest {
    pub fn all_capabilities(&self) -> Vec<Capability> {
        let mut capabilities: Vec<_> = self.actions.keys().copied().collect();
        for capability in &self.capabilities {
            if !capabilities.contains(capability) {
                capabilities.push(*capability);
            }
        }
        capabilities
    }

    pub fn supports(&self, capability: Capability) -> bool {
        self.actions.contains_key(&capability) || self.capabilities.contains(&capability)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CommandPlan {
    pub version: String,
    pub command: Vec<String>,
    pub cwd: Option<String>,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    #[serde(default = "default_success_codes", rename = "successCodes")]
    pub success_codes: Vec<i32>,
}

fn default_success_codes() -> Vec<i32> {
    vec![0]
}

impl CommandPlan {
    pub fn accepts(&self, code: i32) -> bool {
        self.success_codes.contains(&code)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ValidationCheck {
    pub name: String,
    pub status: CheckStatus,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Ok,
    Failed,
}

#[derive(Clone, Debug, Serialize)]
pub struct Validation {
    pub provider: Manifest,
    pub status: CheckStatus,
    pub checks: Vec<ValidationCheck>,
}

#[derive(Clone, Copy, Debug)]
pub enum Action {
    Activity,
    Attach,
    Changes,
    Execute,
    Inspect,
    Launch,
    Guide,
    Stop,
}

impl Action {
    fn stages(self) -> &'static [(Capability, bool)] {
        match self {
            Self::Activity => &[(Capability::SessionInspect, false)],
            Self::Attach => &[
                (Capability::SessionAttach, false),
                (Capability::SessionPersist, true),
                (Capability::TerminalOpen, false),
            ],
            Self::Changes => &[(Capability::ChangesInspect, false)],
            Self::Execute => &[(Capability::ExecutionRun, false)],
            Self::Inspect => &[
                (Capability::SessionInspect, false),
                (Capability::TerminalOpen, false),
            ],
            Self::Launch => &[
                (Capability::SessionLaunch, false),
                (Capability::SessionPersist, true),
                (Capability::ExecutionRun, false),
            ],
            Self::Guide => &[(Capability::SessionGuide, false)],
            Self::Stop => &[(Capability::SessionStop, false)],
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Activity => "activity",
            Self::Attach => "attach",
            Self::Changes => "changes",
            Self::Execute => "execute",
            Self::Inspect => "inspect",
            Self::Launch => "launch",
            Self::Guide => "guide",
            Self::Stop => "stop",
        }
    }
}

pub fn schema() -> serde_json::Value {
    serde_json::to_value(schema_for!(Manifest)).expect("provider schema serializes")
}

pub fn discover(config: &Config) -> Result<Vec<Manifest>> {
    let directory = &config.providers.directory;
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut entries: Vec<_> = fs::read_dir(directory)
        .with_context(|| format!("read provider manifests from {}", directory.display()))?
        .collect::<Result<_, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    let mut providers = Vec::new();
    for entry in entries {
        let path = entry.path();
        if !matches!(
            path.extension().and_then(|value| value.to_str()),
            Some("json" | "yaml" | "yml")
        ) {
            continue;
        }
        let source = fs::read_to_string(&path)?;
        let provider: Manifest = if path.extension().and_then(|value| value.to_str())
            == Some("json")
        {
            serde_json::from_str(&source).with_context(|| format!("parse {}", path.display()))?
        } else {
            serde_yaml::from_str(&source).with_context(|| format!("parse {}", path.display()))?
        };
        validate_manifest(&provider, &path)?;
        providers.push(provider);
    }
    Ok(providers)
}

fn validate_manifest(provider: &Manifest, path: &Path) -> Result<()> {
    if provider.version != "orc.provider/v1" {
        bail!("{}: version must be orc.provider/v1", path.display());
    }
    if provider.name.is_empty()
        || !provider
            .name
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || "._-".contains(ch))
    {
        bail!("{}: provider name is invalid", path.display());
    }
    if provider.command.trim().is_empty() || provider.all_capabilities().is_empty() {
        bail!("{}: command and actions are required", path.display());
    }
    Ok(())
}

fn candidates(providers: &[Manifest], capability: Capability) -> Vec<&Manifest> {
    let mut selected: Vec<_> = providers
        .iter()
        .filter(|provider| provider.supports(capability))
        .collect();
    selected.sort_by(|left, right| {
        right
            .priority
            .cmp(&left.priority)
            .then(left.name.cmp(&right.name))
    });
    selected
}

fn invoke_raw(provider: &Manifest, request: &Value, config: &Config) -> Result<Value> {
    let started = Instant::now();
    let child = Command::new(&provider.command)
        .current_dir(request.get("scope").and_then(Value::as_str).unwrap_or("."))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(error) => {
            record_invocation(
                provider,
                request,
                false,
                started.elapsed().as_millis(),
                &error.to_string(),
            );
            return Err(error).with_context(|| format!("start provider {}", provider.name));
        }
    };
    child
        .stdin
        .take()
        .context("provider stdin")?
        .write_all(&serde_json::to_vec(request)?)?;
    let status = match child.wait_timeout(config.provider_timeout())? {
        Some(status) => status,
        None => {
            child.kill()?;
            let _ = child.wait();
            record_invocation(
                provider,
                request,
                false,
                started.elapsed().as_millis(),
                "provider timed out",
            );
            bail!("{} timed out", provider.name);
        }
    };
    let mut stdout = String::new();
    let mut stderr = String::new();
    child
        .stdout
        .take()
        .context("provider stdout")?
        .read_to_string(&mut stdout)?;
    child
        .stderr
        .take()
        .context("provider stderr")?
        .read_to_string(&mut stderr)?;
    record_invocation(
        provider,
        request,
        status.success(),
        started.elapsed().as_millis(),
        &stderr,
    );
    if !status.success() {
        bail!(
            "{}",
            stderr
                .trim()
                .to_owned()
                .if_empty_then(|| format!("{} exited with {status}", provider.name))
        );
    }
    serde_json::from_str(&stdout)
        .with_context(|| format!("{} returned invalid JSON", provider.name))
}

fn record_invocation(
    provider: &Manifest,
    request: &Value,
    success: bool,
    duration_ms: u128,
    stderr: &str,
) {
    let directory = crate::config::state_home().join("orc/providers");
    if fs::create_dir_all(&directory).is_err() {
        return;
    }
    let path = directory.join(format!("{}.jsonl", provider.name));
    let record = json!({
        "at": chrono::Utc::now(),
        "provider": provider.name,
        "action": request.get("action").and_then(Value::as_str),
        "capability": request.get("capability").and_then(Value::as_str),
        "status": if success { "ok" } else { "failed" },
        "durationMs": duration_ms.min(u64::MAX as u128) as u64,
        "message": stderr.lines().next().unwrap_or_default(),
    });
    let Ok(line) = serde_json::to_string(&record) else {
        return;
    };
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    let _ = writeln!(file, "{line}");
}

pub fn recent_activity(name: &str) -> String {
    let path = crate::config::state_home()
        .join("orc/providers")
        .join(format!("{name}.jsonl"));
    let Ok(source) = fs::read_to_string(path) else {
        return "No provider calls yet.".into();
    };
    let lines: Vec<_> = source.lines().rev().take(100).collect();
    let rendered = lines
        .into_iter()
        .rev()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .map(|record| {
            let at = record
                .get("at")
                .and_then(Value::as_str)
                .and_then(|value| value.get(11..19))
                .unwrap_or("--:--:--");
            let status = record
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let capability = record
                .get("capability")
                .and_then(Value::as_str)
                .unwrap_or("provider.call");
            let duration = record
                .get("durationMs")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let message = record
                .get("message")
                .and_then(Value::as_str)
                .filter(|message| !message.is_empty())
                .map_or_else(String::new, |message| format!(" · {message}"));
            format!("{at}  {status:<6}  {capability:<20}  {duration:>5}ms{message}")
        })
        .collect::<Vec<_>>()
        .join("\n");
    if rendered.is_empty() {
        "No provider calls yet.".into()
    } else {
        rendered
    }
}

#[derive(Deserialize, Serialize)]
struct CachedValue {
    created_at_ms: i64,
    value: Value,
}

fn cache_path(provider: &Manifest, request: &Value) -> Result<PathBuf> {
    let key = serde_json::to_vec(&json!({
        "orcVersion": env!("CARGO_PKG_VERSION"),
        "provider": provider.name,
        "command": provider.command,
        "request": request,
    }))?;
    let digest = hex::encode(Sha256::digest(key));
    Ok(crate::config::state_home()
        .join("orc/cache/providers")
        .join(format!("{digest}.json")))
}

fn invoke_cached(provider: &Manifest, request: &Value, config: &Config) -> Result<Value> {
    if config.cache.provider_ttl_ms == 0 {
        return invoke_raw(provider, request, config);
    }
    let path = cache_path(provider, request)?;
    let now = chrono::Utc::now().timestamp_millis();
    if let Ok(source) = fs::read_to_string(&path)
        && let Ok(cached) = serde_json::from_str::<CachedValue>(&source)
        && now.saturating_sub(cached.created_at_ms) <= config.cache.provider_ttl_ms as i64
    {
        return Ok(cached.value);
    }
    let value = invoke_raw(provider, request, config)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let cached = CachedValue {
        created_at_ms: now,
        value: value.clone(),
    };
    fs::write(path, serde_json::to_vec(&cached)?)?;
    Ok(value)
}

trait EmptyFallback {
    fn if_empty_then(self, fallback: impl FnOnce() -> String) -> String;
}

impl EmptyFallback for String {
    fn if_empty_then(self, fallback: impl FnOnce() -> String) -> String {
        if self.is_empty() { fallback() } else { self }
    }
}

fn command_check(name: String, command: &str) -> ValidationCheck {
    match which::which(command) {
        Ok(path) => ValidationCheck {
            name,
            status: CheckStatus::Ok,
            message: path.display().to_string(),
        },
        Err(error) => ValidationCheck {
            name,
            status: CheckStatus::Failed,
            message: error.to_string(),
        },
    }
}

fn requirements_checks(provider: &Manifest) -> Vec<ValidationCheck> {
    let mut checks = vec![command_check("executable".into(), &provider.command)];
    checks.extend(
        provider
            .requires
            .commands
            .iter()
            .map(|command| command_check(format!("command:{command}"), command)),
    );
    checks.extend(provider.requires.environment.iter().map(|variable| {
        let present = std::env::var_os(variable).is_some();
        ValidationCheck {
            name: format!("environment:{variable}"),
            status: if present {
                CheckStatus::Ok
            } else {
                CheckStatus::Failed
            },
            message: if present { "set" } else { "not set" }.into(),
        }
    }));
    checks.extend(provider.requires.paths.iter().map(|path| {
        let present = path.exists();
        ValidationCheck {
            name: format!("path:{}", path.display()),
            status: if present {
                CheckStatus::Ok
            } else {
                CheckStatus::Failed
            },
            message: if present { "exists" } else { "missing" }.into(),
        }
    }));
    checks
}

fn provider_checks(result: Result<Value>) -> Vec<ValidationCheck> {
    match result {
        Ok(value) => value
            .get("checks")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|item| serde_json::from_value(item.clone()).ok())
            .collect(),
        Err(error) => vec![ValidationCheck {
            name: "provider".into(),
            status: CheckStatus::Failed,
            message: format!("{error:#}"),
        }],
    }
}

pub fn validate_all(config: &Config, scope: &Path, name: Option<&str>) -> Result<Vec<Validation>> {
    let providers = discover(config)?;
    let selected: Vec<_> = providers
        .into_iter()
        .filter(|provider| name.is_none_or(|name| provider.name == name))
        .collect();
    if name.is_some() && selected.is_empty() {
        bail!("unknown provider: {}", name.unwrap_or_default());
    }
    Ok(selected
        .into_iter()
        .map(|provider| {
            let request = json!({
                "version": "orc.provider/v1",
                "action": "validate",
                "capability": "provider.validate",
                "scope": scope,
                "manifest": {
                    "name": provider.name,
                    "kind": provider.kind,
                    "actions": provider.actions,
                },
            });
            let mut checks = vec![ValidationCheck {
                name: "manifest".into(),
                status: CheckStatus::Ok,
                message: "manifest is valid".into(),
            }];
            checks.extend(requirements_checks(&provider));
            checks.extend(provider_checks(invoke_raw(&provider, &request, config)));
            let status = if checks.iter().all(|check| check.status == CheckStatus::Ok) {
                CheckStatus::Ok
            } else {
                CheckStatus::Failed
            };
            Validation {
                provider,
                status,
                checks,
            }
        })
        .collect())
}

fn parse_plan(provider: &Manifest, value: Value) -> Result<Option<CommandPlan>> {
    if value.get("status").and_then(Value::as_str) == Some("declined") {
        return Ok(None);
    }
    let plan: CommandPlan = serde_json::from_value(value)
        .with_context(|| format!("{} returned an invalid command plan", provider.name))?;
    if plan.version != "orc.provider/v1"
        || plan.command.is_empty()
        || plan.command.iter().any(String::is_empty)
        || plan.success_codes.is_empty()
    {
        bail!("{} returned an invalid command plan", provider.name);
    }
    Ok(Some(plan))
}

pub fn resolve_plan(
    config: &Config,
    providers: &[Manifest],
    action: Action,
    request: Value,
) -> Result<CommandPlan> {
    resolve_plan_from(config, providers, action, request, None)
}

pub fn resolve_plan_from(
    config: &Config,
    providers: &[Manifest],
    action: Action,
    mut request: Value,
    mut plan: Option<CommandPlan>,
) -> Result<CommandPlan> {
    for &(capability, optional) in action.stages() {
        let stage = candidates(providers, capability);
        if stage.is_empty() && optional {
            continue;
        }
        if stage.is_empty() {
            bail!("no provider advertises capability {capability}");
        }
        let mut accepted = None;
        for provider in stage {
            request["capability"] = Value::String(capability.to_string());
            request["plan"] = serde_json::to_value(&plan)?;
            if let Some(candidate) = parse_plan(provider, invoke_raw(provider, &request, config)?)?
            {
                accepted = Some(candidate);
                break;
            }
        }
        plan = Some(
            accepted.ok_or_else(|| anyhow!("all providers declined capability {capability}"))?,
        );
    }
    plan.ok_or_else(|| anyhow!("provider chain for {} produced no command", action.name()))
}

#[derive(Clone, Debug)]
pub struct CommandResult {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub fn run_plan(plan: &CommandPlan, scope: &Path) -> Result<CommandResult> {
    let program = plan.command.first().context("command plan is empty")?;
    let output = Command::new(program)
        .args(&plan.command[1..])
        .current_dir(plan.cwd.as_deref().map(Path::new).unwrap_or(scope))
        .envs(&plan.environment)
        .output()?;
    Ok(CommandResult {
        code: output.status.code().unwrap_or(1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

pub fn run_plan_with_timeout(
    plan: &CommandPlan,
    scope: &Path,
    timeout: std::time::Duration,
) -> Result<CommandResult> {
    let program = plan.command.first().context("command plan is empty")?;
    let stdout = tempfile::NamedTempFile::new()?;
    let stderr = tempfile::NamedTempFile::new()?;
    let mut child = Command::new(program)
        .args(&plan.command[1..])
        .current_dir(plan.cwd.as_deref().map(Path::new).unwrap_or(scope))
        .envs(&plan.environment)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout.reopen()?))
        .stderr(Stdio::from(stderr.reopen()?))
        .spawn()
        .with_context(|| format!("start command plan {program}"))?;
    let status = match child.wait_timeout(timeout)? {
        Some(status) => status,
        None => {
            child.kill()?;
            let _ = child.wait();
            bail!("command plan timed out after {}ms", timeout.as_millis());
        }
    };
    Ok(CommandResult {
        code: status.code().unwrap_or(1),
        stdout: String::from_utf8_lossy(&fs::read(stdout.path())?).into_owned(),
        stderr: String::from_utf8_lossy(&fs::read(stderr.path())?).into_owned(),
    })
}

pub fn execute_plan(plan: &CommandPlan, scope: &Path, inherit: bool) -> Result<i32> {
    let program = plan.command.first().context("command plan is empty")?;
    let mut command = Command::new(program);
    command
        .args(&plan.command[1..])
        .current_dir(plan.cwd.as_deref().map(Path::new).unwrap_or(scope))
        .envs(&plan.environment);
    if inherit {
        command
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        return Ok(command.status()?.code().unwrap_or(1));
    }
    let output = command.output()?;
    print!("{}", String::from_utf8_lossy(&output.stdout));
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
    Ok(output.status.code().unwrap_or(1))
}

pub fn capture_plan(
    plan: &CommandPlan,
    scope: &Path,
    timeout: std::time::Duration,
) -> Result<String> {
    let result = run_plan_with_timeout(plan, scope, timeout)?;
    if !plan.accepts(result.code) {
        let message = result.stderr.trim();
        if message.is_empty() {
            bail!("command plan exited with {}", result.code);
        }
        bail!("{message}");
    }
    Ok(result.stdout)
}

pub fn action_request(
    action: Action,
    scope: &Path,
    session: Option<&Session>,
    direction: &str,
) -> Value {
    json!({
        "version": "orc.provider/v1",
        "action": action.name(),
        "scope": scope,
        "direction": direction,
        "session": session,
    })
}

pub fn discover_bindings(
    config: &Config,
    providers: &[Manifest],
    scope: &Path,
    session: &Session,
) -> Vec<ProviderBinding> {
    candidates(providers, Capability::SessionBind).into_iter().filter_map(|provider| {
        let request = json!({
            "version": "orc.provider/v1", "action": "bind", "capability": Capability::SessionBind,
            "scope": scope, "session": session, "plan": null,
        });
        let value = invoke_raw(provider, &request, config).ok()?;
        if value.get("status").and_then(Value::as_str) == Some("declined") { return None; }
        let binding = value.get("binding")?;
        Some(ProviderBinding {
            provider: provider.name.clone(),
            kind: serde_json::from_value(binding.get("kind")?.clone()).ok()?,
            r#ref: binding.get("ref").and_then(Value::as_str).map(str::to_owned),
            status: serde_json::from_value(binding.get("status")?.clone()).ok().unwrap_or(BindingStatus::Unavailable),
            label: binding.get("label").and_then(Value::as_str).unwrap_or(&provider.description).to_owned(),
        })
    }).collect()
}

pub fn describe(
    config: &Config,
    providers: &[Manifest],
    scope: &Path,
    session: &Session,
) -> (Option<String>, Option<String>) {
    for provider in candidates(providers, Capability::SessionDescribe) {
        let request = json!({
            "version": "orc.provider/v1", "action": "describe", "capability": Capability::SessionDescribe,
            "scope": scope, "session": session, "plan": null,
        });
        let Ok(value) = invoke_cached(provider, &request, config) else {
            continue;
        };
        let Some(description) = value.get("description") else {
            continue;
        };
        return (
            description
                .get("title")
                .and_then(Value::as_str)
                .map(str::to_owned),
            description
                .get("goal")
                .and_then(Value::as_str)
                .map(str::to_owned),
        );
    }
    (None, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attach_chain_composes_three_capabilities() {
        assert_eq!(Action::Attach.stages().len(), 3);
        assert!(Action::Attach.stages()[1].1);
    }

    #[test]
    fn bounded_plan_execution_stops_hung_integrations() {
        let plan = CommandPlan {
            version: "orc.provider/v1".into(),
            command: vec!["sleep".into(), "1".into()],
            cwd: None,
            environment: BTreeMap::new(),
            success_codes: vec![0],
        };
        let error =
            run_plan_with_timeout(&plan, Path::new("."), std::time::Duration::from_millis(10))
                .expect_err("sleep must exceed the timeout");
        assert!(error.to_string().contains("timed out"));
    }

    #[test]
    fn command_plan_accepts_provider_declared_exit_codes() {
        let provider = Manifest {
            version: "orc.provider/v1".into(),
            name: "activity".into(),
            description: "Activity".into(),
            kind: ProviderKind::Activity,
            command: "activity-provider".into(),
            actions: BTreeMap::new(),
            capabilities: Vec::new(),
            requires: Requirements::default(),
            priority: 0,
        };
        let plan = parse_plan(
            &provider,
            serde_json::json!({
                "version": "orc.provider/v1",
                "command": ["activity"],
                "successCodes": [0, 2],
            }),
        )
        .expect("plan should parse")
        .expect("provider should accept the request");

        assert!(plan.accepts(2));
        assert!(!plan.accepts(1));
    }
}
