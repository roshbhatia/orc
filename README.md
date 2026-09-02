# Orc

![Orc workflow graph](docs/orc.png)

![Orc animated workflow graph](docs/orc.gif)

Orc is a local control plane for agent harnesses. It records agent sessions and
executes versioned workflow graphs. External providers add harness, execution,
persistence, display, activity, and change integrations.

The core is a standalone Rust CLI. Its full-screen interface uses Ratatui and
Rataflow. Orc does not require a broker, terminal multiplexer, or specific
agent harness.

## Terms

- An orchestrator is the root harness session for one workspace.
- A workflow is a versioned YAML definition stored in Orc's Git catalog.
- A run is one execution of a workflow definition.
- A node is one stage with a role, contract, state, and optional harness
  session.
- A provider is an external command that implements one or more Orc
  capabilities.

Sessions and workflow state are bound to the canonical repository directory.
Resuming a harness in that directory can adopt it as the current orchestrator
without reviving an unrelated workspace.

## Use Orc

Open the dashboard for the current repository:

```bash
orc
```

The dashboard has two main tabs:

- Explorer shows the active run as a graph. It also includes tree and fleet
  views.
- Providers shows the discovered provider manifests.

The graph always places the orchestrator above its stages. Dependency arrows
flow down. Review feedback and report-back arrows remain visible without
changing the layout. Press `Enter` on a run to open its graph. Press `Enter` on
a session to attach through providers.

Use `g`, `t`, and `f` for graph, tree, and fleet views. Use `hjkl` inside the
focused pane. Use `Ctrl-h/j/k/l` to move between the graph, details, and output
panes. `Tab` and `Shift-Tab` change the Log, Activity, Output, and Changes
views. Press `?` for generated key help.

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
orc provider list --json
orc attach <orc-session-id> --direction right
orc inspect <orc-session-id> --direction right
orc disconnect <orc-session-id>
```

![Orc non-interactive commands](docs/orc-noninteractive.gif)

Use `orc session adopt` inside a pre-existing harness session to make it the
new orchestrator for the current directory. Orc archives the previous active
orchestrator incarnation. Use `orc session archive` when an unhooked harness
ends. Harness exit hooks should call `orc session archive --hook-input --quiet`.

## Run a workflow

Create a starter definition in the current workspace's workflow catalog:

```bash
orc workflow init provider-migration
orc workflow edit provider-migration
orc workflow validate "$(orc workflow path provider-migration)"
orc workflow plan provider-migration
orc workflow start provider-migration --background
```

An orchestrator can also propose and start definitions through Orc's MCP
tools. Orc validates the proposal before it commits the YAML definition. Ready
nodes run concurrently. Each agent node chooses a harness, model, and execution
provider. A nested workflow node composes another definition.

Approval modes control human gates:

- `full_auto` runs every ready node.
- `supervised` pauses only at declared or node-level gates.
- `manual` pauses before every node.

Use `orc run approve <run-id>` to continue a waiting run. Use
`orc guide <session-id> --text '...'` for a provider-supported correction.
Use `orc run cancel <run-id>` to stop a run.

Workflow definitions live under `~/.local/share/orc/workflows` by default.
Each workspace gets a directory inside that Git repository. Use
`orc workflow history`, `orc workflow search`, and `orc workflow path` to
inspect prior definitions.

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

Actions resolve through capability chains. Optional steps are marked with `?`:

```text
attach    session.attach -> session.persist? -> terminal.open
inspect   session.inspect -> terminal.open
activity  session.inspect
changes   changes.inspect
launch    session.launch -> session.persist? -> execution.run
execute   execution.run
```

Orc writes one `orc.provider/v1` request to each provider command. A provider
can return a command plan, a session binding, a description, or an explicit
decline. An explicit decline lets the next provider handle that capability.

The final command plan inherits terminal state. Captured output preserves ANSI
color in the TUI. Reconciliation caches content-addressed binding and
description responses for the configured TTL. Orc itself does not import Zmx,
WezTerm, Traces, or Changes.

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
workflows:
  repository: ~/.local/share/orc/workflows
  autoCommit: true
  maxDepth: 10
ui:
  refreshMs: 750
  inspectorPercent: 38
```

