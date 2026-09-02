# Orc

![Orc workflow graph](docs/orc.png)

![Orc animated workflow graph](docs/orc.gif)

Orc is a local control plane for agent harnesses. It records agent sessions and
workflow graphs. External providers add host integrations.

Orc uses the same TypeScript stack as OpenCode: Bun, Effect, Solid, and
OpenTUI.

## Terms

- A session is one harness instance, such as one Codex or Claude conversation.
- A workflow is one goal and its generated execution graph.
- A node is one workflow stage with a role, contract, state, and optional
  session.
- Explorer is a view that nests workflows and sessions under each orchestrator.

## Use Orc

Open the dashboard for the current repository:

```bash
orc
```

The dashboard has three main tabs:

- Explorer shows the complete hierarchy.
- Workflow shows one workflow as a tree or graph.
- Providers shows the discovered provider manifests.

Press `Enter` on a workflow to open it. Press `Enter` on a session to attach.
Use `t` and `g` for workflow tree and graph views. Use `hjkl` in the graph.
Use `i` for activity and `c` for changes. Use `[` and `]` for detail tabs.

Register an orchestrator session:

```bash
orc connect \
  --harness codex \
  --role orchestrator \
  --purpose "Build Orc" \
  --goal "Ship a usable local control plane" \
  --expected-output "Passing checks and a tested TUI" \
  --native-id "$CODEX_THREAD_ID"
```

Register a child session with `--parent <orc-session-id>`.

```bash
orc status --json
orc list --json
orc providers --json
orc attach <orc-session-id> --direction right
orc inspect <orc-session-id> --direction right
orc disconnect <orc-session-id>
```

![Orc non-interactive commands](docs/orc-noninteractive.gif)

Use `orc session adopt` inside a pre-existing harness session to make it the
new orchestrator for the current directory. Orc archives the previous active
orchestrator incarnation. Use `orc session archive` when an unhooked harness
ends. Harness exit hooks should call `orc session archive --hook-input --quiet`.

## Providers

Orc discovers YAML or JSON manifests from `~/.config/orc/providers/`. Set
`ORC_PROVIDERS_DIRECTORY` to use another directory.

Each provider advertises one kind and a set of capabilities:

```yaml
# yaml-language-server: $schema=https://raw.githubusercontent.com/roshbhatia/orc/main/schema/provider.schema.json
version: orc.provider/v1
name: wezterm
description: Open provider command plans in a WezTerm pane
kind: display
command: /path/to/orc-provider-wezterm
actions:
  session.bind: Detect the current WezTerm pane
  terminal.open: Open a command in a split pane
priority: 100
```

The manifest is the language-neutral shim. Its `actions` map advertises what
the provider does. The command can use Bash, TypeScript, Go, Rust, or any other
language that can read JSON from stdin and write JSON to stdout. Orc validates
the manifest, executable, dependencies, and protocol with:

```bash
orc provider validate
orc provider validate wezterm
```

Provider kinds describe session facets:

- `harness` resumes the native harness session.
- `persistence` keeps a process available after its display closes.
- `display` opens the selected command for the user.
- `activity` supplies messages, reasoning summaries, and tool activity.
- `changes` supplies repository changes.

Several providers can bind to one session. Orc reconciles every recorded
session when the dashboard opens. It also reconciles new hook registrations.

An active Zmx binding requires the harness process to start inside Zmx. Zmx
cannot wrap a process that already runs. An existing session can still gain a
Zmx binding when its next harness resume starts through Zmx.

Actions resolve through capability chains:

```text
attach    session.attach -> terminal.open
inspect   session.inspect -> terminal.open
activity  session.inspect
changes   changes.inspect
launch    session.launch
```

Orc writes one `orc.provider/v1` request to each provider command. A provider
can return a command plan, a session binding, a description, or an explicit
decline. An explicit decline lets the next provider handle that capability.

The final command plan inherits terminal state. Captured actions preserve ANSI
color. Orc itself does not import Zmx, WezTerm, Traces, or Changes.

The optional provider packages live in [`extras/`](extras/README.md). Install
only the adapters your environment uses:

```bash
nix profile install github:roshbhatia/orc#provider-harness
nix profile install github:roshbhatia/orc#provider-zmx
nix profile install github:roshbhatia/orc#provider-wezterm
```

Without providers, Orc still records sessions, workflows, nodes, and contracts.
Orc requires no broker process.

## Configure Orc

Orc reads `~/.config/orc/config.yaml`. Set `ORC_CONFIG` to use another file.
The configuration stays directory-independent. Session state remains bound to
the canonical workspace directory.

```yaml
# yaml-language-server: $schema=https://raw.githubusercontent.com/roshbhatia/orc/main/schema/orc.schema.json
cache:
  providerTtlMs: 1000
providers:
  directory: ~/.config/orc/providers
  timeoutMs: 5000
```

