# GitHub Agentic Workflows

Dispatch a compiled GitHub workflow and track its Actions run from an Orc
Execution. The adapter uses `gh` authentication. GitHub stores the engine secret.

The workflow must accept `orc_operation_id` and set its run name to
`orc:${{ inputs.orc_operation_id }}`. Orc supplies the resource UID and generation.
Other workflow inputs come from `spec.inputs`.

```yaml
apiVersion: orc.dev/v1alpha1
kind: Execution
metadata:
  name: document-factory
spec:
  provider: github-aw
  repository: roshbhatia/sysinit.laurel
  workflow: factory.lock.yml
  ref: main
  inputs:
    task: >-
      Review docs/factory.md against the current factory workflow.
      Correct stale commands in that file only. Verify each command with
      its CLI help. Submit a draft PR only if a correction is needed.
```

Apply the file with `orc apply -f execution.yaml`. Use `orc reconcile` to refresh
its status. Set `spec.desiredState: cancelled` and apply it to request cancellation.
Orc retains the run URL as `externalRef` and stores the run result as an artifact.
A successful Actions run does not prove that a draft PR was created.

Install `github:roshbhatia/orc?dir=extras#provider-github-aw` with Nix, or select
the extras `full` bundle. The adapter requires `gh` and `jq`.

## Dispatch receipts

The adapter records a dispatch attempt under `$XDG_STATE_HOME/orc/github-aw`.
It searches GitHub before dispatch and retains the receipt after network errors.
Repeated reconciliation from the same control plane does not resend the request.
GitHub dispatch has no idempotency key. Separate control planes must not dispatch
the same Execution concurrently.

An ambiguous dispatch stays Pending. Inspect GitHub before deleting a receipt
or creating another Execution. Cancellation stays Running until GitHub confirms it.
Changing a resource generation creates a new correlation identifier.

## Demo

The tape reads an existing factory Execution and its real GitHub run. Create the
Execution first and set `ORC_FACTORY_RUN_URL` to its `externalRef`. Recording is
pending an authenticated factory run; tests use explicit protocol fixtures.

## Verified factory run

On September 11, 2026, Orc observed the [factory run](https://github.com/roshbhatia/sysinit.laurel/actions/runs/34635766023) as `Succeeded`.
GitHub created [draft PR #4](https://github.com/roshbhatia/sysinit.laurel/pull/4), changing only `docs/factory.md`.
The recording shows these existing resources. It does not submit or approve a new task.

The agent checked the workflow source and ran `git diff --check`.
Its runner lacked Nix and the `gh aw` extension, so it could not perform those checks.
A successful Execution proves the remote run completed. Review the PR and its verification limits before merging.
