## Purpose

Defines the desired-state resource API, field ownership, reconciliation, artifacts, and durable control-plane events.

## Requirements

### Requirement: Declarative resource operations
Orc MUST support diff, dry-run, apply, get, describe, and delete for Workflow, Run, Session, Execution, EventBinding, and Artifact resources.

#### Scenario: Dry-run apply
- **WHEN** a user applies valid resources with dry-run enabled
- **THEN** Orc MUST report the planned field changes without writing state or invoking a provider

### Requirement: Managed field ownership
Orc MUST track field managers and reject a conflicting change unless the caller explicitly forces ownership transfer.

#### Scenario: Conflicting field update
- **WHEN** a different manager changes an owned field without force
- **THEN** Orc MUST reject the apply and identify the field and current owner

### Requirement: Idempotent reconciliation
Orc MUST reconcile desired resources through capability providers with stable operation identifiers and persist observations only after provider acknowledgement.

#### Scenario: Reconcile unchanged generation
- **WHEN** a resource's observed generation already matches its desired generation
- **THEN** Orc MUST NOT repeat its ensure operation

### Requirement: Durable events and artifacts
Orc MUST retain ordered control events and content-addressed artifacts for later reads, bounded watches, and event delivery.

#### Scenario: Resume an event watch
- **WHEN** a client watches after a known event sequence
- **THEN** Orc MUST emit only later matching events until the requested count or timeout
