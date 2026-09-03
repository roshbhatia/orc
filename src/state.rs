use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[cfg(unix)]
use std::os::fd::AsRawFd;

use crate::{config, domain::WorkspaceState};

pub fn resolve_scope(scope: impl AsRef<Path>) -> Result<PathBuf> {
    fs::canonicalize(scope.as_ref())
        .with_context(|| format!("resolve workspace scope {}", scope.as_ref().display()))
}

pub fn scope_key(scope: &Path) -> String {
    let digest = Sha256::digest(scope.as_os_str().as_encoded_bytes());
    hex::encode(digest)[..20].to_owned()
}

pub fn path(scope: &Path) -> PathBuf {
    config::state_home()
        .join("orc")
        .join(format!("{}.json", scope_key(scope)))
}

fn normalize(mut value: Value) -> Result<Value> {
    let Some(object) = value.as_object_mut() else {
        bail!("state must be an object");
    };
    match object.get("schemaVersion").and_then(Value::as_str) {
        Some("orc.state/v3") => {
            if let Some(sessions) = object.get_mut("sessions").and_then(Value::as_array_mut) {
                for session in sessions {
                    if let Some(session) = session.as_object_mut() {
                        session
                            .entry("providers")
                            .or_insert_with(|| Value::Array(Vec::new()));
                    }
                }
            }
        }
        Some("orc.state/v2") => {
            object.insert("schemaVersion".into(), Value::String("orc.state/v3".into()));
            if let Some(sessions) = object.get_mut("sessions").and_then(Value::as_array_mut) {
                for session in sessions {
                    if let Some(session) = session.as_object_mut() {
                        session
                            .entry("providers")
                            .or_insert_with(|| Value::Array(Vec::new()));
                        session.entry("model").or_insert(Value::Null);
                    }
                }
            }
        }
        Some(version) => bail!("unsupported state version: {version}"),
        None => bail!("state has no schemaVersion"),
    }
    Ok(value)
}

pub fn read(scope: &Path) -> Result<WorkspaceState> {
    let target = path(scope);
    match fs::read_to_string(&target) {
        Ok(source) => {
            let value = normalize(serde_json::from_str(&source).context("parse workspace state")?)?;
            serde_json::from_value(value).context("decode workspace state")
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {
            Ok(WorkspaceState::empty(scope.display().to_string()))
        }
        Err(error) => Err(error).with_context(|| format!("read {}", target.display())),
    }
}

fn write_atomic(state: &WorkspaceState) -> Result<()> {
    let scope = Path::new(&state.scope);
    let target = path(scope);
    let parent = target.parent().context("state path has no parent")?;
    fs::create_dir_all(parent).context("create state directory")?;
    let temporary = parent.join(format!(".{}.tmp", Uuid::new_v4()));
    fs::write(&temporary, serde_json::to_vec_pretty(state)?).context("write temporary state")?;
    fs::rename(&temporary, &target).context("commit workspace state")?;
    Ok(())
}

const LOCK_WAIT_TIMEOUT: Duration = Duration::from_secs(2);
const LOCK_RETRY_DELAY: Duration = Duration::from_millis(10);
const INCOMPLETE_LOCK_GRACE: Duration = Duration::from_millis(250);
const LEGACY_LOCK_OWNER_FILE: &str = "owner.json";

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct LockOwner {
    pid: u32,
    token: String,
    acquired_at_unix_ms: u128,
}

struct Lock {
    path: PathBuf,
    token: String,
}

struct ClaimGuard {
    #[cfg(unix)]
    file: fs::File,
}

impl ClaimGuard {
    fn try_acquire(target: &Path) -> Result<Option<Self>> {
        #[cfg(unix)]
        {
            let path = target.with_file_name(format!(
                ".{}.guard",
                target.file_name().unwrap_or_default().to_string_lossy()
            ));
            let file = fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(path)
                .context("open state lock guard")?;
            unsafe extern "C" {
                fn flock(fd: i32, operation: i32) -> i32;
            }
            const LOCK_EX: i32 = 2;
            const LOCK_NB: i32 = 4;
            if unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } == 0 {
                return Ok(Some(Self { file }));
            }
            let error = std::io::Error::last_os_error();
            if matches!(error.kind(), ErrorKind::WouldBlock) {
                return Ok(None);
            }
            Err(error).context("acquire state lock guard")
        }
        #[cfg(not(unix))]
        {
            let _ = target;
            Ok(Some(Self {}))
        }
    }
}

impl Drop for ClaimGuard {
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

impl Lock {
    fn acquire(target: PathBuf) -> Result<Self> {
        Self::acquire_with(
            target,
            LOCK_WAIT_TIMEOUT,
            LOCK_RETRY_DELAY,
            INCOMPLETE_LOCK_GRACE,
        )
    }

