use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const MAX_LOG_BYTES: u64 = 1024 * 1024;
const STOP_POLL_INTERVAL: Duration = Duration::from_millis(50);
const CONTROL_WAIT_TIMEOUT: Duration = Duration::from_secs(5);

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[cfg(unix)]
use std::os::{fd::AsRawFd, unix::process::CommandExt};

use crate::{
    config::{self, Config},
    control,
    domain::{LifecycleStatus, RegistrationSource, Session},
    provider, state, workflow,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub pid: u32,
    pub token: String,
    pub started_at: DateTime<Utc>,
    pub last_sweep_at: Option<DateTime<Utc>>,
    pub runtime_timeout_seconds: u64,
    pub idle_timeout_seconds: u64,
    #[serde(default)]
    pub executable_path: String,
    #[serde(default)]
    pub executable_identity: String,
    #[serde(default)]
    pub executable_version: String,
    #[serde(default)]
    pub config_fingerprint: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DaemonIdentity {
    executable_path: String,
    executable_identity: String,
    executable_version: String,
    config_fingerprint: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DaemonConfigFingerprint<'a> {
    cache: &'a crate::config::CacheConfig,
    daemon: DaemonRuntimeConfig,
    lifecycle: &'a crate::config::LifecycleConfig,
    providers: &'a crate::config::ProviderConfig,
    provider_runtime: &'a serde_json::Value,
    workflows: &'a crate::config::WorkflowConfig,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DaemonRuntimeConfig {
    scan_interval_ms: u64,
    idle_shutdown_seconds: u64,
    termination_retry_seconds: u64,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SweepReport {
    pub monitored: usize,
    pub unreadable: usize,
    pub terminated: Vec<String>,
    pub failures: Vec<String>,
}

struct DaemonLock {
    #[cfg(unix)]
    file: File,
    #[cfg(unix)]
    identity: File,
    token: String,
}

impl DaemonLock {
    fn acquire() -> Result<Self> {
        fs::create_dir_all(directory()).context("create daemon state directory")?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path())
            .context("open daemon lock")?;
        #[cfg(unix)]
        {
            unsafe extern "C" {
                fn flock(fd: i32, operation: i32) -> i32;
            }
            const LOCK_EX: i32 = 2;
            const LOCK_NB: i32 = 4;
            if unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } != 0 {
                bail!("Orc daemon is already running");
            }
            let token = Uuid::new_v4().to_string();
            let identity = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(identity_lock_path(&token))
                .context("open daemon identity lock")?;
            if unsafe { flock(identity.as_raw_fd(), LOCK_EX | LOCK_NB) } != 0 {
                bail!("acquire daemon identity lock");
            }
            Ok(Self {
                file,
                identity,
                token,
            })
        }
        #[cfg(not(unix))]
        {
            let _ = file;
            Ok(Self {
                token: Uuid::new_v4().to_string(),
            })
        }
    }
}

impl Drop for DaemonLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            unsafe extern "C" {
                fn flock(fd: i32, operation: i32) -> i32;
            }
            const LOCK_UN: i32 = 8;
            let _ = unsafe { flock(self.identity.as_raw_fd(), LOCK_UN) };
            let _ = unsafe { flock(self.file.as_raw_fd(), LOCK_UN) };
            let _ = fs::remove_file(identity_lock_path(&self.token));
        }
    }
}

fn directory() -> PathBuf {
    config::state_home().join("orc/daemon")
}

fn status_path() -> PathBuf {
    directory().join("status.json")
}

fn lock_path() -> PathBuf {
    directory().join("daemon.lock")
}

fn launcher_lock_path() -> PathBuf {
    directory().join("launcher.lock")
}

fn identity_lock_path(token: &str) -> PathBuf {
    directory().join(format!("identity-{token}.lock"))
}

fn log_path() -> PathBuf {
    directory().join("daemon.log")
}

fn previous_log_path() -> PathBuf {
    directory().join("daemon.previous.log")
}

fn stop_request_path(token: &str) -> PathBuf {
    directory().join(format!("stop-{token}.request"))
}

pub fn status() -> Result<Option<Status>> {
    let path = status_path();
    let source = match fs::read_to_string(&path) {
        Ok(source) => source,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("read daemon status"),
    };
    let status: Status = match serde_json::from_str(&source) {
        Ok(status) => status,
        Err(_error) if !lock_held()? => {
            cleanup_status(None)?;
            return Ok(None);
        }
        Err(error) => return Err(error).context("parse daemon status"),
    };
    if process_alive(status.pid) && identity_lock_held(&status.token)? {
        Ok(Some(status))
    } else {
        cleanup_status(Some(&status.token))?;
        Ok(None)
    }
}

fn identity_lock_held(token: &str) -> Result<bool> {
    let path = identity_lock_path(token);
    let held = inspect_lock(&path)?;
    if !held {
        let _ = fs::remove_file(path);
    }
    Ok(held)
}

fn lock_held() -> Result<bool> {
    inspect_lock(&lock_path())
}

fn inspect_lock(path: &Path) -> Result<bool> {
    #[cfg(unix)]
    {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .context("open daemon lock")?;
        unsafe extern "C" {
            fn flock(fd: i32, operation: i32) -> i32;
        }
        const LOCK_EX: i32 = 2;
        const LOCK_NB: i32 = 4;
        const LOCK_UN: i32 = 8;
        if unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } == 0 {
            let _ = unsafe { flock(file.as_raw_fd(), LOCK_UN) };
            return Ok(false);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::WouldBlock {
            return Ok(true);
        }
        Err(error).context("inspect daemon lock")
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(true)
    }
}