Every scalar supports a nested environment override:

```bash
ORC_CACHE_PROVIDER_TTL_MS=0 orc status
ORC_PROVIDERS_DIRECTORY=/tmp/orc-providers orc providers
ORC_PROVIDERS_TIMEOUT_MS=10000 orc
```

`ORC_PROVIDER_DIR` and `ORC_PROVIDER_TIMEOUT_MS` remain compatibility aliases.
The generated schema supports YAML editor validation.

<!-- BEGIN GENERATED:commands -->
## Command reference

### `orc tui`

Open the control plane

```text
orc tui
```

Options:

- `--scope <value>`: Select a workspace scope

### `orc status`

Show workspace status

```text
orc status
```

Options:

- `--scope <value>`: Select a workspace scope
- `--json`: Print JSON

### `orc list`

List registered sessions

```text
orc list
```

Options:

- `--scope <value>`: Select a workspace scope
- `--json`: Print JSON

### `orc providers`

List provider manifests

```text
orc providers
```

Options:

- `--scope <value>`: Select a workspace scope
- `--json`: Print JSON

### `orc provider`

Inspect and validate providers

```text
orc provider list|validate
```


### `orc connect`

Register the current session

```text
orc connect
```

Options:

- `--scope <value>`: Select a workspace scope
- `--harness <value>`: Select an agent harness
- `--model <value>`: Select a harness model
- `--role <orchestrator|planner|researcher|implementer|critic|judge|verifier|operator|generalist|worker>`: Set the agent role
- `--title <value>`: Set the display title
- `--purpose <value>`: Explain why the agent exists
- `--goal <value>`: Set the agent goal
- `--expected-output <value>`: Describe the expected output
- `--success <value>`: Add one success criterion
- `--completion <orchestrator|judge>`: Select the completion target
- `--review-by <value>`: Set the review node
- `--id <value>`: Set the Orc session id
- `--native-id <value>`: Set the harness session id
- `--parent <value>`: Set the parent session
- `--provider-ref <value>`: Set the provider session reference

### `orc session`

Manage sessions

```text
orc session register|current|list|update
```


### `orc run`

Manage workflow runs

```text
orc run create|agent|list|show|update
```


### `orc node`

Manage workflow nodes

```text
orc node upsert|update
```


### `orc launch`

Launch a managed harness

```text
orc launch <harness> [-- args]
```

Options:

- `--scope <value>`: Select a workspace scope
- `--managed <value>`: Set the managed session id
- `--model <value>`: Select a harness model

### `orc attach`

Attach through a session provider

```text
orc attach <session-id>
```

Options:

- `--scope <value>`: Select a workspace scope
- `--direction <right|left|top|bottom>`: Select the split direction

### `orc inspect`

Inspect through a session provider

```text
orc inspect <session-id>
```

Options:

- `--scope <value>`: Select a workspace scope
- `--direction <right|left|top|bottom>`: Select the split direction

### `orc disconnect`

Disconnect a registered session

```text
orc disconnect [session-id]
```

Options:

- `--scope <value>`: Select a workspace scope

### `orc mcp`

Run the MCP server

```text
orc mcp
```


### `orc prompt`

Print the prompt session marker

```text
orc prompt
```

Options:

- `--scope <value>`: Select a workspace scope

### `orc completion`

Generate shell completions

```text
orc completion bash|zsh|fish|nu
```


### `orc help`

Show command help

```text
orc help
```


### `orc version`

Show the Orc version

```text
orc version
```


### `orc completion bash`

Generate Bash completions

```text
orc completion bash
```


### `orc completion fish`

Generate Fish completions

```text
orc completion fish
```


### `orc completion nu`

Generate Nushell completions

```text
orc completion nu
```


### `orc completion zsh`

Generate Zsh completions

```text
orc completion zsh
```


### `orc node upsert`

Create or replace a workflow node

```text
orc node upsert
```

Options:

- `--scope <value>`: Select a workspace scope
- `--run <value>`: Select the workflow run
- `--session <value>`: Set the linked session
- `--status <queued|working|waiting|blocked|failed|done|cancelled|disconnected|archived>`: Set lifecycle status
- `--attempt <value>`: Set the attempt number
- `--depends-on <value>`: Add a dependency node
- `--harness <value>`: Select an agent harness
- `--model <value>`: Select a harness model
- `--role <orchestrator|planner|researcher|implementer|critic|judge|verifier|operator|generalist|worker>`: Set the agent role
- `--title <value>`: Set the display title
- `--purpose <value>`: Explain why the agent exists
- `--goal <value>`: Set the agent goal
- `--expected-output <value>`: Describe the expected output
- `--success <value>`: Add one success criterion
- `--completion <orchestrator|judge>`: Select the completion target
- `--review-by <value>`: Set the review node

### `orc node update`

Update a workflow node status

```text
orc node update
```

