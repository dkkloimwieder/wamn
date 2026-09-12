The final selected tests passed after the shared SQL source refactor.
The complete tested source is `8b2a80add6c0cfc43e2271cc9703e4db268811b6`.
The source refactor is `6949f095641d3426e8bba65c900bb34f570505c9`, and the added integrity assertions are `926997f30d6d09358ed0665403ba73a267db3fd0`.
The final commit removed the redundant test-module path attribute.

All 31 source-map tests passed in 7.500918359030038 seconds.
The [first run](../immutable-row-source-001/README.md) also passed all 75 run-plane tests and retains the original source-map failure.
The native test compilations passed in 44.18560928496299 and 7.795765615010168 seconds.
The [command record](commands.json) preserves exact arguments, source directory, environment inputs, elapsed times, and exits.
All Cargo commands used Rust 1.98.0, offline locked dependencies, two jobs, and this worktree's own target directory.

Four live cases passed on three fresh PostgreSQL 18.6 instances.
The [registration case](registration.log) exercised both schemas, exact replay, concurrent conflicts, successor refusal, and transactional rollback.
The [publication case](publish-release.log) exercised both function owners, PUBLIC execute revocation, and exact UPDATE/DELETE refusals through the installed triggers.
That case also retained package sealing, its row lock, and concurrent attestation winner assertions.
Both [control cases](portable-store.log) passed their existing integrity and tenant-isolation assertions.
The [live command record](postgres-results.json) retains each command and exit.

The [source record](source.json) names the parent commit because the changes were uncommitted during execution.
Every recorded source hash matches the complete tested commit, as recorded in [source-final.json](source-final.json).
Both complete SQL strings retained every original byte, including transaction boundaries and statement order.
The shared fragment retained the original function body, SQLSTATE `55000`, message, and PUBLIC revocation.
Five added development dependencies introduced no production dependency edge.

All three temporary PostgreSQL configuration directories were absent after their test commands returned.
The capture used only the private credentials supplied by each owned temporary instance.
No credentials appear in the saved records, and the capture left other services unchanged.
The [publication map](publication-map.json) records exact original bytes, hashes, and local file modes for both runs.
Validation covers the selected targets and live cases described here.
