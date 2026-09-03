use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError},
    },
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::{
    fd::{AsRawFd, FromRawFd},
    unix::net::UnixStream,
    unix::process::CommandExt,
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
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_PROVIDER_CACHE_ENTRIES: usize = 256;
const MAX_PROVIDER_CACHE_ENTRY_BYTES: u64 = 1024 * 1024;
const MAX_PROVIDER_OUTPUT_BYTES: usize = 1024 * 1024;

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
    Cancel,
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
            Self::Cancel => &[(Capability::ExecutionCancel, false)],
            Self::Changes => &[(Capability::ChangesInspect, false)],
            Self::Execute => &[(Capability::ExecutionRun, true)],
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
            Self::Cancel => "cancel",
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
            match invoke_raw(provider, &request, config, None)
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
    discover_in(config.provider_directories())
}

fn discover_in(directories: impl IntoIterator<Item = PathBuf>) -> Result<Vec<Manifest>> {
    let mut providers = Vec::new();
    let mut names = BTreeSet::new();
    for directory in directories {
        if !directory.exists() {
            continue;
        }
        let mut paths = Vec::new();
        collect_manifest_paths(&directory, &mut paths)?;
        paths.sort();
        let mut directory_names = BTreeSet::new();
        for path in paths {
            let provider = read_manifest(&path)?;
            if !directory_names.insert(provider.name.clone()) {
                bail!(
                    "{}: duplicate provider name {} in {}",
                    path.display(),
                    provider.name,
                    directory.display()
                );
            }
            if names.insert(provider.name.clone()) {
                providers.push(provider);
            }
        }
    }
    Ok(providers)
}

fn read_manifest(path: &Path) -> Result<Manifest> {
    let metadata = fs::metadata(path)?;
    if metadata.len() > MAX_MANIFEST_BYTES {
        bail!("{}: provider manifest exceeds 1 MiB", path.display());
    }
    let source = fs::read_to_string(path)?;
    let provider = if path.extension().and_then(|value| value.to_str()) == Some("json") {
        serde_json::from_str(&source).with_context(|| format!("parse {}", path.display()))?
    } else {
        serde_yaml::from_str(&source).with_context(|| format!("parse {}", path.display()))?
    };
    validate_manifest(&provider, path)?;
    Ok(provider)
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
        ) && fs::metadata(&path)?.is_file()
        {
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

fn candidates_for_request<'a>(
    providers: &'a [Manifest],
    capability: Capability,
    request: &Value,
) -> Vec<&'a Manifest> {
    let selected = request
        .get("providers")
        .and_then(|providers| providers.get(capability.to_string()))
        .and_then(Value::as_str);
    candidates(providers, capability)
        .into_iter()
        .filter(|provider| selected.is_none_or(|name| provider.name == name))
        .collect()
}