Every scalar supports a nested environment override:

```bash
ORC_CACHE_PROVIDER_TTL_MS=0 orc status
ORC_PROVIDERS_DIRECTORY=/tmp/orc-providers orc providers
ORC_PROVIDERS_TIMEOUT_MS=10000 orc
ORC_WORKFLOWS_REPOSITORY=/tmp/orc-workflows orc workflow list
ORC_UI_REFRESH_MS=1000 orc
```

`ORC_PROVIDER_DIR` and `ORC_PROVIDER_TIMEOUT_MS` remain compatibility aliases.
The generated schema supports YAML editor validation.

<!-- BEGIN GENERATED:commands -->
### `orc tui`

Open the control plane

```text
Open the control plane

Usage: tui [OPTIONS]

Options:
      --scope <SCOPE>  [env: ORC_SCOPE=] [default: .]
  -h, --help           Print help
```

### `orc status`

Show workspace status

```text
Show workspace status

Usage: status [OPTIONS]

Options:
      --scope <SCOPE>  [env: ORC_SCOPE=] [default: .]
      --json
  -h, --help           Print help
```

### `orc list`

List registered sessions

```text
List registered sessions

Usage: list [OPTIONS]

Options:
      --scope <SCOPE>  [env: ORC_SCOPE=] [default: .]
      --json
  -h, --help           Print help
```

### `orc connect`

Register the current session

```text
Register the current session

Usage: connect [OPTIONS]

Options:
      --scope <SCOPE>                      [env: ORC_SCOPE=] [default: .]
      --harness <HARNESS>                  [default: unknown]
      --model <MODEL>
      --role <ROLE>                        [default: worker]
      --title <TITLE>                      [default: "Agent session"]
      --purpose <PURPOSE>                  [default: "Agent session"]
      --goal <GOAL>                        [default: "Complete the assigned work"]
      --expected-output <EXPECTED_OUTPUT>  [default: "A verified result"]
      --success <SUCCESS_CRITERIA>
      --completion <COMPLETION>            [default: orchestrator]
      --review-by <REVIEW_BY>
      --id <ID>
      --native-id <NATIVE_ID>
      --parent <PARENT_ID>
      --run <RUN_ID>
      --node <NODE_ID>
      --provider-ref <PROVIDER_REF>
      --source <SOURCE>                    [default: connected]
      --hook-input
      --quiet
  -h, --help                               Print help
```

### `orc session`

Manage sessions

```text
Manage sessions

Usage: session <COMMAND>

Commands:
  register
  adopt
  archive
  current
  list
  update
  help      Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `orc session register`

```text
Usage: register [OPTIONS]

Options:
      --scope <SCOPE>                      [env: ORC_SCOPE=] [default: .]
      --harness <HARNESS>                  [default: unknown]
      --model <MODEL>
      --role <ROLE>                        [default: worker]
      --title <TITLE>                      [default: "Agent session"]
      --purpose <PURPOSE>                  [default: "Agent session"]
      --goal <GOAL>                        [default: "Complete the assigned work"]
      --expected-output <EXPECTED_OUTPUT>  [default: "A verified result"]
      --success <SUCCESS_CRITERIA>
      --completion <COMPLETION>            [default: orchestrator]
      --review-by <REVIEW_BY>
      --id <ID>
      --native-id <NATIVE_ID>
      --parent <PARENT_ID>
      --run <RUN_ID>
      --node <NODE_ID>
      --provider-ref <PROVIDER_REF>
      --source <SOURCE>                    [default: connected]
      --hook-input
      --quiet
  -h, --help                               Print help
