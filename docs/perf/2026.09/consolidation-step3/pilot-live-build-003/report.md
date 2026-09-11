# Pilot driver build results

These records preserve all three native driver build attempts for `wamn-47wm.4.3`.

| Attempt | Source | Exit | Seconds | Result |
| --- | --- | ---: | ---: | --- |
| [001](../pilot-live-build-001/record.json) | `f35902674b3915ba64bcb424063e46dcd5b76945` | 101 | 0.28795714100124314 | The selected `wamn-proof-integration` package has no `wamn-gates` binary. |
| [002](../pilot-live-build-002/record.json) | `f35902674b3915ba64bcb424063e46dcd5b76945` | 0 | 7.746488245960791 | The correct `wamn-gates` package built. |
| [003](record.json) | `1925bea2ccacb28fc6cf768dd02c5a760b62aca3` | 0 | 3.790903319953941 | The driver built with the scoped cleanup correction. |

The successful commands used Rust 1.98.0, the debug profile, two jobs, an empty `RUSTC_WRAPPER`, and `--locked --offline`; their original records preserve the exact arguments, times, binary sizes and hashes, while [publication.json](publication.json) records byte and file-mode comparisons for every copied input.

| Attempt | Binary bytes | Binary SHA-256 |
| --- | ---: | --- |
| 002 | 281959432 | `9995134ec169108cc50695fc68fd5133a7c42b4d59758144a32ee98d09661eaf` |
| 003 | 281957520 | `13f809d973d4018bc841ae71ac9a846889f3de741f806d013a2fcfe458e4e250` |

At source `1925bea2`, [process.rs](../../../../../tests/integration/src/agent_pilot/runner/process.rs) lines 259–260 read the recorded agent and lines 294–296 select the local stub driver, whose [completed arm](../../../../../tools/agent-pilot-stub-driver) at lines 20–25 emits fixed JSON without an API or external agent call.

That commit removes both global `git worktree prune` calls from `down` and `reclaim`, while preserving the selected-worktree condition, owned-process checks and selected Compose-project cleanup.

These are build results and static source observations, so they do not establish lifecycle success, cleanup success or completion of the Dock task, and the original records contain neither a binary file mode nor a tracked-source before/after comparison.
