# local

Prepare the token parser test command.

## Install

```sh
brew install roshbhatia/tap/orc-provider-local
nix profile add 'github:roshbhatia/orc?dir=extras#provider-local'
```

Install the core utility separately, or select its all-provider bundle. Runtime tools still need their own credentials.

## Demo

![Prepare the token parser test command](demo.gif)

[Tape source](demo.tape) · [Task script](demo.sh)

Run `nix develop -c bash extras/local/demo.sh` to run the task without recording.
Run `nix develop -c python3 hack/extra-demos.py local` to record it.