fn invoke_raw(
    provider: &Manifest,
    request: &Value,
    config: &Config,
    tracker_directory: Option<&Path>,
) -> Result<Value> {
    let started = Instant::now();
    let payload = serde_json::to_vec(request)?;
    let mut command = Command::new(&provider.command);
    command
        .current_dir(request.get("scope").and_then(Value::as_str).unwrap_or("."))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let tracker_guard = tracker_directory
        .map(|directory| ProcessTrackerGuard::acquire(directory, Duration::from_secs(5)))
        .transpose()?;
    let mut tracker = tracker_directory.map(ProcessTracker::prepare).transpose()?;
    if let Some(tracker) = tracker.as_mut() {
        tracker.start_monitor()?;
    }
    let mut process_group = if tracker.is_none() {
        Some(ProcessGroup::start(None)?)
    } else {
        None
    };
    prepare_child(
        &mut command,
        tracker
            .as_ref()
            .and_then(ProcessTracker::group_id)
            .or_else(|| process_group.as_ref().and_then(ProcessGroup::id)),
    );
    let child = command.spawn();
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
    drop(tracker_guard);
    let stdout = drain_bounded(child.stdout.take().context("provider stdout")?);
    let stderr = drain_bounded(child.stderr.take().context("provider stderr")?);
    let stdin = child.stdin.take().context("provider stdin")?;
    let input_cancel = Arc::new(AtomicBool::new(false));
    let input_cancel_writer = Arc::clone(&input_cancel);
    let (input_sender, input_receiver) = mpsc::channel();
    let input = thread::spawn(move || {
        let _ = input_sender.send(write_provider_input(stdin, &payload, &input_cancel_writer));
    });
    let timeout = config.provider_timeout().saturating_sub(started.elapsed());
    let status = match child.wait_timeout(timeout)? {
        Some(status) => status,
        None => {
            input_cancel.store(true, Ordering::Relaxed);
            finish_process_control(&mut tracker, &mut process_group, tracker_directory)?;
            let _ = child.wait();
            let _ = input.join();
            discard_drain(stdout);
            discard_drain(stderr);
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
    finish_process_control(&mut tracker, &mut process_group, tracker_directory)?;
    let remaining = config.provider_timeout().saturating_sub(started.elapsed());
    let input_result = input_receiver.recv_timeout(remaining);
    if input_result.is_err() {
        input_cancel.store(true, Ordering::Relaxed);
        let _ = finish_process_control(&mut tracker, &mut process_group, tracker_directory);
    }
    let writer_panicked = input.join().is_err();
    let input_failure = if writer_panicked {
        Some("provider stdin writer panicked".to_owned())
    } else {
        match input_result {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(format!("provider request failed: {error}")),
            Err(RecvTimeoutError::Timeout) => {
                Some("provider timed out while reading its request".into())
            }
            Err(RecvTimeoutError::Disconnected) => Some("provider request writer stopped".into()),
        }
    };
    if let Some(message) = input_failure {
        discard_drain(stdout);
        discard_drain(stderr);
        record_invocation(
            provider,
            request,
            false,
            started.elapsed().as_millis(),
            &message,
        );
        bail!("{}: {message}", provider.name);
    }
    let stdout = finish_drain_with_timeout(
        stdout,
        config.provider_timeout().saturating_sub(started.elapsed()),
    )
    .with_context(|| format!("{} timed out draining stdout", provider.name))?;
    let stderr = finish_drain_with_timeout(
        stderr,
        config.provider_timeout().saturating_sub(started.elapsed()),
    )
    .with_context(|| format!("{} timed out draining stderr", provider.name))?;
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

#[cfg(unix)]
fn write_provider_input<W>(mut writer: W, payload: &[u8], cancel: &AtomicBool) -> io::Result<()>
where
    W: AsRawFd + Write,
{
    unsafe extern "C" {
        fn fcntl(fd: i32, command: i32, argument: i32) -> i32;
    }
    const GET_FLAGS: i32 = 3;
    const SET_FLAGS: i32 = 4;
    #[cfg(target_os = "linux")]
    const NONBLOCK: i32 = 0x0800;
    #[cfg(not(target_os = "linux"))]
    const NONBLOCK: i32 = 0x0004;
    let flags = unsafe { fcntl(writer.as_raw_fd(), GET_FLAGS, 0) };
    if flags < 0 || unsafe { fcntl(writer.as_raw_fd(), SET_FLAGS, flags | NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut written = 0;
    while written < payload.len() {
        if cancel.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "provider request delivery cancelled",
            ));
        }
        match writer.write(&payload[written..]) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(count) => written += count,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn write_provider_input<W>(mut writer: W, payload: &[u8], _cancel: &AtomicBool) -> io::Result<()>
where
    W: Write,
{
    writer.write_all(payload)
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
    let Some(_guard) = ActivityLogGuard::acquire(&path) else {
        return;
    };
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

struct ActivityLogGuard {
    #[cfg(unix)]
    file: File,
}

impl ActivityLogGuard {
    fn acquire(path: &Path) -> Option<Self> {
        #[cfg(unix)]
        {
            let lock_path = path.with_file_name(format!(
                ".{}.lock",
                path.file_name().unwrap_or_default().to_string_lossy()
            ));
            let file = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(lock_path)
                .ok()?;
            unsafe extern "C" {
                fn flock(fd: i32, operation: i32) -> i32;
            }
            const LOCK_EX: i32 = 2;
            const LOCK_NB: i32 = 4;
            if unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } != 0 {
                return None;
            }
            Some(Self { file })
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            Some(Self {})
        }
    }
}

impl Drop for ActivityLogGuard {
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
        return invoke_raw(provider, request, config, None);
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
    let value = invoke_raw(provider, request, config, None)?;
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
        "operationId": "orc-provider-validation-operation",
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
            checks.extend(provider_checks(invoke_raw(
                &provider, &request, config, None,
            )));
            checks.extend(provider.all_capabilities().into_iter().map(|capability| {
                let request = validation_action_request(&provider, capability, scope);
                validate_action_response(
                    &provider,
                    capability,
                    invoke_raw(&provider, &request, config, None),
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
    resolve_plan_from_tracked(config, providers, action, request, None, None, None)
}

pub(crate) fn resolve_plan_tracked(
    config: &Config,
    providers: &[Manifest],
    action: Action,
    request: Value,
    tracker_directory: &Path,
    cancelled: &dyn Fn() -> Result<bool>,
) -> Result<CommandPlan> {
    resolve_plan_from_tracked(
        config,
        providers,
        action,
        request,
        None,
        Some(tracker_directory),
        Some(cancelled),
    )
}

pub fn resolve_plan_from(
    config: &Config,
    providers: &[Manifest],
    action: Action,
    request: Value,
    plan: Option<CommandPlan>,
) -> Result<CommandPlan> {
    resolve_plan_from_tracked(config, providers, action, request, plan, None, None)
}

pub(crate) fn resolve_plan_from_tracked(
    config: &Config,
    providers: &[Manifest],
    action: Action,
    mut request: Value,
    mut plan: Option<CommandPlan>,
    tracker_directory: Option<&Path>,
    cancelled: Option<&dyn Fn() -> Result<bool>>,
) -> Result<CommandPlan> {
    for &(capability, optional) in action.stages() {
        if let Some(check) = cancelled
            && check()?
        {
            bail!("provider resolution cancelled");
        }
        let stage = candidates_for_request(providers, capability, &request);
        let explicitly_selected = request
            .get("providers")
            .and_then(|providers| providers.get(capability.to_string()))
            .and_then(Value::as_str)
            .is_some();
        if stage.is_empty() && optional && !explicitly_selected {
            continue;
        }
        if stage.is_empty() {
            bail!("no provider advertises capability {capability}");
        }
        let mut accepted = None;
        let mut failures = Vec::new();
        for provider in stage {
            if let Some(check) = cancelled
                && check()?
            {
                bail!("provider resolution cancelled");
            }
            request["capability"] = Value::String(capability.to_string());
            request["plan"] = serde_json::to_value(&plan)?;
            match invoke_raw(provider, &request, config, tracker_directory)
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
    run_plan_tracked(plan, scope, None)
}

pub(crate) fn run_plan_tracked(
    plan: &CommandPlan,
    scope: &Path,
    tracker_directory: Option<&Path>,
) -> Result<CommandResult> {
    run_plan_tracked_cancellable(plan, scope, tracker_directory, None)
}

pub(crate) fn run_plan_tracked_cancellable(
    plan: &CommandPlan,
    scope: &Path,
    tracker_directory: Option<&Path>,
    cancelled: Option<&dyn Fn() -> Result<bool>>,
) -> Result<CommandResult> {
    let program = plan.command.first().context("command plan is empty")?;
    let mut command = Command::new(program);
    command
        .args(&plan.command[1..])
        .current_dir(plan.cwd.as_deref().map(Path::new).unwrap_or(scope))
        .envs(&plan.environment)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let tracker_guard = tracker_directory
        .map(|directory| ProcessTrackerGuard::acquire(directory, Duration::from_secs(5)))
        .transpose()?;
    if let Some(check) = cancelled
        && check()?
    {
        bail!("command plan cancelled");
    }
    let mut tracker = tracker_directory.map(ProcessTracker::prepare).transpose()?;
    let mut process_group = if tracker.is_none() {
        Some(ProcessGroup::start(None)?)
    } else {
        None
    };
    if let Some(tracker) = tracker.as_mut() {
        tracker.start_monitor()?;
    }
    prepare_child(
        &mut command,
        tracker
            .as_ref()
            .and_then(ProcessTracker::group_id)
            .or_else(|| process_group.as_ref().and_then(ProcessGroup::id)),
    );
    let mut child = command.spawn()?;
    drop(tracker_guard);
    let stdout = drain_bounded(child.stdout.take().context("command plan stdout")?);
    let stderr = drain_bounded(child.stderr.take().context("command plan stderr")?);
    let status = child.wait()?;
    finish_process_control(&mut tracker, &mut process_group, tracker_directory)?;
    Ok(CommandResult {
        code: status.code().unwrap_or(1),
        stdout: finish_drain(stdout)?,
        stderr: finish_drain(stderr)?,
    })
}

fn finish_process_control(
    tracker: &mut Option<ProcessTracker>,
    process_group: &mut Option<ProcessGroup>,
    tracker_directory: Option<&Path>,
) -> Result<()> {
    let _guard = tracker_directory
        .map(|directory| ProcessTrackerGuard::acquire(directory, Duration::from_secs(5)))
        .transpose()?;
    if let Some(tracker) = tracker.as_mut() {
        tracker.finish_monitor()?;
    }
    if let Some(process_group) = process_group.as_mut() {
        process_group.finish()?;
    }
    Ok(())
}

struct ProcessTracker {
    path: PathBuf,
    file: File,
    process_group: Option<ProcessGroup>,
}

impl ProcessTracker {
    fn prepare(directory: &Path) -> Result<Self> {
        fs::create_dir_all(directory).context("create command tracker directory")?;
        let path = directory.join(format!("{}.process", uuid::Uuid::new_v4()));
        let file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&path)
            .context("create command tracker")?;
        #[cfg(unix)]
        {
            unsafe extern "C" {
                fn flock(fd: i32, operation: i32) -> i32;
            }
            const LOCK_EX: i32 = 2;
            if unsafe { flock(file.as_raw_fd(), LOCK_EX) } != 0 {
                return Err(io::Error::last_os_error()).context("lock command tracker");
            }
        }
        Ok(Self {
            path,
            file,
            process_group: None,
        })
    }

    fn fd(&self) -> i32 {
        #[cfg(unix)]
        {
            self.file.as_raw_fd()
        }
        #[cfg(not(unix))]
        {
            -1
        }
    }

    fn group_id(&self) -> Option<u32> {
        self.process_group.as_ref().and_then(ProcessGroup::id)
    }

    fn start_monitor(&mut self) -> Result<()> {
        #[cfg(unix)]
        {
            let tracker_fd = self.fd();
            self.process_group = Some(ProcessGroup::start(Some(tracker_fd))?);
            self.verify()?;
        }
        Ok(())
    }

    fn verify(&self) -> Result<()> {
        let process_id = self
            .group_id()
            .context("command process monitor is missing")?;
        let bytes = fs::read(&self.path).context("read command tracker")?;
        if bytes.as_slice() != process_id.to_ne_bytes() {
            bail!("command tracker process identity changed");
        }
        Ok(())
    }

    fn finish_monitor(&mut self) -> Result<()> {
        #[cfg(unix)]
        {
            unsafe extern "C" {
                fn flock(fd: i32, operation: i32) -> i32;
            }
            const LOCK_UN: i32 = 8;
            if unsafe { flock(self.file.as_raw_fd(), LOCK_UN) } != 0 {
                return Err(io::Error::last_os_error()).context("unlock command tracker");
            }
        }
        if let Some(mut process_group) = self.process_group.take() {
            process_group.finish()?;
        }
        Ok(())
    }
}

impl Drop for ProcessTracker {
    fn drop(&mut self) {
        self.process_group.take();
        let _ = fs::remove_file(&self.path);
    }
}

struct ProcessGroup {
    monitor: Option<Child>,
    #[cfg(unix)]
    parent_lifeline: Option<UnixStream>,
}

impl ProcessGroup {
    fn start(tracker_fd: Option<i32>) -> Result<Self> {
        #[cfg(unix)]
        {
            let (parent_reader, parent_lifeline) = if tracker_fd.is_none() {
                let (reader, writer) = UnixStream::pair().context("create process lifeline")?;
                (Some(reader), Some(writer))
            } else {
                (None, None)
            };
            let parent_fd = parent_reader.as_ref().map_or(-1, AsRawFd::as_raw_fd);
            #[cfg(not(test))]
            let mut command = {
                let mut command = Command::new(std::env::current_exe()?);
                command
                    .arg("process-monitor")
                    .arg(format!("--tracker-fd={}", tracker_fd.unwrap_or(-1)))
                    .arg(format!("--parent-fd={parent_fd}"))
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .process_group(0);
                command
            };
            #[cfg(test)]
            let mut command = {
                let mut command = Command::new("sh");
                command
                    .args(["-c", TEST_PROCESS_MONITOR])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .process_group(0);
                command
            };
            unsafe {
                command.pre_exec(move || {
                    if let Some(tracker_fd) = tracker_fd {
                        libc::signal(libc::SIGTERM, libc::SIG_IGN);
                        clear_close_on_exec(tracker_fd)?;
                        write_process_id(tracker_fd)?;
                    }
                    if parent_fd >= 0 {
                        clear_close_on_exec(parent_fd)?;
                    }
                    Ok(())
                });
            }
            let monitor = command.spawn().context("start command process monitor")?;
            Ok(Self {
                monitor: Some(monitor),
                parent_lifeline,
            })
        }
        #[cfg(not(unix))]
        {
            let _ = tracker_fd;
            Ok(Self { monitor: None })
        }
    }

    fn id(&self) -> Option<u32> {
        self.monitor.as_ref().map(Child::id)
    }

    fn finish(&mut self) -> Result<()> {
        if let Some(mut monitor) = self.monitor.take() {
            terminate_process_group(&mut monitor)?;
            monitor.wait().context("reap command process monitor")?;
        }
        #[cfg(unix)]
        self.parent_lifeline.take();
        Ok(())
    }
}

#[cfg(all(unix, test))]
const TEST_PROCESS_MONITOR: &str = r#"trap '' TERM
while :; do sleep 3600; done
"#;

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

#[cfg(unix)]
fn clear_close_on_exec(fd: i32) -> io::Result<()> {
    unsafe extern "C" {
        fn fcntl(fd: i32, command: i32, argument: i32) -> i32;
    }
    const GET_DESCRIPTOR_FLAGS: i32 = 1;
    const SET_DESCRIPTOR_FLAGS: i32 = 2;
    const CLOSE_ON_EXEC: i32 = 1;
    let flags = unsafe { fcntl(fd, GET_DESCRIPTOR_FLAGS, 0) };
    if flags < 0 || unsafe { fcntl(fd, SET_DESCRIPTOR_FLAGS, flags & !CLOSE_ON_EXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
fn write_process_id(fd: i32) -> io::Result<()> {
    unsafe extern "C" {
        fn getpid() -> i32;
        fn write(fd: i32, buffer: *const u8, count: usize) -> isize;
    }
    let process_id = (unsafe { getpid() } as u32).to_ne_bytes();
    if unsafe { write(fd, process_id.as_ptr(), process_id.len()) } != process_id.len() as isize {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn monitor_process(tracker_fd: i32, parent_fd: i32) -> Result<()> {
    unsafe {
        libc::signal(libc::SIGTERM, libc::SIG_IGN);
    }
    let _tracker = (tracker_fd >= 0).then(|| unsafe { File::from_raw_fd(tracker_fd) });
    if parent_fd >= 0 {
        let mut parent = unsafe { File::from_raw_fd(parent_fd) };
        let mut sentinel = [0_u8; 1];
        loop {
            match parent.read(&mut sentinel) {
                Ok(0) => break,
                Ok(_) => continue,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error).context("read process lifeline"),
            }
        }
        terminate_current_process_group();
    }
    loop {
        thread::sleep(Duration::from_secs(60));
    }
}

#[cfg(unix)]
fn terminate_current_process_group() -> ! {
    let group = unsafe { libc::getpgrp() };
    if group > 0 {
        unsafe {
            libc::killpg(group, libc::SIGTERM);
        }
        thread::sleep(Duration::from_millis(250));
        unsafe {
            libc::killpg(group, libc::SIGKILL);
        }
    }
    std::process::exit(0)
}

#[cfg(not(unix))]
pub(crate) fn monitor_process(_tracker_fd: i32, _parent_fd: i32) -> Result<()> {
    Ok(())
}

pub(crate) struct ProcessTrackerGuard {
    #[cfg(unix)]
    file: File,
}

impl ProcessTrackerGuard {
    pub(crate) fn acquire(directory: &Path, timeout: Duration) -> Result<Self> {
        fs::create_dir_all(directory).context("create command tracker directory")?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(directory.join(".lock"))
            .context("open command tracker lock")?;
        #[cfg(unix)]
        {
            unsafe extern "C" {
                fn flock(fd: i32, operation: i32) -> i32;
            }
            const LOCK_EX: i32 = 2;
            const LOCK_NB: i32 = 4;
            let deadline = Instant::now() + timeout;
            loop {
                if unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } == 0 {
                    break;
                }
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::WouldBlock {
                    return Err(error).context("acquire command tracker lock");
                }
                if Instant::now() >= deadline {
                    bail!("timed out acquiring the command tracker lock");
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
        Ok(Self {
            #[cfg(unix)]
            file,
        })
    }
}

impl Drop for ProcessTrackerGuard {
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

fn prepare_child(command: &mut Command, process_group: Option<u32>) {
    #[cfg(unix)]
    if let Some(process_group) = process_group {
        unsafe {
            command.pre_exec(move || {
                unsafe extern "C" {
                    fn setpgid(process_id: i32, process_group: i32) -> i32;
                }
                if setpgid(0, process_group as i32) != 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    #[cfg(not(unix))]
    let _ = (command, process_group);
}

pub fn run_plan_with_timeout(
    plan: &CommandPlan,
    scope: &Path,
    timeout: std::time::Duration,
) -> Result<CommandResult> {
    let started = Instant::now();
    let program = plan.command.first().context("command plan is empty")?;
    let mut command = Command::new(program);
    command
        .args(&plan.command[1..])
        .current_dir(plan.cwd.as_deref().map(Path::new).unwrap_or(scope))
        .envs(&plan.environment)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut process_group = ProcessGroup::start(None)?;
    prepare_child(&mut command, process_group.id());
    let mut child = command
        .spawn()
        .with_context(|| format!("start command plan {program}"))?;
    let stdout = drain_bounded(child.stdout.take().context("command plan stdout")?);
    let stderr = drain_bounded(child.stderr.take().context("command plan stderr")?);
    let status = match child.wait_timeout(timeout)? {
        Some(status) => status,
        None => {
            process_group.finish()?;
            let _ = child.wait();
            discard_drain(stdout);
            discard_drain(stderr);
            bail!("command plan timed out after {}ms", timeout.as_millis());
        }
    };
    process_group.finish()?;
    let stdout = match finish_drain_with_timeout(stdout, timeout.saturating_sub(started.elapsed()))
    {
        Ok(stdout) => stdout,
        Err(error) => {
            discard_drain(stderr);
            return Err(error).context("timed command plan did not close stdout");
        }
    };
    let stderr = finish_drain_with_timeout(stderr, timeout.saturating_sub(started.elapsed()))
        .context("timed command plan did not close stderr")?;
    Ok(CommandResult {
        code: status.code().unwrap_or(1),
        stdout,
        stderr,
    })
}

fn terminate_process_group(child: &mut Child) -> Result<()> {
    #[cfg(unix)]
    {
        unsafe extern "C" {
            fn kill(pid: i32, signal: i32) -> i32;
        }
        const SIGKILL: i32 = 9;
        if unsafe { kill(-(child.id() as i32), SIGKILL) } != 0 {
            let error = std::io::Error::last_os_error();
            if child.try_wait()?.is_none() {
                return Err(error).context("terminate command process group");
            }
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        child.kill().context("terminate command")
    }
}

struct BoundedOutput {
    bytes: Vec<u8>,
    truncated: bool,
    stream_open: bool,
}

struct OutputDrain {
    receiver: Receiver<io::Result<BoundedOutput>>,
    cancel: Arc<AtomicBool>,
}

#[cfg(unix)]
fn drain_bounded<R>(mut reader: R) -> OutputDrain
where
    R: AsRawFd + Read + Send + 'static,
{
    let (sender, receiver) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let thread_cancel = Arc::clone(&cancel);
    thread::spawn(move || {
        let result = (|| {
            let mut output = Vec::new();
            let mut buffer = [0_u8; 8192];
            let mut truncated = false;
            let mut stream_open = false;
            loop {
                if thread_cancel.load(Ordering::Relaxed) {
                    stream_open = true;
                    break;
                }
                if !poll_readable(reader.as_raw_fd())? {
                    continue;
                }
                let read = reader.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                let remaining = MAX_PROVIDER_OUTPUT_BYTES.saturating_sub(output.len());
                output.extend_from_slice(&buffer[..read.min(remaining)]);
                truncated |= read > remaining;
            }
            Ok(BoundedOutput {
                bytes: output,
                truncated,
                stream_open,
            })
        })();
        let _ = sender.send(result);
    });
    OutputDrain { receiver, cancel }
}

#[cfg(unix)]
fn poll_readable(fd: i32) -> io::Result<bool> {
    #[repr(C)]
    struct PollFd {
        fd: i32,
        events: i16,
        revents: i16,
    }
    unsafe extern "C" {
        fn poll(fds: *mut PollFd, count: usize, timeout: i32) -> i32;
    }
    const POLLIN: i16 = 0x0001;
    let mut descriptor = PollFd {
        fd,
        events: POLLIN,
        revents: 0,
    };
    let result = unsafe { poll(&mut descriptor, 1, 100) };
    if result >= 0 {
        return Ok(result > 0);
    }
    let error = io::Error::last_os_error();
    if error.kind() == io::ErrorKind::Interrupted {
        Ok(false)
    } else {
        Err(error)
    }
}

fn finish_drain(drain: OutputDrain) -> Result<String> {
    finish_drain_with_timeout(drain, std::time::Duration::from_secs(1))
}

fn finish_drain_with_timeout(drain: OutputDrain, timeout: std::time::Duration) -> Result<String> {
    let output = match drain.receiver.recv_timeout(timeout) {
        Ok(output) => output?,
        Err(RecvTimeoutError::Timeout) => {
            drain.cancel.store(true, Ordering::Relaxed);
            bail!("output stream did not close before the deadline");
        }
        Err(RecvTimeoutError::Disconnected) => bail!("output reader stopped unexpectedly"),
    };
    let mut rendered = String::from_utf8_lossy(&output.bytes).into_owned();
    if output.truncated {
        rendered.push_str("\n[output truncated by Orc]\n");
    }
    if output.stream_open {
        rendered.push_str("\n[output stream remained open after command exit]\n");
    }
    Ok(rendered)
}

fn discard_drain(drain: OutputDrain) {
    drain.cancel.store(true, Ordering::Relaxed);
    let _ = drain
        .receiver
        .recv_timeout(std::time::Duration::from_millis(250));
}

pub fn execute_plan(plan: &CommandPlan, scope: &Path, inherit: bool) -> Result<i32> {
    if inherit {
        return execute_inherited_plan_after_spawn(plan, scope, (), |_| Ok(()));
    }
    let result = run_plan(plan, scope)?;
    print!("{}", result.stdout);
    eprint!("{}", result.stderr);
    Ok(result.code)
}

pub fn execute_inherited_plan_after_spawn<F, G>(
    plan: &CommandPlan,
    scope: &Path,
    guard: G,
    after_spawn: F,
) -> Result<i32>
where
    F: FnOnce(&mut Child) -> Result<()>,
{
    let program = plan.command.first().context("command plan is empty")?;
    let mut command = Command::new(program);
    command
        .args(&plan.command[1..])
        .current_dir(plan.cwd.as_deref().map(Path::new).unwrap_or(scope))
        .envs(&plan.environment)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command.spawn()?;
    if let Err(error) = after_spawn(&mut child) {
        terminate_process_group(&mut child)?;
        let _ = child.wait();
        drop(guard);
        return Err(error);
    }
    drop(guard);
    Ok(child.wait()?.code().unwrap_or(1))
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
        let value = invoke_raw(provider, &request, config, None).ok()?;
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
    use crate::test_support::render_fixture;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    const VALIDATION_MANIFEST: &str = r#"version: orc.provider/v1
name: test
command: {{ command }}
actions:
  changes.inspect: Inspect changes
"#;

    const VALIDATION_PROVIDER: &str = r#"#!/bin/sh
request=$(cat)
printf '%s\n' "$request" >> '{{ calls }}'
case "$request" in
  *provider.validate*)
    cat <<'JSON'
{"version":"orc.provider/v1","checks":[{"name":"protocol","status":"ok","message":"ready"}]}
JSON
    ;;
  *)
    cat <<'JSON'
{"version":"orc.provider/v1","command":["true"],"successCodes":[0]}
JSON
    ;;
esac
"#;

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

        let providers =
            discover_in([config.providers.directory.clone()]).expect("nested provider discovered");

        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].name, "example");
    }

    #[test]
    fn configured_provider_overrides_an_installed_provider() {
        let directory = tempfile::tempdir().expect("provider directories");
        let configured = directory.path().join("configured");
        let installed = directory.path().join("installed");
        fs::create_dir_all(&configured).expect("configured directory");
        fs::create_dir_all(&installed).expect("installed directory");
        fs::write(
            configured.join("provider.yaml"),
            r#"version: orc.provider/v1
name: example
description: configured
command: configured-provider
actions:
  session.bind: Bind a session
"#,
        )
        .expect("configured manifest");
        fs::write(
            installed.join("provider.yaml"),
            r#"version: orc.provider/v1
name: example
description: installed
command: installed-provider
actions:
  session.bind: Bind a session
"#,
        )
        .expect("installed manifest");

        let providers = discover_in([configured, installed]).expect("provider discovery");

        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].description, "configured");
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
    fn provider_timeout_covers_blocked_request_delivery() {
        let directory = tempfile::tempdir().expect("provider directory");
        let command = write_provider(
            directory.path(),
            "blocked-input",
            r#"#!/bin/sh
sleep 10
"#,
        );
        let provider = provider_manifest("blocked-input", &command, 0);
        let mut config = Config::default();
        config.providers.timeout_ms = 10;
        let request = json!({
            "version": "orc.provider/v1",
            "scope": directory.path(),
            "payload": "x".repeat(2 * 1024 * 1024),
        });
        let started = Instant::now();

        let error = invoke_raw(&provider, &request, &config, None)
            .expect_err("provider that ignores input must time out");

        assert!(error.to_string().contains("timed out"));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    #[test]
    fn provider_timeout_covers_descendant_that_holds_request_pipe() {
        let directory = tempfile::tempdir().expect("provider directory");
        let command = write_provider(
            directory.path(),
            "inherited-input",
            r#"#!/bin/sh
(sleep 10) &
exit 0
"#,
        );
        let provider = provider_manifest("inherited-input", &command, 0);
        let mut config = Config::default();
        config.providers.timeout_ms = 20;
        let request = json!({
            "version": "orc.provider/v1",
            "scope": directory.path(),
            "payload": "x".repeat(2 * 1024 * 1024),
        });
        let started = Instant::now();

        let error = invoke_raw(&provider, &request, &config, None)
            .expect_err("provider descendant that holds stdin must time out");

        assert!(error.to_string().contains("timed out"));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    #[test]
    fn request_can_select_a_provider_for_one_capability() {
        let directory = tempfile::tempdir().expect("provider directory");
        let command = directory.path().join("provider");
        let first = provider_manifest("first", &command, 100);
        let second = provider_manifest("second", &command, 0);
        let request = json!({
            "providers": {"changes.inspect": "second"}
        });

        let providers = [first, second];
        let selected = candidates_for_request(&providers, Capability::ChangesInspect, &request);

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].name, "second");
    }

    #[test]
    fn timeout_stops_descendants_that_hold_output_pipes() {
        let plan = CommandPlan {
            version: "orc.provider/v1".into(),
            command: vec!["sh".into(), "-c".into(), "sleep 10 & wait".into()],
            cwd: None,
            environment: BTreeMap::new(),
            success_codes: vec![0],
        };
        let started = Instant::now();

        let error =
            run_plan_with_timeout(&plan, Path::new("."), std::time::Duration::from_millis(10))
                .expect_err("process tree must exceed the timeout");

        assert!(error.to_string().contains("timed out"));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    #[cfg(unix)]
    #[test]
    fn output_drain_fails_when_an_inherited_pipe_stays_open() {
        let (reader, _writer) = std::os::unix::net::UnixStream::pair().expect("pipe fixture");
        let started = Instant::now();

        let error = finish_drain(drain_bounded(reader)).expect_err("open pipe must miss deadline");

        assert!(error.to_string().contains("did not close"));
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    #[cfg(unix)]
    #[test]
    fn tracked_plan_reaps_background_descendants() {
        let directory = tempfile::tempdir().expect("tracker directory");
        let plan = CommandPlan {
            version: "orc.provider/v1".into(),
            command: vec!["sh".into(), "-c".into(), "sleep 10 & printf ready".into()],
            cwd: None,
            environment: BTreeMap::new(),
            success_codes: vec![0],
        };
        let started = Instant::now();

        let result =
            run_plan_tracked(&plan, Path::new("."), Some(directory.path())).expect("tracked plan");

        assert_eq!(result.stdout, "ready");
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(
            fs::read_dir(directory.path())
                .expect("tracker entries")
                .filter_map(|entry| entry.ok())
                .filter(
                    |entry| entry.path().extension().and_then(|value| value.to_str())
                        == Some("process")
                )
                .count(),
            0
        );
    }

    #[test]
    fn command_output_is_bounded_while_the_pipe_is_drained() {
        let plan = CommandPlan {
            version: "orc.provider/v1".into(),
            command: vec![
                "sh".into(),
                "-c".into(),
                format!("yes x | head -c {}", MAX_PROVIDER_OUTPUT_BYTES + 4096),
            ],
            cwd: None,
            environment: BTreeMap::new(),
            success_codes: vec![0],
        };

        let result =
            run_plan_with_timeout(&plan, Path::new("."), std::time::Duration::from_secs(2))
                .expect("large output command");

        assert!(result.stdout.len() < MAX_PROVIDER_OUTPUT_BYTES + 64);
        assert!(result.stdout.ends_with("[output truncated by Orc]\n"));
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
    fn cancellation_at_a_provider_phase_boundary_does_not_start_the_plan() {
        let directory = tempfile::tempdir().expect("tracker directory");
        let tracker_directory = directory.path().join("trackers");
        let marker = directory.path().join("started");
        let plan = CommandPlan {
            version: "orc.provider/v1".into(),
            command: vec![
                "sh".into(),
                "-c".into(),
                format!("touch {}", marker.display()),
            ],
            cwd: None,
            environment: BTreeMap::new(),
            success_codes: vec![0],
        };
        let guard = ProcessTrackerGuard::acquire(&tracker_directory, Duration::from_secs(1))
            .expect("hold tracker boundary");
        let cancelled = Arc::new(AtomicBool::new(true));
        let worker_cancelled = Arc::clone(&cancelled);
        let worker_directory = tracker_directory.clone();
        let worker = std::thread::spawn(move || {
            run_plan_tracked_cancellable(
                &plan,
                Path::new("."),
                Some(&worker_directory),
                Some(&|| Ok(worker_cancelled.load(Ordering::SeqCst))),
            )
        });
        std::thread::sleep(Duration::from_millis(50));
        drop(guard);

        let error = worker
            .join()
            .expect("plan worker")
            .expect_err("cancel plan");

        assert!(error.to_string().contains("cancelled"));
        assert!(!marker.exists());
    }

    #[test]
    fn plan_resolution_continues_after_a_provider_error() {
        let directory = tempfile::tempdir().expect("provider directory");
        let broken = write_provider(
            directory.path(),
            "broken",
            r#"#!/bin/sh
exit 7
"#,
        );
        let working = write_provider(
            directory.path(),
            "working",
            r#"#!/bin/sh
cat >/dev/null
cat <<'JSON'
{"version":"orc.provider/v1","command":["true"],"successCodes":[0]}
JSON
"#,
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
            &render_fixture(
                VALIDATION_PROVIDER,
                serde_json::json!({ "calls": calls.display().to_string() }),
            ),
        );
        fs::write(
            directory.path().join("provider.yaml"),
            render_fixture(
                VALIDATION_MANIFEST,
                serde_json::json!({ "command": provider.display().to_string() }),
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
        let activity = br#"first line
second line
third line
"#;
        fs::write(&path, activity).expect("activity log");

        let tail = read_file_tail(&path, 20).expect("activity tail");

        assert_eq!(String::from_utf8(tail).expect("utf-8 tail"), "third line\n");
    }

    #[cfg(unix)]
    #[test]
    fn activity_log_contention_drops_evidence_without_blocking() {
        let directory = tempfile::tempdir().expect("activity directory");
        let path = directory.path().join("activity.jsonl");
        let _guard = ActivityLogGuard::acquire(&path).expect("activity log lock");

        assert!(ActivityLogGuard::acquire(&path).is_none());
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
