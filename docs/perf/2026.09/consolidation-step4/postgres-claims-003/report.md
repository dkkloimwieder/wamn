All four focused commands passed at source `5ea8ffe9a39547ed6ac52e132696d95b5767c9e5`. The run used Rust 1.98, debug output, two Cargo jobs, and fresh PostgreSQL 18.6.

| Target | Actual outcome | Elapsed seconds |
| --- | --- | ---: |
| Claims | 37 executed cases passed, two bodies stayed unarmed, one case stayed ignored in this command | 5.598577 |
| Separate lifecycle case | One executed case passed | 0.456736 |
| PostgreSQL WIT | One executed case passed | 2.866155 |
| State ownership | 31 executed cases passed | 5.897419 |

The claims command reported 39 passes because two tests returned early. `WAMN_PG_PIPELINE_BENCH` and `WAMN_SCS_OFF_PG_URL` stayed unset. The separate command executed the ignored lifecycle case with `WAMN_POOL_LIFECYCLE_PG_URL` armed.

The refactor moved 24 functions into private `pools` and `transactions` modules. [The source comparison](source-comparison.json) records 69 unchanged production function bodies and unchanged production literal bytes. The parent retained `WamnPostgres`, its maps, and its identity guards.

The separate test correction supplied distinct guest and executor credentials through existing role helpers. It preserved every lifecycle assertion after setup. It changed no production grants or credential checks.

[The command results](results.json) retain exact arguments, exits, times, and original log hashes. [The source record](source-stability.json) shows unchanged source bytes, modes, and HEAD during execution. The capture stopped its owned server and retained its data directory in cache.

[Attempt 001](../postgres-claims-001/report.md) retained the interrupted build. [Attempt 002](../postgres-claims-002/report.md) retained the original fixture failure. These focused results did not replace the integrated workspace run at the stage boundary.
