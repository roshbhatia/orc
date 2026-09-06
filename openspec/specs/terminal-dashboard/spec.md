## Purpose

Defines the terminal control-plane views, navigation, graph presentation, inspection, and safe user actions.

## Requirements

### Requirement: Tree and graph views
Orc MUST provide an orchestrator-rooted tree and a run graph with typed stage relationships and contextual detail.

#### Scenario: Open a run graph
- **WHEN** a user selects a run and opens its graph
- **THEN** Orc MUST place the orchestrator outside the stage ranks and show dependency, delegation, review, feedback, and report relationships

### Requirement: Stable terminal interaction
Orc MUST support keyboard, mouse, and command-line navigation while preserving selection and viewport across content refreshes when selected objects still exist.

#### Scenario: Resize the terminal
- **WHEN** the terminal resizes above the supported minimum
- **THEN** Orc MUST fit the graph with visible margin and keep node and edge labels readable

### Requirement: Contextual inspector
Orc MUST provide details, activity, output, and changes views only when they apply to the selected object.

#### Scenario: Select a session
- **WHEN** a user selects a session and opens Activity
- **THEN** Orc MUST request and display that session's provider-backed activity

### Requirement: Provider-backed open action
Orc MUST route Enter on an openable session or stage through the applicable provider chain and keep the dashboard running if the action fails.

#### Scenario: Open action fails
- **WHEN** an open provider exits unsuccessfully
- **THEN** Orc MUST retain the dashboard and show the failed action and exit reason
