## MODIFIED Requirements

### Requirement: Provider-backed activity
Orc MUST obtain current and historical session activity as bounded snapshots through advertised activity or inspection capabilities, poll the selected source for refreshes, and MUST NOT embed harness-specific readers in core.

#### Scenario: Activity provider accepts
- **WHEN** a bound activity provider accepts a session request
- **THEN** Orc MUST show its newest bounded output and preserve terminal color sequences

#### Scenario: Initial activity read
- **WHEN** a user opens Activity for a session with a compatible provider
- **THEN** Orc MUST request a bounded history ending at the newest available event

#### Scenario: Refreshed activity changes
- **WHEN** a provider poll returns a bounded snapshot whose content changed
- **THEN** Orc MUST replace the prior snapshot while preserving event order and deduplicating overlapping entries

#### Scenario: Refreshed activity is unchanged
- **WHEN** a provider poll returns the same bounded snapshot
- **THEN** Orc MUST retain one copy of each event and avoid a visible duplicate update

#### Scenario: Adopted session has history
- **WHEN** an adopted native session has provider-readable history
- **THEN** Orc MUST backfill that history without launching or resuming the session

#### Scenario: Activity provider is unavailable
- **WHEN** no provider can read current or historical activity
- **THEN** Orc MUST report the source as unavailable instead of reporting an empty successful history

### Requirement: Observed runtime activity
Orc MUST derive active, idle, and stalled presentation from recent provider or session observations rather than desired lifecycle status alone.

#### Scenario: Recent event
- **WHEN** a working session produces activity within the active interval
- **THEN** Orc MUST classify it as active

#### Scenario: Quiet live session
- **WHEN** a working session remains available but produces no activity within the idle interval
- **THEN** Orc MUST classify it as idle rather than completed or failed

#### Scenario: Observation becomes stale
- **WHEN** neither activity nor liveness is observed within the stalled interval
- **THEN** Orc MUST classify the runtime as stalled and retain its declared lifecycle separately
