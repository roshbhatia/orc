# changes

Prepare a diff inspection for the token parser repair.

## Install

```sh
brew install roshbhatia/tap/orc-provider-changes
nix profile add 'github:roshbhatia/orc?dir=extras#provider-changes'
```

Install the core utility separately, or select its all-provider bundle. Runtime tools still need their own credentials.

## Demo

![Prepare a diff inspection for the token parser repair](demo.gif)

[Tape source](demo.tape) · [Task script](demo.sh)

Run `nix develop -c bash extras/changes/demo.sh` to run the task without recording.
Run `nix develop -c python3 hack/extra-demos.py changes` to record it.