```

### `orc session adopt`

```text
Usage: adopt [OPTIONS]

Options:
      --scope <SCOPE>                      [env: ORC_SCOPE=] [default: .]
      --harness <HARNESS>                  [default: unknown]
      --model <MODEL>
      --role <ROLE>                        [default: worker]
      --title <TITLE>                      [default: "Agent session"]
      --purpose <PURPOSE>                  [default: "Agent session"]
      --goal <GOAL>                        [default: "Complete the assigned work"]
      --expected-output <EXPECTED_OUTPUT>  [default: "A verified result"]
      --success <SUCCESS_CRITERIA>
      --completion <COMPLETION>            [default: orchestrator]
      --review-by <REVIEW_BY>
      --native-id <NATIVE_ID>
  -h, --help                               Print help
```

### `orc session archive`

```text
Usage: archive [OPTIONS] [ID]

Arguments:
  [ID]

Options:
      --scope <SCOPE>          [env: ORC_SCOPE=] [default: .]
      --native-id <NATIVE_ID>
      --hook-input
      --quiet
  -h, --help                   Print help
```

### `orc session current`

```text
Usage: current [OPTIONS]

Options:
      --scope <SCOPE>  [env: ORC_SCOPE=] [default: .]
      --json
  -h, --help           Print help
```

### `orc session list`

```text
Usage: list [OPTIONS]

Options:
      --scope <SCOPE>  [env: ORC_SCOPE=] [default: .]
      --json
  -h, --help           Print help
```

### `orc session update`

```text
Usage: update [OPTIONS] --status <STATUS> <ID>

Arguments:
  <ID>

Options:
      --scope <SCOPE>    [env: ORC_SCOPE=] [default: .]
      --status <STATUS>
  -h, --help             Print help
```

### `orc run`

Manage workflow runs

```text
Manage workflow runs

Usage: run <COMMAND>

Commands:
  create
  agent
  list
  show
  update
  resume
  approve
  cancel
  help     Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `orc run create`

```text
Usage: create [OPTIONS] --name <NAME> --goal <GOAL> --expected-output <EXPECTED_OUTPUT>

Options:
      --scope <SCOPE>                      [env: ORC_SCOPE=] [default: .]
      --name <NAME>
      --goal <GOAL>
      --expected-output <EXPECTED_OUTPUT>
      --orchestrator <ORCHESTRATOR_ID>
      --harness <HARNESS>
      --model <MODEL>
      --json
  -h, --help                               Print help
```

### `orc run agent`

```text
Usage: agent [OPTIONS] --role <ROLE> --harness <HARNESS> <ID>

Arguments:
  <ID>

Options:
      --scope <SCOPE>      [env: ORC_SCOPE=] [default: .]
      --role <ROLE>
      --harness <HARNESS>
      --model <MODEL>
  -h, --help               Print help
```

### `orc run list`

```text
Usage: list [OPTIONS]

Options:
      --scope <SCOPE>  [env: ORC_SCOPE=] [default: .]
      --json
  -h, --help           Print help
```

### `orc run show`

```text
Usage: show [OPTIONS] <ID>

Arguments:
  <ID>

Options:
      --scope <SCOPE>  [env: ORC_SCOPE=] [default: .]
      --json
  -h, --help           Print help
```

### `orc run update`

```text
Usage: update [OPTIONS] --status <STATUS> <ID>

Arguments:
  <ID>

Options:
      --scope <SCOPE>    [env: ORC_SCOPE=] [default: .]
      --status <STATUS>
  -h, --help             Print help
```

### `orc run resume`

```text
Usage: resume [OPTIONS] <ID>

Arguments:
  <ID>

Options:
      --scope <SCOPE>  [env: ORC_SCOPE=] [default: .]
  -h, --help           Print help
```

### `orc run approve`

