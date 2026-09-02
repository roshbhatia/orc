# Orc extras

Orc core knows only provider capabilities and the JSON protocol. This directory
contains optional adapters for common local tools.

- `orc-provider-harness` reads an external harness registry and creates resume
  command plans.
- `orc-provider-zmx` adds persistence.
- `orc-provider-wezterm` adds display panes and keeps failed commands visible.
- `orc-provider-traces` adds session descriptions and activity.
- `orc-provider-changes` adds repository changes.

Each adapter discovers its external command through `PATH`. A missing command
returns a provider decline instead of breaking Orc. Consumers install only the
adapters they use and write YAML or JSON manifests under
`~/.config/orc/providers/`. Use `orc provider validate` to test every adapter
without opening the dashboard or launching a harness.

Ready-to-install manifests live in [`manifests/`](manifests/). The combined
extras package installs them under `share/orc/providers` so a system manager
can link selected manifests into the user configuration.
