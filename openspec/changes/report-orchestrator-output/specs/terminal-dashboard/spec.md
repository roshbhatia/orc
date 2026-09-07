## ADDED Requirements

### Requirement: Provider-backed assistant output inspection
The dashboard MUST expose user-visible assistant prose through an Output inspector tab backed only by `messages.read`.

#### Scenario: Output is available
- **WHEN** the selected session has user-visible assistant prose and a provider accepts `messages.read` with `format: jsonl`
- **THEN** Output MUST validate and render bounded chronological `orc.message/v1` records without prompts, reasoning, tool rows, Activity, or structured checkpoint data

#### Scenario: An exact agent is selected
- **WHEN** the selected item is a session or assigned node
- **THEN** Orc MUST read Output only from that session or the node's explicit assigned session respectively

#### Scenario: An unassigned node is selected
- **WHEN** the selected workflow node has no assigned session
- **THEN** Orc MUST show that no agent is assigned and MUST NOT fall back to the run orchestrator

#### Scenario: A run is selected
- **WHEN** a run has an exact orchestrator and explicit member sessions
- **THEN** Orc MUST read each exact session, deduplicate stable record IDs within that native session, and merge all records chronologically with visible time and agent boundaries
- **AND** a failed member read MUST NOT hide successful member records
- **AND** Orc MUST retain that failed member's last good records, mark them stale, and show its source error

#### Scenario: An explicit session reference is unresolved
- **WHEN** a run or node names a session that is absent from workspace state
- **THEN** Orc MUST report the missing reference and MUST NOT relabel it as an unassigned node

#### Scenario: Output refresh succeeds
- **WHEN** the visible Output tab reaches its refresh interval and the provider returns new prose
- **THEN** Orc MUST replace the cached value promptly
- **AND** MUST follow the appended prose only when the viewer was already at the tail
- **AND** MUST preserve the viewer's position when the viewer had scrolled upward

#### Scenario: Output first becomes visible
- **WHEN** Output loads successfully for the first time or the viewer selects another Output subject
- **THEN** Orc MUST show the newest available prose

#### Scenario: Output refresh fails after success
- **WHEN** a refresh fails after Output has loaded successfully
- **THEN** Orc MUST retain the last successful value and expose the refresh error without replacing it with Activity or checkpoint data

#### Scenario: Output cache membership changes
- **WHEN** a run gains or replaces an explicit member session
- **THEN** Orc MUST refresh one stable run cache entry, retain last-good records only for sessions still in the run, and MUST keep all Output cache maps and in-flight reads within their fixed entry bound

#### Scenario: A node is reassigned
- **WHEN** an assigned node changes from one session to another and the new session read fails
- **THEN** Orc MUST NOT render cached Output from the previous session

#### Scenario: No output provider exists
- **WHEN** no provider advertises `messages.read`
- **THEN** Output MUST expose that provider error without falling back to another capability

### Requirement: Provider-owned display binding receipt
Orc MUST let a provider declare that a successful command returns a binding owned by that provider.

#### Scenario: Terminal open returns an active display binding
- **WHEN** an accepted `terminal.open` plan declares a provider binding receipt
- **AND** its command returns a valid active display binding for the issuing provider
- **THEN** Orc MUST atomically persist that binding on the attached session
- **AND** MUST NOT print the receipt as command output

#### Scenario: The user attaches again
- **WHEN** the session has the active display binding returned by its prior terminal open
- **THEN** Orc MUST resolve `terminal.focus` through that binding's provider instead of opening a duplicate display target
- **AND** a disconnected session lifecycle MUST NOT suppress this focus attempt

#### Scenario: Reconciliation overlaps terminal open
- **WHEN** reconciliation observes an older binding and a successful terminal open persists a newer binding before reconciliation commits
- **THEN** Orc MUST preserve the newer binding with a monotonic binding revision and atomic compare-and-merge update
- **AND** the next attach MUST resolve `terminal.focus` without opening a duplicate display target

#### Scenario: A binding receipt is invalid
- **WHEN** a plan names another provider as receipt owner or its successful command returns an invalid binding
- **THEN** Orc MUST reject the receipt without mutating the session binding

### Requirement: Distinct inspector concepts
The dashboard MUST represent Activity, Output, Checkpoint, Gates, and Health with distinct inspector states and labels.

#### Scenario: Structured session report exists
- **WHEN** a selected agent has a stored `reportedOutput` value
- **THEN** Checkpoint MUST render the bounded JSON value and Output MUST remain provider-backed prose

#### Scenario: Structured node result exists
- **WHEN** a selected workflow node has structured output
- **THEN** Checkpoint MUST render that value without copying it into Output

#### Scenario: Activity exists without output
- **WHEN** a selected session has Activity but no successful `messages.read` result
- **THEN** Activity MUST remain available and Output MUST NOT parse, summarize, or copy it

### Requirement: Bounded message capture
Orc MUST bound provider message output by bytes and lines, validate its JSONL
records, produce valid UTF-8, and render only explicitly declared safe ANSI.

#### Scenario: Provider exceeds message limits
- **WHEN** a `messages.read` command emits more than the configured capture bounds
- **THEN** Orc MUST retain the newest complete records within the bounds and mark that earlier output was truncated

#### Scenario: Provider emits unsafe terminal controls
- **WHEN** a plain record contains terminal controls or an ANSI record contains a non-SGR or malformed SGR sequence
- **THEN** Orc MUST remove that sequence before caching or rendering the body
- **AND** Orc MUST reset SGR after every body before rendering another message boundary

#### Scenario: Provider emits malformed or cross-session JSONL
- **WHEN** a record is malformed, uses an unsupported version, or names another native session
- **THEN** Orc MUST reject that source refresh and preserve its prior cached Output
