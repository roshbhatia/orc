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

Orc routes each host action to an external provider. Configure the routes in
`$XDG_CONFIG_HOME/orc/providers.json`:

```json
{
  "providers": {
    "attach": "sysinit",
    "inspect": "sysinit",
    "changes": "sysinit",
    "launch": "sysinit"
  }
}
```

A provider name resolves to `orc-<name>` on `PATH`. The same executable can
serve several routes and compose other commands. Orc passes `attach`, `inspect`,
`changes`, or `launch` as the first argument. The provider reads one
`orc.provider/v1` JSON request from `ORC_PROVIDER_REQUEST`.

Orc selects one provider for each action. A recipe provider owns command
composition. This lets terminal actions retain their process and TTY behavior.

`ORC_PROVIDER_<ACTION>` overrides one route. `ORC_PROVIDER` supplies a common
fallback. `ORC_PROVIDER_CONFIG` selects another config file. A provider reports
success through its exit status. The `changes` action writes display output to
standard output.

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
