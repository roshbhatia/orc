use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
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

const MAX_ACTIVITY_BYTES: u64 = 256 * 1024;
const MAX_ACTIVITY_LINES: usize = 100;
const MAX_PROVIDER_CACHE_ENTRIES: usize = 256;
const MAX_PROVIDER_CACHE_ENTRY_BYTES: u64 = 1024 * 1024;

#[derive(
    Clone, Copy, Debug, Deserialize, Eq, JsonSchema, Ord, PartialEq, PartialOrd, Serialize,
)]
pub enum Capability {
    #[serde(rename = "activity.read")]
    ActivityRead,
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
    #[serde(rename = "terminal.focus")]
    TerminalFocus,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Activity,
    Attach,
    Changes,
    Execute,
    Focus,
    Inspect,
    Launch,
    Guide,
    Stop,
}

impl Action {
    fn stages(self) -> &'static [(Capability, bool)] {
        match self {
            Self::Activity => &[(Capability::ActivityRead, false)],
            Self::Attach => &[
                (Capability::SessionAttach, false),
                (Capability::SessionPersist, true),
                (Capability::TerminalOpen, false),
            ],
            Self::Changes => &[(Capability::ChangesInspect, false)],
            Self::Execute => &[(Capability::ExecutionRun, false)],
            Self::Focus => &[(Capability::TerminalFocus, false)],
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
            Self::Focus => "focus",
            Self::Inspect => "inspect",
            Self::Launch => "launch",
            Self::Guide => "guide",
            Self::Stop => "stop",
        }
    }
}

pub fn resolve_activity_plan(
    config: &Config,
    providers: &[Manifest],
    mut request: Value,
) -> Result<CommandPlan> {
    let capabilities = [
        Capability::ActivityRead,
        Capability::ExecutionLogs,
        Capability::SessionInspect,
    ];
    let mut failures = Vec::new();
    for capability in capabilities {
        for provider in candidates(providers, capability) {
            request["capability"] = Value::String(capability.to_string());
            request["plan"] = Value::Null;
            match invoke_raw(provider, &request, config)
                .and_then(|value| parse_plan(provider, value))
            {
                Ok(Some(plan)) => return Ok(plan),
                Ok(None) => failures.push(format!("{} declined {capability}", provider.name)),
                Err(error) => failures.push(format!("{}: {error:#}", provider.name)),
            }
        }
    }
    if failures.is_empty() {
        bail!("no provider advertises activity.read or execution.logs");
    }
    bail!(
        "no activity provider accepted the session: {}",
        failures.join("; ")
    )
}

pub fn schema() -> serde_json::Value {
    serde_json::to_value(schema_for!(Manifest)).expect("provider schema serializes")
}

pub fn discover(config: &Config) -> Result<Vec<Manifest>> {
    let directory = &config.providers.directory;
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    collect_manifest_paths(directory, &mut paths)?;
    paths.sort();
    let mut providers = Vec::new();
    let mut names = BTreeSet::new();
    for path in paths {
        let source = fs::read_to_string(&path)?;
        let provider: Manifest = if path.extension().and_then(|value| value.to_str())
            == Some("json")
        {
            serde_json::from_str(&source).with_context(|| format!("parse {}", path.display()))?
        } else {
            serde_yaml::from_str(&source).with_context(|| format!("parse {}", path.display()))?
        };
        validate_manifest(&provider, &path)?;
        if !names.insert(provider.name.clone()) {
            bail!(
                "{}: duplicate provider name {}",
                path.display(),
                provider.name
            );
        }
        providers.push(provider);
    }
    Ok(providers)
}

