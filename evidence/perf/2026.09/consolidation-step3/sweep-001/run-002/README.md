# Stage 3 workspace test, run 2

Source `bead5288fca0ad4c642aecf12d7fe3e0d3d00f5f` returned 101 after 137.013 seconds.
The [full classification](classification-002/report.md) lists every one of the 160 failures and 84 explicit skips.
The capture recorded 28,506 tracked files outside Beads with unchanged bytes, modes, and HEAD.

The integration coordinator confirmed that this capture used the restricted tool sandbox.
It denied local sockets in 59 previously passing cases and denied Helm's DNS socket in another existing failure.
The actual failed result remains unchanged. The [host execution result](../run-003/README.md) records the later run separately.

The first detached controller did not create a run directory or produce output. Its [empty log](../controller-002.log) remains.
The supervised capture produced the [run record](run.json), [command](capture-command.json), and [source comparison](source-stability.json).
No earlier sweep result was overwritten.

The retained reducer and finalizer both returned zero.
The first source-review attempt searched `command.rs` for a message owned by `environment.rs` and failed.
Its [command and error](review-attempt-001/review-command.json) remain alongside the [successful review](review-command.json).
The full classification has no unresolved parser entries or causes. It does not change the test exit code.
