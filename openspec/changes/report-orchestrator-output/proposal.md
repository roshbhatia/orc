## Why

Orc exposes an Output tab for orchestrator sessions, but its state model cannot store or report orchestrator output. The tab therefore remains an empty placeholder even after an orchestrator produces a structured result.

## What Changes

- Add an optional reported-output envelope to the persisted session contract without breaking existing workspace files or losing explicit JSON `null`.
- Add one bounded domain operation for an active orchestrator to report or replace its own structured output.
- Expose equivalent CLI and MCP reporting entry points.
- Add an exact non-interactive session read for retained inactive output.
- Render the reported value in the existing orchestrator Output inspector without deriving content from Activity.
- Preserve reported output across same-session registration, refresh, lifecycle updates, and serialization. Root replacement keeps output on the archived reporter.

### Non-goals

- Do not infer output from transcripts, messages, reasoning, or provider logs.
- Do not add output history or provider-specific readers.
- Do not let one harness session report output for another session.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `session-lifecycle`: Persist bounded structured output on an orchestrator session and preserve it across lifecycle operations.
- `orchestrator-api`: Let an orchestrator report structured output through equivalent CLI and MCP operations.
- `terminal-dashboard`: Render explicitly reported orchestrator output in the existing Output inspector.

## Impact

- `src/domain.rs` gains one backward-compatible optional session field.
- `src/control.rs` owns validation, authorization, size limits, persistence, and preservation.
- `src/cli.rs` and `src/mcp.rs` expose the same domain operation.
- `src/tui.rs` renders the stored value.
- Generated schemas, documentation, and tests change with the public contract.

## Behavior

Must do:
- An active, non-terminating orchestrator can report one bounded structured JSON value through CLI or MCP.
- CLI accepts inline JSON or a file, including standard input, so the transport supports values near the domain limit.
- Existing workspaces without session output continue to load with output absent.
- Later registration, adoption, keepalive, and lifecycle changes preserve reported output.
- The Output inspector renders a clearly bounded preview and directs users to the full structured value through the non-interactive session read.
- A preview command reads the complete retained value from the resolved scope. It selects the exact session when that command fits the inspector budget and otherwise uses the bounded session list.
- Invalid JSON, unauthorized roles, inactive sessions, and oversized values leave state unchanged.

Must still hold:
- Orc core remains provider-neutral.
- Node output and session output remain separate contracts.
- CLI and MCP call one domain operation with the same validation rules.
- `ORC_SESSION_ID` is trusted runtime routing context, not authentication. Role and lifecycle checks prevent cooperative or accidental misuse; spoof resistance requires sandboxed execution.

Runs where it ships:
- Domain, CLI, MCP, serialization, generated-interface, and TUI inspector tests run before release.

Human-owned decision:
- The owner decides whether a single replaceable structured value is sufficient before output history is considered.
