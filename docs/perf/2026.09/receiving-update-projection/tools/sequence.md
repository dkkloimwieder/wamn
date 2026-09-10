# Receiving update projection regeneration

Run these steps after the emitter and its tests reach a quiet source boundary.
The owner controls the machine gap and starts each gate.
Use the exclusive transferred cache at `receiving-update-projection-20260910/target`.
No step uses a shared target directory.

`generator-build-001` already builds the `materialize_package` example.
`sequence.sh` records the remaining commands in order.
The script requires `--apply` and new evidence directories.
If a step already ran, execute only its remaining command blocks.
Do not rerun the full script over existing receipts.

The existing materializer creates a separate PostgreSQL 18 database for each package.
It applies Receiving alone for the base package.
It applies Receiving and Acme migrations for the overlay package.
It also regenerates WMS in its own database.
WMS has no generated update affected by this repair and must remain unchanged.
Each package runs `write`, `check`, and `check` through the normal compiled example.

`sqlx_pg18.py` uses the retained `pg_virtualenv -t -v 18` pattern.
It applies the base and overlay migrations to its own fresh database.
It runs normal `cargo sqlx prepare --workspace` for `receiving_sqlx_verifier`, then runs `prepare --check`.
The database URL sets `receiving,public` as the search path.
The helper records all metadata before and after preparation.
It preserves unrelated metadata from its retained original bytes if preparation prunes them.
It checks that the PostgreSQL configuration and listener disappear after the command.

The normal `tools/build-components m1` command builds and virtualizes the release components.
Its captured command explicitly sets `CARGO_TARGET_DIR` to the exclusive cache.
`remint.py` reuses `guest-digest/tools/remint.py` with two cache adjustments.
`remint-reuse.json` records the source hash and those adjustments.
The tool requires the successful captured `m1` command before it changes the authored base pin.
It then regenerates Acme through the normal materializer and checks the exact derived pin references.

The expected SQL repair changes these files in each of Receiving and Acme:

- `generated/sql/purchase_order/update.sql`
- `generated/wamn/purchase_order.rs`
- `generated/contracts/purchase_order/update.operation.json`
- `generated/package-weld.json`

The operation contract and Rust accessor carry the new statement digest.
The package weld carries the new SQL corpus identity.
The unchanged migration catalog retains its schema state identity.
Normal generation determines the actual changed files.
Native projection types, client bindings, TUI output, and source maps require no authored changes for this SQL-only repair.

The SQLx refresh replaces these two baseline query records with the records emitted for the new SQL:

```text
.sqlx/query-ae9bc70de2245821b2776e694c9f169e7bd6dde8895767adeafc42f36f00e71f.json
.sqlx/query-df5a9977e52e7386bd4f49fc96f713418fbf095d63f3c9c284a7725485531645.json
```

The pin refresh changes `packages/client_acme_receiving/wamn.json` and these four derived files:

- `generated/contracts/receiving/record_receipt.inherited-tests.json`
- `generated/contracts/receiving/record_receipt.operation.json`
- `generated/platform-policy/data-access.json`
- `generated/source-map/receiving_record_receipt.json`

Run the generator gate from `docs/operations/build-and-test.md` after its tests are final:

```bash
cargo test -p wamn-schema-generator --all-targets --no-fail-fast --locked --offline
```

Run the focused native caller gate in the same exclusive cache:

```bash
CARGO_TARGET_DIR="$PWD/target" cargo test --manifest-path components/Cargo.toml \
  --locked --offline -p wamn-receiving-data-access \
  -p wamn-client-acme-receiving-data-access --all-targets --no-fail-fast
```

The owner also runs the new PostgreSQL regression that the test agent supplies.
The generator gate alone does not prove an ignored live test ran.
The final offline SQLx gate follows metadata preparation in `sequence.sh`.
The subsequent Receiving journey supplies the real component and served-route proof.

Before resuming `.78`, retain the hashes of both virtualized components and the complete Acme package.
The base pin must equal the hash of `target/virtualized/std-empty-environment/receiving.wasm`.
The paired fresh installs must use the same Acme package bytes and `client_acme_receiving.wasm` bytes.
Only the base migration copy changes for the additive installation.
Do not regenerate Acme against the additive installed database.
Freeze the overlay after this repair and pin refresh, before either installation starts.
