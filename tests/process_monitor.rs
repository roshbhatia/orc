#[cfg(unix)]
mod unix {
    use serde_json::Value;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    const PROVIDER: &str = r#"#!/bin/sh
set -eu

request=$(cat)
group=$(ps -o pgid= -p "$$" | tr -d ' ')
sleep 0.1
kill -0 "$group"

case "$request" in
  *provider.validate*)
    printf '%s\n' '{"version":"orc.provider/v1","checks":[{"name":"monitor","status":"ok","message":"alive"}]}'
    ;;
  *)
    printf '%s\n' '{"version":"orc.provider/v1","command":["true"],"successCodes":[0]}'
    ;;
esac
"#;

    #[test]
    fn provider_invocation_keeps_the_real_process_monitor_alive() {
        let directory = tempfile::tempdir().expect("provider fixture");
        let providers = directory.path().join("providers");
        let provider_directory = providers.join("test");
        fs::create_dir_all(&provider_directory).expect("provider directory");
        let command = provider_directory.join("provider.sh");
        fs::write(&command, PROVIDER).expect("provider script");
        let mut permissions = fs::metadata(&command)
            .expect("provider metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&command, permissions).expect("executable provider");
        fs::write(
            provider_directory.join("provider.yaml"),
            format!(
                r#"version: orc.provider/v1
name: test
command: {}
actions:
  changes.inspect: Inspect changes
"#,
                command.display()
            ),
        )
        .expect("provider manifest");

        let output = Command::new(env!("CARGO_BIN_EXE_orc"))
            .args([
                "provider",
                "validate",
                "test",
                "--scope",
                directory.path().to_str().expect("UTF-8 fixture path"),
                "--json",
            ])
            .env("HOME", directory.path())
            .env("ORC_PROVIDERS_DIRECTORY", &providers)
            .env("XDG_CONFIG_HOME", directory.path().join("config"))
            .env("XDG_STATE_HOME", directory.path().join("state"))
            .output()
            .expect("run Orc provider validation");

        assert!(
            output.status.success(),
            "Orc failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let validations: Value =
            serde_json::from_slice(&output.stdout).expect("validation JSON output");
        assert_eq!(validations[0]["status"], "ok", "{validations:#}");
    }
}
