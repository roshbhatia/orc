## 1. Persisted layout model

- **SHAPE** graph
- MERGE 1.3

- [x] 1.1 Add defaulted visible, last-dock, and graph viewport mode preferences `deps:` none `writes:` src/preferences.rs,src/tui.rs
- [x] 1.2 Centralize rotate, direct dock, hide, restore, resize, and spatial focus transitions `deps:` 1.1 `writes:` src/tui.rs
- [x] 1.3 Add generated key and command actions for the complete layout model `deps:` 1.2 `writes:` src/tui.rs,README.md
- [x] 1.4 Adversarial review: verify preference migration and state transitions `deps:` 1.3 `writes:` openspec/changes/compose-inspector-layouts/review.md

## 2. Responsive render and mouse control

- **SHAPE** graph
- MERGE 2.3

- [x] 2.1 Replace fixed pane maxima with percentage sizes and usable dimension minima `deps:` 1.1 `writes:` src/tui.rs
- [x] 2.2 Add divider geometry and captured drag handling for every edge `deps:` 2.1 `writes:` src/tui.rs
- [x] 2.3 Preserve manual graph pan and zoom across terminal resize `deps:` 1.1,2.1 `writes:` src/tui.rs
- [x] 2.4 Adversarial review: verify geometry, focus, and resize behavior `deps:` 2.3 `writes:` openspec/changes/compose-inspector-layouts/review.md

## 3. Verification and review

- **SHAPE** loop
- **STOP** `cargo test --all-targets`, `openspec validate compose-inspector-layouts --strict`, generated-file checks, and both Nix checks exit 0
- **MAX-ITERS** 3

- [x] 3.1 Add TestBackend and PTY coverage for keys, commands, mouse drag, focus, persistence, compact frames, and terminal resize `deps:` 1.3,2.3 `writes:` src/tui.rs,tests
- [x] 3.2 Run formatting, Clippy with warnings denied, all tests, generation, strict OpenSpec and specutil checks, and both Nix flakes `deps:` 3.1
- [x] 3.3 Run two independent critics and one mediator, then fix only accepted or reframed blockers `deps:` 3.2 `writes:` openspec/changes/compose-inspector-layouts/review.md
- [x] 3.4 Adversarial review: record the mediated terminal state and call diff `deps:` 3.3 `writes:` openspec/changes/compose-inspector-layouts/review.md
