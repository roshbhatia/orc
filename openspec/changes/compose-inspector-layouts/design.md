## Context

`App` already routes keyboard, mouse, and rendering through `src/tui.rs`. `WorkspacePreferences` already persists one dock string, one size percentage, and the active tab under a scope-keyed XDG state path. The current render path can hide an inspector for a small frame, but it also uses fixed 18-row and 60-column maxima. Terminal resize always requests graph fit before the UI can distinguish a fitted viewport from a user-controlled one.

## Goals / Non-Goals

**Goals:**
- Make every inspector layout action use one state transition and one persistence path.
- Match the existing Traces concepts: a requested edge, an effective small-frame edge, a last visible edge, percentage sizing, and a draggable divider.
- Keep graph fit as a deliberate viewport mode instead of an incidental resize side effect.

**Non-Goals:**
- Do not change graph topology or the provider-backed inspector contents.
- Do not store transient focus or scroll state across process restarts.
- Do not change terminal multiplexing or WezTerm configuration in this repository.

## Decisions

### Keep requested layout separate from effective layout

- Decision: Persist the requested edge, visibility, size, and last visible edge. Compute effective visibility from the current frame without rewriting preferences.
  - Alternative rejected: Writing `hidden` during a narrow render loses the user's requested layout and makes a later resize non-reversible.

`Dock::Hidden` remains the internal requested hidden state for minimal churn. `last_visible_dock` records the restore target. `split_body` returns no inspector when the requested layout cannot satisfy both pane minima, but it does not change either field.

### Use percentage sizing with dimension minima

- Decision: Keep one inspector percentage across orientations and clamp the resulting cell size against edge-specific pane minima.
  - Alternative rejected: Fixed maximum rows and columns waste larger terminals and make the same percentage mean different things after rotation.

Stacked panes reserve at least 8 rows for each side. Side-by-side panes reserve at least 32 columns for the inspector and 40 for the main pane. The stored percentage remains within 20 through 80.

### Make the visible main border the draggable divider

- Decision: Use the main pane border adjacent to the inspector as the divider hit area, and keep the inspector border available for its tabs.
  - Alternative rejected: Using the inspector's top border for a bottom dock makes tab selection and resize compete for the same cells.

A pressed divider captures subsequent left-button drag events until release, even outside the original hit row or column. Persistence occurs when the drag ends.

### Derive pane focus from the edge

- Decision: Route `Ctrl-h/j/k/l` and `Ctrl-w h/j/k/l` through one spatial focus function.
  - Alternative rejected: Fixed `Ctrl-j/l` and `Ctrl-k/h` pairs cross the wrong edge after rotation.

### Persist explicit graph viewport mode

- Decision: Persist `fit` or `manual`. Zoom and pan enter manual mode; explicit fit and graph relayout enter fit mode. Terminal resize refits only in fit mode.
  - Alternative rejected: Inferring mode from zoom or offsets cannot distinguish a manual reset from a fit result.

### Keep help declarative

- Decision: Extend the existing binding and command tables, and generate the footer, popup, leader hint, and command completion from them.
  - Alternative rejected: Inline status strings drift from the actions they document.

## Risks / Trade-offs

- A one-cell divider is a small mouse target. The persistent keyboard controls remain equivalent.
- One percentage across orientations can change the absolute pane size after rotation. The stable ratio is more predictable than silently maintaining separate hidden values.
- Old preference files do not carry an explicit last edge. Deserialization defaults to bottom, while a valid visible legacy dock becomes the last edge during application.

## Migration Plan

1. Add defaulted preference fields and apply legacy dock semantics.
2. Centralize dock, visibility, rotate, resize, and focus transitions.
3. Add divider geometry and captured drag handling.
4. Preserve fit versus manual mode across terminal resize.
5. Validate TestBackend behavior, a PTY input sequence, generated artifacts, OpenSpec, Rust, and Nix checks.

Rollback can remove the new behavior while leaving the added JSON fields ignored by older readers.

## Rollout & Gating

The complete Rust suite, PTY interaction fixture, generated-file check, strict OpenSpec validation, specutil lint, and both Nix flakes MUST pass before merge. A live terminal spot-check remains model evidence, not owner approval. Reverting the layout commit is the runtime rollback.

## Adversarial Review

Two independent read-only critics MUST review the named revision for state correctness and terminal interaction. A third independent mediator MUST reject nits and scope expansion, and MUST leave no accepted or reframed blocker before merge.
