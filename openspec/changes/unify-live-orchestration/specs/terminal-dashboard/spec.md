## MODIFIED Requirements

### Requirement: Stable terminal interaction
Orc MUST support keyboard, mouse, and command-line navigation, refresh promptly after workspace state commits, and preserve selection and viewport when the selected objects still exist.

#### Scenario: Resize the terminal
- **WHEN** the terminal resizes above the supported minimum
- **THEN** Orc MUST fit the graph with visible margin and keep node and edge labels readable

#### Scenario: Orchestrator changes the graph
- **WHEN** the orchestrator commits a session, run, node, edge, status, or activity change
- **THEN** an open dashboard MUST request the new snapshot promptly without manual refresh

#### Scenario: Atomic state replacement
- **WHEN** a state writer replaces the workspace file atomically
- **THEN** the dashboard MUST detect the replacement and refresh the graph

#### Scenario: Watch notification is missed
- **WHEN** the dashboard misses a state notification
- **THEN** bounded periodic refresh MUST recover the latest committed snapshot

#### Scenario: Selected object remains
- **WHEN** a refresh changes content but retains the selected object
- **THEN** Orc MUST retain selection, inspector tab, pan, and zoom

### Requirement: Provider-backed open action
Orc MUST route Enter on an openable session or stage through the applicable resume and display provider chain, including an active session that has no current display binding.

#### Scenario: Active displayed session
- **WHEN** the selected active session has an active display reference and focus capability
- **THEN** Enter MUST focus that display

#### Scenario: Active undisplayed session
- **WHEN** the selected active session has no display reference and providers accept session attach and terminal open
- **THEN** Enter MUST resume the native session and open it in the selected display direction

#### Scenario: Disconnected resumable session
- **WHEN** the selected disconnected session has an accepted resume and display route
- **THEN** Enter MUST resume and open that session

#### Scenario: Open action fails
- **WHEN** a resume, focus, or display provider exits unsuccessfully
- **THEN** Orc MUST retain the dashboard and show the failed capability and provider result

### Requirement: Runtime state animation
The dashboard MUST animate active and idle runtimes distinctly, MUST use a static empty marker for work that has not started, and MUST honor reduced-motion configuration.

#### Scenario: Active runtime
- **WHEN** a session or node is working and has no idle or stalled observation
- **THEN** its configured working animation MUST advance while the dashboard is visible

#### Scenario: Idle runtime
- **WHEN** a working session or node is observed idle
- **THEN** its configured idle animation MUST be visually distinct from active work

#### Scenario: Work has not started
- **WHEN** a node is pending or queued without runtime evidence
- **THEN** Orc MUST render a static empty marker

#### Scenario: Reduced motion
- **WHEN** reduced motion is enabled
- **THEN** Orc MUST render deterministic static frames for active and idle states

### Requirement: Structured orchestrator checkpoint inspection
The dashboard MUST expose a Checkpoint inspector tab for the orchestrator root and MUST render only structured checkpoint data explicitly reported for that root.

#### Scenario: Root checkpoint is reported
- **WHEN** the selected orchestrator root has explicitly reported structured output
- **THEN** the Checkpoint tab MUST render that data without replacing or duplicating the Activity or Output view

#### Scenario: Root checkpoint is absent
- **WHEN** the selected orchestrator root has no explicitly reported structured output
- **THEN** the Checkpoint tab MUST show an explicit empty state

#### Scenario: Root has rendered activity only
- **WHEN** the selected orchestrator root has messages, reasoning summaries, tool activity, or provider logs but no structured output
- **THEN** the Checkpoint tab MUST remain empty and MUST NOT parse, summarize, or infer checkpoint data from that activity
