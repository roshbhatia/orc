# harness

Prepare the token parser review session.

## Install

```sh
brew install roshbhatia/tap/orc-provider-harness
nix profile add 'github:roshbhatia/orc?dir=extras#provider-harness'
```

Install the core utility separately, or select its all-provider bundle. Runtime tools still need their own credentials.

## Demo

![Prepare the token parser review session](demo.gif)

[Tape source](demo.tape) · [Task script](demo.sh)

Run `nix develop -c bash extras/harness/demo.sh` to run the task without recording.
Run `nix develop -c python3 hack/extra-demos.py harness` to record it.
