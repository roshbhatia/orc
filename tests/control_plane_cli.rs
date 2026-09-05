use std::{fs, path::Path, process::Command};

use tempfile::TempDir;

fn command(state: &Path, scope: &Path, arguments: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_orc"))
        .env("XDG_STATE_HOME", state)
        .env("XDG_CONFIG_HOME", state.join("config"))
        .env_remove("ORC_SESSION_ID")
        .args(arguments)
        .args(["--scope", scope.to_str().unwrap()])
        .output()
        .unwrap()
}

#[cfg(unix)]
fn local_provider_directory(state: &Path) -> std::path::PathBuf {
    let providers = state.join("providers");
    fs::create_dir_all(&providers).unwrap();
    let provider = Path::new(env!("CARGO_MANIFEST_DIR")).join("extras/local/provider.sh");
    fs::write(
        providers.join("local.yaml"),
        format!(
            r#"version: orc.provider/v1
name: local
command: {}
actions:
  execution.ensure: Ensure local desired state
  execution.observe: Observe local desired state
  execution.cancel: Cancel local desired state
"#,
            provider.display()
        ),
    )
    .unwrap();
    providers
}

#[cfg(unix)]
fn command_with_provider(
    state: &Path,
    scope: &Path,
    providers: &Path,
    arguments: &[&str],
) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_orc"))
        .env("XDG_STATE_HOME", state)
        .env("XDG_CONFIG_HOME", state.join("config"))
        .env("ORC_PROVIDER_DIR", providers)
        .env_remove("ORC_SESSION_ID")
        .args(arguments)
        .args(["--scope", scope.to_str().unwrap()])
        .output()
        .unwrap()
}

fn write_resource(directory: &Path, goal: &str) -> std::path::PathBuf {
    let path = directory.join("resource.yaml");
    fs::write(
        &path,
        format!(
            r#"apiVersion: orc.dev/v1alpha1
kind: Workflow
metadata:
  name: build
spec:
  goal: {goal}
  stages: []
"#
        ),
    )
    .unwrap();
    path
}

#[test]
fn apply_diff_get_and_conflict_are_real_cli_paths() {
    let state = TempDir::new().unwrap();
    let scope = TempDir::new().unwrap();
    let path = write_resource(scope.path(), "ship");
    let path = path.to_str().unwrap();

    let applied = command(
        state.path(),
        scope.path(),
        &["apply", "-f", path, "--field-manager", "first"],
    );
    assert!(
        applied.status.success(),
        "{}",
        String::from_utf8_lossy(&applied.stderr)
    );
    assert!(String::from_utf8_lossy(&applied.stdout).contains("workflows/build\tcreate"));

    let unchanged = command(state.path(), scope.path(), &["diff", "-f", path]);
    assert!(unchanged.status.success());
    assert!(String::from_utf8_lossy(&unchanged.stdout).contains("unchanged"));

    let listed = command(
        state.path(),
        scope.path(),
        &["get", "workflow", "build", "-o", "json"],
    );
    assert!(listed.status.success());
    assert!(String::from_utf8_lossy(&listed.stdout).contains("\"goal\": \"ship\""));

    let described = command(
        state.path(),
        scope.path(),
        &["describe", "workflow", "build"],
    );
    assert!(described.status.success());
    assert!(String::from_utf8_lossy(&described.stdout).contains("reason: Created"));

    let events = command(
        state.path(),
        scope.path(),
        &["events", "workflow", "build", "--json"],
    );
    assert!(events.status.success());
    assert!(String::from_utf8_lossy(&events.stdout).contains("\"reason\": \"Created\""));

    let preview_delete = command(
        state.path(),
        scope.path(),
        &["delete", "workflow", "build", "--dry-run"],
    );
    assert!(preview_delete.status.success());
    assert!(
        command(state.path(), scope.path(), &["get", "workflow", "build"])
            .status
            .success()
    );

    write_resource(scope.path(), "replace");
    let conflict = command(
        state.path(),
        scope.path(),
        &["apply", "-f", path, "--field-manager", "second"],
    );
    assert!(!conflict.status.success());
    assert!(String::from_utf8_lossy(&conflict.stderr).contains("owned by first"));

    let deleted = command(state.path(), scope.path(), &["delete", "workflow", "build"]);
    assert!(deleted.status.success());
    assert!(
        !command(state.path(), scope.path(), &["get", "workflow", "build"])
            .status
            .success()
    );
}

#[test]
fn apply_dry_run_does_not_create_cli_state() {
    let state = TempDir::new().unwrap();
    let scope = TempDir::new().unwrap();
    let path = write_resource(scope.path(), "ship");

    let applied = command(
        state.path(),
        scope.path(),
        &["apply", "-f", path.to_str().unwrap(), "--dry-run"],
    );
    assert!(applied.status.success());
    let listed = command(state.path(), scope.path(), &["get", "workflow", "build"]);
    assert!(!listed.status.success());
}

