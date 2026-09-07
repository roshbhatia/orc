## Rubric

The review bound the proposal's behavior and non-goals, the design decisions,
and the rollout gates. In particular, it tested stable layout state, usable pane
minima, manual graph viewport preservation, declarative help, and compatibility
with existing preference files. The change affects one terminal-dashboard
capability, so its review cap was four rounds.

## Selection

Adversarial review was required by the design and task 3.3. Deterministic
`specutil check` passed before the model review. Two independent read-only
critics reviewed the uncommitted working tree against base `df776ad`, including
the `calldiff diff df776ad --max-depth 3` call paths. An independent read-only
mediator adjudicated every objection.

## Round 1

Mediation returned 3 ACCEPT, 0 REFRAME, 0 REJECT, and 0 DEFER verdicts.

1. A terminal at least 1,000 columns wide overflowed `u16` percentage math.
   Divider drag and rendering could produce 65 percent instead of 80 percent.
   This violated percentage sizing and all-edge resize behavior.
2. At 80 by 24, rotating a graph inspector from right to top met both declared
   pane minima, but a graph-only full-body fallback still hid the inspector and
   moved focus. This violated stable rotate state and usable pane minima.
3. Help input allowed scrolling to `row_count - 1`, while rendering clamped to
   `row_count - visible_rows`. A user could overscroll, then press Up without a
   visible response. This violated declarative, usable generated help.

## Revisions

- Widened divider and render percentage calculations to `u32`. A 1,000-column
  regression now verifies that 80 percent renders as 800 columns.
- Removed the graph-only full-body override. Declared pane minima now decide
  whether a split is usable. An 80 by 24 right-to-top rotation regression keeps
  the inspector visible and focused.
- Added one viewport-aware help scroll maximum shared by keyboard input, mouse
  input, and rendering. The compact help regression verifies its lower bound,
  immediate reverse scrolling, and access to every layout binding.

The independent mediator re-read the current files and ran the three focused
regressions. It marked all three accepted objections resolved and found no open
reframed or deferred issue. No critic or mediator changed the working tree.

## Terminal state

No surviving objection. This is model evidence, not owner approval. The live
terminal usability decision remains owner-owned.