    fn acquire_with(
        target: PathBuf,
        timeout: Duration,
        retry_delay: Duration,
        incomplete_grace: Duration,
    ) -> Result<Self> {
        let started = Instant::now();
        loop {
            if let Some(_guard) = ClaimGuard::try_acquire(&target)? {
                if let Some(lock) = Self::claim(&target)? {
                    return Ok(lock);
                }
                if Self::reclaim_if_stale(&target, incomplete_grace)?
                    && let Some(lock) = Self::claim(&target)?
                {
                    return Ok(lock);
                }
            }

            if started.elapsed() >= timeout {
                bail!("timed out waiting for state lock: {}", target.display());
            }
            thread::sleep(retry_delay.min(timeout.saturating_sub(started.elapsed())));
        }
    }

    fn claim(path: &Path) -> Result<Option<Self>> {
        let token = Uuid::new_v4().to_string();
        let owner = LockOwner {
            pid: std::process::id(),
            token: token.clone(),
            acquired_at_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
        };
        let parent = path.parent().context("state lock has no parent")?;
        fs::create_dir_all(parent).context("create state lock directory")?;
        let temporary = sibling_path(path, "claim");
        let result = (|| {
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)
                .context("create state lock claim")?;
            serde_json::to_writer(&mut file, &owner).context("write state lock owner")?;
            file.sync_all().context("flush state lock owner")?;
            match fs::hard_link(&temporary, path) {
                Ok(()) => Ok(Some(Self {
                    path: path.to_owned(),
                    token,
                })),
                Err(error) if error.kind() == ErrorKind::AlreadyExists => Ok(None),
                Err(error) => Err(error).context("claim state lock"),
            }
        })();
        let _ = fs::remove_file(temporary);
        result
    }

    fn reclaim_if_stale(path: &Path, incomplete_grace: Duration) -> Result<bool> {
        if !lock_is_stale(path, incomplete_grace)? {
            return Ok(false);
        }

        let stale_path = sibling_path(path, "stale");
        match fs::rename(path, &stale_path) {
            Ok(()) => {
                if stale_path.is_dir() {
                    fs::remove_dir_all(&stale_path).with_context(|| {
                        format!("remove stale state lock {}", stale_path.display())
                    })?;
                } else {
                    fs::remove_file(&stale_path).with_context(|| {
                        format!("remove stale state lock {}", stale_path.display())
                    })?;
                }
                Ok(true)
            }
            Err(error)
                if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::AlreadyExists) =>
            {
                Ok(false)
            }
            Err(error) => Err(error).context("reclaim stale state lock"),
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let owned = read_lock_owner(&self.path)
            .is_ok_and(|owner| owner.token == self.token && owner.pid == std::process::id());
        if !owned {
            return;
        }

        let _ = fs::remove_file(&self.path);
    }
}

fn read_lock_owner(path: &Path) -> Result<LockOwner> {
    let owner_path = if path.is_dir() {
        path.join(LEGACY_LOCK_OWNER_FILE)
    } else {
        path.to_owned()
    };
    let source = fs::read(owner_path).context("read state lock owner")?;
    serde_json::from_slice(&source).context("decode state lock owner")
}

fn lock_is_stale(path: &Path, incomplete_grace: Duration) -> Result<bool> {
    match read_lock_owner(path) {
        Ok(owner) => Ok(!pid_is_alive(owner.pid)),
        Err(_) => {
            let modified = fs::metadata(path)
                .and_then(|metadata| metadata.modified())
                .with_context(|| format!("inspect state lock {}", path.display()))?;
            Ok(modified.elapsed().unwrap_or_default() >= incomplete_grace)
        }
    }
}

fn sibling_path(path: &Path, state: &str) -> PathBuf {
    let name = path
        .file_name()
        .map_or_else(|| "state.lock".into(), |name| name.to_string_lossy());
    path.with_file_name(format!(".{name}.{state}.{}", Uuid::new_v4()))
}

#[cfg(unix)]
fn pid_is_alive(pid: u32) -> bool {
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }

    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }

    if unsafe { kill(pid as i32, 0) } == 0 {
        return true;
    }

    const ESRCH: i32 = 3;
    // ESRCH alone proves absence, so unknown errors cannot steal a live owner's lock.
    std::io::Error::last_os_error().raw_os_error() != Some(ESRCH)
}

#[cfg(not(unix))]
fn pid_is_alive(_pid: u32) -> bool {
    true
}

