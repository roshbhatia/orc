## Context

See `proposal.md` for motivation. Orc already persists structured node output and enforces a one-megabyte serialized limit. Session state has no equivalent field or reporting operation, so the orchestrator Output inspector cannot leave its empty state.

## Goals / Non-Goals

**Goals:**

- Reuse one structured JSON value and the existing output-size bound.
- Keep old workspace files readable.
- Give CLI and MCP one authorization and mutation path.
- Preserve Output and Activity as separate user concepts.

**Non-Goals:**

- Infer output from transcripts, provider logs, or the final visible message.
- Add provider-specific output readers.
- Add output history or artifact storage in this change.
- Let one orchestrator report output for another orchestrator.

## Decisions

### Store an optional reported-output envelope on the session

Add an optional, defaulted envelope containing the structured value to the session record. The envelope makes an explicit JSON `null` distinct from an absent report after serialization. It also keeps old workspace files valid. A separate output resource would add identity and consistency work without providing useful history in this change.

### Report through one self-scoped domain operation

The domain operation accepts a session identifier, verifies that the workspace and session are active, verifies that the session is not terminating and has the orchestrator role, enforces the shared serialized-size limit, and replaces the value in one workspace transaction. MCP supplies its current session from the runtime context. CLI requires `ORC_SESSION_ID` and uses the same operation.

`ORC_SESSION_ID` is trusted routing metadata, not an authentication credential. Exact session selection plus role and lifecycle checks prevent cooperative or accidental cross-session reports. A same-user process can spoof this environment value and access the same state files. Strong spoof resistance therefore requires an execution provider that isolates environment and filesystem access.

An operator can inspect any retained session with `orc session show <id> --json`, including archived sessions, but cannot pass a target identifier to the reporting command. MCP keeps the same caller-scoped reporting rule.

### Accept JSON inline or through a file stream at the CLI boundary

The CLI accepts either one inline JSON argument or `--file <path>`, with `-` reading standard input, and parses it before the transaction. The two sources are mutually exclusive. MCP already transports JSON values. Strings remain valid structured values when callers intentionally quote them as JSON. This avoids an ambiguous plain-text fallback and process argument limits.

### Render stored output through an output-specific bound

The Output inspector streams pretty JSON into a byte-and-line capped writer. Serialization stops when either inspector limit is reached, so a compact value cannot amplify into an unbounded pretty-printed allocation. It renders the complete value when serialization finishes inside the bound. Larger values receive an output-specific preview labeled with the number of rendered prefix bytes and a shell-safe `orc session show --json --scope <scope> -- <id>` command. The option terminator keeps option-like session identifiers positional. If an unusually large identifier would push the inspector body past its generic budget, Orc uses `orc session list --json --scope <scope>` instead. Both commands return the complete retained value from the TUI's resolved scope. Missing data retains the current explicit empty state. Activity content and Activity-specific truncation labels never enter this path.

### Return a bounded report receipt

The report mutation returns a receipt instead of the updated session. The receipt contains the constant `reported` status, the compact serialized input byte count, and an RFC 3339 UTC update time with millisecond precision. CLI and MCP therefore acknowledge a near-limit report with bounded output instead of pretty-printing the stored value again. Callers use `orc session show` when they need the retained value.

### Advertise MCP tools only in active session context

MCP tool discovery resolves `ORC_SCOPE` and `ORC_SESSION_ID` through the same active-session lookup used by tool calls. It returns the full provider-neutral catalog only when both values identify an active Orc session. Otherwise it returns an empty catalog. This makes Orc composable with direct harness use without adding harness-specific fallbacks.

### Keep reports with the session that produced them

Same-session registration, refresh, keepalive, and lifecycle mutations preserve the envelope. Root adoption archives the old orchestrator with its output intact and creates the replacement without output. Copying output would falsely attribute one session's report to another.

## Risks / Trade-offs

- A single value has no history. The durable state remains small, and providers can own larger artifact histories.
- The compact one-megabyte domain limit does not bound pretty-printed workspace state. The inspector stops serialization at its own limits, but state persistence still pretty-serializes the complete workspace. A separate follow-up must bound or compact persistence without changing the on-disk contract in this patch.
- Adding a field changes generated schemas. Defaulted deserialization and generated-file checks protect backward compatibility.
- Same-user processes can spoof `ORC_SESSION_ID` unless their execution environment isolates Orc state and routing variables. The CLI describes this value as routing context and does not claim an authentication boundary.

## Migration Plan

1. Add the optional field and preservation tests.
2. Add the domain report operation and inverse authorization and size tests.
3. Expose CLI and MCP entry points and update generated interfaces.
4. Render the value and test reported, absent, and activity-only states.
5. Validate old workspace fixtures and the full package before release.

Rollback to `v0.10.9` can discard reported output on the next state write because that binary does not retain unknown fields. Before rollback, export the workspace with the current binary and retain that file for restore after re-upgrade. Treat rollback after a report as destructive to the new output field and require explicit confirmation.

## Rollout & Gating

The change can ship after focused report tests, the full Rust suite, generated-file checks, strict OpenSpec validation, and a packaged TUI assertion all pass. The sysinit and Laurel inputs update only after the tagged release succeeds on the three supported platforms.

## Adversarial Review

An implementation critic checks authorization, lost-update risk, backward-compatible deserialization, size enforcement, and Activity isolation. A separate mediator accepts, rejects, reframes, or defers each objection before rollout.
