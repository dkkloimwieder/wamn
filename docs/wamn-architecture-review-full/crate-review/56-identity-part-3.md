# 56 identity, part 3: orgs, applications, connections, host memory

Scope: `crates/identity/platform/src/` `org.rs`, `application.rs`, `configuration.rs`, `paging.rs`, `connections/*`, `connection_projection.rs`, `auth_changes.rs`, `connection_changes.rs`, `kv_memory.rs`, `nats.rs` (3,872 lines). Nothing built or run.

## Files read

- Headers and public items of every file in scope.
- `paging.rs` (cursor); `connections/administration.rs` (dispatch, `validate_request`, `validates`); `connection_projection.rs` (document read and decode); `auth_changes.rs`, `connection_changes.rs` (publishers); `nats.rs`; the `registry.*` SQL of `org.rs`.

## Callers read

- Users of `validates`, `validate_connection_parameters`, `validate_version_parameters` (grep); `boon` users in the workspace.
- Users of `auth_changes`, `connection_changes`, `kv_memory`, `nats` (grep: 57 files); NATS connect code in the workspace.
- `registry.*` and `catalog.*` uses in this crate (tab 55 dependency check).

## Earlier rulings checked

- Tab 02 finding 5 (confirmed 2026-11-12): registry SQL lives only in the registry crate, renamed `wamn-registry`.
- Tab 17 (host memory, reader side): `moka` for the token map; finding 5 puts the connection subject parse in this crate.
- Tab 27 finding 3: `moka` pool per environment for administration.
- Tab 51 finding 1: the generator's cursor copy is deleted; `wamn-data-access::cursor` is the guest cursor.
- Tab 55: SQLx (finding 1), one error type (finding 5), name types (finding 4).

## One purpose per file

- `org.rs`: org membership and admin grants (system rows); also registry writes (finding 1).
- `application.rs`: the same grants as application rows in a project database.
- `configuration.rs`: platform configuration rows of a project database.
- `connections/*`: integration and connection administration; credential writes.
- `connection_projection.rs`: copy connection rows from the system database to a project database.
- `auth_changes.rs`, `connection_changes.rs`, `kv_memory.rs`: delete host memory keys after a commit.
- `nats.rs`: the NATS client of non-host platform processes (not identity; finding 6).
- `paging.rs`: list cursors for control and identity lists.

## Findings

**1. Registry SQL in the identity crate.**

Fact: `org.rs` (9 uses: create project, environment status, lists), `connection_projection.rs` (11, `registry.connection_projection`), `control.rs` (4), `connections/administration.rs` (4, `registry.configure_*`), `application.rs` (1). `connection_projection.rs` reads a JSON document from a SQL function and decodes it field by field (`rows_from_document`, `text`).

Options:

- A: the statements move to `wamn-registry` as typed functions (tab 02 finding 5, SQLx per U2); identity calls them. The projection returns typed rows; `rows_from_document` goes.

**2. Connection administration takes JSON text and an operation name.**

Fact: `execute`, `execute_with_store`, `execute_in_environment` each wrap `execute_in`; the request is a JSON string; `validate_request` lists allowed keys per operation name by hand; behaviour switches on strings such as `"connection.write"`; the environment key is built with `format!("{org}--{project}--{env}")`.

Options:

- A: one typed request enum (serde, tagged by operation), parsed once; one `execute` with its optional store and environment as arguments. Delete the key lists and the two wrappers. The environment key comes from `wamn-registry`.

**3. A hand-written JSON Schema check.**

Fact: `connections/administration.rs::validates` checks a subset of JSON Schema (type, enum, lengths, bounds, items, required, properties, `additionalProperties`) for integration parameters, credentials and auth. `boon` is used in `wamn-catalog`, `wamn-engine` and `wamn-client-core`. These schemas are data from integration types, read at run time.

Options:

- A: `boon` (Draft 2020-12) checks them; the custom `immutable` keyword stays as its own check. Delete `validates`.

**4. A second cursor format.**

Fact: `paging.rs` encodes `{v, operation, scope, order, key}` as base64 of canonical JSON for control and identity lists. Guests use `wamn-data-access::cursor`.

Options:

- A: one versioned keyset cursor type in one crate, used by guests and by control lists.
- B: keep two: guest data cursors and control list cursors have different scopes and readers.

Detail (fact, `wamn-data-access/src/cursor.rs` and `cursor/id.rs`, 411 lines; `paging.rs`, 183 lines):

- App cursor: the guest's generated query operations over application models. Shape `{direction, field, id, key, v}`. The client picks the sort field and direction from the model's declared sorts; the key is typed by its column (text, UUID, timestamp, numeric) with the row `id` as tie-breaker. Runs in WASM.
- Platform cursor: control and identity lists over the system database (orgs, projects, environments, members, audiences), served by host control routes. Shape `{v, operation, scope, order, key}`. Each list has one fixed order (four `Order` values); keys are text; the cursor is bound to the operation and scope so it cannot be replayed on another list. `ListPage` also parses `limit` (1 to 100) and writes `{rows, next_cursor, total}`.
- Shared: unpadded base64url of canonical JSON, version 1, and refusal of any spelling the encoder would not write. About 20 lines each.
- Platform models in a project database (user, role) are served by generated CRUD, so they use the app cursor already.

Revised options (2026-11-12):

- A: one cursor envelope (encode, decode, canonical check, version) in one crate; each side keeps its own payload struct. About 20 lines become one copy.
- B: keep two; they share only the envelope.

**5. Two memory writers that differ only in keys.**

Fact: `AuthChanges` and `ConnectionChanges` each hold `Option<async_nats::Client>`, turn their drops into `Deletion`s and call `kv_memory::delete`; `ConnectionChanges::connect` goes through `AuthChanges`.

Options:

- A: one writer with one client; each drop set names its bucket and keys. Delete the second client wrapper and the `From` conversion.

**6. NATS connect is written in many places.**

Fact: connect code in `identity/platform/src/nats.rs` (`PlatformNats::from_env`, rustls by hand), `runtime/src/kv_memory.rs`, `control/environment/src/platform/broker.rs`, `control/environment/src/event_advisories.rs`, `services/host`, `services/cdc-reader`.

Options:

- A: one connect function with one TLS setup, in one crate that all six can link; `nats.rs` leaves identity. The home is decided with the NATS owner direction (one NATS); carried to the host and control tabs.

## Follows from earlier rulings

- Tab 55 finding 5: `AdministrationError`, `GrantError`, `ConnectionError`, `ProjectionError` fold into the one crate error type.
- Tab 55 finding 1: SQLx and typed rows here too.
- Tab 54 finding 5: `open_environment_store` takes the store name; `put` takes the coordinate.
- Tab 55 dependency check: `paging.rs` uses `wamn-digest` for canonical JSON (tab 01).

## Tests deleted, changed or moved

- Finding 2: tests of allowed-key lists become serde tests of the request enum.
- Finding 3: tests of `validates` go; `boon` is not retested.

## Not read

- `org.rs` grant logic; `application.rs::mutate_grant`; `configuration.rs` writes; `connections/references.rs` in full.

## Rulings

- 2026-11-12: all findings A. Finding 4 is the revised A: one cursor envelope (encode, decode, canonical check, version) in one crate; the app and platform cursors keep their own payloads. Finding 6: the home of the one NATS connect function is decided in the host and control tabs.
