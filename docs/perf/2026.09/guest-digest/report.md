# Guest digest isolation

`wamn-10yt.61` gives each selected package its own Cargo invocation in `tools/build-components`.
The package inventory, workspace targets, build profile, artifact paths, and JSON plan schema remain unchanged.
The HTTP shell retains its isolated dependency features.
The development loop passes the plan bytes through without interpreting the build groups.

All nine shared Wasm guests have identical SHA-256 values under `m1` and `proof`.
The builds used separate targets, the same source bytes, Rust 1.98.0, and an empty `RUSTC_WRAPPER`.
The `m1` profile selected nine packages.
The `proof` profile selected 24 packages, including ten supporting libraries.
[The complete artifact comparison](all-guests-002/result.json) records all paths, sizes, hashes, and selections.
The initial comparison covered only `cdylib` targets and omitted command-style guests.
Its narrower result remains in `all-guests-001`, beside the original comparison script.

All seven selector tests passed with ignored tests included.
The existing cross-profile digest assertion also passed with both real artifact plans.
The original batching failed `selector_tools_execute_exact_fake_cargo_argv` with exit 101.
Restoring the fixed tool made that test pass, with its original modification time restored.
[The negative control](mutation-001/result.json) records both outcomes.
Scoped Clippy passed with existing warnings.

The normal `m1` command built and normalized the components successfully.
A fresh PostgreSQL 18.6 instance accepted both shipped package checks before the pin changed.
The materializer then regenerated Acme and accepted its new output.
Only the authored Receiving pin and four derived Acme JSON files changed.
The Receiving digest changed from `f7427c1425e02cfd856a0a8174fb30c86da5748321bb4c1ea896f457fc1139ff` to `bb2aa728d578c8a5181b5127b1238acccde844b9394561e25182106517e3177f`.
[The regeneration receipt](remint-001/result.json) records the checks, exact changed paths, and completed container cleanup.

The base was `a48f7af827bb835b619eb8aa24db745916d24824`.
`preparation-001/source.patch` records the tested tool and test changes.
`final-checks-001/result.json` records all eight final source files and confirms that the built artifacts stayed unchanged after regeneration.
The report keeps the earlier failed cross-profile evidence by reference in `preparation-001/prior-failure.json`.

These are local build, compiler, lint, artifact, and disposable database proofs.
No cluster journey, full workspace sweep, Docker image comparison, or two-checkout digest comparison ran for this change.
The result does not claim release readiness.
