## Context

Orc already has two distinct data sources. Activity is a provider-backed operational stream. A session or workflow node can also report one structured JSON value into `WorkspaceState`. The dashboard labeled the structured value Output, although users use Output to mean visible assistant prose.

## Goals / Non-Goals

**Goals:**

- Give assistant prose, operational activity, and structured checkpoints distinct contracts.
- Read prose through a provider-neutral command plan.
- Keep refreshes bounded, responsive, and failure tolerant.
- Preserve the existing structured report API and on-disk format.

**Non-Goals:**

- Store transcripts in Orc state.
- Teach Orc core about a harness, transcript path, or Traces.
- Infer assistant prose from operational activity.
- Add output history or artifact storage.

## Decisions

### Reserve Output for provider-backed assistant prose

`messages.read` accepts the standard session request with `format: jsonl` and
returns a command plan. The command prints chronological `orc.message/v1`
records with a stable ID, exact native session identity, RFC 3339 timestamp,
body, and optional truncation and body-format fields. Providers exclude user
prompts, reasoning, and tool rows. Orc does not parse a native transcript.

Plain bodies discard all terminal controls. A record can explicitly declare
`bodyFormat: ansi`; Orc then keeps only SGR sequences whose parameters contain
digits, semicolons, or colons. It strips every other CSI, OSC, C0, and C1
control. Orc styles its own time and agent boundaries after validation.
It resets SGR after every body so provider styling cannot cross a boundary.

Resolution never falls back to `activity.read`, `execution.logs`, or `session.inspect`. A provider may advertise both Activity and Output, but each capability follows its own plan.

### Rename structured reported output to Checkpoint in the inspector

The persisted `reportedOutput` field, `orc session report`, and MCP reporting tool remain unchanged for compatibility. Only the dashboard concept changes. Session and node structured JSON appears under Checkpoint. Run gates and provider health use their own enum variants instead of sharing a generic Result variant.

This is a UI correction, not a state migration. Old workspaces remain readable, and an explicit JSON `null` remains distinct from an absent checkpoint.

### Keep Output cache state independent

The TUI keeps per-selected-subject Output values, load times, in-flight markers,
and refresh errors separately from Activity. Session, node, and run keys never
share a cache entry. It polls only while the Output tab is visible. A successful
refresh replaces the value and clears the error. A failed refresh records the
error without deleting the last successful value or changing inspector scroll.

Session selection has one exact source. An assigned node has only its explicit
`sessionId`; an unassigned node has no source. A run includes its exact
orchestrator, node assignments, and sessions whose `runId` names the run. Orc
deduplicates those source sessions, reads each through `messages.read`, merges
records by timestamp, and deduplicates stable IDs within each native session.
It retains successful sources when another run member fails and labels the
partial result. It also retains that failed member's prior records and marks
them stale. An explicit session reference that cannot resolve remains a visible
source error; Orc does not relabel it as unassigned.

Session, node, and run cache keys use stable selected-object identity. They do
not include mutable membership. Each value records its current exact membership;
when membership changes, Orc drops records from removed sessions before a new
read. Orc caps all Output cache maps and in-flight reads at 256 selected objects.
Boundaries show time, title, role, and harness. When those labels collide for
distinct sessions, Orc adds the shortest unique session suffix.

The first successful load follows the newest line. Selecting another Output subject or returning to Output also follows its newest line. A refresh follows appended content only while the viewer is already at the tail. Scrolling upward disables tail following until the viewport reaches the tail again.

The refresh interval reuses the configured live-activity interval. This avoids another timing setting while preserving prompt updates after the provider reports new messages.

### Bound text at the provider boundary and inspector boundary

The provider request supplies byte and line limits. Orc also caps captured
stdout using tail retention, so an uncooperative provider cannot allocate an
unbounded TUI value. Truncation drops an incomplete leading record. The parser
rejects malformed remaining JSONL, wrong session identities, unsupported
versions, empty IDs, and conflicting duplicate IDs. The merged cache and final
inspector each apply their own byte and record or line bounds.

### Keep adoption and backfill metadata-only

Session registration and provider enrichment continue to store identity, title, purpose, goal, and bindings. They do not copy transcript content into `WorkspaceState`. The Output cache is process-local and can be rebuilt from the selected provider.

### Persist provider-owned display binding receipts

A command plan may declare that its successful stdout is a provider binding receipt. The declaration names the provider that issued the plan. Orc validates that ownership during plan resolution, parses stdout with the existing binding contract, and persists the binding only after an accepted exit code.

The receipt remains optional. Plans without one keep their current stdout behavior. A display extra can wrap its terminal creation command, convert the returned terminal identifier into an active display binding, and let later attach actions resolve `terminal.focus`. Orc core does not know the terminal program or identifier format.

Provider discovery runs outside the state lock because providers can block. Its
binding observations are therefore optimistic. When Orc commits an enrichment,
it compares each provider-and-kind slot's monotonic revision with the snapshot
used for discovery. An unchanged slot accepts the observation. A changed slot
preserves its newer binding or removal, including an identical-value
remove-and-add sequence. Every production binding mutation advances the affected
slot's revision. Unrelated slots still accept fresh observations. Liveness and
managed-launch readiness derive from the committed binding set, not from the
stale observation. Synthetic launch-owner reservations prove cancellation
authority but never prove runtime liveness. Finalization never replaces a
concurrent terminal or error state.

An active display receipt also owns `terminal.focus`. Focus resolution selects
that receipt's provider even when another display provider has higher priority.
The receipt remains focusable after the session becomes disconnected, so a
later attach does not create a duplicate terminal target. Ambiguous focus
ownership fails without falling back to another terminal open.

### Implement Traces as an optional extra

The Traces manifest advertises `messages.read`. Its adapter runs:

```text
traces --view output --format jsonl --once --session <native-id> [--service <harness>] --color never
```

The adapter validates the `traces.message/v1` stream and rewrites only its
version field to `orc.message/v1`. Orc knows only the manifest capability and
its own returned record contract. Provider validation runs the same structured
command against a deterministic local record and requires the exact session,
timestamp, and body before accepting an installed Traces binary.

## Risks / Trade-offs

- A provider failure can make Output stale. The inspector shows the last good value with an explicit refresh error.
- Process-local caching means a restarted TUI rereads Output. It prevents transcript content from entering durable orchestration state.
- Providers define what counts as user-visible prose. Provider validation and focused adapter tests protect the contract boundary, but Orc cannot semantically inspect arbitrary text.

## Migration Plan

1. Add `messages.read` to the provider schema and resolver.
2. Split inspector variants and move structured reports to Checkpoint.
3. Add independent Output refresh state and tests.
4. Extend the optional Traces provider and its deterministic adapter test.
5. Regenerate interfaces and run the full release checks.

Rollback removes the live Output reader but does not alter stored workspace data. Structured reports remain in the existing `reportedOutput` field.

## Rollout & Gating

Ship after focused provider and TUI tests, the full Rust suite, generated-file checks, strict OpenSpec validation, and Nix checks pass.

## Adversarial Review

One reviewer checks provider and TUI semantics. A second reviewer checks the isolated CLI and live refresh behavior. Both reviews must verify Activity, Output, and Checkpoint isolation, exact `messages.read` compatibility, loading and error retention, scroll behavior, and complete OpenSpec artifacts. Only concrete correctness blockers change the implementation.
