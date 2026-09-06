> The keywords MUST, MUST NOT, SHOULD, SHOULD NOT, and MAY in this document are to be interpreted as described in [RFC 2119](https://datatracker.ietf.org/doc/html/rfc2119).

## Why

Orc can store a workflow graph, but the dashboard does not yet present the active harness as a live control plane. Activity can remain empty, Enter can appear to succeed without a durable open path, node adoption is hard to discover, and graph mutations arrive through periodic polling.

## What Changes

- Refresh and backfill bounded activity snapshots for the selected session, including messages, reasoning summaries, tool activity, and provider logs.
- Expose an Output inspector tab for the orchestrator root and show an honest empty state until structured output is reported.
- Make Enter resume an active but undisplayed session through the provider chain, then open its display and report the exact failure stage when it cannot.
- Expose node adoption as an explicit CLI and MCP operation that preserves the node's contract, runtime selection, result, and accounting fields.
- Let `orc session register --bind-current` discover existing provider bindings for only the newly registered current session without turning discovery failure into registration failure.
- Derive active, idle, and stalled animation from observed runtime activity instead of the desired node status alone.
- Wake the dashboard when the orchestrator changes sessions, runs, nodes, edges, or activity, with bounded polling as recovery.
- Keep the current selection, inspector, pan, and zoom when a live update does not remove the selected object.

### Non-goals

- Orc MUST NOT add provider names or harness-specific transcript readers to core.
- This change MUST NOT infer a historical workflow graph from transcript text.
- This change MUST NOT merge the declarative resource store with the workspace dashboard projection.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `session-lifecycle`: Make node adoption explicit, preserve all node runtime and result fields, and add bounded current-session binding discovery.
- `activity-observation`: Add polled activity snapshots, history backfill, and observable runtime activity states.
- `terminal-dashboard`: Define Enter resume behavior, root-agent output inspection, working and idle animation, and prompt graph refresh.
- `orchestrator-api`: Expose equivalent node adoption and current state reads through CLI and MCP.

## Behavior

Must do:
- Enter on an active undisplayed session resumes it through attach and display providers, decided by a PTY provider fixture.
- CLI and MCP node adoption preserve every seeded runtime and result field, decided by the adoption integration tests.
- `session register --bind-current` sends a bounded `session.bind` request with `rebindCurrent=true` and `currentSessionId` equal to the new `.session.id`, decided by provider request fixtures.
- Binding decline, failure, or timeout never changes successful registration into failure, decided by inverse registration fixtures.
- The Activity inspector displays new provider output and adopted history, decided by bounded activity fixtures.
- The orchestrator Output inspector shows only reported structured output, or an explicit empty state when none exists, decided by inspector fixtures.
- A committed graph mutation refreshes an open dashboard without manual input, decided by the atomic-rename watcher test.
- Working and idle states use distinct configured frames, decided by deterministic animation samples.

Must still hold:
- Orc core remains provider-neutral, decided by structural provider-boundary checks.
- Invalid ownership or lineage leaves session and node state unchanged, decided by inverse adoption tests.
- Current-session binding does not describe providers, reconcile all sessions, or mutate unrelated sessions, and ordinary reconciliation omits `currentSessionId`, decided by call and state assertions.
- Refresh retains selection and viewport when topology is unchanged, decided by graph snapshot tests.
- Reduced-motion mode renders stable frames, decided by deterministic animation samples.

Runs where it ships:
- The PTY dashboard test runs the packaged Orc binary with real state-file replacement and provider commands, avoiding an in-process model that hides terminal exit and watcher behavior.

Human-owned decision:
- The owner confirms that the working and idle frames communicate the intended state without visual noise.

## Impact

Modified code:
- `src/control.rs`, `src/cli.rs`, and `src/mcp.rs` share node adoption and preservation rules.
- `src/tui.rs` reuses the current provider activity, graph snapshot, inspector, preferences, and animation paths.
- `assets/animations.yaml` supplies working and idle frames.

Dependencies:
- A filesystem notification library wakes the dashboard after atomic state replacement.

Impactful and irreversible actions:
- None. Release, push, deployment, and OpenSpec archive remain outside this change artifact.

Gating signal: formatting, tests, strict OpenSpec validation, provider checks, and the packaged PTY dashboard test MUST pass before release work begins.
