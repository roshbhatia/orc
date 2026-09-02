use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
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
    pub priority: i64,
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
    let mut child = Command::new(&provider.command)
        .current_dir(request.get("scope").and_then(Value::as_str).unwrap_or("."))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("start provider {}", provider.name))?;
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

#[derive(Deserialize, Serialize)]
struct CachedValue {
    created_at_ms: i64,
    value: Value,
}

fn cache_path(provider: &Manifest, request: &Value) -> Result<PathBuf> {
    let key = serde_json::to_vec(&json!({
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

pub fn validate_all(config: &Config, scope: &Path, name: Option<&str>) -> Result<Vec<Validation>> {
    let providers = discover(config)?;
    let selected: Vec<_> = providers
        .into_iter()
        .filter(|provider| name.is_none_or(|name| provider.name == name))
        .collect();
    if name.is_some() && selected.is_empty() {
        bail!("unknown provider: {}", name.unwrap_or_default());
    }
    Ok(selected.into_iter().map(|provider| {
        let request = json!({
            "version": "orc.provider/v1",
            "action": "validate",
            "capability": "provider.validate",
            "scope": scope,
            "manifest": { "name": provider.name, "kind": provider.kind, "actions": provider.actions },
        });
        let result = invoke_raw(&provider, &request, config);
        let mut checks = vec![ValidationCheck {
            name: "manifest".into(), status: CheckStatus::Ok, message: "manifest and executable are valid".into()
        }];
        match result {
            Ok(value) => {
                if let Some(items) = value.get("checks").and_then(Value::as_array) {
                    for item in items {
                        if let Ok(check) = serde_json::from_value(item.clone()) { checks.push(check); }
                    }
                }
            }
            Err(error) => checks.push(ValidationCheck { name: "provider".into(), status: CheckStatus::Failed, message: format!("{error:#}") }),
        }
        let status = if checks.iter().all(|check| check.status == CheckStatus::Ok) { CheckStatus::Ok } else { CheckStatus::Failed };
        Validation { provider, status, checks }
    }).collect())
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

pub fn capture_plan(plan: &CommandPlan, scope: &Path) -> Result<String> {
    let result = run_plan(plan, scope)?;
    if result.code != 0 {
        bail!("{}", result.stderr.trim());
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
        let value = invoke_cached(provider, &request, config).ok()?;
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
}
