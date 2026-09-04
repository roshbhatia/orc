# Orc extras

Orc core knows only provider capabilities and the JSON protocol. This directory
contains optional adapters for common local tools.

- `orc-provider-harness` reads an external harness registry and creates resume
  and launch command plans.
- `orc-provider-local` executes command plans on the current machine.
- `orc-provider-zmx` adds persistence.
- `orc-provider-wezterm` adds display panes and keeps failed commands visible.
- `orc-provider-traces` adds session descriptions and activity.
- `orc-provider-changes` adds repository changes.

Each provider directory owns three files: `default.nix`, `provider.yaml`, and
`provider.sh`. The Nix file declares only that provider's runtime packages. The
manifest declares its protocol capabilities and required commands. The script
implements those capabilities. Shared protocol response helpers live under
`lib/`; they do not select providers or external products.

Each provider package contains its ready-to-install manifest under
`share/orc/providers` and exposes any fixed command dependency. A missing
dynamic harness command returns a provider decline instead of breaking Orc.
Consumers can install one `provider-*` output, the default bundle with every
provider, or `full` with core and every provider. The extras flake owns all
external product inputs. The root Orc flake remains provider-neutral. Use
`orc provider validate` to test adapters without opening the dashboard or
launching a harness. Each packaged adapter supplies its own Bash and command
dependencies. Validation therefore works under a clean process environment;
the parent Orc process does not need provider commands on its `PATH`.
