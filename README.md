# Orc

Orc is a local control plane for agent harnesses. It records session contracts,
shows their tree, and opens native harness sessions through existing terminal
tools.

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

Register a child session with `--parent <orc-session-id>`. Add `--zmx
<session-name>` when ZMX owns the harness process.

```bash
orc status --json
orc list --json
orc attach <orc-session-id> --direction right
orc traces <orc-session-id> --direction right
orc disconnect <orc-session-id>
```

Orc becomes active when the first session connects. It becomes idle when the
last active session disconnects. Orc does not require a broker process.

## Dashboard keys

```text
j, down       select next session
k, up         select previous session
tab           switch Explorer and Runs
enter         attach through ZMX in a WezTerm split
t             open Traces in a WezTerm split
q, escape     quit
```

## Develop Orc

```bash
nix develop --accept-flake-config
bun install --frozen-lockfile
bun run check
nix build --accept-flake-config
nix flake check --accept-flake-config
```

Run `bun run nix:lock` after a dependency change.
