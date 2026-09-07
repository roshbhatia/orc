## Why

Orc used Output for structured JSON reported through its control-plane API. Users expect Output to show the assistant prose they would see in the harness. Activity and structured checkpoints are different data and need separate views.

## What Changes

- Add provider-neutral `messages.read` discovery and command-plan execution.
- Validate provider-neutral message records and render user-visible assistant
  prose for exact sessions, run members, and assigned workflow nodes.
- Rename the structured reported-output view to Checkpoint without removing its CLI or MCP report API.
- Keep Activity, Gates, Health, Checkpoint, and Output as distinct inspector states.
- Poll only the visible Output view, cache it separately, open at the newest message, follow new messages only from the tail, and preserve its last good value on refresh errors.
- Let a terminal provider declare that a successful open command returns its active display binding, so later attach actions can focus that target.
- Extend the Traces extra with a `messages.read` adapter. Orc core remains unaware of Traces or any harness transcript format.

### Non-goals

- Do not persist transcripts or assistant messages in `WorkspaceState`.
- Do not derive Output from Activity, structured checkpoints, reasoning, prompts, or tool rows.
- Do not remove the existing session report API.
- Do not add provider-specific readers to Orc core.

## Capabilities

### New Capabilities

- `messages.read`: Return a command plan that prints bounded, chronological
  `orc.message/v1` JSONL records for one exact session.

### Modified Capabilities

- `terminal-dashboard`: Separate Output, Checkpoint, Activity, Gates, and Health and refresh visible provider-backed Output.
- `session-lifecycle`: Keep the existing structured reported-output envelope as checkpoint state.
- `orchestrator-api`: Keep the existing CLI and MCP report operations as the checkpoint reporting contract.

## Impact

- `src/provider.rs` gains the provider-neutral capability, JSONL contract,
  resolution, validation, and bounded capture path.
- `src/provider.rs` also gains an optional provider-owned binding receipt for command plans.
- `src/tui.rs` gains separate inspector variants and explicit Output tail-follow state.
- `src/control.rs` persists a validated binding receipt after a successful terminal open.
- `extras/traces/` implements the optional Traces adapter and translates its
  native record stream into `orc.message/v1`.
- `schema/message.schema.json` documents the record contract.
- `extras/wezterm/` wraps terminal creation and returns the resulting display binding through the generic receipt contract.
- Generated provider schemas and reference documentation include `messages.read`.

## Behavior

Must do:
- Output shows only user-visible assistant prose supplied by validated
  `orc.message/v1` records from `messages.read`.
- Activity retains messages, thinking, tool calls, and provider activity exactly as before.
- Checkpoint shows the existing structured node or session report.
- A failed refresh keeps the last successful Output value visible and reports the refresh error.
- A first Output load and a newly selected Output subject show the newest message. Refresh follows appended messages only while the viewer remains at the tail.
- A successful terminal open can return an active display binding. The next attach focuses that binding instead of opening another target.
- Provider output remains bounded and valid UTF-8. Plain records discard all
  terminal controls. Records that explicitly declare ANSI retain only safe SGR.
- Selecting a session reads only that session. Selecting an assigned node reads
  only its explicit session. An unassigned node reads no session. Selecting a
  run merges its exact orchestrator and explicit member sessions chronologically.
- Stable record IDs deduplicate overlapping reads. Each selected session, node,
  and run keeps an independent process-local cache entry. The cache has a fixed
  entry bound. A partial run refresh marks a failed member's retained records
  stale instead of discarding its last good Output.

Must still hold:
- Orc core stays provider-neutral.
- Adoption and backfill remain provider-backed and do not store transcript content in workspace state.
- CLI and MCP structured reporting continue to call one domain operation.

Runs where it ships:
- Provider, TUI, extra adapter, generated-interface, Rust, Nix, and strict OpenSpec checks run before release.

Human-owned decision:
- The owner chooses which optional `messages.read` provider to install.