fn collect_manifest_paths(directory: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(directory)
        .with_context(|| format!("read provider manifests from {}", directory.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_manifest_paths(&path, paths)?;
        } else if matches!(
            path.extension().and_then(|value| value.to_str()),
            Some("json" | "yaml" | "yml")
        ) {
            paths.push(path);
        }
    }
    Ok(())
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
    let stdout = tempfile::NamedTempFile::new().context("create provider stdout buffer")?;
    let stderr = tempfile::NamedTempFile::new().context("create provider stderr buffer")?;
    let child = Command::new(&provider.command)
        .current_dir(request.get("scope").and_then(Value::as_str).unwrap_or("."))
        .stdin(Stdio::piped())
        .stdout(Stdio::from(stdout.reopen()?))
        .stderr(Stdio::from(stderr.reopen()?))
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
    let stdout = String::from_utf8_lossy(&fs::read(stdout.path())?).into_owned();
    let stderr = String::from_utf8_lossy(&fs::read(stderr.path())?).into_owned();
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
    compact_activity_log(&path);
    let message: String = stderr
        .lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(512)
        .collect();
    let record = json!({
        "at": chrono::Utc::now(),
        "provider": provider.name,
        "action": request.get("action").and_then(Value::as_str),
        "capability": request.get("capability").and_then(Value::as_str),
        "status": if success { "ok" } else { "failed" },
        "durationMs": duration_ms.min(u64::MAX as u128) as u64,
        "message": message,
        "scope": request.get("scope").and_then(Value::as_str),
    });
    let Ok(line) = serde_json::to_string(&record) else {
        return;
    };
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    let _ = writeln!(file, "{line}");
}

fn compact_activity_log(path: &Path) {
    let Ok(metadata) = fs::metadata(path) else {
        return;
    };
    if metadata.len() <= MAX_ACTIVITY_BYTES {
        return;
    }
    let Ok(tail) = read_file_tail(path, MAX_ACTIVITY_BYTES / 2) else {
        return;
    };
    let _ = fs::write(path, tail);
}

fn read_file_tail(path: &Path, max_bytes: u64) -> Result<Vec<u8>> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    let start = length.saturating_sub(max_bytes);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::with_capacity((length - start) as usize);
    file.read_to_end(&mut bytes)?;
    if start > 0
        && let Some(line_start) = bytes.iter().position(|byte| *byte == b'\n')
    {
        bytes.drain(..=line_start);
    }
    Ok(bytes)
}

pub fn recent_activity(name: &str) -> String {
    let path = crate::config::state_home()
        .join("orc/providers")
        .join(format!("{name}.jsonl"));
    let Ok(source) = read_file_tail(&path, MAX_ACTIVITY_BYTES) else {
        return "No provider calls yet.".into();
    };
    let source = String::from_utf8_lossy(&source);
    let lines: Vec<_> = source.lines().rev().take(MAX_ACTIVITY_LINES).collect();
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
    if fs::metadata(&path).is_ok_and(|metadata| metadata.len() <= MAX_PROVIDER_CACHE_ENTRY_BYTES)
        && let Ok(source) = fs::read_to_string(&path)
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
    let directory = path
        .parent()
        .context("provider cache path has no parent")?
        .to_path_buf();
    let encoded = serde_json::to_vec(&cached)?;
    if encoded.len() as u64 > MAX_PROVIDER_CACHE_ENTRY_BYTES {
        return Ok(value);
    }
    fs::write(path, encoded)?;
    prune_provider_cache(&directory);
    Ok(value)
}

fn prune_provider_cache(directory: &Path) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    let mut entries: Vec<_> = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                return None;
            }
            let modified = entry.metadata().ok()?.modified().ok()?;
            path.is_file().then_some((modified, path))
        })
        .collect();
    if entries.len() <= MAX_PROVIDER_CACHE_ENTRIES {
        return;
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
    let remove_count = entries.len() - MAX_PROVIDER_CACHE_ENTRIES;
    for (_, path) in entries.into_iter().take(remove_count) {
        let _ = fs::remove_file(path);
    }
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

fn failed_check(name: impl Into<String>, message: impl Into<String>) -> ValidationCheck {
    ValidationCheck {
        name: name.into(),
        status: CheckStatus::Failed,
        message: message.into(),
    }
}

fn provider_checks(result: Result<Value>) -> Vec<ValidationCheck> {
    match result {
        Ok(value) => parse_provider_checks(&value)
            .unwrap_or_else(|error| vec![failed_check("provider", format!("{error:#}"))]),
        Err(error) => vec![failed_check("provider", format!("{error:#}"))],
    }
}

fn parse_provider_checks(value: &Value) -> Result<Vec<ValidationCheck>> {
    if value.get("version").and_then(Value::as_str) != Some("orc.provider/v1") {
        bail!("validation response has an invalid or missing version");
    }
    let items = value
        .get("checks")
        .and_then(Value::as_array)
        .filter(|items| !items.is_empty())
        .context("validation response must contain at least one check")?;
    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let check: ValidationCheck = serde_json::from_value(item.clone())
                .with_context(|| format!("validation check {} is malformed", index + 1))?;
            if check.name.trim().is_empty() || check.message.trim().is_empty() {
                bail!(
                    "validation check {} has an empty name or message",
                    index + 1
                );
            }
            Ok(check)
        })
        .collect()
}

