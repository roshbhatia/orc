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

`messages.read` accepts the standard session request and returns a command plan. The command prints chronological, user-visible assistant text. Providers exclude user prompts, reasoning, and tool rows. Orc does not parse a native transcript.

Resolution never falls back to `activity.read`, `execution.logs`, or `session.inspect`. A provider may advertise both Activity and Output, but each capability follows its own plan.

### Rename structured reported output to Checkpoint in the inspector

The persisted `reportedOutput` field, `orc session report`, and MCP reporting tool remain unchanged for compatibility. Only the dashboard concept changes. Session and node structured JSON appears under Checkpoint. Run gates and provider health use their own enum variants instead of sharing a generic Result variant.

This is a UI correction, not a state migration. Old workspaces remain readable, and an explicit JSON `null` remains distinct from an absent checkpoint.

### Keep Output cache state independent

The TUI keeps per-session Output values, load times, in-flight markers, and refresh errors separately from Activity. It polls only while the Output tab is visible. A successful refresh replaces the value and clears the error. A failed refresh records the error without deleting the last successful value or changing inspector scroll.

The refresh interval reuses the configured live-activity interval. This avoids another timing setting while preserving prompt updates after the provider reports new messages.

### Bound text at the provider boundary and inspector boundary

The provider request supplies byte and line limits. Orc also caps captured stdout using tail retention, so an uncooperative provider cannot allocate an unbounded TUI value. Truncation drops an incomplete leading line, preserves valid UTF-8, and retains complete ANSI sequences in ordinary line-oriented output. The inspector applies its existing final byte and line bounds before rendering ANSI into Ratatui text.

### Keep adoption and backfill metadata-only

Session registration and provider enrichment continue to store identity, title, purpose, goal, and bindings. They do not copy transcript content into `WorkspaceState`. The Output cache is process-local and can be rebuilt from the selected provider.

### Implement Traces as an optional extra

The Traces manifest advertises `messages.read`. Its adapter returns this plan:

```text
traces --view output --once --session <native-id> [--service <harness>] --color always
```

The command contract returns user-visible assistant prose only. Orc knows only the manifest capability and returned command plan.

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