pub fn update<T>(
    scope: &Path,
    transform: impl FnOnce(&mut WorkspaceState) -> Result<T>,
) -> Result<T> {
    let target = path(scope);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).context("create state directory")?;
    }
    let _lock = Lock::acquire(target.with_extension("json.lock"))?;
    let mut state = read(scope)?;
    let result = transform(&mut state)?;
    state.updated_at = Utc::now();
    let active = state.sessions.iter().any(|session| session.status.active());
    state.active = active;
    write_atomic(&state)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier, atomic::AtomicUsize, atomic::Ordering};
    use tempfile::TempDir;

    #[test]
    fn scope_key_is_stable() {
        assert_eq!(
            scope_key(Path::new("/tmp/example")),
            scope_key(Path::new("/tmp/example"))
        );
        assert_eq!(scope_key(Path::new("/tmp/example")).len(), 20);
    }

    #[test]
    fn lock_writes_owner_metadata_and_releases_its_directory() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("state.lock");

        let lock = Lock::acquire_with(
            path.clone(),
            Duration::from_millis(10),
            Duration::ZERO,
            Duration::ZERO,
        )
        .unwrap();
        let owner = read_lock_owner(&path).unwrap();

        assert_eq!(owner.pid, std::process::id());
        assert_eq!(owner.token, lock.token);
        assert!(path.is_file());
        drop(lock);
        assert!(!path.exists());
    }

    #[test]
    fn live_owner_is_not_reclaimed() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("state.lock");
        let _lock = Lock::acquire_with(
            path.clone(),
            Duration::from_millis(10),
            Duration::ZERO,
            Duration::ZERO,
        )
        .unwrap();

        let error =
            Lock::acquire_with(path.clone(), Duration::ZERO, Duration::ZERO, Duration::ZERO)
                .err()
                .unwrap();

        assert!(
            error
                .to_string()
                .contains("timed out waiting for state lock")
        );
        assert!(path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn dead_owner_is_reclaimed() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("state.lock");
        fs::create_dir(&path).unwrap();
        let owner = LockOwner {
            pid: u32::MAX,
            token: Uuid::new_v4().to_string(),
            acquired_at_unix_ms: 0,
        };
        fs::write(
            path.join(LEGACY_LOCK_OWNER_FILE),
            serde_json::to_vec(&owner).unwrap(),
        )
        .unwrap();

        let lock = Lock::acquire_with(
            path.clone(),
            Duration::from_millis(10),
            Duration::ZERO,
            Duration::ZERO,
        )
        .unwrap();

        assert_eq!(read_lock_owner(&path).unwrap().pid, std::process::id());
        drop(lock);
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_stale_reclaim_has_one_owner() {
        let directory = TempDir::new().unwrap();
        let path = Arc::new(directory.path().join("state.lock"));
        let owner = LockOwner {
            pid: u32::MAX,
            token: Uuid::new_v4().to_string(),
            acquired_at_unix_ms: 0,
        };
        fs::write(path.as_ref(), serde_json::to_vec(&owner).unwrap()).unwrap();
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
                    let lock = Lock::acquire_with(
                        path.as_ref().clone(),
                        Duration::from_millis(50),
                        Duration::from_millis(1),
                        Duration::ZERO,
                    )
                    .ok();
                    if lock.is_some() {
                        owners.fetch_add(1, Ordering::SeqCst);
                    }
                    release.wait();
                    lock
                })
            })
            .collect::<Vec<_>>();
        release.wait();
        assert_eq!(owners.load(Ordering::SeqCst), 1);
        for worker in workers {
            worker.join().unwrap();
        }
    }

    #[test]
    fn incomplete_recent_lock_is_not_reclaimed() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("state.lock");
        fs::create_dir(&path).unwrap();

        let result = Lock::acquire_with(
            path.clone(),
            Duration::ZERO,
            Duration::ZERO,
            Duration::from_secs(1),
        );

        assert!(result.is_err());
        assert!(path.exists());
    }

    #[test]
    fn incomplete_expired_lock_is_reclaimed() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("state.lock");
        fs::create_dir(&path).unwrap();
        fs::write(path.join(LEGACY_LOCK_OWNER_FILE), b"partial").unwrap();

        let lock = Lock::acquire_with(
            path.clone(),
            Duration::from_millis(10),
            Duration::ZERO,
            Duration::ZERO,
        )
        .unwrap();

        assert_eq!(read_lock_owner(&path).unwrap().token, lock.token);
    }

    #[test]
    fn lock_does_not_release_another_owners_claim() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("state.lock");
        let lock = Lock::acquire_with(
            path.clone(),
            Duration::from_millis(10),
            Duration::ZERO,
            Duration::ZERO,
        )
        .unwrap();
        let replacement = LockOwner {
            pid: std::process::id(),
            token: Uuid::new_v4().to_string(),
            acquired_at_unix_ms: 0,
        };
        fs::write(&path, serde_json::to_vec(&replacement).unwrap()).unwrap();

        drop(lock);

        assert_eq!(read_lock_owner(&path).unwrap().token, replacement.token);
    }
}
