The corrected helper passed on fresh PostgreSQL 18.6 with ordinary `/usr/bin/psql` at `6c795d37`.
The [third command](attempt-003/command.txt) applied the Receiving migration and executed five helper calls.
Seed, repeated seed and seed after cleanup each left one owned order and one owned location.
Both cleanup calls left zero owned rows, and every call preserved the distinct unrelated order and location.
The [results](attempt-003/result.json) record the exact row counts, complete unrelated rows and unchanged source hashes.

All five helper calls exited 0, and the test command exited 0.
The [command log](attempt-003/commands.json) retains the exact SQL, client arguments and redacted output.
Container removal exited 0, and the subsequent Docker inspection reported that the owned container no longer existed.
This test started no development services and ran no builds.

The first two attempts stopped before schema application because their `PGDATABASE` URI did not select the fresh server.
The [system client attempt](attempt-001/result.json) selected the missing local socket on port 5435.
The [native client attempt](attempt-002/result.json) selected the missing local socket on port 5432.
Both attempts removed their owned containers and preserved the source at `52ac90a6`.
Commit `6c795d37` corrected the helper to pass the explicit `--dbname` argument used in the successful third attempt.
