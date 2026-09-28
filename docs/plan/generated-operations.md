# Generated operations

Updated through: 2026-09-28, `main` at `c3e7074c7`. Draft for owner review, issue `wamn-iowb`.

## 1. Goal

An author declares a generated operation (`get`, `query`, `create`, `update` or `delete`) in `wamn.json`, runs `materialize_package write`, and the release serves it. The author writes no Rust, no WIT and no JSON for it.

Today the generator emits the SQL, the statement accessors, the codec, the contracts and the route input schema. The author still writes five things by hand for each operation. This plan moves all five into the generator. Authored operations, such as `inventory.move`, keep their authored data function and handler.

## 2. Fixed rules

- A generated operation has no hand-written file. Its handler, data function, world export, route entry and component declaration entry are generated files.
- The generator writes only under `generated/`. It never edits an authored file.
- An authored operation keeps its authored SQL, data function and handler. The generator emits for it what it emits today, and nothing more.
- The wire contract does not change. A generated handler answers the same contract, error literals and details that the hand-written handler answers today. A publish of the same package before and after the change produces the same contract digests.
- Business rules stay out of generated code. A rule that a generated operation carries today moves to the schema or the manifest, or the operation becomes authored. The generator does not take a hook or a callback for one rule.
- One route entry and one declaration entry for each operation. An operation that appears in both the generated and the authored files is a generation error.
- `definition-hash` stays `canonical_json_sha256` of the unresolved definition. The generator computes it with the same function that publish uses.

## 3. Current state

Measured on `main` at `c3e7074c7` on 2026-09-28.

The platform has 34 generated operations in six packages. WMS has 15 on 5 models, Receiving 7, the platform fixture 8, Acme 2, the fixture overlay 1 and the edge samples 1.

For each generated operation, an author writes these five things by hand:

1. A handler module in the component, for example `apps/wamn_wms/component/src/product.rs`. The module includes `generated/wit/product_<op>_codec.rs`, defines `handle`, and calls `codec::export_operation!`. A create handler takes the codec's `Transaction`. The other kinds take a `Connection`.
2. A data function in the data crate, for example `apps/wamn_wms/data/src/product.rs`. It parses the scalars, calls the `pub(crate)` accessor in `generated/wamn/<model>.rs`, and converts `StatementError` to the package's `AccessError`. The data crate also carries `data/src/generated.rs`, which includes each `generated/wamn/<model>.rs` by hand.
3. An `export` line and a `path` entry in the inline WIT world in `component/src/lib.rs`.
4. A route entry in `publication/attachments.json`. Since `wamn-4omo`, its input schema is a `$ref` to `generated/routes/<model>/<op>.json`, and `route_schema::resolve_attachment` inlines it and recomputes the hash. The rest of the entry is authored.
5. An operation entry in the component declaration, for example `publication/components/wms.json.in`.

WMS writes 633 lines of handler and 767 lines of data function for its 15 generated operations. After a rename of the model, `location.rs` and `product.rs` are the same file in both crates.

Four generated operations carry a business rule today:

| Operation | Rule | Where |
|---|---|---|
| `packaging.create` (WMS) | refuses the status `consumed` | `data/src/packaging.rs:129-137` |
| `packaging.query` (WMS) | dispatches over 8 authored sort variants | `data/src/packaging.rs:180-214` |
| `supplier.create` (Receiving) | refuses a blank name | `data/src/supplier.rs:79-85` |
| `widget.delete` (fixture) | reads the row again to report the observed revision on a conflict | `data/src/widget.rs:192-212` |

The packages use two error styles. WMS, the edge samples and the fixture read error details from JSON (`error.detail()[key]`). Receiving and Acme use typed accessors (`field()`, `constraint()`, `observed_row_version()`). Each package copies `data/src/error.rs`, and each copy adds its own kinds for its authored operations.

## 4. Design

### 4.1 Data functions

The generator emits `generated/data/<model>.rs` with one `pub async fn` for each generated operation. It takes the contract's request fields and returns the contract's row or a generated error. It does the work of today's hand-written data function:

- It parses each scalar in its one wire spelling.
- `get` answers `not_found` when no row returns.
- `query` pages with the platform cursor, reads one row more than the limit, and dispatches over the declared sort variants, including authored variants that the manifest names.
- `create` maps a constraint failure to the constraint literals that the contract declares.
- `update` and `delete` read the `outcome` column of their generated SQL and answer `not_found`, `concurrency_conflict` with both revisions, or the row.

The generated error is one type for each package, `generated/data/error.rs`. It carries the contract's error literal and its detail, and it is the type that `codec::map_error` takes. The authored `data/src/error.rs` keeps the kinds of the authored operations and converts into the same literal and detail shape.

The cursor, page and scalar code that each data crate copies today (`cursor.rs`, `page.rs`, `scalar.rs` in WMS) moves to a platform crate that the generated code calls. It is the same code, in one place.

### 4.2 Handlers and the world

The generator emits `generated/component/<model>.rs`. It holds one module for each generated operation, with the `codec` include, the `handle` function that calls the generated data function, and the `export_operation!` call. It also emits `generated/component/mod.rs`, which declares each model module.

The generator emits the component world as `generated/wit/world.wit`. The world exports every generated operation and includes an authored world, `component/wit/authored.wit`, for the authored operations. WIT `include` joins the two. The component's `wit_bindgen::generate!` names the generated world instead of an inline one.

An authored component then keeps only its authored modules and one line, `mod generated { include!(...generated/component/mod.rs) }`.

### 4.3 Route entries and declaration entries

The generator emits `generated/publication/attachments.json` with one route entry for each generated operation, and `generated/publication/component-operations.json` with one declaration entry for each. It computes each `definition-hash`. The authored `publication/attachments.json` and the component declaration keep only the authored operations.

Publish, `materialize_package` and the client builders read both files. An operation or an attachment id that appears in both is refused.

### 4.4 The four business rules

- `packaging.create` refuses `consumed`: the manifest already narrows `values.status` to `available` and `held`. If that narrowing refuses `consumed` before the data function runs, the Rust check is a duplicate and goes. Issue 1 must verify this first.
- `packaging.query` sort variants: the generated dispatch in §4.1 covers it.
- `supplier.create` refuses a blank name: a schema CHECK, `name <> ''`, on `receiving.supplier`. The constraint literal comes from the contract, as for every other CHECK.
- `widget.delete` reports the observed revision: the generated delete SQL returns the revision it read in its `outcome` row, as update does.

After §4.4, no generated operation needs authored Rust.

## 5. Issues

1. Generated data functions and the generated error type (§4.1), and the shared cursor, page and scalar crate. Verify the `consumed` narrowing first. Test: the platform fixture's generated operations run through the new functions against PostgreSQL, with every refusal of the contract.
2. Generated handlers and the generated world (§4.2), on the platform fixture.
3. Generated route and declaration entries, and the reads in publish, `materialize_package` and the clients (§4.3). Test: a publish of the fixture before and after the change has the same contract digests and route hashes.
4. WMS takes the generated files and deletes its hand-written ones, with the §4.4 rules.
5. Receiving, Acme, the fixture overlay and the edge samples take the generated files, with the §4.4 rules.
6. Closeout: docs, the workspace test run, and a count of the deleted lines.

## 6. Out of scope

- Authored operations. Their data function, handler, route entry and declaration entry stay authored.
- A new operation kind, or a change to the SQL that the generator emits for an existing kind.
- The TypeScript and Rust clients, apart from reading the generated route file.
- Merging the two error styles of §3 beyond what §4.1 needs. The authored kinds of Receiving and Acme keep their typed accessors.

## 7. Questions for the owner

1. Does a generated operation ever need to become authored without a change of kind? This plan has no switch for it. The alternative is a manifest field, `handler: authored`, that makes the generator skip §4.1 to §4.3 for one operation.
2. The generated error type in §4.1 takes the JSON-detail style. Is that acceptable for Receiving and Acme, whose generated operations use typed accessors today?
3. §4.4 moves the blank-name rule into a schema CHECK. Is a migration of `receiving.supplier` acceptable, or does the rule go to the manifest instead?
4. Are the generated publication files in §4.3 acceptable, or do the route and declaration entries go into the authored files between generated markers?