fn capability_action(capability: Capability) -> &'static str {
    match capability {
        Capability::ActivityRead => "activity",
        Capability::ChangesInspect => "changes",
        Capability::SessionAttach => "attach",
        Capability::SessionBind => "bind",
        Capability::SessionDescribe => "describe",
        Capability::SessionInspect => "inspect",
        Capability::SessionLaunch => "launch",
        Capability::SessionPersist => "persist",
        Capability::SessionStop => "stop",
        Capability::TerminalOpen => "open",
        Capability::TerminalFocus => "focus",
        Capability::ExecutionRun => "execute",
        Capability::ExecutionCancel => "cancel",
        Capability::ExecutionStatus => "status",
        Capability::ExecutionLogs => "logs",
        Capability::SessionGuide => "guide",
    }
}

fn validation_action_request(provider: &Manifest, capability: Capability, scope: &Path) -> Value {
    json!({
        "version": "orc.provider/v1",
        "action": capability_action(capability),
        "capability": capability,
        "scope": scope,
        "direction": "right",
        "command": ["true"],
        "environment": {},
        "plan": {
            "version": "orc.provider/v1",
            "command": ["true"],
            "cwd": scope,
            "environment": {},
            "successCodes": [0]
        },
        "rebindCurrent": false,
        "session": {
            "id": "orc-provider-validation",
            "nativeId": "orc-provider-validation",
            "traceId": null,
            "harness": "orc-provider-validation",
            "model": null,
            "role": "worker",
            "title": "Provider validation",
            "purpose": "Validate an advertised provider action",
            "goal": "Return a protocol-valid response without executing its command plan",
            "expectedOutput": "A declined response or a valid binding, description, or command plan",
            "successCriteria": [],
            "completion": "orchestrator",
            "reviewBy": null,
            "parentId": null,
            "runId": null,
            "nodeId": null,
            "providerRef": null,
            "providers": [{
                "provider": provider.name,
                "kind": provider.kind,
                "ref": "orc-provider-validation",
                "status": "active",
                "label": "Provider validation"
            }],
            "directory": scope,
            "registration": "managed",
            "status": "working",
            "connectedAt": "1970-01-01T00:00:00Z",
            "updatedAt": "1970-01-01T00:00:00Z"
        },
        "manifest": {
            "name": provider.name,
            "kind": provider.kind,
            "actions": provider.actions,
        },
    })
}

