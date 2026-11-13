# 07 `wamn-component-policy`

Path `crates/platform/component-policy`. `src/lib.rs` 898 lines (521 non-test, 377 test) and `build.rs` 87 lines. No dependencies. Five crates depend on it. Both files were read in full. Every public symbol's callers were searched, and the places that hold another list of allowed imports were opened. Nothing was built or run.

## One purpose

Decide which interfaces a component may import. (fact)

The name fits. It has no SQL.

## What it holds and who calls it

Callers are files outside the crate, test code excluded.

| Part | What it is | Callers |
| --- | --- | --- |
| `CAPABILITY_REGISTRY`, `capability_row`, `import_posture`, `is_registered_package`, `Posture` | A table of 11 packages an application component may import, each marked ambient or effect | `engine` `component_admission.rs`, `declared_world.rs`; `catalog` `component_facts.rs` |
| `analyze_tenant`, `APPLICATION_DENIED_INTERFACES` | The application rule: every import must match a row; an effect row also needs a grant; two interfaces are refused always | `engine` `component_admission.rs` (the doc comment in `control/package` names it) |
| `analyze` with `PolicyProfile::Integration`, `integration_import` | The integration rule: a fixed list written as a `match`, not taken from the table | `engine` `component_admission.rs::validate_integration_admission` |
| `analyze` with `PolicyProfile::FirstParty`, `screen_imports`, `denied_imports`, `DENIED_EGRESS_PKG`, `EgressGuardError::RawSocketImport` | The platform rule: refuse only `wasi:sockets` | None in production. One test helper: `tests/conformance/src/socket_test.rs`. |
| `import_pkg`, `import_version` | Split an import name into package and version | `engine`, `catalog`, `component-composer` |
| `STATEMENTS_INTERFACE`, `STATEMENT_INSTANCES_SECTION`, `instance_type` | Facts about a composed component: the statements interface name and the custom section that maps imports to members | `component-composer` (writes the section), `engine` `statement_instances.rs` (reads it) |
| `build.rs`, `WASI_CEILING`, `WASI_FAMILY`, `admits_version` | The build script runs `cargo metadata`, finds the WIT files of the locked `wasmtime-wasi`, and writes the highest `0.2.N` it finds. A `wasi:*` row admits `0.2.0` to that number. | Internal; `engine` `declared_world.rs` relies on it through `capability_row` |

## What serves another purpose

| # | Finding | Mark |
| --- | --- | --- |
| 1 | The three composed-component items are not import policy. They are here because this crate sits under both the composer and the engine. | fact |
| 2 | The 25-line file header describes one rule, refusing `wasi:sockets`, as what this module is. That rule is the `FirstParty` path, which no production code calls. | fact |

## Where its definitions are repeated

The question "what may a component import, and what does the host offer" is answered in nine places. (fact; each was opened unless marked)

| # | Place | What it holds |
| --- | --- | --- |
| 3 | `CAPABILITY_REGISTRY` | 11 rows for applications |
| 4 | `integration_import` in this crate | A second allow list, for integrations, as a `match` |
| 5 | `APPLICATION_DENIED_INTERFACES` | `wamn:postgres/client`, `wamn:integration/bound` |
| 6 | `engine/src/component_admission.rs::is_application_operation_import` | A namespace rule: `wasi` and `wamn` are platform. The registry's own comment says it replaced namespace rules. |
| 7 | `engine/src/declared_world.rs::linker` | A `match` on interface names that links each WASI interface by hand |
| 8 | `control/package/src/connection_requirement.rs` | `interface.starts_with("wamn:postgres/")` |
| 9 | `tools/component-virtualization.json` | The components whose WASI imports are rewritten (read earlier) |
| 10 | The WIT files under `wit/` | Each package's version. Eight registry rows restate a version from these files. |
| 11 | `tests/conformance/tests/capability_registry.rs` | Asserts the number of rows and the list of effect rows |

Two rows look stale (fact):

- `wamn:postgres` has a row for `0.1.0` and a row for `0.4.0`. `wit/wamn-postgres-0.4/package.wit` says 0.4.0 is the one version in the tree and the host serves only it.
- `wit/` has `wamn:operation` and `wamn:application-dispatch`. Neither has a row. The engine admits them through the namespace rule of row 6.

## Earlier rulings that meet this crate

Not decided again here.

- Standard WASI imports (Architecture tab): add the standard WASI packages to the list, rely on an empty host context, stop rewriting guest bytes. That adds rows to the table and removes place 9.
- Dynamic read queries (Architecture tab): an application may run them. `APPLICATION_DENIED_INTERFACES` refuses `wamn:postgres/client`, the only interface that takes SQL text. The two cannot both stand. Which interface a dynamic read uses is decided with the runtime.

## Options

| # | For | Option | What goes | What is lost |
| --- | --- | --- | --- | --- |
| 1 | finding 2 | Delete the `FirstParty` path | `PolicyProfile`, `screen_imports`, `denied_imports`, `DENIED_EGRESS_PKG`, `RawSocketImport`, the file header, about eight tests, the helper in `socket_test.rs` | The conformance test that platform components import no sockets. No production path runs that rule today. |
| 2 | places 3, 4, 5 | One table. Each row says which component type may import the package: application, integration, or both. The denied list becomes rows that applications do not have. | `integration_import`, `APPLICATION_DENIED_INTERFACES`, `is_denied_to_applications` | Nothing. One place to read. A denial is then by interface for two packages, so those rows are keyed by interface, not package. |
| 3 | stale rows | Remove the `wamn:postgres` `0.1.0` row. Add rows for `wamn:operation` and `wamn:application-dispatch`. | One row; the reason for the namespace rule in place 6 | Nothing, if no component imports 0.1.0. Not checked against built components, since nothing was built. |
| 4 | finding 1 | Move the three composed-component items to `wamn-component-composer`, which writes them | Three items leave this crate | Nothing, if the engine may depend on the composer. Not checked. |
| 5 | `build.rs` | Keep it. It refuses, at publish, a WASI version the host cannot link. | Nothing | A build script that runs `cargo metadata`. The alternative is to admit any `0.2.N` and let the host refuse at load. |
| 6 | place 11 | The conformance test stops asserting the row count and the effect list | Two assertions | Nothing. They change whenever a row is added. This follows the repository's own test rule 2. |

