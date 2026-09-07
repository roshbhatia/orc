## Why

A working run can outlive its executor and keep working nodes with no assigned session. The dashboard then reports contradictory state, while the daemon ignores the run. Orc needs a deterministic recovery state machine that stops unsafe replay and gives the orchestrator a durable repair decision.

## What Changes

- Detect working runs that have neither a live executor nor a valid active session assignment.
- Block interrupted nodes before any workflow executor restart.
- Restart only stored workflow definitions whose revision still matches, and only in autonomous mode.
- Persist bounded recovery events, retry state, and explicit repair proposals.
- Block manual work and invalid workflow definitions without choosing a harness, model, execution provider, or replacement session.
- Report recovery drift through daemon sweeps and `orc doctor`.

### Non-goals

- Do not replay an interrupted node automatically.
- Do not infer or fabricate session assignments.
- Do not change workflow node retry budgets, timeouts, or keepalive semantics.
- Do not make Orc core aware of a harness or execution provider.

## Capabilities

### New Capabilities

- None.

### Modified Capabilities

- `workflow-execution`: Detect and recover invalid working-state projections without replaying interrupted work.
- `orchestrator-api`: Expose recovery issues and durable repair proposals through doctor output.

## Impact

- `src/domain.rs` stores the bounded recovery state machine.
- `src/workflow.rs` detects drift, blocks interrupted work, validates stored definitions, and restarts safe autonomous executions.
- `src/daemon.rs` reconciles inconsistent runs and retryable recovery work.
- `src/control.rs` and `src/cli.rs` expose recovery health and repair results.

## Behavior

Must do:
- A run MUST NOT remain `working` when it has neither a live matching executor lease nor a valid active session assigned to working work.
- Orc MUST block every interrupted working node before starting a replacement executor.
- Automatic executor recovery MUST require autonomous mode and an unchanged stored workflow definition and revision.
- Manual orphan work MUST become blocked with a durable `resolve_assignment` proposal.
- A missing, unreadable, or changed workflow definition MUST become blocked with a durable `restore_definition` proposal.
- Recovery retries MUST use separate bounded backoff state and MUST NOT consume or reset workflow node attempts.
- Recovery events MUST be typed, bounded, ordered, and persisted.
- `orc doctor` MUST report inconsistent and pending-repair runs. `orc doctor --repair` MUST apply only deterministic transitions.

Must still hold:
- Provider selection, harness selection, model selection, and execution placement remain orchestrator or owner decisions.
- Existing workflow retry, deadline, lease, and keepalive behavior remains unchanged.
- Existing workspaces deserialize with empty recovery state.

Runs where it ships:
- Domain, workflow, daemon, doctor, generated-interface, Rust, strict OpenSpec, and both Nix checks pass.

Human-owned decision:
- A user or orchestrator decides how to resolve blocked assignment and definition proposals.
