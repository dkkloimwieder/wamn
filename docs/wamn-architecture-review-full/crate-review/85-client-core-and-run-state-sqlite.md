# 85 wamn-client and wamn-run-state-sqlite

Scope at `epic/dep7` (b94833fd): `crates/client/core` (`wamn-client`, 1,604 source lines, 1,614 test lines) and `crates/execution/run-state-sqlite` (437 source lines). Nothing built or run.

## Earlier work checked first

- Owner 2026-10-16: the terminal client is deleted; the generated SolidJS web client is the only operator client.
- Tab 71 5 A: one route client on hyper for the `wamn` CLI.
- Tab 01: `wamn-execution-contract` stops existing. Tab 77 2 A: routes are normalized once at build.
- Tab 78, U2: SQLx replaces `rusqlite`. Tab 58: the edge service, the one user of the SQLite store.

## Files read

- `client/core`: `Cargo.toml`, `lib.rs` header and items, `descriptor.rs` header, `route.rs` header.
- `run-state-sqlite`: `Cargo.toml`, `lib.rs` header.

## Callers read

- Repository grep for `wamn_client` and `wamn-client`; `crates/schema/generator/src` file list (client emitters); grep for `wamn_run_state_sqlite`.

## One purpose per file

- `wamn-client`: a route client's semantics (base URL, credentials and sessions, request validation against JSON Schema, envelopes, errors, paging) behind a `Transport` trait, plus `FieldDescriptor` for generated bindings. It has no HTTP transport of its own (no HTTP crate in its dependencies).
- `wamn-run-state-sqlite`: the edge's SQLite intent store, one file and one writer.

## Findings

**1. `wamn-client` has no production user.**

Fact:

- It is a dev-dependency only: `crates/control/dev/tests/support/control_operations.rs` uses it; `apps/wamn_receiving/tests/Cargo.toml` declares it and its sources do not use it (grep).
- The generator emits TypeScript clients only (`client_ts.rs`); there is no Rust client emitter. `descriptor.rs` describes fields "as generated code carries them" for a terminal or form; the terminal client is deleted.
- The `wamn` CLI calls routes with its own reqwest functions (`component_verbs::call`, `invoke`).

Options:

- A: delete `wamn-client`. The dev test support calls routes through the CLI's route client (tab 71 5 A).
- B: keep it as the one Rust route client: add a hyper transport, use it from the CLI, delete `descriptor.rs`.

Recommend A. Inference: the CLI calls control routes with a token and JSON, which needs no sessions, paging or client-side schema checks.

## Follows from earlier rulings

- U2: `wamn-run-state-sqlite` moves from `rusqlite` to SQLx SQLite; WAL, `synchronous=FULL` and the exclusive lock become SQLx connect options.
- Tab 01: `wamn-client`'s `wamn-execution-contract` import goes with finding 1.

## Fine as is

- `wamn-run-state-sqlite`: one owner of the edge intent file.

## Tests deleted, changed or moved

- Finding 1 A: `client/core/tests` (`captured.rs`, `invoke.rs`, `request.rs`, `session.rs`, `generated_schema.rs`, 1,614 lines) and its five entries in `tests/selection.toml`; `control_operations.rs` switches client.

## Not read

- `wamn-client` bodies; `run-state-sqlite` body.

## Rulings

- 2026-11-12: finding 1 A (wamn-client deleted; dev test support uses the CLI's route client).
