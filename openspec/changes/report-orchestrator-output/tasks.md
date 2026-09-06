## 1. Structured session output

- **SHAPE** graph
- MERGE 1.3

- [x] 1.1 Add a backward-compatible reported-output envelope with JSON-null round-trip and preservation tests `deps:` none `writes:` src/domain.rs,src/control.rs
- [x] 1.2 Add bounded self-scoped orchestrator reporting with terminating-session, oversized-value, and unchanged-state inverse tests `deps:` 1.1 `writes:` src/control.rs
- [x] 1.3 Adversarial review: audit state integrity, authorization, and size enforcement `deps:` 1.2 `writes:` openspec/changes/report-orchestrator-output/review.md

## 2. Public entry points and inspection

- **SHAPE** graph
- MERGE 2.3

- [x] 2.1 Add equivalent CLI inline/file/stdin and MCP report operations with generated interface coverage `deps:` 1.2 `writes:` src/cli.rs,src/mcp.rs
- [x] 2.2 Render complete or honestly bounded session output in the TUI and keep Activity isolated `deps:` 2.1 `writes:` src/tui.rs
- [x] 2.3 Adversarial review: audit interface parity and inspector behavior `deps:` 2.2 `writes:` openspec/changes/report-orchestrator-output/review.md

## 3. Verification and rollout

- **SHAPE** loop
- **STOP** `cargo test --all-targets`, generated-file checks, and strict OpenSpec validation exit 0
- **MAX-ITERS** 3
- TERMINAL STALLED after 2 iterations without fewer failing checks, or CAPPED at MAX-ITERS

- [x] 3.1 Regenerate schemas, completions, and reference documentation
- [x] 3.2 Run focused and full checks, strict OpenSpec validation, and mediated adversarial review
- [ ] 3.3 Commit, release, update sysinit and Laurel, switch, and verify the installed flow
- [ ] 3.4 Follow up separately on compact or bounded workspace-state persistence; this change does not alter state serialization `deps:` 3.2 `writes:` src/state.rs
