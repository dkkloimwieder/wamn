# RC application result — 2026-09-11

This [passing run](result.json) used `1c4a2676375a6757c17f5b7e12f2c01d104ff7ce`, and its [command](../rc-live-command-003/record.json) exited 0 after 226.500 seconds.
The [socketguard result](socketguard-verdict.json) records P2 and P3 raw-socket refusal and standard-component admission, while [traceproof](traceproof-verdict.json) records the expected trace context through `wamn:connection/http`.
Both tests completed, and the separate [cleanup record](cleanup.json) reports success while the original result retains `cleanup: null`.
The result still defers `wamn-0h0g.15.153` with the exact disposition `post-merge; not claimed`.
All raw files, their copied `0600` or `0664` modes, and the original [88-entry hash list](evidence.sha256) remain unchanged, and that list excludes this later note.
