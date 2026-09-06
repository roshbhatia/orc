## Purpose

Defines registration, lineage, adoption, provider bindings, and supervised lifecycle behavior for harness sessions.

## Requirements

### Requirement: Session contracts and lineage
Orc MUST store each session's role, purpose, goal, expected output, success criteria, harness, model, parent, run, and node association.

#### Scenario: Managed child registration
- **WHEN** an orchestrator registers a managed child for a workflow node
- **THEN** Orc MUST record its parent, run, node, contract, lifecycle owner, and lease policy

### Requirement: Root session adoption
Orc MUST let an existing native session replace the workspace orchestrator without launching that session.

#### Scenario: Replace an orchestrator
- **WHEN** an operator adopts an existing session as the new orchestrator
- **THEN** Orc MUST archive the previous orchestrator and reparent its active descendants and active runs atomically

### Requirement: Node session adoption
Orc MUST let an existing active session adopt a mutable workflow node without replacing the node's contract or result data.

#### Scenario: Adopt an unowned node
- **WHEN** an authorized operator or owning orchestrator assigns an active session to an unowned node
- **THEN** Orc MUST link both records without launching or resuming the session

#### Scenario: Conflicting node owner
- **WHEN** an active node already has another active session owner
- **THEN** Orc MUST reject the adoption

### Requirement: Managed session leases
Orc MUST enforce hard runtime and renewable idle deadlines only for managed sessions.

#### Scenario: Idle lease expires
- **WHEN** a managed session exceeds its idle deadline without an authorized keepalive
- **THEN** the supervisor MUST invoke the recorded lifecycle owner and persist the termination reason

#### Scenario: Connected session remains unmanaged
- **WHEN** a connected or hook-registered session has no managed lifecycle owner
- **THEN** Orc MUST NOT terminate it because of a lease
