# Orc

Orc is a local control plane for agent harnesses. It records session contracts,
shows their tree, and delegates host actions through an optional provider.

Orc uses the same TypeScript stack as OpenCode: Bun, Effect, TypeScript Native
Preview, Solid, and OpenTUI.

## Use Orc

Open the dashboard for the current repository:

```bash
orc
```

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

Register a child session with `--parent <orc-session-id>`. Add
`--provider-ref <id>` when a provider owns the harness process.

```bash
orc status --json
orc list --json
orc attach <orc-session-id> --direction right
orc inspect <orc-session-id> --direction right
orc disconnect <orc-session-id>
```

Orc discovers external providers from `$XDG_CONFIG_HOME/orc/providers/*.json`.
Each manifest advertises capabilities and one command:

```json
{
  "version": "orc.provider/v1",
  "name": "terminal",
  "command": "/path/to/orc-provider-terminal",
  "capabilities": ["terminal.open"],
  "priority": 10
}
```

The command can use an absolute path or a name on `PATH`. Orc selects the
highest-priority provider for each capability. Equal top priorities produce an
ambiguity error.

Actions resolve to capability chains:

```text
attach   session.attach -> terminal.open
inspect  session.inspect -> terminal.open
changes  changes.inspect
launch   session.launch
```

Orc writes one `orc.provider/v1` request to each provider's standard input.
The request includes the capability and the prior command plan. A provider
writes the next command plan to standard output:

```json
{
  "version": "orc.provider/v1",
  "command": ["session-tool", "attach", "session-id"],
  "cwd": "/work/project",
  "environment": {}
}
```

The final plan runs with inherited terminal state. Captured actions preserve
standard output, standard error, and terminal colors. `ORC_PROVIDER_DIR` selects
another manifest directory for tests or temporary configurations.

Without a provider, Orc still records sessions, runs, nodes, and contracts.
Direct `orc launch` also works. Use `--managed <id>` to delegate a launch.

Orc becomes active when the first session connects. It becomes idle when the
last active session disconnects. Orc does not require a broker process.

The dashboard generates its footer and help view from the same bindings that
handle input. A binding change updates both views.

## Develop Orc

```bash
nix develop --accept-flake-config
bun install --frozen-lockfile
bun run check
nix build --accept-flake-config
nix flake check --accept-flake-config
```

Run `bun run nix:lock` after a dependency change.
