## ADDED Requirements

### Requirement: Run recovery diagnostics
Orc MUST expose inconsistent and pending-repair runs through doctor diagnostics and MUST limit repair to deterministic state transitions.

#### Scenario: Doctor inspects an orphan working run
- **WHEN** an operator runs `orc doctor` for a workspace with orphan working nodes
- **THEN** Orc MUST identify the affected run, reason, proposed action, and whether automatic repair is allowed

#### Scenario: Doctor repairs manual orphan work
- **WHEN** an authorized operator runs `orc doctor --repair` for manual orphan work
- **THEN** Orc MUST block the run and record a resolve-assignment proposal
- **AND** MUST NOT create or launch a session

#### Scenario: Doctor repairs a supervised workflow
- **WHEN** an authorized operator repairs a valid supervised or approval-gated workflow without a live executor
- **THEN** Orc MUST block interrupted work and record a restart proposal without starting it

#### Scenario: Daemon repeats a recovery sweep
- **WHEN** the daemon inspects a run whose recovery transition already completed
- **THEN** Orc MUST leave the state unchanged and MUST NOT append duplicate events

#### Scenario: Workspace directory is unavailable
- **WHEN** the daemon finds a persisted working run whose workspace directory is missing
- **THEN** Orc MUST block the run with a durable restore proposal
- **AND** MUST NOT launch an executor or provider

#### Scenario: Owner satisfies a repair proposal
- **WHEN** an owner supplies a valid active assignment or executor for a working run with a pending proposal
- **THEN** Orc MUST record the reconciliation and clear the pending proposal exactly once
