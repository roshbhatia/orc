## Purpose

Defines reusable workflow documents and their versioned repository before any execution is materialized.

## Requirements

### Requirement: Versioned workflow definitions
Orc MUST validate workflow YAML and store accepted definitions in a workspace-addressable Git catalog.

#### Scenario: Save valid workflow
- **WHEN** a valid definition is saved
- **THEN** Orc MUST preserve its revision and make it available to history and search

#### Scenario: Reject invalid workflow
- **WHEN** a definition contains an unknown dependency, invalid route, invalid reviewer, or invalid limit
- **THEN** Orc MUST reject it before versioning or execution

### Requirement: Explicit stage contracts
Each agent stage MUST declare a non-orchestrator role, goal, expected output, and runtime selection through its own values or workflow defaults.

#### Scenario: Agent stage lacks a harness
- **WHEN** an agent stage and its defaults omit a harness
- **THEN** Orc MUST reject the workflow

### Requirement: Typed orchestration relationships
Workflow definitions MUST express dependency, routing, review, feedback, completion, parallel, and nested-workflow relationships as validated graph data.

#### Scenario: Orchestrator remains outside the stage set
- **WHEN** Orc plans a workflow
- **THEN** it MUST represent the orchestrator as the run owner and MUST NOT accept it as a stage role
