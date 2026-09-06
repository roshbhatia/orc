## Purpose

Defines the external provider contract that adds runtime integrations without coupling Orc core to specific tools.

## Requirements

### Requirement: Capability-based provider discovery
Orc MUST discover external provider manifests and select them by advertised capability, requirements, priority, and request acceptance.

#### Scenario: Provider declines a request
- **WHEN** a provider returns an explicit decline
- **THEN** Orc MUST try the next eligible provider and retain the decline reason if no provider accepts

#### Scenario: No provider is installed
- **WHEN** no provider advertises an optional integration
- **THEN** Orc MUST retain provider-neutral session and workflow state while reporting that integration as unavailable

### Requirement: Composable provider plans
Orc MUST compose attach, launch, execution, inspection, activity, guidance, and stop actions from capability stages declared by providers.

#### Scenario: Attach chain
- **WHEN** an attach request has an eligible harness, persistence, and display route
- **THEN** Orc MUST pass the evolving command plan through the applicable provider stages in deterministic order

### Requirement: Provider execution boundary
Orc MUST bound provider execution time and output, preserve declared success codes, and surface command failures without recording optimistic success.

#### Scenario: Provider timeout
- **WHEN** a provider exceeds the configured timeout
- **THEN** Orc MUST stop waiting, report the timeout, and leave observed state unchanged

### Requirement: Provider validation
Orc MUST validate manifest structure, host requirements, and provider-specific checks without executing an unrelated capability.

#### Scenario: Missing dependency
- **WHEN** a provider requires a command that is absent
- **THEN** provider validation MUST fail with the missing requirement
