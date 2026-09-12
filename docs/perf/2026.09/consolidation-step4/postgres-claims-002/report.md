The first completed test run found one stale lifecycle fixture. The claims target reported 39 passes and one ignored case. The separate lifecycle command failed.

The failed fixture gave every authority class its guest credential. [The database query](lifecycle-membership.stdout) found no executor role. The platform checkout returned `PgError::ConnectionUnavailable`, as [the original failure log](lifecycle.stderr) records.

The WIT target passed one case, and the ownership target passed 31 cases. The benchmark and SCS-off test bodies stayed unarmed. Rust reported both early returns as passes.

[The command results](results.json) retain exact arguments, exits, elapsed times, and log hashes. [The source record](source-stability.json) shows unchanged bytes and modes. The capture stopped its owned PostgreSQL 18.6 server.

[The next run](../postgres-claims-003/report.md) used a separate correction for the test setup. This directory retains the original failure.
