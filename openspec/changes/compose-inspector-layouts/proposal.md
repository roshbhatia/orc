> The keywords MUST, MUST NOT, SHOULD, SHOULD NOT, and MAY in this document are to be interpreted as described in [RFC 2119](https://datatracker.ietf.org/doc/html/rfc2119).

## Why

The inspector already supports four edges, but its controls do not form one stable layout system. Hiding loses the prior edge, fixed size caps waste larger terminals, mouse dragging is absent, and terminal resize can replace a manual graph viewport with a fit.

## What Changes

- Treat the inspector edge, visibility, size, active tab, and last visible edge as one per-workspace preference.
- Rotate the visible inspector clockwise, dock it directly, hide and restore it, and switch tabs without resetting inspector state.
- Resize the focused inspector from the keyboard or by dragging the divider on any edge.
- Make spatial pane focus follow the inspector edge.
- Keep a usable minimum for each pane without fixed maximum row or column caps.
- Preserve a manual graph viewport across terminal resize while continuing to refit a graph in fit mode.
- Generate key and command help from the same action definitions used for dispatch.

### Non-goals

- This change MUST NOT alter workflow topology, graph layout, provider contracts, or session state.
- This change MUST NOT persist transcript content or inspector scroll offsets.
- This change MUST NOT add a second preferences store.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `terminal-dashboard`: Define stable inspector layout, resize, focus, persistence, and graph viewport behavior.

## Behavior

Must do:
- Rotate and directly dock the inspector without changing its selection, tab, focus, or scroll, decided by input-state tests.
- Hide and restore the last visible edge and size, decided by persisted-preference tests.
- Resize every edge by key and divider drag while retaining usable pane minima, decided by TestBackend geometry tests.
- Route pane focus according to the visible edge, decided by all-edge key tests.
- Preserve manual graph pan and zoom on terminal resize and refit only fit-mode graphs, decided by resize event tests.

Must still hold:
- Small frames hide the inspector only for that render and do not rewrite workspace preferences.
- Footer, popup, leader, and command help use the declared binding and command tables.
- Existing preference files deserialize without migration work.

Human-owned decision:
- The owner confirms that the four-edge controls and divider affordance are understandable in a live terminal.

## Impact

Modified code:
- `src/tui.rs` owns layout actions, input, hit testing, rendering, and interaction tests.
- `src/preferences.rs` stores the additional workspace-local layout fields.
- `README.md` documents the generated interaction language.

No external API or provider contract changes. Existing preference files continue to deserialize through defaults.
