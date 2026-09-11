#[test]
fn orchestration_contracts_hold_through_real_cli_and_providers() {
    let result = std::process::Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/orchestration_e2e.py"
        ))
        .arg(env!("CARGO_BIN_EXE_orc"))
        .output()
        .expect("run orchestration regressions");
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}