#[cfg(unix)]
#[test]
fn completed_local_one_shot_retires_without_a_cancel_action() {
    let state = TempDir::new().unwrap();
    let scope = TempDir::new().unwrap();
    let providers = local_provider_directory(state.path());
    let resources = scope.path().join("resources.yaml");
    let write_workflow = |stages: &str| {
        fs::write(
            &resources,
            format!(
                r#"apiVersion: orc.dev/v1alpha1
kind: Workflow
metadata:
  name: release
spec:
  stages: {stages}
---
apiVersion: orc.dev/v1alpha1
kind: Run
metadata:
  name: release-1
spec:
  workflowRef: release
"#
            ),
        )
        .unwrap();
    };
    let invoke = || {
        command_with_provider(
            state.path(),
            scope.path(),
            &providers,
            &["apply", "-f", resources.to_str().unwrap()],
        )
    };

    write_workflow(
        r#"
    - name: build
      provider: local
      command:
        - /bin/sh
        - -c
        - |
          printf '%s\n' '{"phase":"Succeeded"}'"#,
    );
    let created = invoke();
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );

    write_workflow("[]");
    let retired = invoke();
    assert!(
        retired.status.success(),
        "{}",
        String::from_utf8_lossy(&retired.stderr)
    );
    let execution = command(
        state.path(),
        scope.path(),
        &["get", "execution", "release-1-build", "-o", "json"],
    );
    let execution = String::from_utf8_lossy(&execution.stdout);
    assert!(execution.contains("\"desiredState\": \"cancelled\""));
    assert!(execution.contains("\"phase\": \"Succeeded\""));
}

#[cfg(unix)]
#[test]
fn failed_local_cancel_retries_with_the_same_operation() {
    let state = TempDir::new().unwrap();
    let scope = TempDir::new().unwrap();
    let providers = local_provider_directory(state.path());
    let resources = scope.path().join("resources.yaml");
    let ensure_attempts = state.path().join("ensure-attempts");
    let cancel_attempts = state.path().join("cancel-attempts");
    let cancel_ready = state.path().join("cancel-ready");
    let yaml_string = |path: &Path| serde_json::to_string(&path.display().to_string()).unwrap();
    fs::write(
        &resources,
        format!(
            r#"apiVersion: orc.dev/v1alpha1
kind: Workflow
metadata:
  name: release
spec:
  stages:
    - name: service
      provider: local
      command:
        - /bin/sh
        - -c
        - |
          printf '%s\n' ensure >> "$ENSURE_ATTEMPTS"
          printf '%s\n' '{{"phase":"Running"}}'
      environment:
        ENSURE_ATTEMPTS: {}
      actions:
        execution.observe:
          command:
            - /bin/sh
            - -c
            - printf '%s\n' '{{"phase":"Running"}}'
        execution.cancel:
          command:
            - /bin/sh
            - -c
            - |
              printf '%s\n' "$ORC_OPERATION_ID" >> "$CANCEL_ATTEMPTS"
              if [ ! -e "$CANCEL_READY" ]; then
                : > "$CANCEL_READY"
                exit 7
              fi
              printf '%s\n' '{{"phase":"Cancelled"}}'
          environment:
            CANCEL_ATTEMPTS: {}
            CANCEL_READY: {}
---
apiVersion: orc.dev/v1alpha1
kind: Run
metadata:
  name: release-1
spec:
  workflowRef: release
"#,
            yaml_string(&ensure_attempts),
            yaml_string(&cancel_attempts),
            yaml_string(&cancel_ready),
        ),
    )
    .unwrap();
    let created = command_with_provider(
        state.path(),
        scope.path(),
        &providers,
        &["apply", "-f", resources.to_str().unwrap()],
    );
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );

    fs::write(
        &resources,
        r#"apiVersion: orc.dev/v1alpha1
kind: Workflow
metadata:
  name: release
spec:
  stages: []
"#,
    )
    .unwrap();
    let first_cancel = command_with_provider(
        state.path(),
        scope.path(),
        &providers,
        &["apply", "-f", resources.to_str().unwrap()],
    );
    assert!(!first_cancel.status.success());
    let failed = command(
        state.path(),
        scope.path(),
        &["get", "execution", "release-1-service", "-o", "json"],
    );
    let failed: serde_json::Value = serde_json::from_slice(&failed.stdout).unwrap();
    assert_eq!(failed[0]["status"]["phase"], "Running");
    assert_eq!(failed[0]["spec"]["desiredState"], "cancelled");
    let generation = failed[0]["metadata"]["generation"].as_u64().unwrap();

    let retry = command_with_provider(state.path(), scope.path(), &providers, &["reconcile"]);
    assert!(
        retry.status.success(),
        "{}",
        String::from_utf8_lossy(&retry.stderr)
    );
    let cancelled = command(
        state.path(),
        scope.path(),
        &["get", "execution", "release-1-service", "-o", "json"],
    );
    let cancelled: serde_json::Value = serde_json::from_slice(&cancelled.stdout).unwrap();
    assert_eq!(cancelled[0]["status"]["phase"], "Cancelled");
    assert_eq!(cancelled[0]["metadata"]["generation"], generation);
    assert_eq!(
        fs::read_to_string(&ensure_attempts)
            .unwrap()
            .lines()
            .count(),
        1
    );
    let operation_ids = fs::read_to_string(&cancel_attempts).unwrap();
    let operation_ids = operation_ids.lines().collect::<Vec<_>>();
    assert_eq!(operation_ids.len(), 2);
    assert_eq!(operation_ids[0], operation_ids[1]);
}

