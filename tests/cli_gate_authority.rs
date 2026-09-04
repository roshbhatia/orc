use std::{fs, path::PathBuf, process::Command};

use serde_json::Value;
use tempfile::TempDir;

struct GateFixture {
    _directory: TempDir,
    scope: PathBuf,
    home: PathBuf,
    providers: PathBuf,
    run_id: String,
}

impl GateFixture {
    fn waiting() -> Self {
        let directory = TempDir::new().expect("fixture directory");
        let scope = directory.path().join("project");
        let home = directory.path().join("home");
        let providers = directory.path().join("providers");
        fs::create_dir_all(&scope).expect("workspace scope");
        fs::create_dir_all(&home).expect("fixture home");
        fs::create_dir_all(&providers).expect("provider directory");

        let fixture = Self {
            _directory: directory,
            scope,
            home,
            providers,
            run_id: String::new(),
        };
        fixture.assert_success(
            fixture
                .command()
                .args([
                    "connect",
                    "--scope",
                    fixture.scope_str(),
                    "--id",
                    "orchestrator",
                    "--native-id",
                    "native-orchestrator",
                    "--role",
                    "orchestrator",
                    "--harness",
                    "test",
                    "--quiet",
                ])
                .output()
                .expect("register orchestrator"),
        );

        let workflow = fixture._directory.path().join("user-gate.yaml");
        fs::write(
            &workflow,
            r#"version: orc.workflow/v1
name: user-gate
description: Verify CLI gate authority
goal: Approve a user-only gate
expected_output: An approved gate
entry_point: wait
approval:
  mode: autonomous
  gates:
    - id: user-only
      before: wait
      reason: A user must approve this gate
      authority: user
steps:
  - name: wait
    type: human_gate
    purpose: Wait for a user decision
    goal: Receive explicit user approval
    expected_output: An approved decision
"#,
        )
        .expect("workflow fixture");

        let started = fixture
            .command()
            .args([
                "workflow",
                "start",
                workflow.to_str().expect("UTF-8 workflow path"),
                "--scope",
                fixture.scope_str(),
                "--json",
            ])
            .output()
            .expect("start workflow");
        fixture.assert_success(started.clone());
        let run: Value = serde_json::from_slice(&started.stdout).expect("workflow run JSON");
        let run_id = run["id"].as_str().expect("workflow run id").to_owned();

        let resumed = fixture
            .command()
            .args(["run", "resume", &run_id, "--scope", fixture.scope_str()])
            .output()
            .expect("resume workflow");
        fixture.assert_success(resumed);

        Self { run_id, ..fixture }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_orc"));
        command
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join("config"))
            .env("XDG_DATA_HOME", self.home.join("data"))
            .env("XDG_STATE_HOME", self.home.join("state"))
            .env("ORC_PROVIDERS_DIRECTORY", &self.providers)
            .env("ORC_WORKFLOWS_REPOSITORY", self.home.join("workflows"))
            .env("ORC_WORKFLOWS_AUTO_COMMIT", "false")
            .env("ORC_DAEMON_AUTOSTART", "false")
            .env_remove("ORC_SESSION_ID")
            .env_remove("ORC_SCOPE");
        command
    }

    fn scope_str(&self) -> &str {
        self.scope.to_str().expect("UTF-8 workspace scope")
    }

    fn assert_success(&self, output: std::process::Output) {
        assert!(
            output.status.success(),
            "Orc failed with {}:\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn connected_orchestrator_cannot_approve_a_user_only_gate() {
    let fixture = GateFixture::waiting();
    let output = fixture
        .command()
        .env("ORC_SESSION_ID", "orchestrator")
        .env("ORC_SCOPE", &fixture.scope)
        .args([
            "run",
            "approve",
            &fixture.run_id,
            "--scope",
            fixture.scope_str(),
            "--gate",
            "user-only",
            "--no-resume",
        ])
        .output()
        .expect("approve as connected orchestrator");

    assert!(!output.status.success(), "orchestrator approval succeeded");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("requires user approval"),
        "unexpected error: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn external_cli_approves_a_user_only_gate_as_the_user() {
    let fixture = GateFixture::waiting();
    let output = fixture
        .command()
        .args([
            "run",
            "approve",
            &fixture.run_id,
            "--scope",
            fixture.scope_str(),
            "--gate",
            "user-only",
            "--no-resume",
        ])
        .output()
        .expect("approve as external user");

    fixture.assert_success(output.clone());
    let run: Value = serde_json::from_slice(&output.stdout).expect("approved workflow run JSON");
    assert_eq!(run["pendingGates"], serde_json::json!([]));
}
