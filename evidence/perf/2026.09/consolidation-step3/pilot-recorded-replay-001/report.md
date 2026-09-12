# Pilot recorded replay

Six recorded sets reproduce their original outcomes and all 66 step verdicts. Run 030 lacks the hidden grading inputs needed for comparison.

| Recorded run | Original outcome | Tracked-input exit | Recovered-input exit | Comparison |
|---|---|---:|---:|---|
| 001 | FAIL | 30 | — | Exact outcome and 11 step verdicts retained |
| 010 | PASS | 0 | — | Outcome and 11 step verdicts retained |
| 011 | PASS | 0 | — | Outcome and 11 step verdicts retained |
| 012 | PASS | 0 | — | Outcome and 11 step verdicts retained |
| 030 | FAIL | 10 | — | Hidden task/steps absent from both authorized historical roots |
| 031 | PASS | 10 | 0 | Recovered original inputs retain outcome and 11 step verdicts |
| 032 | PASS | 10 | 0 | Recovered original inputs retain outcome and 11 step verdicts |

The nine invocations use the existing `target/debug/wamn-gates agent-pilot-grade --replay` binary. Its source is `cc729d9eafebc6d459921785e7f15971e113cef9`.
[The result](result.json) records arguments, exits, elapsed times, section comparisons, input hashes, and output locations.

The first seven invocations isolate both grading lookup directories and use only tracked inputs.
Two additional invocations use exact copies of the original task and steps files for runs 031 and 032.
[The recovery record](recovery.json) lists each requested historical file and whether it exists.

All original records, copied inputs, repository HEAD, and the 281,795,424-byte executable stay unchanged.
The executable SHA-256 is `d34f90211d439c4e7f4b198c597ff2878d917d0f19e6702273ef1b2dfaf68cae`.
No Cargo command, service, or agent runs.

The original worktrees and published contracts are absent. This replay does not reconstruct them or establish current application behavior or contract equality.
Runs 010–012 retain their outcomes despite a changed explanation that does not determine the result.
The old text is `no replay step in the fixture`. The new text is `no step declares claim-replay`.
