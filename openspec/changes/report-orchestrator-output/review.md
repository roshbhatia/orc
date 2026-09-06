# Planning Review

The adversarial reviewer found six material contract defects. The independent mediator accepted four and reframed two. The revised artifacts now:

- preserve explicit JSON `null` through a reported-output envelope;
- accept large CLI values through a file or standard input;
- bound TUI rendering with an Output-specific preview and full-value command;
- keep output with the archived reporting session during root replacement;
- reject reports from terminating sessions; and
- state the destructive rollback constraint and required export.

## Decision

Approved for implementation after the six corrections. Implementation review remains required by tasks 1.3, 2.3, and 3.2.

## Implementation Mediation

### Round 1

- ACCEPT: add an exact non-interactive read for retained inactive sessions.
- ACCEPT: include the selected session and resolved scope in ordinary truncated-output recovery commands.
- REFRAME: treat `ORC_SESSION_ID` as trusted routing context. Role and lifecycle checks prevent cooperative or accidental misuse, while spoof resistance requires sandboxed execution.

### Round 2

- ACCEPT: put option-like session identifiers after a `--` terminator in generated recovery commands.
- REJECT: do not remove native-incarnation remapping. It provides intentional continuity when an archived harness incarnation resumes through its active replacement.

### Round 3

- REJECT: repeated objections to native-incarnation continuity add no new defect.
- REFRAME: a very large session identifier can make the exact recovery command exceed the generic inspector budget. Use the bounded session-list read when the exact command does not fit, and keep Activity truncation labels out of Output.

The focused CLI and TUI regressions cover exact inactive reads, option-like identifiers, shell-safe scopes, the ordinary exact command, and the bounded list fallback.

### Round 4

- ACCEPT: stream pretty JSON into a byte-and-line capped inspector writer. Compact input size does not bound pretty-print amplification.
- ACCEPT: return a fixed-size report receipt from CLI and MCP instead of the complete updated session.
- DEFER: workspace state still uses complete pretty serialization. Track that persistence concern as a separate follow-up because changing the on-disk writer is outside this output-report patch.

### Round 5

The state, interface, and TUI critics returned CLEAN. The independent mediator
also returned CLEAN after checking the focused tests, strict specification
review, and diff integrity. The deferred workspace-state persistence concern
remains task 3.4 and does not block this change.
