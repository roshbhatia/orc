# github-aw

Track a real factory task through GitHub Actions.

## Install

```sh
brew install roshbhatia/tap/orc-provider-github-aw
nix profile add 'github:roshbhatia/orc?dir=extras#provider-github-aw'
```

Install the core utility separately, or select its all-provider bundle. Runtime tools still need their own credentials.

See [usage and dispatch receipts](usage.md). Set `ORC_FACTORY_SCOPE` to the scope containing `factory-docs-credentials`. Set `ORC_FACTORY_RUN_URL` to its run URL and `ORC_FACTORY_PR_URL` to its draft PR URL. The recording observes existing resources without dispatching another task.

## Demo

![Track a real factory task through GitHub Actions](demo.gif)

[Tape source](demo.tape) · [Task script](demo.sh)

Run `nix develop -c bash extras/github-aw/demo.sh` to run the task without recording.
Run `nix develop -c uv run --script hack/extra-demos.py github-aw` to record it.
