## ADDED Requirements

### Requirement: Orchestrator output inspection
The dashboard MUST expose an Output inspector tab for a selected orchestrator session, MUST render only that session's explicitly reported structured output, and MUST keep rendering work bounded without using Activity truncation labels.

#### Scenario: Root output fits the inspector bound
- **WHEN** the selected orchestrator session has reported structured output whose rendered JSON fits the inspector bound
- **THEN** the Output tab MUST render the complete readable JSON without replacing or duplicating Activity

#### Scenario: Root output exceeds the inspector bound
- **WHEN** the selected orchestrator session has valid output whose rendered JSON exceeds the inspector bound
- **THEN** the Output tab MUST stop serialization at its byte or line limit and show an honestly labeled rendered-prefix preview with a shell-safe non-interactive command that returns the complete retained value from the resolved Orc scope

#### Scenario: Root output is absent
- **WHEN** the selected orchestrator session has no reported structured output
- **THEN** the Output tab MUST show an explicit empty state

#### Scenario: Activity exists without output
- **WHEN** the selected orchestrator has activity but no structured output
- **THEN** Orc MUST NOT parse, summarize, or copy Activity into Output
