# PostgreSQL results after unused wiring removal

The fresh PostgreSQL run at `d3c6788c` passed 22 tests and failed two.
Both test compilations passed.
The executor test restored the three obsolete grants, applied the retained role setup, and demonstrated their removal through database access refusals.
All 20 cases in the role-isolation test passed.

The runtime test reached released and candidate resolution before it failed on a connection revision.
The test expected revision 1, but its own fixture increments the initial revision to 2.
That expectation predates this change.
Later candidate and credential assertions did not run.

The event-materializer test passed its exact grant assertions but returned no package row where it expected `receiving@1.0.0`.
The suspected cause is statement evaluation order in its tenant-setting query.
That diagnosis remains unconfirmed in this run.
Neither failed test counts as a pass.

All three owned containers were removed, and the source stayed unchanged.
The [summary](summary.json) records each failure and its scope.
The [live results](live/result.json) link the executed test groups and their cleanup results.
The complete command logs, executable hashes, and source records remain beside those results.