pub fn start() -> Result<Status> {
    start_with_config(&config::load()?)
}

fn start_with_config(config: &Config) -> Result<Status> {
    fs::create_dir_all(directory()).context("create daemon state directory")?;
    let expected = daemon_identity(config)?;
    loop {
        let launcher = acquire_blocking_lock(&launcher_lock_path())?;
        if let Some(status) = status()? {
            if daemon_matches(&status, &expected) {
                let _ = fs::remove_file(stop_request_path(&status.token));
                return Ok(status);
            }
            request_restart(&status, launcher)?;
            continue;
        }
        rotate_log()?;
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path())
            .context("open daemon log")?;
        let error_log = log.try_clone().context("clone daemon log handle")?;
        let mut command = Command::new(std::env::current_exe().context("resolve Orc executable")?);
        command
            .args(["daemon", "run"])
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(error_log));
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn().context("start Orc daemon")?;
        thread::spawn(move || {
            let _ = child.wait();
        });

        let deadline = Instant::now() + Duration::from_secs(2);
        let mut restart_requested = false;
        while Instant::now() < deadline {
            if let Some(status) = status()? {
                if daemon_matches(&status, &expected) {
                    return Ok(status);
                }
                request_restart(&status, launcher)?;
                restart_requested = true;
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }
        if restart_requested {
            continue;
        }
        if Instant::now() >= deadline {
            bail!(
                "Orc daemon did not become ready; see {}",
                log_path().display()
            );
        }
    }
}

pub fn ensure_running(_config: &Config) -> Result<Option<Status>> {
    #[cfg(test)]
    return Ok(None);
    #[cfg(not(test))]
    if _config.daemon.autostart {
        start_with_config(_config).map(Some)
    } else {
        Ok(None)
    }
}

fn request_restart(status: &Status, launcher: File) -> Result<()> {
    let request = stop_request_path(&status.token);
    fs::write(&request, status.token.as_bytes()).context("request Orc daemon restart")?;
    drop(launcher);
    if !wait_for_identity_release(&status.token, CONTROL_WAIT_TIMEOUT)? {
        if !stop_requested(&status.token)? {
            bail!("Orc daemon restart was cancelled by new managed work");
        }
        bail!("Orc daemon restart is still pending");
    }
    cleanup_status(Some(&status.token))?;
    let _ = fs::remove_file(request);
    Ok(())
}

pub fn stop() -> Result<bool> {
    fs::create_dir_all(directory()).context("create daemon state directory")?;
    let launcher = acquire_blocking_lock(&launcher_lock_path())?;
    let Some(status) = status()? else {
        return Ok(false);
    };
    let request = stop_request_path(&status.token);
    fs::write(&request, status.token.as_bytes()).context("request Orc daemon stop")?;
    drop(launcher);
    if !wait_for_identity_release(&status.token, CONTROL_WAIT_TIMEOUT)? {
        if !stop_requested(&status.token)? {
            bail!("Orc daemon stop was cancelled by new managed work");
        }
        bail!("Orc daemon stop is still pending");
    }
    cleanup_status(Some(&status.token))?;
    let _ = fs::remove_file(request);
    Ok(true)
}

fn wait_for_identity_release(token: &str, timeout: Duration) -> Result<bool> {
    let deadline = Instant::now() + timeout;
    while identity_lock_held(token)? {
        if !stop_requested(token)? {
            return Ok(false);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        thread::sleep(STOP_POLL_INTERVAL);
    }
    Ok(true)
}

fn acquire_blocking_lock(path: &Path) -> Result<File> {
    acquire_lock(path, CONTROL_WAIT_TIMEOUT)
}

fn acquire_lock(path: &Path, timeout: Duration) -> Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .context("open daemon launcher lock")?;
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
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::WouldBlock {
                return Err(error).context("acquire daemon launcher lock");
            }
            if Instant::now() >= deadline {
                bail!("timed out acquiring the Orc daemon launcher lock");
            }
            thread::sleep(STOP_POLL_INTERVAL);
        }
    }
    Ok(file)
}

pub fn run(config: &Config) -> Result<()> {
    let daemon_lock = DaemonLock::acquire()?;
    let identity = daemon_identity(config)?;
    let mut status = Status {
        pid: std::process::id(),
        token: daemon_lock.token.clone(),
        started_at: Utc::now(),
        last_sweep_at: None,
        runtime_timeout_seconds: config.lifecycle.runtime_timeout_seconds,
        idle_timeout_seconds: config.lifecycle.idle_timeout_seconds,
        executable_path: identity.executable_path,
        executable_identity: identity.executable_identity,
        executable_version: identity.executable_version,
        config_fingerprint: identity.config_fingerprint,
    };
    let stop_request = stop_request_path(&daemon_lock.token);
    let _ = fs::remove_file(&stop_request);
    write_status(&status)?;
    let mut idle_since = None;
    let mut last_failures = Vec::new();
    loop {
        if let Some(launcher) = claim_stop(&daemon_lock.token)? {
            return shutdown(daemon_lock, launcher, &stop_request);
        }
        let report = safe_sweep(config, Utc::now());
        status.last_sweep_at = Some(Utc::now());
        write_status(&status)?;
        if report.failures != last_failures {
            for failure in &report.failures {
                append_log(failure)?;
            }
            last_failures = report.failures.clone();
        }
        if report_is_idle(&report) {
            let since = idle_since.get_or_insert_with(Instant::now);
            if config.daemon.idle_shutdown_seconds > 0
                && since.elapsed() >= Duration::from_secs(config.daemon.idle_shutdown_seconds)
            {
                let launcher = acquire_blocking_lock(&launcher_lock_path())?;
                if report_is_idle(&safe_sweep(config, Utc::now())) {
                    return shutdown(daemon_lock, launcher, &stop_request);
                }
                drop(launcher);
                idle_since = None;
            }
        } else {
            idle_since = None;
        }
        if wait_for_next_sweep(&daemon_lock.token, config.daemon.scan_interval_ms)? {
            continue;
        }
    }
}

