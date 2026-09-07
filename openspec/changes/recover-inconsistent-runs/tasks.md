## 1. Persisted recovery state

- **SHAPE** graph
- MERGE 1.3

- [x] 1.1 Add typed proposals, typed events, bounded history, and backward-compatible defaults `deps:` none `writes:` src/domain.rs
- [x] 1.2 Add deterministic drift detection and the explicit transition table `deps:` 1.1 `writes:` src/workflow.rs
- [x] 1.3 Add focused serialization, assignment, definition-drift, and no-replay tests `deps:` 1.2 `writes:` src/domain.rs,src/workflow.rs
- [x] 1.4 Adversarial review: verify transitions, identity checks, and no-replay behavior `deps:` 1.3 `writes:` openspec/changes/recover-inconsistent-runs/review.md

## 2. Reconciliation surfaces

- **SHAPE** graph
- MERGE 2.3

- [x] 2.1 Reconcile every inconsistent or retryable run from daemon sweeps `deps:` 1.2 `writes:` src/daemon.rs
- [x] 2.2 Report recovery issues and apply deterministic repairs through doctor `deps:` 1.2 `writes:` src/control.rs,src/cli.rs
- [x] 2.3 Add daemon and doctor crash-recovery and idempotence tests `deps:` 2.1,2.2 `writes:` src/daemon.rs,src/control.rs
- [x] 2.4 Adversarial review: verify daemon retry and doctor repair boundaries `deps:` 2.3 `writes:` openspec/changes/recover-inconsistent-runs/review.md

## 3. Verification

- **SHAPE** loop
- **STOP** `cargo test --all-targets`, generated checks, strict OpenSpec validation, and both Nix checks exit 0
- **MAX-ITERS** 3

- [x] 3.1 Regenerate schemas, completions, and reference documentation `deps:` 2.3
- [x] 3.2 Run formatting, clippy, Rust tests, strict OpenSpec, generated checks, and both Nix flakes `deps:` 3.1
- [x] 3.3 Complete adversarial review and record mediated findings `deps:` 3.2 `writes:` openspec/changes/recover-inconsistent-runs/review.md
