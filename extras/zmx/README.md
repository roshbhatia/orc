# zmx

Prepare a persistent repair session.

## Install

```sh
brew install roshbhatia/tap/orc-provider-zmx
nix profile add 'github:roshbhatia/orc?dir=extras#provider-zmx'
```

Install the core utility separately, or select its all-provider bundle. Runtime tools still need their own credentials.

## Demo

![Prepare a persistent repair session](demo.gif)

[Tape source](demo.tape) · [Task script](demo.sh)

Run `nix develop -c bash extras/zmx/demo.sh` to run the task without recording.
Run `nix develop -c python3 hack/extra-demos.py zmx` to record it.