```text
Usage: approve [OPTIONS] <ID>

Arguments:
  <ID>

Options:
      --scope <SCOPE>  [env: ORC_SCOPE=] [default: .]
      --gate <GATE>
      --no-resume
  -h, --help           Print help
```

### `orc run cancel`

```text
Usage: cancel [OPTIONS] <ID>

Arguments:
  <ID>

Options:
      --scope <SCOPE>  [env: ORC_SCOPE=] [default: .]
  -h, --help           Print help
```

### `orc node`

Manage workflow nodes

```text
Manage workflow nodes

Usage: node <COMMAND>

Commands:
  upsert
  update
  help    Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `orc node upsert`

```text
Usage: upsert [OPTIONS] --run <RUN_ID> <ID>

Arguments:
  <ID>

Options:
      --scope <SCOPE>                      [env: ORC_SCOPE=] [default: .]
      --run <RUN_ID>
      --harness <HARNESS>                  [default: unknown]
      --model <MODEL>
      --role <ROLE>                        [default: worker]
      --title <TITLE>                      [default: "Agent session"]
      --purpose <PURPOSE>                  [default: "Agent session"]
      --goal <GOAL>                        [default: "Complete the assigned work"]
      --expected-output <EXPECTED_OUTPUT>  [default: "A verified result"]
      --success <SUCCESS_CRITERIA>
      --completion <COMPLETION>            [default: orchestrator]
      --review-by <REVIEW_BY>
      --session <SESSION_ID>
      --status <STATUS>                    [default: queued]
      --attempt <ATTEMPT>                  [default: 0]
      --depends-on <DEPENDS_ON>
  -h, --help                               Print help
```

### `orc node update`

```text
Usage: update [OPTIONS] --run <RUN_ID> --status <STATUS> <ID>

Arguments:
  <ID>

Options:
      --scope <SCOPE>    [env: ORC_SCOPE=] [default: .]
      --run <RUN_ID>
      --status <STATUS>
  -h, --help             Print help
```

### `orc provider`

Manage provider manifests

```text
Manage provider manifests

Usage: provider <COMMAND>

Commands:
  list
  validate
  help      Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `orc provider list`

```text
Usage: list [OPTIONS]

Options:
      --scope <SCOPE>  [env: ORC_SCOPE=] [default: .]
      --json
  -h, --help           Print help
```

### `orc provider validate`

```text
Usage: validate [OPTIONS] [NAME]

Arguments:
  [NAME]

Options:
      --scope <SCOPE>  [env: ORC_SCOPE=] [default: .]
      --json
  -h, --help           Print help
```

### `orc workflow`

Manage versioned workflow definitions

```text
Manage versioned workflow definitions

Usage: workflow <COMMAND>

Commands:
  init
  import
  list
  show
  edit
  validate
  plan
  start
  history
  search
  path
  help      Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `orc workflow init`

```text
Usage: init [OPTIONS] <NAME>

Arguments:
  <NAME>

Options:
      --scope <SCOPE>  [env: ORC_SCOPE=] [default: .]
  -h, --help           Print help
```

### `orc workflow import`

```text
Usage: import [OPTIONS] <PATH>

Arguments:
  <PATH>

Options:
      --scope <SCOPE>  [env: ORC_SCOPE=] [default: .]
  -h, --help           Print help
```

### `orc workflow list`

```text
Usage: list [OPTIONS]

Options:
      --scope <SCOPE>  [env: ORC_SCOPE=] [default: .]
  -h, --help           Print help
```

### `orc workflow show`

```text
Usage: show [OPTIONS] <NAME>

Arguments:
  <NAME>

Options:
      --scope <SCOPE>  [env: ORC_SCOPE=] [default: .]
  -h, --help           Print help
```

### `orc workflow edit`

```text
Usage: edit [OPTIONS] <NAME>

Arguments:
  <NAME>

Options:
      --scope <SCOPE>  [env: ORC_SCOPE=] [default: .]
  -h, --help           Print help
```

### `orc workflow validate`

```text
Usage: validate <WORKFLOW>

