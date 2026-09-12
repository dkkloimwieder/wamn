The final source is `abcad384593b5c60f4c98c7615185fc171f4ac56`, based on `2adc1f6ff9d29f7a1cd7a79840bc1fadf08605a0`.
The change renames four native types and their direct callers.
Physical Receiving fields remain unchanged.
The ten captured source files match the commit exactly.
[The source comparison](result.json) records their hashes and modes.

The final test run passed 149 cases, with three existing ignored cases.
The authoring contract passed 14 cases.
The control tests passed 123 development cases and one component admission case.
The worker management tests passed 11 cases.
[The commands](commands.json) record actual arguments, source location, times, and exit codes.
[The development log](command-3.log) names the ignored cases.

The generated authoring schema remains exactly 28,029 bytes.
Its SHA256 is `53ca54c5573fad582edcaaf61b9f7279ab90acfec64ea327670453fa976766ea`.
Four checked-in schema files also remain unchanged.
[The schema comparison](schema-comparison.json) records each hash.
[All five deserialization errors](serde-renamed-errors.json) match the actual base errors byte for byte.
[The error comparison](serde-comparison.json) records the compiled probe and source hash.

`GateResult` exposes one derived struct from a private module.
The struct retains its frozen external name and schema description.
Schemars 0.8.22 derives its internal `schema_id()` from the Rust module path, so that internal value changes.
A maintained-source search found no direct caller of that method.
The complete generated JSON remains unchanged.
Two internal malformed-response messages use result wording, while their error kinds remain unchanged.

The earlier captures remain intact.
[Draft 001](../native-results-001/serde-comparison.json) changed the scalar and sequence errors after the direct rename.
[Draft 002](../native-results-002/serde-comparison.json) preserved scalar errors but changed the empty-sequence error through the `expecting` override.
[Draft 003](../native-results-003/serde-comparison.json) passed comparisons with a custom deserializer, which the final source replaces with one derived struct.
All three draft test runs passed their selected cases.
The initial base schema command did not capture elapsed time.

The run used the isolated worktree's own target, Rust 1.98.0, two build jobs, and locked offline dependencies.
The tests compiled the changed library callers and used their local fixtures.
No database services started, and the ignored database cases did not run.
The source remained unchanged during each recorded test run.
[The publication map](publication-map.json) preserves all 77 original files, totaling 522,636 bytes, with their original modes and hashes.
