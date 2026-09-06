## ADDED Requirements

### Requirement: Structured orchestrator output
Orc MUST persist at most one explicitly reported structured output envelope for each orchestrator session, MUST preserve an explicitly reported JSON `null` as distinct from absent output, MUST accept existing session records without that field, and MUST preserve the envelope across same-session registration, refresh, and lifecycle updates.

#### Scenario: Existing workspace lacks session output
- **WHEN** Orc reads a workspace written before structured session output existed
- **THEN** Orc MUST treat the session output as absent without rejecting or rewriting unrelated state

#### Scenario: Orchestrator replaces its output
- **WHEN** an active orchestrator reports a valid structured output value more than once
- **THEN** Orc MUST atomically replace only its prior output and update the session timestamp

#### Scenario: Orchestrator reports JSON null
- **WHEN** an active orchestrator explicitly reports JSON `null`
- **THEN** Orc MUST preserve that report as present across a serialize and read cycle

#### Scenario: Output exceeds the bound
- **WHEN** serialized structured output exceeds the documented output limit
- **THEN** Orc MUST reject the report without changing the session or workspace

#### Scenario: Same session changes later
- **WHEN** a session with reported output is registered again, kept alive, refreshed, or changes lifecycle state
- **THEN** Orc MUST preserve the reported output unless an explicit output report replaces it

#### Scenario: Another session replaces the root
- **WHEN** an operator adopts another session as the workspace orchestrator
- **THEN** the archived reporting session MUST retain its output and the replacement orchestrator MUST start without output
