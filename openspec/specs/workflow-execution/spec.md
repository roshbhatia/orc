## Purpose

Defines deterministic workflow materialization, state transitions, gates, retries, reports, and completion behavior.

## Requirements

### Requirement: Deterministic materialization
Orc MUST materialize a workflow revision into one run with stable node contracts and dependency edges.

#### Scenario: Materialize a saved workflow
- **WHEN** a user starts a valid workflow revision
- **THEN** Orc MUST create one run whose nodes preserve that revision's contracts and relationships

### Requirement: Dependency-ordered execution
Orc MUST execute ready stages in deterministic dependency levels while allowing independent stages to run concurrently within configured limits.

#### Scenario: Dependency is incomplete
- **WHEN** a queued stage depends on a non-terminal predecessor
- **THEN** Orc MUST NOT claim that stage

### Requirement: Valid lifecycle transitions
Orc MUST enforce subject-specific state transitions for runs, nodes, and sessions.

#### Scenario: Terminal node update
- **WHEN** a caller attempts to move a completed node back to working outside the retry path
- **THEN** Orc MUST reject the transition

### Requirement: Authorized node reporting
Orc MUST accept node reports only from the assigned active managed session or the run's active orchestrator.

#### Scenario: Worker reports another node
- **WHEN** a worker reports a node assigned to another session
- **THEN** Orc MUST reject the report without changing node output or accounting

### Requirement: Gates, retries, and limits
Orc MUST stop advancement at unresolved gates and MUST apply declared retry, iteration, timeout, and budget limits before starting more work.

#### Scenario: Human gate is pending
- **WHEN** the next ready stage has an unresolved human gate
- **THEN** Orc MUST mark the run waiting and MUST NOT start that stage
