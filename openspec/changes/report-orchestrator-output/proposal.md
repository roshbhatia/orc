## Why

Orc used Output for structured JSON reported through its control-plane API. Users expect Output to show the assistant prose they would see in the harness. Activity and structured checkpoints are different data and need separate views.

## What Changes

- Add provider-neutral `messages.read` discovery and command-plan execution.
- Render user-visible assistant prose in Output for sessions, runs, and assigned workflow nodes.
- Rename the structured reported-output view to Checkpoint without removing its CLI or MCP report API.
- Keep Activity, Gates, Health, Checkpoint, and Output as distinct inspector states.
- Poll only the visible Output view, cache it separately, preserve its last good value on refresh errors, and keep scroll position stable.
- Extend the Traces extra with a `messages.read` adapter. Orc core remains unaware of Traces or any harness transcript format.

### Non-goals

- Do not persist transcripts or assistant messages in `WorkspaceState`.
- Do not derive Output from Activity, structured checkpoints, reasoning, prompts, or tool rows.
- Do not remove the existing session report API.
- Do not add provider-specific readers to Orc core.

## Capabilities

### New Capabilities

- `messages.read`: Return a command plan that prints bounded, chronological, user-visible assistant prose for one exact session.

### Modified Capabilities

- `terminal-dashboard`: Separate Output, Checkpoint, Activity, Gates, and Health and refresh visible provider-backed Output.
- `session-lifecycle`: Keep the existing structured reported-output envelope as checkpoint state.
- `orchestrator-api`: Keep the existing CLI and MCP report operations as the checkpoint reporting contract.

## Impact

- `src/provider.rs` gains the provider-neutral capability, resolution, validation, and bounded capture path.
- `src/tui.rs` gains separate inspector variants and Output cache state.
- `extras/traces/` implements the optional Traces adapter through its native non-interactive output view.
- Generated provider schemas and reference documentation include `messages.read`.

## Behavior

Must do:
- Output shows only user-visible assistant prose supplied by `messages.read`.
- Activity retains messages, thinking, tool calls, and provider activity exactly as before.
- Checkpoint shows the existing structured node or session report.
- A failed refresh keeps the last successful Output value visible and reports the refresh error.
- Provider output remains bounded and valid UTF-8. ANSI styling remains safe to render after truncation.
- Selecting a run or unassigned node reads from its orchestrator session. An assigned node reads from its assigned session.

Must still hold:
- Orc core stays provider-neutral.
- Adoption and backfill remain provider-backed and do not store transcript content in workspace state.
- CLI and MCP structured reporting continue to call one domain operation.

Runs where it ships:
- Provider, TUI, extra adapter, generated-interface, Rust, Nix, and strict OpenSpec checks run before release.

Human-owned decision:
- The owner chooses which optional `messages.read` provider to install.
