# Pilot setup run 002

The second pilot setup failed before the local stub launched.

| Action | Exit | Seconds | Result |
| --- | ---: | ---: | --- |
| [up](up-record.json) | 10 | 216.7598277659854 | Standup could not find its identity executable. |
| launch | Unexecuted | Unexecuted | Setup did not finish. |
| [down](down-record.json) | 0 | 2.594984684023075 | The cleanup command completed. |

[record.json](record.json) identifies driver source `8671f63c3eda1a114417edf5e9023166757fa597` and prepared product source `9d16e94026095ab60a1f1b6bc0a901337c0396bc`. It retains the exact commands and final driver binary hash. The run did not execute the stub, an external agent, grading, or the Dock application.

All five Cargo builds passed. [build.json](build.json) records six artifact hashes and unchanged prepared source during the builds. The reviewed private log reports five healthy services and successful OCI publication. The run passed the corrected database-name requirement and reached the identity-executable requirement.

At driver source `8671f63c`, [runner.rs](../../../../../tests/integration/src/agent_pilot/runner.rs) lines 801–824 omit `wamn-identity` from the build commands. At product source `9d16e940`, [pat_issuer.rs](../../../../../services/ctl/src/dev/pat_issuer.rs) lines 220–237 require `WAMN_IDENTITY_BINARY` or `wamn-identity` beside the CLI. These source facts explain the next setup failure. The raw private log remains withheld, and [publication.json](publication.json) identifies its original path, size and hash.

The [post-run observation](post-run-observation.json) found no containers, network or volumes for Compose project `wamn-pilot-902s`. All six reserved ports were closed. The observation still listed the frozen `wamn` kind cluster. Both source worktrees retained their recorded revisions and clean status.

The later [credential cleanup record](private-environment-cleanup.json) records removal of only this run's environment directory. It records 15 removed files and retention of the run records and prepared worktree. The working records and build cache remain private.

The original [secret review](secret-review.json) compared seven known credentials and common credential shapes before cleanup. It found zero matches in the reviewed files. No new comparison used the removed credential files. [raw-log-restriction.json](raw-log-restriction.json) preserves the automatic rejection because the prior explicit authorization covered run 901s only. This result does not establish a successful pilot or application run.
