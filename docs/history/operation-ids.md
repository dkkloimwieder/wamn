# Operation ids

Updated through: 2026-09-29, `main` at `e1d076e43`.

## 1. Goal

The package version is authored once, in `wamn.json` as `package.version`, and derived everywhere else. An authored file names an operation by its reference, `<package>:<interface>/<operation>`, for example `wamn-receiving:location/list`. Generation and publish derive the sealed operation id, `<package>:<interface>/<operation>@<version>`, from the reference and the package version.

The sealed ids do not change. WIT packages, component worlds, contracts, the files under `generated/publication/`, the client names, grants and the host's statement sets keep the version, as WASI practice requires.

Today an authored file spells the sealed id, so a manifest change that needs a new package version edits about 120 authored files (`wamn-cuqc`). After this change, a version change is one line in `wamn.json` and a regeneration.

## 2. Fixed rules

- An authored file names an operation by its reference, with no `@version`. A reference in an authored file that carries a version refuses, with a message that names `wamn.json` as the place of the version.
- The version of a reference comes from exactly one place. A reference to an operation of the package itself takes `package.version`. A reference to an operation of a base package takes the `version` of that base in `base_dependencies`.
- The sealed id is `<reference>@<version>`. One function builds it: `canonical_operation_identity` (`crates/schema/generator/src/manifest.rs:2232-2247`). Every producer and every resolver calls it.
- Generated files carry the sealed id: the WIT package headers and the world, the contracts, `generated/publication/`, the clients and the no-op crates.
- Publish, the declaration render and `resolve_attachment` resolve an authored reference to its sealed id before any reader sees it. No reader downstream of them sees a reference.
- The digest test of `wamn-iowb.3` stays with its pinned values. A sealed id that changes is a defect of this change.

## 3. Current state

Measured on `main` at `e1d076e43` on 2026-09-29.

| Place | Today |
| --- | --- |
| Id builder | `canonical_operation_identity` builds the sealed id. `generate/publication.rs:51` builds it again with its own `format!`. |
| Authored WIT | `component/wit/authored.wit` exports each custom operation and imports each pre-commit slot by its sealed id, for example `export wamn-receiving:receiving/record-receipt@1.0.0;`. It includes the generated world. |
| Authored route entries | `publication/attachments.json` spells the sealed id in `operation` and `registered-operation` of each custom operation route. |
| Authored declarations | `publication/components/*.json.in` spells the sealed id in each operation key, `registered-operation` and `pre-commit`, and repeats the version in `scope.package-version`. |
| Wirings | A wiring node names an application operation by its sealed id, for example `apps/client_acme_receiving/publication/wirings/`. |
| Web routes | `web/src/routes.tsx` keys `onOpen` and `onFill` by the sealed id, for example `apps/wamn_wms/web/src/routes.tsx:68`. |
| Tests | Application tests spell sealed ids as constants, for example `apps/client_acme_receiving/tests/acme_overlay_publication.rs:17-35`. |
| Readers | `read_package_attachments` and `render_declaration_document` merge the authored file with the generated file (`crates/schema/generator/src/route_schema.rs`, `crates/control/lib/src/component_declaration.rs`). `resolve_attachment` checks a definition hash and resolves an input schema reference (`route_schema.rs:219-243`). None of them resolves an operation id. |
| Count | 1,416 sealed ids under `apps/`. Most are generated. The authored ones are in the places above. |

## 4. Design

### 4.1 One builder, one resolver

`generate/publication.rs` calls `canonical_operation_identity` and loses its own `format!`. A new function, `resolve_operation_reference`, takes a reference and the package manifest. It refuses a reference that carries `@`. It finds the owning package by the prefix: the package itself, or one entry of `base_dependencies`. A prefix that names neither refuses. It returns the sealed id through `canonical_operation_identity`.

### 4.2 Authored WIT

The generated world also exports each custom operation and imports each pre-commit slot, from the `custom_operations` of `wamn.json`. `authored.wit` keeps its own package header and its `include` of the generated world, and loses every `export` and `import` of an application operation. An authored world package such as `wamn:receiving-component@0.1.0` is the component's own version, not the package version, and stays.

### 4.3 Route entries, declarations and wirings

An authored route entry, declaration or wiring names each operation by its reference. The declaration template loses `scope.package-version`. The renderer fills it from `wamn.json`, as it fills `__TENANT_ID__`. `read_package_attachments`, `render_declaration_document` and the wiring reader resolve each reference with `resolve_operation_reference`. Publish reads through them, so it sees sealed ids only. `resolve_attachment` refuses an authored `operation` or `registered-operation` that carries a version.

### 4.4 Web routes and tests

The generated TypeScript client gives each operation its reference beside its sealed id. The query table keys `onOpen` and `onFill` by the reference, so `routes.tsx` names references. So a page cannot show two versions of one package at once, and that loss against the sealed id is intended. An application test names an operation by its reference and gets the sealed id from the generated client, or from `resolve_operation_reference` over the package `wamn.json`. A test value that is opaque data, such as a history row in `apps/wamn_receiving/data/src/read.rs:228`, stays as it is.

### 4.5 The digest test

`the_fixture_publishes_what_it_published_before_generated_route_entries` (`crates/control/lib/src/publish_release/attachments.rs:422-447`) keeps its three pinned values: the 12 route definition hashes, `FIXTURE_ATTACHMENTS_DIGEST` and `FIXTURE_DECLARATION_DIGEST`. The fixture's authored files change to references, and the readers resolve them. Equal digests make sure that the resolved documents are the documents of today, byte for byte.

### 4.6 What moves

No sealed id moves, so no contract digest, component digest, base pin, route definition hash or serving manifest digest moves. Only the authored files change. A component whose authored WIT loses its exports keeps the same world, because the generated world now carries them. The issue that moves them makes sure that each component world is the same before and after.

## 5. Issues

One branch, one agent (the routes agent). Each issue lands with its tests. No stop between them.

1. The resolver. `resolve_operation_reference` and its refusals, and `publication.rs` calls `canonical_operation_identity`. Generator unit tests: a reference of the package, a reference of a base, a reference with a version, and an unknown prefix.
2. WIT. The generated world exports the custom operations and imports the pre-commit slots, and every `authored.wit` loses them. Regenerate all packages. Every component world is the same before and after.
3. Route entries, declarations and wirings. The readers resolve references, the renderer fills `scope.package-version`, and `resolve_attachment` refuses a versioned authored id. Every authored publication file names references. The digest test passes with its pinned values, and the application publication tests run as they are.
4. Web routes and tests. The TypeScript client carries references, the query table keys by them, `routes.tsx` and the application tests name references. Regenerate the clients. The web checks run as they are. This issue changes `web/ui`, so the routes agent tells the table agent which files go first.
5. Closeout. `docs/architecture` states where the version is authored and which files carry the sealed id. `wamn-cuqc` closes with the count of authored files that a version change edits, which is one. Workspace test run, log path in the close reason, cluster stages noted pending, merge to main.

## 6. Out of scope

- Sealed ids in stored data. Grants, route rows, library keys, history rows and write log rows keep what they hold. They are sealed ids, and a new package version writes new ones, as today.
- Platform WIT interface versions, such as `wamn:node@0.1.0` and `wamn:postgres@0.2.0`. They are not operation ids.
- The version of a base in `base_dependencies`. It is the dependent package's own record of which base it overlays, with the base digest, and it stays authored.
