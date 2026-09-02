# Orc

![Orc workflow graph](docs/orc.png)

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

## Providers

Orc discovers manifests from `~/.config/orc/providers/*.json`. Set
`ORC_PROVIDER_DIR` to use another directory.

Each provider advertises one kind and a set of capabilities:

```json
{
  "version": "orc.provider/v1",
  "name": "wezterm",
  "kind": "display",
  "command": "/path/to/orc-provider-wezterm",
  "capabilities": ["session.bind", "terminal.open"],
  "priority": 100
}
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

Without providers, Orc still records sessions, workflows, nodes, and contracts.
Orc requires no broker process.

## Develop Orc

```bash
nix develop --accept-flake-config
bun install --frozen-lockfile
bun run check
nix build --accept-flake-config
nix flake check --accept-flake-config
./hack/screenshots.sh
```

Run `bun run nix:lock` after a dependency change.
