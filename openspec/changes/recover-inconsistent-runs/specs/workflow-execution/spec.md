## ADDED Requirements

### Requirement: Deterministic run recovery
Orc MUST reconcile a working run that has neither a live matching executor nor a valid active session assignment into an explicit safe recovery state.

#### Scenario: Autonomous stored workflow loses its executor
- **WHEN** an autonomous run has an unchanged stored workflow revision and its matching executor lease is no longer live
- **THEN** Orc MUST block each interrupted working node before it starts a replacement executor
- **AND** MUST NOT consume or reset a workflow node retry attempt

#### Scenario: Automatic recovery cannot start
- **WHEN** the replacement executor fails to start
- **THEN** Orc MUST keep the run queued with bounded infrastructure backoff and a durable restart proposal

#### Scenario: Replacement process is not ready
- **WHEN** a replacement process exists but has not acknowledged its matching recovery operation
- **THEN** Orc MUST retain the durable restart proposal
- **AND** MUST NOT report the run as recovered

#### Scenario: Recovery mode changes during backoff
- **WHEN** an automatic restart is queued and the workspace leaves autonomous mode before the next attempt
- **THEN** Orc MUST block the run with the approval authority required by the current mode
- **AND** MUST NOT start the executor

#### Scenario: Recovery races a terminal transition
- **WHEN** a recovery failure arrives after the run became terminal or another recovery operation replaced it
- **THEN** Orc MUST leave the current run state unchanged

#### Scenario: Stored definition changed
- **WHEN** a working run's stored workflow is missing, unreadable, or does not match its recorded revision
- **THEN** Orc MUST block the run with a durable restore-definition proposal
- **AND** MUST NOT execute the changed definition

#### Scenario: Manual work has no assignment
- **WHEN** a manual working run has no working node assigned to a valid active session
- **THEN** Orc MUST block the run with a durable resolve-assignment proposal
- **AND** MUST NOT select a harness, model, execution provider, or replacement session

#### Scenario: Working node has a valid assignment
- **WHEN** a working node references an active matching managed session or its active run orchestrator
- **THEN** Orc MUST treat the assignment as live work and MUST NOT replace it

#### Scenario: Assigned session impersonates the orchestrator
- **WHEN** a node references the run orchestrator ID but that session does not have the orchestrator role
- **THEN** Orc MUST reject the assignment as invalid

#### Scenario: Recovery history grows
- **WHEN** more recovery events arrive than the state bound permits
- **THEN** Orc MUST retain the newest typed events with bounded valid UTF-8 messages
