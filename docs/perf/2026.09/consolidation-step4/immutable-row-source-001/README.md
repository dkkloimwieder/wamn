The first test run passed 75 run-plane cases and failed one of 31 source-map cases.
The [commands](commands.json) and [raw source-map log](command-2.log) retain that failure.
The run stopped before the PostgreSQL binaries compiled.

The source reader classified the registration rollback fixture as production SQL.
The base already declared that test module with a redundant path attribute between `#[cfg(test)]` and its module declaration.
That attribute hid the test condition from the existing source reader.
Commit `8b2a80add6c0cfc43e2271cc9703e4db268811b6` removed only the redundant attribute.
The [second run](../immutable-row-source-002/README.md) contains the successful rerun and live tests.

Both complete SQL strings matched their original bytes before live testing.
The [direct comparison](comparison.json) and [compiled comparison](compiled-string-comparison.json) retain their hashes and lengths.
The shared function fragment contained 260 bytes with SHA-256 `0f64ebaf346ee818df03340c5e63b79602e61d33de2a931d826da664452c6e36`.
The [source snapshot](source.json) records the uncommitted files used for this first run.
No database service ran during this attempt.
