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
- **THEN** Orc MUST replace the cached value promptly and preserve a valid scroll position

#### Scenario: Output refresh fails after success
- **WHEN** a refresh fails after Output has loaded successfully
- **THEN** Orc MUST retain the last successful value and expose the refresh error without replacing it with Activity or checkpoint data

#### Scenario: No output provider exists
- **WHEN** no provider advertises `messages.read`
- **THEN** Output MUST expose that provider error without falling back to another capability

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
