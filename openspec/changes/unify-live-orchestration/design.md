## Context

> The keywords MUST, MUST NOT, SHOULD, SHOULD NOT, and MAY in this document are to be interpreted as described in [RFC 2119](https://datatracker.ietf.org/doc/html/rfc2119).

Scope includes the workspace session and run projection used by Orc's terminal dashboard, provider-backed activity, resume actions, node adoption, and dashboard refresh. Scope does not include replacing provider manifests, adding a specific harness integration, or merging the declarative resource store with the workspace projection.

Workspace updates use locked atomic file replacement. The dashboard reads snapshots on a timer, activity reads run only for the visible inspector, and provider commands remain the boundary for harness, display, and activity behavior.

## Goals / Non-Goals

### Goals

- An orchestrator mutation MUST wake an open dashboard without requiring manual refresh.
- Enter MUST resume an active but undisplayed session when attach and display providers accept it.
- Activity MUST show the selected session's recent history and refresh it from bounded provider snapshots.
- The orchestrator root MUST expose an Output tab without treating activity text as structured output.
- CLI and MCP node adoption MUST call one domain operation and preserve server-owned node fields.
- Current-session registration MAY bind an existing surface through a bounded, best-effort provider call.
- Working and idle states MUST use distinct configurable animation frames.

### Non-Goals

- This change MUST NOT add provider names or harness-specific transcript readers to Orc core.
- This change MUST NOT make Orc a security boundary between same-user processes.
- This change MUST NOT infer and commit a historical workflow graph from transcript text.
- This change MUST NOT replace the separate declarative resource API in this iteration.

## Decisions

### Watch the workspace state target and retain polling recovery

- Decision: Watch the state file's parent directory and request a snapshot refresh for events that target the state file.
  - Alternative rejected: Faster polling adds continuous reads and still delays every update by the polling interval.

The dashboard MUST watch the parent directory of the workspace state file because writers commit with atomic rename. A matching create, modify, remove, or rename event marks refresh pending. Access-only and unrelated path events are ignored. The existing bounded timer remains as recovery when an operating-system notification is dropped.

The watcher only requests a snapshot. It does not read or render from the notification callback, so one code path continues to apply state.

### Separate topology changes from content changes

- Decision: Rebuild graph layout only when node identity or edges change, and update other card content in place.
  - Alternative rejected: Rebuilding and fitting on every state write moves the graph while the user reads it.

The graph signature MUST contain identity and topology fields only. Status, activity, heartbeat, cost, and other card content MUST update in place. This prevents each prompt or status event from resetting layout, pan, zoom, or selection.

### Resume active undisplayed sessions through the attach chain

- Decision: Use focus when available, otherwise run the attach and display chain even when the session lifecycle is active.
  - Alternative rejected: Implicit inspection does not attach to the working harness as requested.

Enter first uses an active display reference when a focus provider accepts it. Otherwise, Orc MUST invoke `session.attach`, allow optional persistence transformation, then invoke `terminal.open`. Active lifecycle status does not block this resume path. The provider decides whether the native harness can resume or mirror that session.

Inspection remains an explicit action and MUST NOT replace the requested Enter behavior.

### Bind only the newly registered current session

- Decision: `orc session register --bind-current` performs one bounded, best-effort `session.bind` discovery for the newly registered current session with `rebindCurrent=true` and `currentSessionId` equal to `.session.id`.
  - Alternative rejected: Running provider descriptions and all-session reconciliation expands a registration hook into unrelated discovery and state mutation.

Orc MUST commit a valid registration independently of binding discovery. It MUST limit discovery to providers that advertise `session.bind` and to the newly registered current session. The request MUST set `currentSessionId` to the exact identifier in its `.session.id` field. A provider MAY treat that exact identity match as proof that the request represents the current session when native environment identity is unavailable. It MUST NOT accept a mismatch as that proof. Orc MUST bound the total discovery work through the configured provider limits. A provider decline, error, malformed response, or timeout MUST NOT make registration fail.

The bind-current path MUST NOT request provider descriptions, invoke all-session reconciliation, or mutate another session. Ordinary reconciliation MUST omit `currentSessionId`, so it cannot claim current-session identity through this marker. When a provider reports an existing display binding, Orc records it on the new session. Enter can then focus that surface through the advertised focus capability instead of opening a concurrent resume path.

### Resolve one activity subject per selection

- Decision: Resolve one labeled session source from the selected session, run, or node before requesting activity.
  - Alternative rejected: Node-local reports omit the live harness transcript and cannot backfill adopted history.

A selected session reads its own activity. A selected run reads the run orchestrator. A node with a bound session reads that session. A node without a bound session reads the run orchestrator and labels the fallback. Activity requests MUST poll for the newest bounded provider snapshot. Orc MUST replace the displayed snapshot when its content changes and MUST NOT duplicate unchanged events across refreshes.

Provider history is the backfill source. Orc MUST NOT synthesize reasoning or tool events that the provider cannot supply.

### Keep orchestrator output separate from activity

- Decision: Expose an Output tab for the orchestrator root and render only explicitly reported structured output.
  - Alternative rejected: Parsing rendered messages or activity would invent an output contract that the session state does not provide.

The orchestrator Output tab MUST remain available even when the root session has no structured output field. In that case, it MUST show an explicit empty state. Orc MUST NOT copy, parse, summarize, or infer output from messages, reasoning summaries, tool activity, or provider logs. When a structured root output is reported through a supported state contract, the tab MAY render that value without changing the Activity view.

### Keep adoption as one atomic domain operation

- Decision: Route CLI and MCP adoption through one transaction that changes ownership and missing lineage only.
  - Alternative rejected: A general node upsert can erase omitted server-owned runtime fields and cannot validate ownership atomically.

CLI and MCP MUST call the same adoption operation. The operation validates workspace, run, node, session, current ownership, and lineage before changing either record. It changes only node ownership and missing session lineage, then records one adoption activity event. Repeated adoption is idempotent.

The node's harness, model, execution provider, judge policy, prompt, input, output, activity history, tokens, cost, attempt, and retry fields remain server-owned and MUST survive adoption and repeated upsert operations when omitted.

### Render state through configured animation roles

- Decision: Resolve working and idle glyphs through the shared animation configuration and reduced-motion preference.
  - Alternative rejected: A hard-coded spinner bypasses the existing user-owned frame and timing configuration.

The shared animation configuration MUST define `working` and `idle` roles. Working lifecycle uses the working frames unless runtime observation classifies it as idle or stalled. Pending and queued work without execution stays static. Terminal states keep deterministic status glyphs. Reduced-motion mode selects one stable frame for each role.


## Rollout & Gating

Adoption and field-preservation tests MUST pass before the CLI and MCP entry points are accepted. Activity and state-watcher tests MUST pass before the TUI path is accepted. The packaged PTY test and an owner animation spot-check gate any later release. Removing the state watcher returns Orc to bounded polling and is the runtime kill switch.

## Risks / Trade-offs

- A file watcher can coalesce writes. The timer reloads the complete snapshot, so a missed notification delays rather than loses state.
- An attach provider may reject resuming an active native session. Orc preserves the dashboard and surfaces the provider failure.
- Best-effort current-session binding can leave a session undisplayed when every provider declines or fails. Registration still succeeds, and Enter retains the normal attach and display fallback.
- Provider snapshots can overlap or repeat. Orc MUST replace or deduplicate repeated content without assuming transport ordering or continuity.
- Fallback node activity can be mistaken for node-local work. The inspector labels the run-orchestrator source.
- The root session currently has no structured output field. The Output tab therefore shows an empty state until a supported report supplies one.
- Animation increases redraws. Orc disables animation under reduced motion and SHOULD avoid polling hidden inspector sources.

## Validation

- Atomic rename refresh: replace the state file and observe one pending refresh; inverse: an unrelated file event does not refresh the dashboard.
- Stable graph: change status and activity without changing topology and confirm selection and viewport remain; inverse: add an edge and confirm topology rebuilds.
- Enter resume: select an active undisplayed session and confirm `session.attach` and `terminal.open` run; inverse: an active display uses focus without resume.
- Current binding: register with `--bind-current` and confirm only the new session receives accepted `session.bind` results with `rebindCurrent=true` and matching `currentSessionId`; inverse: ordinary reconcile omits the marker, and decline, mismatch, error, malformed output, and timeout still return successful registration without description or all-session reconcile calls.
- Adoption: run equivalent CLI and MCP requests and compare state; inverse: an active conflicting owner leaves both records unchanged.
- Field preservation: seed every node runtime and result field, adopt and upsert it, then compare all omitted fields.
- Activity: select a node with and without a session and confirm the exact source label and bounded output; inverse: no accepting provider reports unavailable.
- Root output: select the orchestrator and confirm the Output tab shows reported structured output or the explicit empty state; inverse: activity content never appears as output.
- Animation: sample working, idle, pending, done, failed, and reduced-motion frames at deterministic times.

## Migration Plan

1. Add domain and entry-point tests for adoption and field preservation.
2. Add bounded bind-current registration tests, including decline, failure, timeout, and unrelated-session inverses.
3. Add state watcher and content-only graph refresh tests.
4. Add activity subject fallback, bounded snapshot replacement, deduplication, and polling tests.
5. Add the orchestrator Output tab and verify the empty state does not derive content from Activity.
6. Add configurable working and idle frames with reduced-motion tests.
7. Exercise Enter against focus, active resume, disconnected resume, and failure providers in a PTY.

Rollback removes the watcher and new entry points while retaining the existing state schema. Adoption writes no new required field, so a rollback MUST still read states written by this change.

## Adversarial Review

`specutil check` MUST validate the artifacts before implementation review. A separate review record MUST hold any surviving correctness, state-integrity, and terminal-interaction objections. Model review does not replace owner approval.

## Open Questions

None.
