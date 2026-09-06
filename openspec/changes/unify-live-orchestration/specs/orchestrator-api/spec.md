## MODIFIED Requirements

### Requirement: Equivalent interactive entry points
Orc MUST expose node adoption through non-interactive CLI and MCP with the same authorization, lifecycle, lineage, and preservation rules.

#### Scenario: Adopt a node through an entry point
- **WHEN** an authorized caller adopts an existing session through CLI or MCP
- **THEN** both entry points MUST enforce the same ownership and mutation rules

#### Scenario: Explicit CLI adoption
- **WHEN** an authorized caller names a run, node, and existing session through CLI
- **THEN** Orc MUST adopt that session or return the same domain error the MCP operation would return

#### Scenario: Current-session MCP adoption
- **WHEN** an active Orc session invokes MCP node adoption without another session identifier
- **THEN** Orc MUST use the caller's active session as the adoption candidate

#### Scenario: Explicit MCP adoption
- **WHEN** the owning orchestrator invokes MCP node adoption with another session identifier
- **THEN** Orc MUST validate that candidate before changing the node

### Requirement: Current state consumption
Orc MUST let interactive clients read the current committed workspace snapshot and poll for replacement snapshots.

#### Scenario: Client polls after a prior read
- **WHEN** a client polls after reading an earlier workspace snapshot
- **THEN** Orc MUST return the current committed snapshot so the client can compare and replace local state

#### Scenario: Client starts without a revision
- **WHEN** a client has no prior workspace snapshot
- **THEN** Orc MUST return the current committed snapshot
