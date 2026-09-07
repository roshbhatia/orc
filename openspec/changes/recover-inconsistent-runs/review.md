# Review

Three independent critics reviewed run recovery for state-machine safety,
executor identity, crash convergence, and operational behavior. They found ten
material objections. The implementation resolved every accepted objection.

The revised recovery path:

- re-evaluates autonomy before every automatic attempt;
- serializes transitions against execution identity;
- keeps a per-attempt proposal through child readiness acknowledgement;
- preserves proposals across lease contention and startup failure;
- blocks definition drift instead of executing or failing the changed graph;
- rejects stale failures after cancellation or replacement;
- validates root session roles and reciprocal worker lineage;
- reconciles satisfied proposals exactly once; and
- blocks persisted runs when their workspace is unavailable.

## Evidence

- `cargo fmt --all -- --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test --all-targets`: 413 unit tests and all integration tests passed
- `./hack/generate.sh --check`
- `openspec validate recover-inconsistent-runs --strict --no-interactive`
- `nix flake check --accept-flake-config --print-build-logs`
- `nix flake check --accept-flake-config --print-build-logs` in `extras/`

## Decision

The independent mediator returned `NO SURVIVING OBJECTION` after reviewing the
final fixes and focused regressions. The change is approved for integration.
