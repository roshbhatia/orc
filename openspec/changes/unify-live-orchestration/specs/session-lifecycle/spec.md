## MODIFIED Requirements

### Requirement: Node session adoption
Orc MUST expose node adoption through CLI and MCP, and MUST assign an existing active session without launching it or replacing stored node contract, runtime, result, activity, or accounting fields.

#### Scenario: Adopt an unowned node
- **WHEN** an authorized operator or owning orchestrator assigns an active session to an unowned node
- **THEN** Orc MUST link both records and preserve the node's harness, model, execution, judge policy, prompt, input, output, activity, token use, and cost

#### Scenario: Adopt through CLI or MCP
- **WHEN** the same valid adoption is requested through CLI or MCP
- **THEN** both entry points MUST enforce the same scope, ownership, lineage, and lifecycle rules

#### Scenario: Repeat an adoption
- **WHEN** the assigned session adopts the same node again
- **THEN** Orc MUST backfill missing session lineage without duplicating adoption activity

#### Scenario: Conflicting node owner
- **WHEN** an active node already has another active session owner
- **THEN** Orc MUST reject the adoption without changing either session or node

## ADDED Requirements

### Requirement: Current-session binding discovery
`orc session register --bind-current` MUST perform bounded, best-effort `session.bind` discovery with `rebindCurrent=true` and `currentSessionId` equal to `.session.id` for only the newly registered current session, and binding discovery MUST NOT determine registration success.

#### Scenario: Existing surface is discovered
- **WHEN** a provider accepts `session.bind` for the newly registered current session and reports an existing display binding
- **THEN** Orc MUST record that binding on the new session so an open action can focus the existing surface

#### Scenario: Binding discovery declines or fails
- **WHEN** every eligible provider declines, errors, returns malformed output, or reaches its timeout
- **THEN** Orc MUST keep the successful registration and leave the new session without those bindings

#### Scenario: Current session binding is requested
- **WHEN** registration runs with `--bind-current`
- **THEN** every binding request MUST set `rebindCurrent=true`, MUST set `currentSessionId` to the exact new `.session.id`, and MUST target only that session

#### Scenario: Provider lacks native current-session identity
- **WHEN** a binding provider cannot verify current-session identity from its native environment and `currentSessionId` exactly matches `.session.id`
- **THEN** the provider MAY treat the exact match as current-session proof

#### Scenario: Binding identity does not match
- **WHEN** a binding request contains a `currentSessionId` that differs from `.session.id`
- **THEN** the provider MUST NOT treat the marker as current-session proof

#### Scenario: Ordinary reconciliation binds sessions
- **WHEN** Orc performs ordinary session reconciliation
- **THEN** its binding requests MUST omit `currentSessionId`

#### Scenario: Binding discovery remains scoped
- **WHEN** registration runs with `--bind-current`
- **THEN** Orc MUST NOT request provider descriptions, invoke all-session reconciliation, or mutate an unrelated session