fn safe_sweep(config: &Config, now: DateTime<Utc>) -> SweepReport {
    sweep_at(config, now).unwrap_or_else(|error| SweepReport {
        unreadable: 1,
        failures: vec![format!("sweep Orc state: {error:#}")],
        ..SweepReport::default()
    })
}

fn report_is_idle(report: &SweepReport) -> bool {
    report.monitored == 0 && report.unreadable == 0
}

fn claim_stop(token: &str) -> Result<Option<File>> {
    if !stop_requested(token)? {
        return Ok(None);
    }
    let launcher = acquire_blocking_lock(&launcher_lock_path())?;
    if stop_requested(token)? {
        Ok(Some(launcher))
    } else {
        Ok(None)
    }
}

fn shutdown(daemon_lock: DaemonLock, launcher: File, stop_request: &Path) -> Result<()> {
    let token = daemon_lock.token.clone();
    drop(daemon_lock);
    cleanup_status(Some(&token))?;
    let _ = fs::remove_file(stop_request);
    drop(launcher);
    Ok(())
}

pub fn sweep(config: &Config) -> Result<SweepReport> {
    if status()?.is_some() {
        bail!("the running Orc daemon owns lease sweeps");
    }
    sweep_at(config, Utc::now())
}

fn sweep_at(config: &Config, now: DateTime<Utc>) -> Result<SweepReport> {
    let mut report = SweepReport::default();
    let root = config::state_home().join("orc");
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(report),
        Err(error) => return Err(error).context("read Orc state directory"),
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                report.unreadable += 1;
                report.failures.push(format!("read state entry: {error}"));
                continue;
            }
        };
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        merge_report(&mut report, sweep_state_file(config, &path, now));
    }
    Ok(report)
}

fn sweep_state_file(config: &Config, path: &Path, now: DateTime<Utc>) -> SweepReport {
    let scope = match scope_from_state_file(path) {
        Ok(scope) => scope,
        Err(error) => {
            return SweepReport {
                unreadable: 1,
                failures: vec![format!("read {}: {error:#}", path.display())],
                ..SweepReport::default()
            };
        }
    };
    match fs::metadata(&scope) {
        Ok(metadata) if metadata.is_dir() => sweep_scope_at(config, &scope, now),
        Ok(_) => SweepReport::default(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            sweep_persisted_scope_at(config, &scope, now)
        }
        Err(error) => SweepReport {
            unreadable: 1,
            failures: vec![format!("inspect workspace {}: {error}", scope.display())],
            ..SweepReport::default()
        },
    }
}

fn sweep_scope_at(config: &Config, scope: &Path, now: DateTime<Utc>) -> SweepReport {
    sweep_scope_at_mode(config, scope, now, false)
}

fn sweep_persisted_scope_at(config: &Config, scope: &Path, now: DateTime<Utc>) -> SweepReport {
    sweep_scope_at_mode(config, scope, now, true)
}

