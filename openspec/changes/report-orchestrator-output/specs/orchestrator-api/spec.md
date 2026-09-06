## ADDED Requirements

### Requirement: Orchestrator output reporting
Orc MUST expose equivalent CLI and MCP operations that let an active orchestrator report structured output for itself through one domain operation.

#### Scenario: MCP starts outside an active Orc session
- **WHEN** the MCP server receives tool discovery without a scope and session identity that resolve to an active Orc session
- **THEN** Orc MUST return an empty tool catalog

#### Scenario: MCP starts inside an active Orc session
- **WHEN** the MCP server receives tool discovery with a scope and session identity that resolve to an active Orc session
- **THEN** Orc MUST return the complete provider-neutral Orc tool catalog

#### Scenario: Current orchestrator reports through MCP
- **WHEN** an active orchestrator calls the output-reporting MCP operation with a structured value
- **THEN** Orc MUST persist that value on the caller's own session and return a bounded receipt with reported status, compact input byte count, and fixed-width update time

#### Scenario: Current orchestrator reports through CLI
- **WHEN** an active orchestrator invokes the output-reporting CLI operation with the same structured value
- **THEN** Orc MUST apply the same validation, authorization, and persistence rules as MCP

#### Scenario: Near-limit report is acknowledged
- **WHEN** CLI or MCP accepts a structured value near the domain limit
- **THEN** Orc MUST return the bounded report receipt without serializing the complete updated session into the response

#### Scenario: CLI reads a large value from standard input
- **WHEN** an active orchestrator reports valid structured output near the domain limit through the CLI standard-input path
- **THEN** Orc MUST persist the same value that MCP would accept without requiring it to fit in one process argument

#### Scenario: Worker attempts to report root output
- **WHEN** a worker session invokes the orchestrator output-reporting operation
- **THEN** Orc MUST reject the request without changing session state

#### Scenario: Terminating orchestrator attempts to report output
- **WHEN** an orchestrator whose lifecycle is terminating invokes an output-reporting operation
- **THEN** Orc MUST reject the request without changing session state

#### Scenario: Report is not valid JSON
- **WHEN** the CLI receives an output value that is not valid JSON
- **THEN** Orc MUST fail before changing workspace state

#### Scenario: Operator reads retained output by exact session ID
- **WHEN** an operator requests an inactive or archived session through `orc session show <id> --json`
- **THEN** Orc MUST return that exact session without granting it current-session authority
