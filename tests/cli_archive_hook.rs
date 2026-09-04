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
