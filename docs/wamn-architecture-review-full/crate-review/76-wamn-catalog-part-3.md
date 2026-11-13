# 76 wamn-catalog, part 3

Scope: `crates/catalog/model` at `epic/dep7` (b94833fd), part 3 of 3: `host_route.rs` (958) with `host_route/contract_files.rs` (251) and 177 contract JSON files, `wiring.rs` (951), `wiring_compatibility.rs` (679), `connection.rs` (766), `workflow_gate.rs` (319), `signed_file_read.rs` (191), `feed.rs`, `package.rs`. Nothing built or run.

## Earlier work checked first

- Tab 29: `wiring_compatibility.rs` and `workflow_gate.rs` read as callers; finding 2 A: the router compiles from the catalog `WiringDocument`.
- Tab 31 1 B: host routes become native ingress service routes. Tab 72 2 B: control routes become a native HTTP service.
- Tab 25 2 B: signed file results go through `wamn:operation`.
- Tab 63 1 A: an application is its release. Tab 65 4 A: org admin everywhere; environment admin over its projects and applications; project admin over its applications; the order org, environment, project, application is carried to the registry crate.
- Tab 01: `wamn-execution-contract` stops existing (its parts go to `wamn-scalar` and `wamn-digest`).

## Files read

- `host_route.rs`: header, item list, `HostRouteSet`, `HostHandler`, `HostRouteAuthority`. `contract_files.rs` header; the `host_route/` tree (file count).
- Headers of `wiring.rs`, `wiring_compatibility.rs`, `connection.rs`, `workflow_gate.rs`, `signed_file_read.rs`, `feed.rs`, `package.rs`; `connection.rs` item list; `wiring.rs::fixture_graph`.

## Callers read

- Repository grep for `JsonSchema`, `schema_for`, `schemars::`, `ConnectionTypeDescriptor`, `fixture_graph`.

## One purpose per file

- `host_route.rs`, `contract_files.rs`: the routes the host serves itself, their contracts and who may call them.
- `wiring.rs`: the workflow graph document. `wiring_compatibility.rs`: check a graph against component facts. `workflow_gate.rs`: check a graph against a serving release.
- `connection.rs`: connection types and credential handles. `signed_file_read.rs`: file reads named by an operation's result. `feed.rs`, `package.rs`: as named.

## Findings

**1. Connection types derive `JsonSchema`, and no schema is generated from them.**

Fact: `connection.rs` derives `schemars::JsonSchema` on 8 types. The only `schema_for!` in the repository is `wamn-authoring-model::lib.rs` for `AuthoringDocument`, which uses no catalog connection type (grep). `schemars` is a dependency of `wamn-catalog` for these derives only.

Options:

- A: delete the derives and the `schemars` dependency of `wamn-catalog`.

Recommend A.

## Follows from earlier rulings

- Tab 65 4 A and the registry carry: `HostRouteAuthority` has `Member`, `Admin` (application admin), `OrgAdmin`, `ProjectAdmin` and `Granted`, and no environment admin. `ProjectAdmin` reads "org-admin, or project-admin in the project the request names". The authority set gains environment admin and follows the ruled order.
- Tab 72 2 B: the `Control` handlers of `HostHandler` move with the control service; the contract files stay as the API it serves.
- Tab 63 1 A: `workflow_gate.rs` resolves a node "within the packages of its application first, then within any package of the release"; with one application per release, it resolves within the release.
- Tab 01: the `wamn-execution-contract` imports (`TestSetCase`, `validate_cases` in `wiring.rs`) move to their new crates.
- Tab 72 3 A: `wiring.rs` header cites bead ids.

## Fine as is

- `wiring.rs`, `wiring_compatibility.rs` (tab 29), `signed_file_read.rs` (tab 25), `connection.rs` types, `feed.rs`, `package.rs`, `contract_files.rs`.
- `fixture_graph` is behind `cfg(any(test, feature = "test-util"))`.
- Host route handlers check the caller's role again inside their write transaction; that closes the gap between check and write.

## Tests deleted, changed or moved

- Finding 1: none.
- Follows: `HostRouteAuthority` tests gain the environment admin case; `workflow_gate.rs` application-first resolution tests change.

## Not read

- Bodies of `wiring.rs`, `wiring_compatibility.rs`, `connection.rs`, `workflow_gate.rs`.
- Whether the host gates a workflow again on every `workflow.start` or once per loaded release.

## Rulings

- 2026-11-12: finding 1 A (JsonSchema derives and the schemars dependency go).
