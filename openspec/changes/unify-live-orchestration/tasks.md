## 1. Live orchestration paths

- **SHAPE** graph
- MERGE 1.6

- [x] 1.1 Preserve node state and expose atomic adoption `deps:` none `writes:` src/control.rs
- [x] 1.2 Expose equivalent adoption entry points `deps:` 1.1 `writes:` src/cli.rs,src/mcp.rs
- [x] 1.3 Add bounded best-effort bind-current registration `deps:` none `writes:` src/control.rs,src/cli.rs
- [x] 1.4 Add selected-session activity and resume behavior `deps:` none `writes:` src/tui.rs
- [x] 1.5 Add root Output inspection, prompt refresh, and configured runtime animation `deps:` none `writes:` src/tui.rs,assets/animations.yaml
- [x] 1.6 Merge adoption, binding, activity, resume, and refresh behavior `deps:` 1.2,1.3,1.4,1.5 `writes:` src/control.rs,src/cli.rs,src/mcp.rs,src/tui.rs,assets/animations.yaml
- [x] 1.7 Adversarial review (`adversarial-review` skill): validate behavior and record surviving objections `deps:` 1.6 `writes:` openspec/changes/unify-live-orchestration/review.md

## 2. Verification loop

- **SHAPE** loop
- **STOP** `cargo test --all-targets` and `openspec validate unify-live-orchestration --strict` exit 0
- **MAX-ITERS** 4
- TERMINAL STALLED after 2 iterations without fewer failing checks, or CAPPED at MAX-ITERS

- [x] 2.1 Gather: run formatting, unit, integration, provider, and PTY checks
- [x] 2.2 Act: fix only failures mapped to the change behavior
- [x] 2.3 Verify: rerun the stop commands and affected provider or PTY checks
- [x] 2.4 Adversarial review (`adversarial-review` skill): run deterministic lint and record the terminal review state

## 3. Rollout

- [x] 3.1 Apply: prepare release and deployment work, gated on the verification loop reaching its STOP condition
- [x] 3.2 Confirm: the owner accepts the working and idle animation language before release