fn sweep_scope_at_mode(
    config: &Config,
    scope: &Path,
    now: DateTime<Utc>,
    persisted_only: bool,
) -> SweepReport {
    let mut report = SweepReport::default();
    let workspace = match state::read(scope) {
        Ok(workspace) => workspace,
        Err(error) => {
            report.unreadable += 1;
            report
                .failures
                .push(format!("read {}: {error:#}", scope.display()));
            return report;
        }
    };
    if !persisted_only {
        for run in workspace.runs.iter().filter(|run| {
            (run.status == LifecycleStatus::Terminating
                && run.parent_run_id.as_ref().is_none_or(|parent_id| {
                    !workspace.runs.iter().any(|parent| {
                        parent.id == *parent_id && parent.status == LifecycleStatus::Terminating
                    })
                }))
                || (run.resume_requested
                    && run.status.active()
                    && run.status != LifecycleStatus::Terminating)
                || (run.status == LifecycleStatus::Working && run.process_id.is_some())
        }) {
            report.monitored += 1;
            if run.status == LifecycleStatus::Terminating {
                if let Err(error) = workflow::cancel(config, scope, &run.id) {
                    report.failures.push(format!(
                        "resume workflow cancellation {} in {}: {error:#}",
                        run.id,
                        scope.display()
                    ));
                }
                continue;
            }
            if run.resume_requested {
                if let Err(error) = workflow::spawn(config, scope, &run.id) {
                    report.failures.push(format!(
                        "resume workflow {} in {}: {error:#}",
                        run.id,
                        scope.display()
                    ));
                }
                continue;
            }
            match workflow::executor_active(scope, run) {
                Ok(true) => {}
                Ok(false) => {
                    if let Err(error) = workflow::spawn(config, scope, &run.id) {
                        report.failures.push(format!(
                            "recover workflow {} in {}: {error:#}",
                            run.id,
                            scope.display()
                        ));
                    }
                }
                Err(error) => report.failures.push(format!(
                    "inspect workflow {} in {}: {error:#}",
                    run.id,
                    scope.display()
                )),
            }
        }
    }
    for session in workspace
        .sessions
        .iter()
        .filter(|session| monitored(session))
    {
        report.monitored += 1;
        if retry_delayed(config, session, now) {
            continue;
        }
        let pending_claim = session
            .termination_reason
            .as_deref()
            .filter(|reason| reason.starts_with("termination pending "));
        let failed_termination = session
            .termination_reason
            .as_deref()
            .is_some_and(|reason| reason.starts_with("termination failed:"));
        if let Some(claim) = pending_claim {
            match control::termination_claim_active(scope, &session.id, claim) {
                Ok(true) => continue,
                Ok(false) => {}
                Err(error) => {
                    report.failures.push(format!(
                        "inspect termination claim for {}: {error:#}",
                        session.id
                    ));
                    continue;
                }
            }
        }
        let reason = if pending_claim.is_some() {
            session
                .termination_cause
                .as_deref()
                .unwrap_or("resuming interrupted termination")
        } else if failed_termination {
            let Some(reason) = failed_termination_reason(config, session, now) else {
                continue;
            };
            reason
        } else if let Some(reason) = expiration_reason(config, session, now) {
            reason
        } else {
            continue;
        };
        let observed_heartbeat = (pending_claim.is_none() && reason == "idle timeout exceeded")
            .then_some(session.heartbeat_at);
        let termination = if persisted_only {
            control::terminate_expired_persisted_scope(
                config,
                scope,
                &session.id,
                reason,
                observed_heartbeat,
                pending_claim,
            )
        } else {
            control::terminate_expired(
                config,
                scope,
                &session.id,
                reason,
                observed_heartbeat,
                pending_claim,
            )
        };
        match termination {
            Ok(stopped)
                if stopped.status == crate::domain::LifecycleStatus::Cancelled
                    && stopped.termination_reason.as_deref() == Some(reason) =>
            {
                report.terminated.push(session.id.clone());
            }
            Ok(_) => {}
            Err(error) => report.failures.push(format!(
                "terminate {} in {}: {error:#}",
                session.id,
                scope.display()
            )),
        }
    }
    report
}

fn retry_delayed(config: &Config, session: &Session, now: DateTime<Utc>) -> bool {
    if config.daemon.termination_retry_seconds == 0
        || !session
            .termination_reason
            .as_deref()
            .is_some_and(|reason| reason.starts_with("termination failed:"))
    {
        return false;
    }
    let attempted_at = session.termination_attempt_at.unwrap_or(session.updated_at);
    let elapsed = (now - attempted_at).num_seconds();
    elapsed >= 0 && (elapsed as u64) < config.daemon.termination_retry_seconds
}

fn failed_termination_reason<'a>(
    config: &Config,
    session: &'a Session,
    now: DateTime<Utc>,
) -> Option<&'a str> {
    match session.termination_cause.as_deref() {
        Some("idle timeout exceeded") => expiration_reason(config, session, now),
        Some(reason) => Some(reason),
        None => Some("retrying failed termination"),
    }
}

fn merge_report(report: &mut SweepReport, addition: SweepReport) {
    report.monitored += addition.monitored;
    report.unreadable += addition.unreadable;
    report.terminated.extend(addition.terminated);
    report.failures.extend(addition.failures);
}

fn monitored(session: &Session) -> bool {
    let failed_termination = session
        .termination_reason
        .as_deref()
        .is_some_and(|reason| reason.starts_with("termination failed:"));
    session.registration == RegistrationSource::Managed
        && control::terminable(session)
        && (failed_termination
            || !matches!(
                session.status,
                crate::domain::LifecycleStatus::Done
                    | crate::domain::LifecycleStatus::Failed
                    | crate::domain::LifecycleStatus::Cancelled
                    | crate::domain::LifecycleStatus::Archived
            ))
}

fn expiration_reason(
    config: &Config,
    session: &Session,
    now: DateTime<Utc>,
) -> Option<&'static str> {
    let runtime = session
        .runtime_timeout_seconds
        .unwrap_or(config.lifecycle.runtime_timeout_seconds);
    let runtime_elapsed = (now - session.connected_at).num_seconds();
    if runtime > 0 && runtime_elapsed >= 0 && runtime_elapsed as u64 >= runtime {
        return Some("runtime timeout exceeded");
    }
    let idle = session
        .idle_timeout_seconds
        .unwrap_or(config.lifecycle.idle_timeout_seconds);
    let heartbeat = session.heartbeat_at.unwrap_or(session.updated_at);
    let idle_elapsed = (now - heartbeat).num_seconds();
    if idle > 0 && idle_elapsed >= 0 && idle_elapsed as u64 >= idle {
        return Some("idle timeout exceeded");
    }
    None
}

