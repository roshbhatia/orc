use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

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

struct Lock(PathBuf);

impl Lock {
    fn acquire(target: PathBuf) -> Result<Self> {
        for _ in 0..100 {
            match fs::create_dir(&target) {
                Ok(()) => return Ok(Self(target)),
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => return Err(error).context("acquire state lock"),
            }
        }
        bail!("timed out waiting for state lock: {}", target.display())
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.0);
    }
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

    #[test]
    fn scope_key_is_stable() {
        assert_eq!(
            scope_key(Path::new("/tmp/example")),
            scope_key(Path::new("/tmp/example"))
        );
        assert_eq!(scope_key(Path::new("/tmp/example")).len(), 20);
    }
}
