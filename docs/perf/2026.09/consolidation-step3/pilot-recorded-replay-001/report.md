# Pilot recorded replay

Six recorded sets reproduced their original outcomes and all 66 step verdicts; run 030 lacks the hidden grading inputs needed to produce a comparison.

| Recorded run | Original outcome | Tracked-input exit | Recovered-input exit | Comparison |
|---|---|---:|---:|---|
| 001 | FAIL | 30 | — | Exact outcome and 11 step verdicts retained |
| 010 | PASS | 0 | — | Outcome and 11 step verdicts retained |
| 011 | PASS | 0 | — | Outcome and 11 step verdicts retained |
| 012 | PASS | 0 | — | Outcome and 11 step verdicts retained |
| 030 | FAIL | 10 | — | Hidden task/steps absent from both authorized historical roots |
| 031 | PASS | 10 | 0 | Recovered original inputs retain outcome and 11 step verdicts |
| 032 | PASS | 10 | 0 | Recovered original inputs retain outcome and 11 step verdicts |

The nine invocations used the existing `target/debug/wamn-gates agent-pilot-grade --replay` binary at repository HEAD `cc729d9eafebc6d459921785e7f15971e113cef9`, with full arguments, exits, elapsed times, section comparisons, input hashes and output locations in [result.json](result.json).

Tracked-input runs isolated both grading lookup roots, while the two additional runs used byte-identical copies of the authorized legacy `031` and `032` task/steps files; [recovery.json](recovery.json) records every checked file and its presence or absence.

All original records, copied original inputs, repository HEAD and the 281,795,424-byte executable stayed unchanged, with executable SHA-256 `d34f90211d439c4e7f4b198c597ff2878d917d0f19e6702273ef1b2dfaf68cae`; no Cargo command, service or agent launched.

The original worktrees and published contracts are absent and were not reconstructed, so this run does not establish current application behavior or contract equality; runs 010–012 also retain their outcome despite the non-gating explanation changing from `no replay step in the fixture` to `no step declares claim-replay`.