fn scope_from_state_file(path: &Path) -> Result<PathBuf> {
    let value: serde_json::Value = serde_json::from_slice(
        &fs::read(path).with_context(|| format!("read {}", path.display()))?,
    )
    .with_context(|| format!("parse {}", path.display()))?;
    let scope = value
        .get("scope")
        .and_then(serde_json::Value::as_str)
        .context("state has no scope")?;
    Ok(PathBuf::from(scope))
}

fn daemon_identity(config: &Config) -> Result<DaemonIdentity> {
    let executable = std::env::current_exe().context("resolve Orc executable")?;
    let executable = fs::canonicalize(&executable).unwrap_or(executable);
    let executable_identity = hash_reader(
        File::open(&executable)
            .with_context(|| format!("open Orc executable {}", executable.display()))?,
    )?;
    Ok(DaemonIdentity {
        executable_path: executable.display().to_string(),
        executable_identity,
        executable_version: env!("CARGO_PKG_VERSION").into(),
        config_fingerprint: config_fingerprint(config)?,
    })
}

fn config_fingerprint(config: &Config) -> Result<String> {
    let provider_runtime = provider::runtime_fingerprint(config)?;
    config_fingerprint_for_runtime(config, &provider_runtime)
}

fn config_fingerprint_for_runtime(
    config: &Config,
    provider_runtime: &serde_json::Value,
) -> Result<String> {
    let relevant = DaemonConfigFingerprint {
        cache: &config.cache,
        daemon: DaemonRuntimeConfig {
            scan_interval_ms: config.daemon.scan_interval_ms,
            idle_shutdown_seconds: config.daemon.idle_shutdown_seconds,
            termination_retry_seconds: config.daemon.termination_retry_seconds,
        },
        lifecycle: &config.lifecycle,
        providers: &config.providers,
        provider_runtime,
        workflows: &config.workflows,
    };
    let encoded = serde_json::to_vec(&relevant).context("encode Orc daemon configuration")?;
    Ok(hex::encode(Sha256::digest(encoded)))
}

fn hash_reader(mut reader: impl Read) -> Result<String> {
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = reader.read(&mut buffer).context("read Orc executable")?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(hex::encode(digest.finalize()))
}

fn daemon_matches(status: &Status, expected: &DaemonIdentity) -> bool {
    status.executable_path == expected.executable_path
        && status.executable_identity == expected.executable_identity
        && status.executable_version == expected.executable_version
        && status.config_fingerprint == expected.config_fingerprint
}

fn write_status(status: &Status) -> Result<()> {
    fs::create_dir_all(directory()).context("create daemon state directory")?;
    let temporary = directory().join(format!(".status-{}.tmp", std::process::id()));
    fs::write(&temporary, serde_json::to_vec_pretty(status)?)
        .context("write temporary daemon status")?;
    fs::rename(temporary, status_path()).context("commit daemon status")
}

fn cleanup_status(expected_token: Option<&str>) -> Result<()> {
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path())
        .context("open daemon cleanup lock")?;
    #[cfg(unix)]
    {
        unsafe extern "C" {
            fn flock(fd: i32, operation: i32) -> i32;
        }
        const LOCK_EX: i32 = 2;
        const LOCK_NB: i32 = 4;
        if unsafe { flock(lock.as_raw_fd(), LOCK_EX | LOCK_NB) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::WouldBlock {
                return Ok(());
            }
            return Err(error).context("acquire daemon cleanup lock");
        }
    }
    let remove = match fs::read_to_string(status_path()) {
        Ok(source) => match serde_json::from_str::<Status>(&source) {
            Ok(status) => expected_token.is_none_or(|token| status.token == token),
            Err(_) => expected_token.is_none(),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error).context("read daemon status during cleanup"),
    };
    if remove {
        fs::remove_file(status_path()).context("remove stale daemon status")?;
    }
    drop(lock);
    Ok(())
}

fn stop_requested(token: &str) -> Result<bool> {
    let path = stop_request_path(token);
    match fs::read_to_string(path) {
        Ok(requested) => Ok(requested == token),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).context("read Orc daemon stop request"),
    }
}

fn wait_for_next_sweep(token: &str, interval_ms: u64) -> Result<bool> {
    if interval_ms > 3_600_000 {
        bail!("daemon scan interval exceeds the supported platform clock range");
    }
    let deadline = Instant::now()
        .checked_add(Duration::from_millis(interval_ms))
        .context("daemon scan interval exceeds the platform clock range")?;
    while Instant::now() < deadline {
        if stop_requested(token)? {
            return Ok(true);
        }
        thread::sleep(STOP_POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
    }
    Ok(false)
}

fn rotate_log() -> Result<()> {
    let current = log_path();
    let previous = previous_log_path();
    let mut source = match File::open(&current) {
        Ok(source) => source,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("open previous daemon log"),
    };
    let length = source.metadata()?.len();
    let offset = length.saturating_sub(MAX_LOG_BYTES);
    source.seek(SeekFrom::Start(offset))?;
    let mut content = Vec::with_capacity((length - offset) as usize);
    source.read_to_end(&mut content)?;
    fs::write(&previous, content).context("preserve previous daemon log")?;
    OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(current)
        .context("rotate daemon log")?;
    Ok(())
}

fn append_log(message: &str) -> Result<()> {
    let path = log_path();
    let content = message.as_bytes();
    let kept = &content[content
        .len()
        .saturating_sub(MAX_LOG_BYTES.saturating_sub(1) as usize)..];
    let current = fs::metadata(&path).map_or(0, |metadata| metadata.len());
    if current.saturating_add(kept.len() as u64 + 1) > MAX_LOG_BYTES {
        rotate_log()?;
    }
    let mut log = OpenOptions::new().create(true).append(true).open(path)?;
    log.write_all(kept)?;
    writeln!(log).context("write daemon log")
}

fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        unsafe extern "C" {
            fn kill(pid: i32, signal: i32) -> i32;
        }
        if unsafe { kill(pid as i32, 0) } == 0 {
            return true;
        }
        std::io::Error::last_os_error().kind() == std::io::ErrorKind::PermissionDenied
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        control::{Contract, SessionLink},
        domain::{
            BindingStatus, CompletionTarget, LifecycleStatus, ProviderBinding, ProviderKind,
            SessionRole,
        },
        test_support::render_fixture,
    };
    use std::os::unix::fs::PermissionsExt;

    const STOP_PROVIDER: &str = r#"#!/bin/sh
