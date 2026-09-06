use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

use tempfile::TempDir;

fn archive_hook(root: &Path, state_home: &Path, scope: &Path) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_orc"))
        .args([
            "session",
            "archive",
            "--scope",
            scope.to_str().expect("UTF-8 workspace scope"),
            "--hook-input",
            "--quiet",
        ])
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_STATE_HOME", state_home)
        .env("ORC_DAEMON_AUTOSTART", "false")
        .env("ORC_SCOPE", scope)
        .env("ORC_SESSION_ID", "stale-orc-session")
        .env_remove("ORC_HARNESS")
        .env_remove("ORC_NATIVE_SESSION_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start Orc archive hook");
    child
        .stdin
        .take()
        .expect("archive hook stdin")
        .write_all(br#"{"session_id":"missing-native-session"}"#)
        .expect("write archive hook input");
    child.wait_with_output().expect("finish Orc archive hook")
}

fn orc_command(root: &Path, state_home: &Path, scope: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orc"));
    command
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_STATE_HOME", state_home)
        .env("ORC_DAEMON_AUTOSTART", "false")
        .env("ORC_SCOPE", scope)
        .env_remove("ORC_SESSION_ID")
        .env_remove("ORC_HARNESS")
        .env_remove("ORC_NATIVE_SESSION_ID");
    command
}

#[test]
fn archive_hook_ignores_a_missing_active_session() {
    let fixture = TempDir::new().expect("fixture directory");
    let scope = fixture.path().join("project");
    let state_home = fixture.path().join("state");
    fs::create_dir_all(&scope).expect("workspace scope");

    let output = archive_hook(fixture.path(), &state_home, &scope);

    assert!(
        output.status.success(),
        "archive hook failed with {}:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty(), "quiet hook wrote output");
}

#[test]
fn archive_hook_preserves_state_failures() {
    let fixture = TempDir::new().expect("fixture directory");
    let scope = fixture.path().join("project");
    let state_home = fixture.path().join("state-file");
    fs::create_dir_all(&scope).expect("workspace scope");
    fs::write(&state_home, "not a directory").expect("state path fixture");

    let output = archive_hook(fixture.path(), &state_home, &scope);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success(), "archive hook hid a state failure");
    assert!(
        stderr.contains("read ") && stderr.contains("state-file/orc/"),
        "unexpected archive failure: {stderr}"
    );
}

#[test]
fn stale_cross_harness_archive_hook_does_not_close_the_active_session() {
    let fixture = TempDir::new().expect("fixture directory");
    let scope = fixture.path().join("project");
    let state_home = fixture.path().join("state");
    fs::create_dir_all(&scope).expect("workspace scope");

    let root = orc_command(fixture.path(), &state_home, &scope)
        .args([
            "session",
            "register",
            "--scope",
            scope.to_str().expect("UTF-8 workspace scope"),
            "--id",
            "codex-root",
            "--native-id",
            "shared-native",
            "--harness",
            "codex",
            "--role",
            "orchestrator",
        ])
        .output()
        .expect("register root");
    assert!(
        root.status.success(),
        "{}",
        String::from_utf8_lossy(&root.stderr)
    );
    let worker = orc_command(fixture.path(), &state_home, &scope)
        .args([
            "session",
            "register",
            "--scope",
            scope.to_str().expect("UTF-8 workspace scope"),
            "--id",
            "claude-worker",
            "--native-id",
            "shared-native",
            "--harness",
            "claude",
            "--role",
            "worker",
            "--parent",
            "codex-root",
        ])
        .output()
        .expect("register worker");
    assert!(
        worker.status.success(),
        "{}",
        String::from_utf8_lossy(&worker.stderr)
    );
    let archived = orc_command(fixture.path(), &state_home, &scope)
        .args([
            "session",
            "archive",
            "codex-root",
            "--scope",
            scope.to_str().expect("UTF-8 workspace scope"),
        ])
        .output()
        .expect("archive old root");
    assert!(
        archived.status.success(),
        "{}",
        String::from_utf8_lossy(&archived.stderr)
    );
    let replacement = orc_command(fixture.path(), &state_home, &scope)
        .args([
            "session",
            "adopt",
            "--scope",
            scope.to_str().expect("UTF-8 workspace scope"),
            "--native-id",
            "shared-native",
            "--harness",
            "codex",
        ])
        .output()
        .expect("adopt replacement root");
    assert!(
        replacement.status.success(),
        "{}",
        String::from_utf8_lossy(&replacement.stderr)
    );
    let replacement_id = String::from_utf8(replacement.stdout)
        .expect("replacement id is UTF-8")
        .trim()
        .to_owned();

    let mut hook = orc_command(fixture.path(), &state_home, &scope);
    let mut hook = hook
        .args([
            "session",
            "archive",
            "--scope",
            scope.to_str().expect("UTF-8 workspace scope"),
            "--hook-input",
            "--quiet",
        ])
        .env("ORC_SESSION_ID", "codex-root")
        .env("ORC_HARNESS", "claude")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start stale archive hook");
    hook.stdin
        .take()
        .expect("archive hook stdin")
        .write_all(br#"{"session_id":"shared-native"}"#)
        .expect("write archive hook input");
    let hook = hook.wait_with_output().expect("finish stale archive hook");
    assert!(
        hook.status.success(),
        "{}",
        String::from_utf8_lossy(&hook.stderr)
    );

    let listed = orc_command(fixture.path(), &state_home, &scope)
        .args([
            "session",
            "list",
            "--scope",
            scope.to_str().expect("UTF-8 workspace scope"),
            "--json",
        ])
        .output()
        .expect("list sessions");
    assert!(
        listed.status.success(),
        "{}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let sessions: serde_json::Value =
        serde_json::from_slice(&listed.stdout).expect("session list JSON");
    assert_eq!(
        sessions
            .as_array()
            .and_then(|sessions| sessions
                .iter()
                .find(|session| session["id"] == "claude-worker"))
            .and_then(|session| session["status"].as_str()),
        Some("working")
    );
    assert_eq!(
        sessions
            .as_array()
            .and_then(|sessions| sessions
                .iter()
                .find(|session| session["id"] == replacement_id))
            .and_then(|session| session["status"].as_str()),
        Some("working")
    );
}
