# Pilot setup and stub run 003

The pilot setup, local stub launch and cleanup commands passed.

| Action | Exit | Seconds | Result |
| --- | ---: | ---: | --- |
| [up](up-record.json) | 0 | 226.09740108100232 | Setup completed. |
| [launch](launch-record.json) | 0 | 0.19713726796908304 | The local stub reported completed. |
| [down](down-record.json) | 0 | 3.0152367659611627 | Cleanup completed. |

[record.json](record.json) identifies driver source `7adc577ec85e5a496ce82ddb2adc75a38bbfd12c` and prepared product source `9d16e94026095ab60a1f1b6bc0a901337c0396bc`. It preserves the exact commands, times, exits and final driver binary hash.

All five Cargo build calls passed. At driver source `7adc577e`, [runner.rs](../../../../../tests/integration/src/agent_pilot/runner.rs) lines 801–836 contain those calls. The host call also builds `wamn-identity`. [build.json](build.json) records seven artifact hashes and equal prepared source hashes before and after those builds.

The private run result records the stub's completed outcome, no remaining cleanup resources and zero `wamn dev` runs. [publication.json](publication.json) records these facts and the private result file hashes. Its three output events match the existing completed stub fixture. The private result files and transcript remain uncopied.

At driver source `7adc577e`, [process.rs](../../../../../tests/integration/src/agent_pilot/runner/process.rs) lines 294–297 select the local stub executable. Its [completed mode](../../../../../tools/agent-pilot-stub-driver) emits a fixed successful result without an external agent call. This test did not implement Dock or run grading.

The [post-run observation](post-run-observation.json) found no containers, network or volumes for Compose project `wamn-pilot-903s`. All six reserved ports were closed. The observation still listed the frozen `wamn` kind cluster. Both source worktrees retained their recorded revisions and clean status after the stub finished.

The [credential review](secret-review.json) compared 17 known values across 16 files before cleanup. All exact matches and listed pattern matches were zero. The review states its limits and does not establish that the raw log contains no other sensitive data.

The later [cleanup record](private-environment-cleanup.json) records removal of only run 903s's private environment directory. It records 30 removed files and retention of the run records and prepared worktree. No new comparison used the removed credentials.

The private raw setup log remains excluded. Its path, byte count and hash are in [publication.json](publication.json). The prior explicit authorization covers run 901s only. No copy of this run's raw log was attempted.
