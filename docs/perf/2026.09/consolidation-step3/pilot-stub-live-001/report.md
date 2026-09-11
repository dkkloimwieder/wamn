# Pilot setup run 001

The pilot setup failed before the local stub launched.

| Action | Exit | Seconds | Result |
| --- | ---: | ---: | --- |
| [up](up-record.json) | 10 | 869.8389976429753 | Standup rejected the system database name. |
| launch | Unexecuted | Unexecuted | Setup did not finish. |
| [down](down-record.json) | 0 | 2.299268930044491 | The cleanup command completed. |

The controller selected source `1925bea2ccacb28fc6cf768dd02c5a760b62aca3` and the declared Dock task. [record.json](record.json) retains the exact commands and final binary hash. The run did not execute the stub, an external agent, grading, or the Dock application.

All five Cargo builds passed before standup. [build.json](build.json) records six artifact hashes and unchanged tracked source during those builds. The prepared worktree stayed at `9d16e94026095ab60a1f1b6bc0a901337c0396bc`. The private environment log records five build completions, five healthy services, and successful OCI publication. [publication.json](publication.json) identifies that unchanged private log by its path, size and hash.

At source `1925bea2`, [environment.rs](../../../../../tests/integration/src/agent_pilot/runner/environment.rs) lines 191–192 pass `/postgres` as the system database path. The existing [PAT preflight](../../../../../services/ctl/src/dev/pat_issuer.rs) at lines 209–216 requires `/wamn_system`. That source mismatch explains the standup refusal. The private log payload remains withheld.

The [post-run observation](post-run-observation.json) found no containers, network or volumes for Compose project `wamn-pilot-901s`. All six reserved ports were closed. The observation still listed the frozen `wamn` kind cluster. It also recorded clean source and prepared worktrees. The private run directory, environment and build target remained available at that observation.

The later [credential cleanup record](private-environment-cleanup.json) records removal of only this run's private environment directory. It records 15 removed files and retention of the run records and prepared worktree. The build cache remains private.

The [secret review](secret-review.json) found no known run credentials or matching credential patterns in the reviewed files. Automatic approval review still rejected publication of the private raw environment log. The raw log remains outside these committed results. [publication.json](publication.json) records each copied original file's hash and mode. It references task inputs through existing source paths and hashes without copying them.

This result does not establish a successful pilot or application run.