cat >/dev/null
cat <<'JSON'
{"version":"orc.provider/v1","command":["touch","{{ marker }}"]}
JSON
"#;

    const STOP_PROVIDER_MANIFEST: &str = r#"version: orc.provider/v1
name: stopper
command: {{ command }}
actions:
  session.stop: Stop a session
"#;

    #[test]
    fn launcher_lock_contention_has_a_deadline() {
        let directory = tempfile::tempdir().expect("daemon lock directory");
        let path = directory.path().join("launcher.lock");
        let _owner = acquire_lock(&path, Duration::ZERO).expect("launcher lock");

        let error = acquire_lock(&path, Duration::ZERO).expect_err("contended lock must fail");

        assert!(error.to_string().contains("timed out"));
    }

    #[test]
    fn unreadable_state_prevents_idle_shutdown() {
        let report = SweepReport {
            unreadable: 1,
            ..SweepReport::default()
        };

        assert!(!report_is_idle(&report));
        assert!(report_is_idle(&SweepReport::default()));
    }

    #[test]
    fn config_fingerprint_tracks_only_daemon_runtime_settings() {
        let config = Config::default();
        let baseline = config_fingerprint(&config).expect("config fingerprint");
        assert_eq!(
            baseline,
            config_fingerprint(&config).expect("stable config fingerprint")
        );

        let mut ui_change = config.clone();
        ui_change.ui.refresh_ms += 1;
        assert_eq!(
            baseline,
            config_fingerprint(&ui_change).expect("UI-independent fingerprint")
        );

        let mut launch_change = config.clone();
        launch_change.daemon.autostart = !launch_change.daemon.autostart;
        assert_eq!(
            baseline,
            config_fingerprint(&launch_change).expect("launch-independent fingerprint")
        );

        let mut lifecycle_change = config;
        lifecycle_change.lifecycle.idle_timeout_seconds += 1;
        assert_ne!(
            baseline,
            config_fingerprint(&lifecycle_change).expect("changed config fingerprint")
        );

        let providers_v1 = serde_json::json!({"runtimeGeneration": "providers-v1"});
        let providers_v2 = serde_json::json!({"runtimeGeneration": "providers-v2"});
        assert_ne!(
            config_fingerprint_for_runtime(&Config::default(), &providers_v1)
                .expect("first provider generation"),
            config_fingerprint_for_runtime(&Config::default(), &providers_v2)
                .expect("second provider generation")
        );
    }

    #[test]
    fn daemon_status_requires_the_current_binary_and_configuration() {
        let expected = DaemonIdentity {
            executable_path: "/nix/store/current/bin/orc".into(),
            executable_identity: "sha256-current".into(),
            executable_version: "1.2.3".into(),
            config_fingerprint: "config-current".into(),
        };
        let mut status = Status {
            pid: 42,
            token: "token".into(),
            started_at: Utc::now(),
            last_sweep_at: None,
            runtime_timeout_seconds: 60,
            idle_timeout_seconds: 30,
            executable_path: expected.executable_path.clone(),
            executable_identity: expected.executable_identity.clone(),
            executable_version: expected.executable_version.clone(),
            config_fingerprint: expected.config_fingerprint.clone(),
        };
        assert!(daemon_matches(&status, &expected));

        status.executable_identity = "sha256-old".into();
        assert!(!daemon_matches(&status, &expected));
        status.executable_identity = expected.executable_identity.clone();
        status.config_fingerprint = "config-old".into();
        assert!(!daemon_matches(&status, &expected));
        status.config_fingerprint = expected.config_fingerprint.clone();
        status.executable_version = "1.2.2".into();
        assert!(!daemon_matches(&status, &expected));
    }

    #[test]
    fn deleted_workspace_state_is_dormant_and_preserved() {
        let directory = tempfile::tempdir().expect("daemon fixture");
        let missing_scope = directory.path().join("deleted-workspace");
        let state_file = directory.path().join("persisted.json");
        fs::write(
            &state_file,
            serde_json::to_vec(&serde_json::json!({
                "scope": missing_scope.display().to_string()
            }))
            .expect("persisted state fixture"),
        )
        .expect("write persisted state fixture");

        let report = sweep_state_file(&Config::default(), &state_file, Utc::now());

        assert!(report_is_idle(&report));
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert!(
            state_file.exists(),
            "persisted state must remain recoverable"
        );
    }

    #[test]
    fn deleted_workspace_keeps_managed_sessions_under_supervision() {
        let directory = tempfile::tempdir().expect("daemon fixture");
        let missing_scope = directory.path().join("deleted-workspace");
        let now = Utc::now();
        state::update(&missing_scope, |workspace| {
            workspace.sessions.push(session(now));
            Ok(())
        })
        .expect("persist managed session");
        let state_file = state::path(&missing_scope);
        let mut config = Config::default();
        config.lifecycle.runtime_timeout_seconds = 0;
        config.lifecycle.idle_timeout_seconds = 0;

        let report = sweep_state_file(&config, &state_file, now);

        assert_eq!(report.monitored, 1);
        assert!(!report_is_idle(&report));
        assert!(report.failures.is_empty());
        assert!(state_file.exists());
        let _ = fs::remove_file(state_file);
    }

    fn session(at: DateTime<Utc>) -> Session {
        Session {
            id: "worker".into(),
            native_id: "native".into(),
            trace_id: None,
            harness: "test".into(),
            model: None,
            role: SessionRole::Worker,
            title: "worker".into(),
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
            registration: RegistrationSource::Managed,
            status: LifecycleStatus::Working,
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
    fn hard_timeout_does_not_move_with_heartbeat() {
        let mut config = Config::default();
        config.lifecycle.runtime_timeout_seconds = 60;
        config.lifecycle.idle_timeout_seconds = 60;
        let start = Utc::now();
        let mut session = session(start);
        session.heartbeat_at = Some(start + chrono::Duration::seconds(59));
        assert_eq!(
            expiration_reason(&config, &session, start + chrono::Duration::seconds(60)),
            Some("runtime timeout exceeded")
        );
    }

    #[test]
    fn maximum_timeout_does_not_wrap() {
        let mut config = Config::default();
        config.lifecycle.runtime_timeout_seconds = u64::MAX;
        config.lifecycle.idle_timeout_seconds = u64::MAX;
        let start = Utc::now();
        assert_eq!(
            expiration_reason(
                &config,
                &session(start),
                start + chrono::Duration::seconds(1)
            ),
            None
        );
    }

    #[test]
    fn maximum_scan_interval_returns_an_error() {
        let error = wait_for_next_sweep("not-a-live-daemon", u64::MAX)
            .expect_err("oversized interval must fail");

        assert!(error.to_string().contains("platform clock range"));
    }

    #[test]
    fn heartbeat_renews_idle_timeout() {
        let mut config = Config::default();
        config.lifecycle.runtime_timeout_seconds = 0;
        config.lifecycle.idle_timeout_seconds = 60;
        let start = Utc::now();
        let mut session = session(start);
        session.heartbeat_at = Some(start + chrono::Duration::seconds(30));
        assert_eq!(
            expiration_reason(&config, &session, start + chrono::Duration::seconds(61)),
            None
        );
        assert_eq!(
            expiration_reason(&config, &session, start + chrono::Duration::seconds(90)),
            Some("idle timeout exceeded")
        );
    }

    #[test]
    fn only_managed_sessions_are_monitored() {
        let now = Utc::now();
        let mut candidate = session(now);
        assert!(monitored(&candidate));
        candidate.role = SessionRole::Orchestrator;
        assert!(monitored(&candidate));
        candidate.registration = RegistrationSource::Connected;
        assert!(!monitored(&candidate));
        candidate.registration = RegistrationSource::Managed;
        candidate.status = LifecycleStatus::Terminating;
        candidate.termination_reason = Some("termination pending test".into());
        assert!(monitored(&candidate));
    }

    #[test]
    fn failed_termination_waits_before_retrying() {
        let mut config = Config::default();
        config.daemon.termination_retry_seconds = 60;
        let now = Utc::now();
        let mut candidate = session(now);
        candidate.termination_reason = Some("termination failed: unavailable".into());
        candidate.termination_attempt_at = Some(now);

        assert!(retry_delayed(
            &config,
            &candidate,
            now + chrono::Duration::seconds(59)
        ));
        assert!(!retry_delayed(
            &config,
            &candidate,
            now + chrono::Duration::seconds(60)
        ));
    }

    #[test]
    fn renewed_idle_lease_suppresses_a_failed_termination_retry() {
        let directory = tempfile::tempdir().expect("scope");
        let scope_directory = directory.path().join("scope");
        fs::create_dir_all(&scope_directory).expect("scope directory");
        let scope = fs::canonicalize(scope_directory).expect("canonical scope");
        let mut config = Config::default();
        config.providers.directory = directory.path().join("missing-providers");
        config.lifecycle.runtime_timeout_seconds = 0;
        config.lifecycle.idle_timeout_seconds = 60;
        config.daemon.termination_retry_seconds = 0;
        let linked = control::register(
            &scope,
            Contract {
                role: SessionRole::Worker,
                ..Contract::default()
            },
            SessionLink {
                id: Some(format!("worker-{}", Uuid::new_v4())),
                native_id: Some(Uuid::new_v4().to_string()),
                source: RegistrationSource::Connected,
                ..SessionLink::default()
            },
        )
        .expect("managed session");
        state::update(&scope, |workspace| {
            workspace.sessions[0].registration = RegistrationSource::Managed;
            workspace.sessions[0].providers.push(ProviderBinding {
                provider: "missing-stopper".into(),
                kind: ProviderKind::Persistence,
                r#ref: Some("managed-process".into()),
                status: BindingStatus::Active,
                label: "Launch ownership: missing stop provider".into(),
            });
            Ok(())
        })
        .expect("record missing lifecycle owner");
        control::terminate(&config, &scope, &linked.id, "idle timeout exceeded")
            .expect_err("missing provider fails the first termination");
        let failed = state::read(&scope)
            .expect("failed termination state")
            .sessions
            .into_iter()
            .find(|session| session.id == linked.id)
            .expect("managed session");

        let renewed = control::keepalive(&scope, &linked.id).expect("renew idle lease");
        let report = sweep_scope_at(
            &config,
            &scope,
            renewed.heartbeat_at.expect("renewed heartbeat") + chrono::Duration::seconds(1),
        );

        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert!(report.terminated.is_empty());
        assert_eq!(renewed.status, LifecycleStatus::Working);
        assert_eq!(
            renewed.termination_operation_id,
            failed.termination_operation_id
        );
        assert_eq!(
            renewed.termination_cause.as_deref(),
            Some("idle timeout exceeded")
        );
        let _ = fs::remove_file(state::path(&scope));
    }

    #[test]
    fn sweep_finishes_an_interrupted_workflow_cancellation() {
        let directory = tempfile::tempdir().expect("scope");
        let scope = directory.path().join("scope");
        fs::create_dir_all(&scope).expect("scope directory");
        let scope = fs::canonicalize(scope).expect("canonical scope");
        let run = control::create_run(
            &scope,
            "cancelled-work".into(),
            "test cancellation recovery".into(),
            "cancelled run".into(),
            None,
            None,
            None,
        )
        .expect("workflow run");
        state::update(&scope, |workspace| {
            let selected = workspace
                .runs
                .iter_mut()
                .find(|candidate| candidate.id == run.id)
                .expect("run");
            selected.status = LifecycleStatus::Terminating;
            Ok(())
        })
        .expect("interrupt cancellation");

        let report = sweep_scope_at(&Config::default(), &scope, Utc::now());

        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert_eq!(
            state::read(&scope)
                .expect("workspace")
                .runs
                .into_iter()
                .find(|candidate| candidate.id == run.id)
                .expect("run")
                .status,
            LifecycleStatus::Cancelled
        );
        let _ = fs::remove_file(state::path(&scope));
    }

    #[test]
    fn expired_idle_lease_stops_and_cancels_the_session() {
        let directory = tempfile::tempdir().expect("daemon fixture");
        let scope = directory.path().join("scope");
        let providers = directory.path().join("providers");
        fs::create_dir_all(&scope).expect("scope");
        fs::create_dir_all(&providers).expect("providers");
        let scope = fs::canonicalize(scope).expect("canonical scope");
        let marker = directory.path().join("stopped");
        let script = directory.path().join("provider.sh");
        fs::write(
            &script,
            render_fixture(
                STOP_PROVIDER,
                serde_json::json!({ "marker": marker.display().to_string() }),
            ),
        )
        .expect("provider script");
        let mut permissions = fs::metadata(&script)
            .expect("script metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).expect("script permissions");
        fs::write(
            providers.join("provider.yaml"),
            render_fixture(
                STOP_PROVIDER_MANIFEST,
                serde_json::json!({ "command": script.display().to_string() }),
            ),
        )
        .expect("provider manifest");
        let mut config = Config::default();
        config.providers.directory = providers;
        config.lifecycle.runtime_timeout_seconds = 0;
        config.lifecycle.idle_timeout_seconds = 1;
        let linked = control::register(
            &scope,
            Contract {
                role: SessionRole::Worker,
                ..Contract::default()
            },
            SessionLink {
                id: Some(format!("worker-{}", uuid::Uuid::new_v4())),
                native_id: Some(uuid::Uuid::new_v4().to_string()),
                run_id: Some("test-run".into()),
                source: RegistrationSource::Connected,
                ..SessionLink::default()
            },
        )
        .expect("managed session");
        state::update(&scope, |workspace| {
            workspace.sessions[0].registration = RegistrationSource::Managed;
            workspace.sessions[0].providers.push(ProviderBinding {
                provider: "stopper".into(),
                kind: ProviderKind::Persistence,
                r#ref: Some("managed-process".into()),
                status: BindingStatus::Active,
                label: "Launch ownership: test stop provider".into(),
            });
            Ok(())
        })
        .expect("record lifecycle owner");

        let report = sweep_scope_at(
            &config,
            &scope,
            linked.connected_at + chrono::Duration::seconds(2),
        );

        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert_eq!(report.terminated.len(), 1);
        assert_eq!(report.terminated.first(), Some(&linked.id));
        assert!(marker.exists());
        let stopped = state::read(&scope)
            .expect("state")
            .sessions
            .into_iter()
            .find(|session| session.id == linked.id)
            .expect("stopped session");
        assert_eq!(stopped.status, LifecycleStatus::Cancelled);
        assert_eq!(
            stopped.termination_reason.as_deref(),
            Some("idle timeout exceeded")
        );
        let _ = fs::remove_file(state::path(&scope));
    }
}
