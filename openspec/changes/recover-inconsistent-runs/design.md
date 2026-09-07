## Context

The daemon currently inspects a working workflow only when `processId` is present. A crash can clear or omit that field while working nodes remain unassigned. Manual runs can enter the same contradictory state through the orchestration API. Restarting either kind blindly can repeat side effects or invent execution choices.

`calldiff tree -e sweep_scope_at_mode --max-depth 3` confirms that the daemon selects runs before calling workflow lifecycle operations. `calldiff` does not resolve the existing module-qualified calls to `workflow::executor_active` and `workflow::spawn`, so the design does not assume an unverified reusable edge. Recovery enters through a new workflow operation.

## Goals / Non-Goals

**Goals:**

- Make contradictory run state converge to a safe durable state.
- Keep automatic repair limited to a validated stored workflow.
- Preserve enough bounded evidence for an orchestrator to decide the next action.
- Make repeated daemon sweeps idempotent and rate limited.

**Non-Goals:**

- Retry interrupted task work.
- Select a provider, harness, model, or session.
- Merge the workspace projection with declarative control-plane resources.
- Add a new workflow approval mechanism.

## Decisions

### Model recovery as explicit persisted states

- Decision: Store attempts, retry time, one proposal, and typed events on each run. Events are capped at 64 and messages at 1 KiB.
- Alternative rejected: Reuse Checkpoint or node Activity as the only record. Checkpoint belongs to agent output, while a run with no nodes has no Activity owner.

The transition table is:

| Current observation | Mode | Next state | Proposal | Side effect |
| --- | --- | --- | --- | --- |
| live matching executor | any | unchanged | none | none |
| valid active assignment | any | unchanged | none | none |
| valid stored workflow, executor absent | autonomous | queued | restart executor | block working nodes, then start executor |
| valid stored workflow, executor absent | supervised or approval-gated | blocked | restart executor | none |
| missing, unreadable, or changed definition | any | blocked | restore definition | none |
| manual work without a valid assignment | any | blocked | resolve assignment | none |
| automatic restart fails | autonomous | queued | restart executor | retry after bounded backoff |
| owner satisfies a blocked repair | any | working | none | record reconciliation and clear the proposal |

### Validate identity and revision before automatic restart

- Decision: A workflow is recoverable only when its stored definition path loads and its current content hashes to the stored revision. Recovery serializes the state transition against the execution-identity lock, then compares the observed process and nonce under the state lock.
- Alternative rejected: Restart whenever `definition` is present. A stale path or changed file would execute a different graph.

### Acknowledge readiness from the replacement executor

- Decision: Each automatic attempt has a durable operation ID and a five-second start grace. Lease contention defers instead of reporting success. The child clears the proposal and records `recovered` only after it owns the execution lease, stops interrupted commands, validates the stored revision, discovers providers, and commits its process identity. Startup failures update only the matching queued operation.
- Alternative rejected: Treat a successful process spawn as recovery. A child can still fail before the workflow executor is ready.

### Block interrupted work before replacement execution

- Decision: Change every working node to blocked and clear `currentNode`, `processId`, and `executionNonce` before spawning. The replacement executor may settle the graph, but it cannot rerun the interrupted node until an explicit retry changes that node.
- Alternative rejected: Let the replacement executor infer which operation committed. External side effects do not provide a transactional commit boundary.

### Treat assignments as linked active identities

- Decision: A valid assignment requires a working node to reference an active non-terminating session. A non-root assigned session must point back to the same run and node. The run orchestrator may be assigned directly to a node without changing its root linkage.
- Alternative rejected: Count the mere presence of `sessionId`. Archived, mismatched, or missing records do not prove work is running.

### Separate infrastructure recovery from workflow retries

- Decision: Recovery attempts use a short start grace and exponential failure backoff capped at 60 seconds. Every queued attempt re-evaluates the current autonomy mode. They do not modify node `attempt`, `retryAfter`, workflow deadlines, session leases, or heartbeats.
- Alternative rejected: Consume the node retry budget when the controller process fails. That budget describes task failures, not control-plane availability.

### Keep doctor repair deterministic

- Decision: Doctor reports every detected issue and pending proposal. `--repair` may block unsafe work or schedule a validated autonomous restart. It never resolves assignment or definition proposals.
- Alternative rejected: Let doctor choose a replacement. That would bypass the declared autonomy and provider contracts.

## Risks / Trade-offs

- A blocked interrupted node needs an explicit retry or edit. This is safer than repeating an external side effect.
- A valid long-running assigned session can keep a run working without an executor. If that session later disconnects, the next sweep detects the drift.
- Automatic restart failure leaves the run queued during backoff. The persisted proposal and event history explain why it has not advanced.
- A missing workspace cannot launch providers. Persisted-only sweeps block its working runs with a durable restore proposal.

## Migration Plan

1. Add defaulted recovery state. Existing state files deserialize without migration.
2. Add pure detection and transition tests.
3. Route daemon sweeps and doctor through the recovery operation.
4. Regenerate schemas and documentation.

Rollback ignores the defaulted recovery field and restores the old daemon selector. No provider or transcript data changes.

## Rollout & Gating

Ship after deterministic crash fixtures, repeated-sweep tests, all Rust checks, strict OpenSpec validation, generated checks, and both Nix flakes pass.

## Adversarial Review

Independent critics test stale identity races, side-effect replay, assignment spoofing, backoff loops, definition drift, and doctor overreach. A separate mediator accepts only concrete failures against the Behavior criteria.
