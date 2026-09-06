## Purpose

Defines bounded agent activity, runtime observations, and the distinction between missing, idle, stalled, and failed activity sources.

## Requirements

### Requirement: Provider-backed activity
Orc MUST obtain session activity through advertised activity or inspection capabilities without embedding harness-specific readers in core.

#### Scenario: Activity provider accepts
- **WHEN** a bound activity provider accepts a session request
- **THEN** Orc MUST show its newest bounded output and preserve terminal color sequences

### Requirement: Bounded output retention
Orc MUST bound captured activity by bytes and lines while retaining the newest complete content and marking truncation.

#### Scenario: Activity exceeds bounds
- **WHEN** provider output exceeds either configured bound
- **THEN** Orc MUST retain the newest output and show that earlier activity was omitted

### Requirement: Distinct runtime observations
Orc MUST distinguish active, idle, stalled, terminal, unavailable, loading, empty, and failed activity states.

#### Scenario: No provider accepts
- **WHEN** no provider can read activity for a session
- **THEN** Orc MUST report activity as unavailable instead of reporting an empty successful history
