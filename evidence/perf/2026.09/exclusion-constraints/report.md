# Generated exclusion refusals

Issue: `wamn-10yt.72`.

Receiving and Acme classify an exclusion only when the operation's generated allowlist names its exact constraint.
The operation emits `exclusion_violation` with the constraint name.
Empty lists, unknown names, missing names, and the wrong error class still produce `internal_error` without a constraint name.
Current package contracts declare no exclusions, so their exclusion lists remain empty.
Acme retains its existing handling of unique, foreign-key, and check violations.
The existing Receiving vocabulary discrepancy remains owned by `wamn-10yt.67`.

The vocabulary guards compare each generated exclusion list with the exact names in its operation contract.
They exclude the inactive exclusion class from the package vocabulary only when its generated list is empty.
They add no unconditional exception for an undeclared refusal.

## Local evidence

| Receipt | Result |
| --- | --- |
| `focused-002` | Native tests report 52 passes in 5.366 seconds. Two cases report that their PostgreSQL fixture is unarmed. The other 50 cases execute. |
| `postgres-004` | Both armed generated-policy cases pass in 9.105 seconds, including two rejected mutants and both restored cases. |
| `wasm-clippy-002` | Denying Clippy passes for both data crates and both application guests on `wasm32-wasip2`. |
| `native-clippy-001` | Strict native Clippy fails on 56 existing `result_large_err` diagnostics in generated accessors. |
| `native-clippy-comparison-001` | Normal native Clippy passes on the base and final source. Warnings change from 114 to 112, with no added identities. |
| `m1-remint-001` | The normal `m1` build and virtualization pass in 113.401 seconds. |
| `pin-001` | The materializer writes and then accepts the dependent package on fresh PostgreSQL. Cleanup succeeds. |

The PostgreSQL tool copies each package into a temporary fixture and adds one real exclusion constraint.
The normal materializer emits each fixture's SQL, Rust projection, and closed error contract.
Each generated UPDATE causes PostgreSQL to return SQLSTATE `23P01` and its actual constraint name.
The tool records those diagnostics, then feeds them into the production classifier, update policy, and JSON serializer.
The Rust test asserts SQLSTATE `23P01` before supplying the existing typed statement error class.
This is a local boundary proof, not a deployed host invocation or a full test of WIT error translation.

The negative controls replace each production update's generated exclusion list with an empty list.
Each control fails `operation::tests::generated_update_exclusion_from_postgres` with exit 101 after successful compilation.
The complete response changes from `exclusion_violation` to `internal_error`.
Both restored tests pass.
The tool restores the source and generated projections with their exact original bytes, then removes the disposable database.

## Required digest refresh

The normal `m1` build produces the release artifact that the authored dependency names.
Its Receiving digest is `sha256:cd494c3413f9987a645ea9320f64f3d9dc5b269296feb6a3289edb84ef065d2d`.
The remint changes the Acme manifest and four generated files that carry its manifest hash or inherited dependency.
The package weld remains unchanged.
No Cargo manifest or workspace inventory changes.
The separate profile-dependent digest finding remains `wamn-10yt.61`.

## Earlier attempts

`focused-001` passes with the same two unarmed fixture cases.
`postgres-001` fails because the fixture uses a constraint name that violates the authored naming convention.
`postgres-002` fails because explicit exclusion ownership reports an unknown constraint.
That generator defect remains open as `wamn-10yt.79`.
The successful fixtures omit that optional ownership declaration.
`postgres-003` passes before the final Clippy correction.
`wasm-clippy-001` rejects identical fallback arms, which the final classifier merges.
The native comparison finds the same fallback warning on the base, once per target.
The 112 remaining native warnings repeat 56 existing error-size diagnostics on library and test targets.
The unchanged `StatementError` occupies 144 bytes on native targets.
Every failed receipt remains in this directory.

## Repeat the proof

Use an isolated worktree with the PostgreSQL 18 image and both Rust targets available.
Choose a new evidence directory for each command.

```bash
python3 docs/perf/2026.09/exclusion-constraints/tools/postgres.py \
  --tree "$PWD" --evidence-dir /path/to/new/postgres-proof

cargo test --manifest-path components/Cargo.toml --locked --offline --no-fail-fast \
  -p wamn-receiving-data-access -p wamn-client-acme-receiving-data-access \
  -p receiving -p client-acme-receiving --all-targets -- --include-ignored --nocapture

cargo clippy --manifest-path components/Cargo.toml --locked --offline \
  -p wamn-receiving-data-access -p wamn-client-acme-receiving-data-access \
  -p receiving -p client-acme-receiving --target wasm32-wasip2 --no-deps -- -D warnings
```

The PostgreSQL tool temporarily replaces two generated `purchase_order.rs` files in that worktree.
Do not run another compiler against the same worktree during its negative controls.
The retained remint tool requires a successful captured `tools/build-components m1` receipt before it writes the authored pin.
