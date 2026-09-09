# traces

Read the repair session activity.

## Install

```sh
brew install roshbhatia/tap/orc-provider-traces
nix profile add 'github:roshbhatia/orc?dir=extras#provider-traces'
```

Install the core utility separately, or select its all-provider bundle. Runtime tools still need their own credentials.

## Demo

![Read the repair session activity](demo.gif)

[Tape source](demo.tape) · [Task script](demo.sh)

Run `nix develop -c bash extras/traces/demo.sh` to run the task without recording.
Run `nix develop -c python3 hack/extra-demos.py traces` to record it.