#[cfg(unix)]
#[test]
fn provider_reconcile_and_event_delivery_are_idempotent() {
    use std::os::unix::fs::PermissionsExt;

    let state = TempDir::new().unwrap();
    let scope = TempDir::new().unwrap();
    let providers = state.path().join("providers");
    fs::create_dir_all(&providers).unwrap();
    let marker = state.path().join("deliveries");
    let provider = providers.join("control.sh");
    fs::write(
        &provider,
        r#"#!/bin/sh
set -eu
request=$(cat)
case "$request" in
  *execution.ensure*)
    printf '%s\n' '{"version":"orc.provider/v1","phase":"Succeeded","outputs":{"result":"verified"}}'
    ;;
  *event.deliver*)
    operation_id=${request#*\"operationId\":\"}
    operation_id=${operation_id%%\"*}
    receipt="$MARKER.$operation_id"
    if [ ! -e "$receipt" ]; then
      printf '%s\n' delivered >> "$MARKER"
      : > "$receipt"
      printf '%s\n' 'not-json'
    else
      printf '%s\n' '{"version":"orc.provider/v1","status":"delivered"}'
    fi
    ;;
  *execution.logs*)
    printf '%s\n' '{"version":"orc.provider/v1","status":"ok","logs":"build complete\n"}'
    ;;
  *)
    printf '%s\n' '{"version":"orc.provider/v1","status":"declined","reason":"unsupported fixture"}'
    ;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(&provider, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(
        providers.join("control.yaml"),
        format!(
            r#"version: orc.provider/v1
name: control-test
command: {}
actions:
  execution.ensure: Ensure an execution
  execution.logs: Read execution logs
  event.deliver: Deliver an event
"#,
            provider.display()
        ),
    )
    .unwrap();
    let resources = scope.path().join("resources.yaml");
    fs::write(
        &resources,
        r#"apiVersion: orc.dev/v1alpha1
kind: Execution
metadata:
  name: build
spec:
  desiredState: running
---
apiVersion: orc.dev/v1alpha1
kind: EventBinding
metadata:
  name: notify
spec:
  reasons: [Created, Reconciled]
"#,
    )
    .unwrap();

    let applied = Command::new(env!("CARGO_BIN_EXE_orc"))
        .env("XDG_STATE_HOME", state.path())
        .env("XDG_CONFIG_HOME", state.path().join("config"))
        .env("ORC_PROVIDER_DIR", &providers)
        .env("MARKER", &marker)
        .env_remove("ORC_SESSION_ID")
        .args([
            "apply",
            "-f",
            resources.to_str().unwrap(),
            "--scope",
            scope.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!applied.status.success());
    let mut delivery_counts = vec![fs::read_to_string(&marker).unwrap().lines().count()];
    for _ in 0..1 {
        let reconciled = Command::new(env!("CARGO_BIN_EXE_orc"))
            .env("XDG_STATE_HOME", state.path())
            .env("XDG_CONFIG_HOME", state.path().join("config"))
            .env("ORC_PROVIDER_DIR", &providers)
            .env("MARKER", &marker)
            .env_remove("ORC_SESSION_ID")
            .args(["reconcile", "--scope", scope.path().to_str().unwrap()])
            .output()
            .unwrap();
        assert!(
            reconciled.status.success(),
            "{}",
            String::from_utf8_lossy(&reconciled.stderr)
        );
        delivery_counts.push(fs::read_to_string(&marker).unwrap().lines().count());
    }

    assert!(delivery_counts[0] > 0);
    assert_eq!(delivery_counts[0], delivery_counts[1]);
    let execution = command(
        state.path(),
        scope.path(),
        &["get", "execution", "build", "-o", "json"],
    );
    assert!(String::from_utf8_lossy(&execution.stdout).contains("Succeeded"));
    let logs = Command::new(env!("CARGO_BIN_EXE_orc"))
        .env("XDG_STATE_HOME", state.path())
        .env("XDG_CONFIG_HOME", state.path().join("config"))
        .env("ORC_PROVIDER_DIR", &providers)
        .env_remove("ORC_SESSION_ID")
        .args([
            "logs",
            "execution",
            "build",
            "--scope",
            scope.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(logs.status.success());
    assert_eq!(String::from_utf8_lossy(&logs.stdout), "build complete\n");
}
