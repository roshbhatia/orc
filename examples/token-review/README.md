# Token parser review

Plan an authorization-header repair with research, implementation, and verification tasks.
Research identifies prefix and empty-token failures. Verification can send findings back to implementation.

Run `./run.sh` from the repository that owns the repair. The script creates Orc task state and starts no agent.
Harness hooks can bind running sessions to the tasks.

![Token parser review](demo.gif)

[Tape source](demo.tape)

The recording uses isolated task state and an offline workflow fixture.
Its local token-parser regression tests run before recording.
Run `nix develop -c bash hack/screenshots.sh` to regenerate it.
