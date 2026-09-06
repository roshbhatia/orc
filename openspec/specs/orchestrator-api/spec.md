## Purpose

Defines the scoped command and MCP operations available to users, orchestrators, and assigned agent sessions.

## Requirements

### Requirement: Explicit active context
Orc MCP operations MUST require an active workspace session identity and MUST resolve it within the declared workspace scope.

#### Scenario: Missing session identity
- **WHEN** a process calls an Orc MCP tool without `ORC_SESSION_ID`
- **THEN** Orc MUST reject the call instead of selecting the most recent session

### Requirement: Orchestrator authority
Only the active orchestrator or an external operator MUST control managed session lifecycle, run lifecycle, workflow proposals, node definitions, and gates.

#### Scenario: Worker changes another node
- **WHEN** a worker calls an orchestrator-only operation
- **THEN** Orc MUST reject it without mutating state

### Requirement: Assigned agent reporting
An assigned active agent session MUST be able to read its context and report activity, output, status, token use, and cost for its own node.

#### Scenario: Assigned worker completes
- **WHEN** a worker reports its assigned node done with output
- **THEN** Orc MUST update the node and run accounting atomically

### Requirement: Equivalent interactive entry points
Orc MUST expose workflow and node control through non-interactive commands and MCP without changing their authorization or lifecycle rules.

#### Scenario: Adopt a node through an entry point
- **WHEN** an authorized caller adopts an existing session through CLI or MCP
- **THEN** both entry points MUST enforce the same ownership and mutation rules
