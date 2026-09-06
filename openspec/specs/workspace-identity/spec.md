## Purpose

Defines how Orc isolates orchestration state and identifies one control-plane root for each workspace.

## Requirements

### Requirement: Canonical workspace scope
Orc MUST bind state to the canonical Git root, or to the canonical directory when no Git root exists.

#### Scenario: Nested repository path
- **WHEN** a command runs from a nested directory in a Git repository
- **THEN** Orc MUST use the repository root as its workspace scope

#### Scenario: Independent directory
- **WHEN** a command runs outside a Git repository
- **THEN** Orc MUST use that canonical directory without sharing state with another directory

### Requirement: One active orchestrator
Orc MUST have at most one active orchestrator session in a workspace.

#### Scenario: Conflicting registration
- **WHEN** a second native session registers as orchestrator without adoption
- **THEN** Orc MUST reject it and identify the existing orchestrator

### Requirement: Separate native and Orc identities
Orc MUST retain the harness-native session identifier separately from its workspace-scoped Orc identifier.

#### Scenario: Repeated registration
- **WHEN** the same native session registers again in the same workspace
- **THEN** Orc MUST refresh its existing non-archived Orc session instead of creating another active identity
