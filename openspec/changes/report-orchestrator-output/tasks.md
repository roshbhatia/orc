## 1. Provider-backed assistant output

- **SHAPE** graph
- MERGE 1.3

- [x] 1.1 Add provider-neutral `messages.read` discovery, validation, plan resolution, and bounded capture `deps:` none `writes:` src/provider.rs
- [x] 1.2 Add separate Output cache, loading, refresh-error, and polling state without persisting transcript data `deps:` 1.1 `writes:` src/tui.rs
- [x] 1.3 Add the optional Traces adapter for the native one-shot output command `deps:` 1.1 `writes:` extras/traces
- [x] 1.4 Adversarial review: verify provider neutrality, capability isolation, capture bounds, and adapter compatibility `deps:` 1.3 `writes:` openspec/changes/report-orchestrator-output/review.md

## 2. Inspector semantics

- **SHAPE** graph
- MERGE 2.2

- [x] 2.1 Split Activity, Output, Checkpoint, Gates, and Health variants and move structured reports to Checkpoint `deps:` 1.2 `writes:` src/tui.rs
- [x] 2.2 Add focused separation, refresh-preservation, UTF-8, ANSI, and adapter tests `deps:` 1.3,2.1 `writes:` src/provider.rs,src/tui.rs,extras/traces/test.sh
- [x] 2.3 Adversarial review: verify inspector selection, loading, error retention, scrolling, and live polling `deps:` 2.2 `writes:` openspec/changes/report-orchestrator-output/review.md

## 3. Existing structured checkpoint contract

- **SHAPE** graph
- MERGE 3.4

- [x] 3.1 Keep the backward-compatible `reportedOutput` envelope and JSON-null round trip
- [x] 3.2 Keep bounded self-scoped CLI and MCP report operations
- [x] 3.3 Keep complete or honestly bounded structured JSON rendering under Checkpoint
- [x] 3.4 Adversarial review: retain the prior structured report integrity and authorization findings `deps:` 3.3 `writes:` openspec/changes/report-orchestrator-output/review.md

## 4. Verification and rollout

- **SHAPE** loop
- **STOP** `cargo test --all-targets`, generated-file checks, strict OpenSpec validation, and Nix checks exit 0
- **MAX-ITERS** 3

- [x] 4.1 Regenerate schemas, completions, and reference documentation
- [x] 4.2 Run focused and full checks, strict OpenSpec validation, and Nix checks
- [ ] 4.3 Complete mediated review, commit, release, update consumers, and verify the installed flow
