## ADDED Requirements

### Requirement: Provider-backed assistant output inspection
The dashboard MUST expose user-visible assistant prose through an Output inspector tab backed only by `messages.read`.

#### Scenario: Output is available
- **WHEN** the selected session has user-visible assistant prose and a provider accepts `messages.read`
- **THEN** Output MUST render the bounded chronological prose without prompts, reasoning, tool rows, Activity, or structured checkpoint data

#### Scenario: A run or node resolves its output session
- **WHEN** the selected item is a run, assigned node, or unassigned node
- **THEN** Orc MUST read Output from the run orchestrator, assigned session, or run orchestrator respectively

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
- **THEN** Orc MUST resolve `terminal.focus` instead of opening a duplicate display target

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
Orc MUST bound provider message output by bytes and lines and MUST produce valid UTF-8 and safely renderable ANSI text.

#### Scenario: Provider exceeds message limits
- **WHEN** a `messages.read` command emits more than the configured capture bounds
- **THEN** Orc MUST retain the newest complete content within the bounds and mark that earlier output was truncated