Options:

- `--scope <value>`: Select a workspace scope
- `--run <value>`: Select the workflow run
- `--status <queued|working|waiting|blocked|failed|done|cancelled|disconnected|archived>`: Set lifecycle status

### `orc provider list`

List provider manifests

```text
orc provider list
```

Options:

- `--scope <value>`: Select a workspace scope
- `--json`: Print JSON

### `orc provider validate`

Validate provider dependencies and protocol behavior

```text
orc provider validate [name]
```

Options:

- `--scope <value>`: Select a workspace scope
- `--json`: Print JSON

### `orc run create`

Create a workflow run

```text
orc run create
```

Options:

- `--scope <value>`: Select a workspace scope
- `--name <value>`: Set the run name
- `--goal <value>`: Set the run goal
- `--expected-output <value>`: Describe the expected output
- `--orchestrator <value>`: Select the orchestrator session
- `--harness <value>`: Select the default harness
- `--model <value>`: Select the default model
- `--json`: Print JSON

### `orc run agent`

Set a run role harness

```text
orc run agent
```

Options:

- `--scope <value>`: Select a workspace scope
- `--harness <value>`: Select an agent harness
- `--model <value>`: Select a harness model
- `--role <orchestrator|planner|researcher|implementer|critic|judge|verifier|operator|generalist|worker>`: Set the agent role

### `orc run list`

List workflow runs

```text
orc run list
```

Options:

- `--scope <value>`: Select a workspace scope
- `--json`: Print JSON

### `orc run show`

Show one workflow run

```text
orc run show
```

Options:

- `--scope <value>`: Select a workspace scope
- `--json`: Print JSON

### `orc run update`

Update a workflow run status

```text
orc run update
```

Options:

- `--scope <value>`: Select a workspace scope
- `--status <queued|working|waiting|blocked|failed|done|cancelled|disconnected|archived>`: Set lifecycle status

### `orc session adopt`

Adopt the current harness as a new orchestrator

```text
orc session adopt
```

Options:

- `--scope <value>`: Select a workspace scope
- `--harness <value>`: Select an agent harness
- `--model <value>`: Select a harness model
- `--role <orchestrator|planner|researcher|implementer|critic|judge|verifier|operator|generalist|worker>`: Set the agent role
- `--title <value>`: Set the display title
- `--purpose <value>`: Explain why the agent exists
- `--goal <value>`: Set the agent goal
- `--expected-output <value>`: Describe the expected output
- `--success <value>`: Add one success criterion
- `--completion <orchestrator|judge>`: Select the completion target
- `--review-by <value>`: Set the review node
- `--native-id <value>`: Set the harness session id

### `orc session archive`

Archive a session incarnation

```text
orc session archive [session-id]
```

Options:

- `--scope <value>`: Select a workspace scope
- `--native-id <value>`: Match the harness session id
- `--hook-input`: Read session data from standard input
- `--quiet`: Suppress the session id

### `orc session register`

Register a session

```text
orc session register
```

Options:

- `--scope <value>`: Select a workspace scope
- `--harness <value>`: Select an agent harness
- `--model <value>`: Select a harness model
- `--role <orchestrator|planner|researcher|implementer|critic|judge|verifier|operator|generalist|worker>`: Set the agent role
- `--title <value>`: Set the display title
- `--purpose <value>`: Explain why the agent exists
- `--goal <value>`: Set the agent goal
- `--expected-output <value>`: Describe the expected output
- `--success <value>`: Add one success criterion
- `--completion <orchestrator|judge>`: Select the completion target
- `--review-by <value>`: Set the review node
- `--id <value>`: Set the Orc session id
- `--native-id <value>`: Set the harness session id
- `--parent <value>`: Set the parent session
- `--run <value>`: Link a workflow run
- `--node <value>`: Link a workflow node
- `--source <connected|hook|managed>`: Set the registration source
- `--provider-ref <value>`: Set the provider session reference
- `--hook-input`: Read session data from standard input
- `--quiet`: Suppress the session id

### `orc session current`

Show the current session

```text
orc session current
```

Options:

- `--scope <value>`: Select a workspace scope
- `--json`: Print JSON

### `orc session list`

List sessions

```text
orc session list
```

Options:

- `--scope <value>`: Select a workspace scope
- `--json`: Print JSON

### `orc session update`

Update a session status

```text
orc session update
```

Options:

- `--scope <value>`: Select a workspace scope
- `--status <queued|working|waiting|blocked|failed|done|cancelled|disconnected|archived>`: Set lifecycle status
<!-- END GENERATED:commands -->

## Develop Orc

```bash
nix develop --accept-flake-config
bun install --frozen-lockfile
bun run check
bun run generate
nix build --accept-flake-config
nix flake check --accept-flake-config
./hack/screenshots.sh
```

Run `bun run nix:lock` after a dependency change.