Arguments:
  <WORKFLOW>

Options:
  -h, --help  Print help
```

### `orc workflow plan`

```text
Usage: plan [OPTIONS] <WORKFLOW>

Arguments:
  <WORKFLOW>

Options:
      --scope <SCOPE>  [env: ORC_SCOPE=] [default: .]
      --json
  -h, --help           Print help
```

### `orc workflow start`

```text
Usage: start [OPTIONS] <WORKFLOW>

Arguments:
  <WORKFLOW>

Options:
      --scope <SCOPE>  [env: ORC_SCOPE=] [default: .]
      --background
      --json
  -h, --help           Print help
```

### `orc workflow history`

```text
Usage: history [OPTIONS] <NAME>

Arguments:
  <NAME>

Options:
      --scope <SCOPE>  [env: ORC_SCOPE=] [default: .]
  -h, --help           Print help
```

### `orc workflow search`

```text
Usage: search <QUERY>

Arguments:
  <QUERY>

Options:
  -h, --help  Print help
```

### `orc workflow path`

```text
Usage: path [OPTIONS] <NAME>

Arguments:
  <NAME>

Options:
      --scope <SCOPE>  [env: ORC_SCOPE=] [default: .]
  -h, --help           Print help
```

### `orc launch`

Launch a managed harness

```text
Launch a managed harness

Usage: launch [OPTIONS] <HARNESS> [-- <ARGS>...]

Arguments:
  <HARNESS>
  [ARGS]...

Options:
      --scope <SCOPE>      [env: ORC_SCOPE=] [default: .]
      --model <MODEL>
      --managed <MANAGED>
  -h, --help               Print help
```

### `orc attach`

Attach through a provider chain

```text
Attach through a provider chain

Usage: attach [OPTIONS] <ID>

Arguments:
  <ID>

Options:
      --scope <SCOPE>          [env: ORC_SCOPE=] [default: .]
      --direction <DIRECTION>  [default: right] [possible values: right, left, top, bottom]
  -h, --help                   Print help
```

### `orc inspect`

Inspect through a provider chain

```text
Inspect through a provider chain

Usage: inspect [OPTIONS] <ID>

Arguments:
  <ID>

Options:
      --scope <SCOPE>          [env: ORC_SCOPE=] [default: .]
      --direction <DIRECTION>  [default: right] [possible values: right, left, top, bottom]
  -h, --help                   Print help
```

### `orc disconnect`

Disconnect a registered session

```text
Disconnect a registered session

Usage: disconnect [OPTIONS] [ID]

Arguments:
  [ID]

Options:
      --scope <SCOPE>  [env: ORC_SCOPE=] [default: .]
  -h, --help           Print help
```

### `orc guide`

Send mid-run guidance

```text
Send mid-run guidance

Usage: guide [OPTIONS] --text <TEXT> <ID>

Arguments:
  <ID>

Options:
      --scope <SCOPE>  [env: ORC_SCOPE=] [default: .]
      --text <TEXT>
  -h, --help           Print help
```

### `orc mcp`

Run the MCP server

```text
Run the MCP server

Usage: mcp

Options:
  -h, --help  Print help
```

### `orc prompt`

Print the prompt session marker

```text
Print the prompt session marker

Usage: prompt [OPTIONS]

Options:
      --scope <SCOPE>  [env: ORC_SCOPE=] [default: .]
  -h, --help           Print help
```

### `orc completion`

Generate shell completions

```text
Generate shell completions

Usage: completion <SHELL>

Arguments:
  <SHELL>  [possible values: bash, zsh, fish, nu]

Options:
  -h, --help  Print help
```

### `orc schema`

Print generated JSON schemas

```text
Print generated JSON schemas

Usage: schema <SCHEMA>

Arguments:
  <SCHEMA>  [possible values: config, provider, workflow, state]

Options:
  -h, --help  Print help
```


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
