Wiring activation now uses the catalog Rust decision inside the existing promotion transaction. The behavior commit is `ad11bbfc3dddddf51ab7e87bee5e1177245275ef`. The separate fresh-catalog test correction is `68420419e9acf7339b4ef4035b605cc9d4087076`.

The final commands ran against committed source `68420419e9acf7339b4ef4035b605cc9d4087076`. [The result record](postcommit.json) lists commands, source hashes, elapsed times, and cleanup results. The source remained clean and unchanged throughout these commands. All 11 selected cases passed, with no ignored cases in the selected runs.

| Selected cases | Result | Command time |
| --- | --- | --- |
| [Catalog unit tests](catalog-unit-002.log) | 4 passed | 0.281 seconds |
| [Promotion tests](promotion-live-002.log) | 5 passed | 3.295 seconds with PostgreSQL setup and cleanup |
| [Catalog live tests](catalog-live-003.log) | 2 passed | 3.202 seconds with PostgreSQL setup and cleanup |

The promotion run includes four existing unit tests and one new live transaction test. The live case refuses absent definitions, wrong release versions, foreign tenant facts, and retired wirings. It retains exact retries, history counts, rollback, and the real PostgreSQL serialization refusal between competing writers. The unit tests retain disabled-state behavior and refusal precedence. The catalog tests retain activation history, rollback, application permissions, and document hashes after a database round trip.

[The first catalog run failed](catalog-live.log) because its second test expected a deleted upgrade section. Its activation case passed. The separate test correction installs the current catalog and uses a distinct document coordinate. It retains the node, terminal, and hash assertions. [The next fresh-catalog run passed both cases](catalog-live-002.log), followed by the final committed-source run above.

Each live command used its own disposable PostgreSQL 18 cluster. All five clusters were removed, including the failed run's cluster. The individual cluster records and final result record contain these cleanup results. [The native build](ctl-build.json) passed in 399.714 seconds with Rust 1.98.0 and two build jobs. Compiler warnings remain in the unchanged logs.

[The original-file map](original-files.json) records raw file hashes, sizes, and local modes. The logs contain no private keys, credential URLs, or bearer headers. These focused results do not establish the stage-wide test result. The parent step retains its separate workspace run and baseline comparison.
