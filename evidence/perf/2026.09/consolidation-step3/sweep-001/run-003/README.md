# Stage 3 workspace test, run 3

Source `60adfdd3902ec26070d98296071605543f857c0b` returned 101 after 121.916 seconds.
The [full classification](classification-002/report.md) lists every one of the 101 failures and 84 explicit skips.
The capture recorded 28,506 tracked files outside Beads with unchanged bytes, modes, and HEAD.

This capture used the host execution context and the same retained Cargo command.
Only the WMS README changed since run 2. All 59 [cases that encountered denied sockets](classification-002/socket-refusals-now-passed.json) now passed.
The chart test again reached its previous Kubernetes discovery refusal.

All remaining failures match run 1 by identity and normalized cause.
The three corrected cases and all 34 moved app cases passed.
The result retains 2,338 reported passes, including the 84 explicit skips, and six passing doctests.
Skipped work did not run. This failed sweep does not establish application or wave completion.

The reducer, finalizer, and source review returned zero. No unresolved parser entries or cause reviews remain.
The [run record](run.json), [command](capture-command.json), [source comparison](source-stability.json), and [earlier restricted run](../run-002/README.md) remain separate.
