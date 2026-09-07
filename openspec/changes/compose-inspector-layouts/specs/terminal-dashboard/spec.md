## MODIFIED Requirements

### Requirement: Stable terminal interaction

Orc MUST support keyboard, mouse, and command-line navigation while preserving selection and viewport across content refreshes when selected objects still exist. The inspector MUST rotate, dock, hide, restore, and resize without changing its selected object, active tab, focus, or scroll position except when hiding requires focus to return to the main pane.

#### Scenario: Rotate a visible inspector
- **WHEN** a user rotates the inspector
- **THEN** Orc MUST move it clockwise through bottom, right, top, and left
- **AND** the selected item, active inspector tab, focus, and scroll position MUST remain unchanged

#### Scenario: Resize the terminal
- **WHEN** the terminal resizes above the supported minimum
- **THEN** Orc MUST fit the graph with visible margin and keep node and edge labels readable

#### Scenario: Hide and restore the inspector
- **WHEN** a user hides and then restores the inspector
- **THEN** Orc MUST restore its last visible edge and saved size
- **AND** a temporary small-terminal fallback MUST NOT overwrite those preferences

#### Scenario: Resize the inspector
- **WHEN** a user resizes the focused inspector with `-` or `=`, or drags its divider
- **THEN** Orc MUST resize it within minimum usable main and inspector dimensions
- **AND** Orc MUST persist the resulting percentage for that workspace

#### Scenario: Move focus spatially
- **WHEN** a user invokes pane focus in the direction occupied by the other pane
- **THEN** focus MUST cross the divider according to the current inspector edge
- **AND** an unrelated direction MUST remain in the focused pane

#### Scenario: Resize a manual graph viewport
- **WHEN** the terminal resizes after a user zooms or pans the graph
- **THEN** Orc MUST clamp the manual viewport without requesting a new fit
- **AND** a graph in fit mode MUST refit to the new canvas

### Requirement: Contextual inspector

Orc MUST provide details, activity, output, and changes views only when they apply to the selected object. The active inspector tab, edge, visibility, size, and last visible edge MUST persist in workspace-local XDG state.

#### Scenario: Restart a workspace dashboard
- **WHEN** a user reopens Orc for the same resolved directory
- **THEN** Orc MUST restore the saved inspector layout and active applicable tab
- **AND** another resolved directory MUST retain its own layout state

#### Scenario: Select a session
- **WHEN** a user selects a session and opens Activity
- **THEN** Orc MUST request and display that session's provider-backed activity
