# ts-cli-template

A Nix-first template for TypeScript command-line tools built with Bun and
Effect.

The example keeps stdout machine-readable, models argument failures as Effect
errors, and consumes shared terminal vocabulary from `ts-utils`.

## Start a project

Create a repository from this template. Then replace the package and binary
names.

```bash
rg -l 'example|ts-cli-template' | xargs sed -i '' -e 's/example/PROJECT/g' -e 's/ts-cli-template/PROJECT/g'
bun install
bun run nix:lock
```

## Development

```bash
nix develop --accept-flake-config
bun install --frozen-lockfile
bun run check
nix build --accept-flake-config
nix flake check --accept-flake-config
```

## Example

```bash
nix run . -- Roshan
nix run . -- --json Roshan
```

Run `bun run nix:lock` after a dependency change.
