The retained workspace test run failed with exit 101 after 533.769 seconds at source `5ef028880f4c83283ae344008bbb298adbd88003`.
The [full classification](classification-002/report.md) lists all 104 failures and 84 explicit skips, with no unresolved cause or parser entries.
The [exact command](command.json), [environment names](environment-names.json), [full output](workspace.log), and [source comparison](source-stability.json) preserve the run, which used no armed live inputs and changed no tracked source bytes or modes.
The [changed cases](classification-002/changed-failures.json) include three new ordinary test failures, while [17 mappings](classification-002/moved-failure-identities.json) identify older failures under their app test owners.
The [initial classification](classification-001/report.md) and [failed first source-review attempt](review-attempt-001/review-command.json) remain unchanged, and this result does not establish native C or wave completion.