Places 6, 7 and 8 have no option here. Each is in another crate and is decided in that crate's tab; the direction from the single-source rule is that they read this table.

## Rulings and questions (2026-11-11)

### Dynamic SQL

Owner ruling: we must accept dynamic SQL.

What that means in this crate (fact): the entry `wamn:postgres/client` leaves `APPLICATION_DENIED_INTERFACES`, so an application component may import the interface that takes SQL text.

What this crate cannot do (fact): the `client` interface in `wit/wamn-postgres-0.4/package.wit` has `query`, `execute` and a `transaction` resource. It reads and writes. The ruling in the Architecture tab is that dynamic SQL is for queries and runs in a read-only transaction. An import list cannot tell a read from a write. That limit has to be applied by the host's Postgres plugin for an application caller. The plugin was not read for this tab.

### Why there is a build script

Owner question: why do we need the build script.

What it does (fact): it finds the WIT files of the locked `wasmtime-wasi` and writes one number, the highest `0.2.N` they declare. A `wasi:*` row then admits `0.2.0` up to that number and refuses anything higher.

Why a script and not a constant (fact, from its comment): so that a Wasmtime upgrade moves the number with no source edit.

What the number protects (fact, `engine/src/declared_world.rs::linker`): nothing at link time. The linker drops the version from the import name and links by interface name alone. The only reader of the number is `capability_row`.

So the script exists to refuse, at publish, a component built against a newer WASI patch than the host's Wasmtime ships. Inference: such a component fails at load only if it calls a function the older host lacks; one that does not would have worked.

Upstream wasmCloud has no such limit (from the earlier read of the upstream source).

| # | Option | What goes | What is lost |
| --- | --- | --- | --- |
| 5a | Drop the limit. A `wasi:*` row admits any `0.2.N`. | `build.rs`, `WASI_CEILING`, the patch arithmetic in `admits_version`, the test `wasi_ceiling_is_the_linked_wit_patch`, the `serde_json` build dependency | A refusal at publish for a too-new WASI patch. The host refuses at load if a function is really missing. |
| 5b | Keep it | Nothing | A build step that runs `cargo metadata` |

Option 5 above recommended keeping it. With the linker fact, 5a is the option that matches the delete preference.

### Rulings

Owner ruling (2026-11-11): accept. Options 1, 2, 3, 4, 5a and 6.

| # | Ruling |
| --- | --- |
| 1 | The `FirstParty` path, the file header and their tests are deleted. |
| 2 | One table. Each row says which component type may import it. `integration_import` and `APPLICATION_DENIED_INTERFACES` go. |
| 3 | The `wamn:postgres` `0.1.0` row goes. `wamn:operation` and `wamn:application-dispatch` get rows. |
| 4 | The three composed-component items move to `wamn-component-composer`. |
| 5a | The build script and the patch limit go. A `wasi:*` row admits any `0.2.N`. |
| 6 | The conformance test stops asserting the row count and the effect list. |
| Dynamic SQL | `wamn:postgres/client` is no longer refused to applications. |

Carried to other tabs:

- Runtime: the Postgres plugin must hold an application's raw SQL to a read-only transaction. Not read yet.
- Engine: `is_application_operation_import` and `declared_world.rs::linker` read this table. Ruling 3 removes the reason for the namespace rule.
- `control/package`: `connection_requirement.rs` reads this table.
- To check when the change is made: that the engine may depend on the composer (ruling 4); that no built component imports `wamn:postgres@0.1.0` (ruling 3).

## Not read

- The test half of `lib.rs`, beyond test names.
- `tests/conformance/tests/capability_registry.rs`, beyond the lines found by search.
- The runtime's plugin registration, which is the list of what the host really offers. It is the tenth place and belongs to the runtime tab.
- Where `wamn:modbus@1.0.0` is defined; `wit/` has no directory for it.

## Tests deleted, changed or moved (added 2026-11-12)

Checked at `epic/dep7` (b94833fd) by grep. Test names are fact; the mapping to rulings is by what each test names (inference).

Deleted:

- `src/lib.rs::an_application_component_is_refused_client_and_a_platform_component_is_not` (the `FirstParty` path, ruling 1; dynamic SQL ruling).
- `src/lib.rs::wasi_ceiling_is_the_linked_wit_patch` (the patch limit, ruling 5a).

Changed:

- `src/lib.rs` `wasi_rows_match_the_family_and_authored_rows_stay_exact`, `posture_is_declared_and_an_unregistered_package_has_none`, `integration_profile_grants_only_bound_sockets_and_ambient_imports`, `tenant_policy_is_exact_and_has_no_namespace_wildcard`, `tenant_policy_refuses_every_unlisted_authority_surface`: one table with a component type per row (ruling 2); no `wamn:postgres` 0.1.0 row; rows for `wamn:operation` and `wamn:application-dispatch` (ruling 3).
- `tests/conformance/tests/capability_registry.rs` (2 tests): stops asserting the row count and the effect list (ruling 6) and moves into this crate (tab 86 finding 7).

Moved: any tests of the three composed-component items, to `wamn-component-composer` (ruling 4). None found by name.
