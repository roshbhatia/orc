# Review

The adversarial reviewer found six material contract defects. The independent mediator accepted four and reframed two. The revised artifacts now:

- preserve explicit JSON `null` through a reported-output envelope;
- accept large CLI values through a file or standard input;
- bound TUI rendering with an Output-specific preview and full-value command;
- keep output with the archived reporting session during root replacement;
- reject reports from terminating sessions; and
- state the destructive rollback constraint and required export.

## Decision

The structured checkpoint contract and provider-backed assistant Output implementation are approved after mediated corrections. Release remains pending task 4.3.

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

### Round 6

The Output semantics reviewer found three concrete defects:

- a stray escape before UTF-8 could panic the sanitizer and leave Output loading forever;
- generic inspector truncation used an Activity label for Output; and
- the earlier live-orchestration delta still assigned structured JSON to Output.

The implementation now sanitizes without panics, uses inspector-specific omission labels, and assigns structured data to Checkpoint in both active changes.

The live-behavior reviewer found that Output inherited Activity's active-session filter. A completed run could show an Output tab but could not read its archived orchestrator. Output now has a separate subject resolver that supports archived sessions while Activity retains its active-only policy.

Both independent reviewers returned CLEAN after the corrections. They verified provider neutrality, exact Traces command compatibility, session and workflow selection, Activity and Checkpoint isolation, loading and error retention, scroll preservation, generated interfaces, and complete OpenSpec artifacts. Native tests ran without an Orc runtime or broker.

### Round 7

A live foundation review found two receipt-boundary defects. The binding parser accepted an old protocol version, and a rejected receipt command could print its reserved stdout. Orc now validates the receipt envelope before the binding and suppresses receipt stdout for every exit status. Focused regressions cover both failures.

The reviewer returned CLEAN after rechecking Output tail following, scrolled-up preservation, error retention, provider ownership, accepted-exit-only persistence, and first-open/second-focus behavior. All checks remained native and isolated from the Orc runtime and broker.