fn validate_action_response(
    provider: &Manifest,
    capability: Capability,
    result: Result<Value>,
) -> ValidationCheck {
    let name = format!("action:{capability}");
    let result = result.and_then(|value| {
        if value.get("version").and_then(Value::as_str) != Some("orc.provider/v1") {
            bail!("response has an invalid or missing version");
        }
        if value.get("status").and_then(Value::as_str) == Some("declined") {
            value
                .get("reason")
                .and_then(Value::as_str)
                .filter(|reason| !reason.trim().is_empty())
                .context("declined response is missing a reason")?;
            return Ok("declined the deterministic validation fixture".to_owned());
        }
        match capability {
            Capability::SessionBind => {
                let binding = value
                    .get("binding")
                    .context("response is missing binding")?;
                serde_json::from_value::<BindingStatus>(
                    binding
                        .get("status")
                        .context("binding is missing status")?
                        .clone(),
                )?;
                serde_json::from_value::<ProviderKind>(
                    binding
                        .get("kind")
                        .context("binding is missing kind")?
                        .clone(),
                )?;
                binding
                    .get("label")
                    .and_then(Value::as_str)
                    .filter(|label| !label.trim().is_empty())
                    .context("binding is missing label")?;
            }
            Capability::SessionDescribe => {
                let description = value
                    .get("description")
                    .and_then(Value::as_object)
                    .context("response is missing description")?;
                if !description
                    .values()
                    .filter_map(Value::as_str)
                    .any(|text| !text.trim().is_empty())
                {
                    bail!("description has no text fields");
                }
            }
            _ => {
                parse_plan(provider, value)?.context("provider declined without a status")?;
            }
        }
        Ok("returned a protocol-valid response".to_owned())
    });
    match result {
        Ok(message) => ValidationCheck {
            name,
            status: CheckStatus::Ok,
            message,
        },
        Err(error) => failed_check(name, format!("{error:#}")),
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
            checks.extend(provider.all_capabilities().into_iter().map(|capability| {
                let request = validation_action_request(&provider, capability, scope);
                validate_action_response(
                    &provider,
                    capability,
                    invoke_raw(&provider, &request, config),
                )
            }));
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
        let mut failures = Vec::new();
        for provider in stage {
            request["capability"] = Value::String(capability.to_string());
            request["plan"] = serde_json::to_value(&plan)?;
            match invoke_raw(provider, &request, config)
                .and_then(|value| parse_plan(provider, value))
            {
                Ok(Some(candidate)) => {
                    accepted = Some(candidate);
                    break;
                }
                Ok(None) => failures.push(format!("{} declined", provider.name)),
                Err(error) => failures.push(format!("{}: {error:#}", provider.name)),
            }
        }
        plan = Some(accepted.ok_or_else(|| {
            anyhow!(
                "no provider accepted capability {capability}: {}",
                failures.join("; ")
            )
        })?);
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
    rebind_current: bool,
) -> Vec<ProviderBinding> {
    candidates(providers, Capability::SessionBind).into_iter().filter_map(|provider| {
        let request = json!({
            "version": "orc.provider/v1", "action": "bind", "capability": Capability::SessionBind,
            "scope": scope, "session": session, "plan": null,
            "rebindCurrent": rebind_current,
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
    use std::os::unix::fs::PermissionsExt;

    fn provider_manifest(name: &str, command: &Path, priority: i64) -> Manifest {
        Manifest {
            version: "orc.provider/v1".into(),
            name: name.into(),
            description: name.into(),
            kind: ProviderKind::Integration,
            command: command.display().to_string(),
            actions: BTreeMap::from([(Capability::ChangesInspect, "Inspect changes".into())]),
            capabilities: Vec::new(),
            requires: Requirements::default(),
            priority,
        }
    }

    fn write_provider(directory: &Path, name: &str, source: &str) -> PathBuf {
        let path = directory.join(name);
        fs::write(&path, source).expect("provider script");
        let mut permissions = fs::metadata(&path)
            .expect("provider metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).expect("executable provider");
        path
    }

    #[test]
    fn discovers_nested_provider_manifests() {
        let directory = tempfile::tempdir().expect("provider directory");
        let nested = directory.path().join("example");
        fs::create_dir_all(&nested).expect("nested provider directory");
        fs::write(
            nested.join("provider.yaml"),
            r#"version: orc.provider/v1
name: example
command: example-provider
actions:
  session.bind: Bind a session
"#,
        )
        .expect("provider manifest");
        let mut config = Config::default();
        config.providers.directory = directory.path().to_path_buf();

        let providers = discover(&config).expect("nested provider discovered");

        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].name, "example");
    }

    #[test]
    fn attach_chain_composes_three_capabilities() {
        assert_eq!(Action::Attach.stages().len(), 3);
        assert!(Action::Attach.stages()[1].1);
    }

    #[test]
    fn focus_uses_only_the_display_integration() {
        assert_eq!(
            Action::Focus.stages(),
            &[(Capability::TerminalFocus, false)]
        );
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

    #[test]
    fn plan_resolution_continues_after_a_provider_error() {
        let directory = tempfile::tempdir().expect("provider directory");
        let broken = write_provider(directory.path(), "broken", "#!/bin/sh\nexit 7\n");
        let working = write_provider(
            directory.path(),
            "working",
            "#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{\"version\":\"orc.provider/v1\",\"command\":[\"true\"],\"successCodes\":[0]}'\n",
        );
        let providers = vec![
            provider_manifest("broken", &broken, 100),
            provider_manifest("working", &working, 0),
        ];

        let plan = resolve_plan(
            &Config::default(),
            &providers,
            Action::Changes,
            action_request(Action::Changes, directory.path(), None, "right"),
        )
        .expect("lower-priority provider should handle the action");

        assert_eq!(plan.command, ["true"]);
    }

    #[test]
    fn provider_validation_rejects_missing_and_malformed_checks() {
        for value in [
            json!({"version": "orc.provider/v1", "status": "ok"}),
            json!({"version": "orc.provider/v1", "checks": []}),
            json!({
                "version": "orc.provider/v1",
                "checks": [{"name": 1, "status": "ok", "message": "invalid"}]
            }),
        ] {
            let checks = provider_checks(Ok(value));
            assert_eq!(checks.len(), 1);
            assert_eq!(checks[0].status, CheckStatus::Failed);
            assert_eq!(checks[0].name, "provider");
        }
    }

    #[test]
    fn validation_exercises_each_advertised_action() {
        let directory = tempfile::tempdir().expect("provider directory");
        let calls = directory.path().join("calls.jsonl");
        let provider = write_provider(
            directory.path(),
            "provider",
            &format!(
                r#"#!/bin/sh
request=$(cat)
printf '%s\n' "$request" >> '{}'
case "$request" in
  *provider.validate*) printf '%s\n' '{{"version":"orc.provider/v1","checks":[{{"name":"protocol","status":"ok","message":"ready"}}]}}' ;;
  *) printf '%s\n' '{{"version":"orc.provider/v1","command":["true"],"successCodes":[0]}}' ;;
esac
"#,
                calls.display()
            ),
        );
        fs::write(
            directory.path().join("provider.yaml"),
            format!(
                "version: orc.provider/v1\nname: test\ncommand: {}\nactions:\n  changes.inspect: Inspect changes\n",
                provider.display()
            ),
        )
        .expect("provider manifest");
        let mut config = Config::default();
        config.providers.directory = directory.path().to_path_buf();

        let validations = validate_all(&config, directory.path(), None).expect("validation");

        assert_eq!(validations.len(), 1);
        assert_eq!(validations[0].status, CheckStatus::Ok);
        assert!(
            validations[0]
                .checks
                .iter()
                .any(|check| check.name == "action:changes.inspect")
        );
        let calls = fs::read_to_string(calls).expect("provider calls");
        assert!(calls.contains("provider.validate"));
        assert!(calls.contains("changes.inspect"));
    }

    #[test]
    fn activity_tail_discards_partial_first_line() {
        let directory = tempfile::tempdir().expect("activity directory");
        let path = directory.path().join("activity.jsonl");
        fs::write(&path, b"first line\nsecond line\nthird line\n").expect("activity log");

        let tail = read_file_tail(&path, 20).expect("activity tail");

        assert_eq!(String::from_utf8(tail).expect("utf-8 tail"), "third line\n");
    }

    #[test]
    fn provider_cache_pruning_keeps_a_bounded_number_of_entries() {
        let directory = tempfile::tempdir().expect("cache directory");
        for index in 0..=MAX_PROVIDER_CACHE_ENTRIES {
            fs::write(directory.path().join(format!("{index:03}.json")), b"{}").expect("cache");
        }

        prune_provider_cache(directory.path());

        assert_eq!(
            fs::read_dir(directory.path())
                .expect("cache entries")
                .count(),
            MAX_PROVIDER_CACHE_ENTRIES
        );
    }
}
