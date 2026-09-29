# Operation ids

Updated through: 2026-09-29, `main` at `e1d076e43`.

## 1. Goal

An operation id is `<package>:<interface>/<operation>`, for example `wamn-receiving:location/list`. It carries no `@version`. The package version is on the package coordinate only: `catalog.packages`, the release membership, and the `scope.package-version` of a declaration.

Today every operation id ends in the package version, for example `wamn-receiving:location/list@1.0.0`. A manifest change that needs a new package version therefore renames every operation of the package, in about 120 authored files (`wamn-cuqc`). After this change, a new package version renames no file.

## 2. Fixed rules

- One spelling of an operation id: `<package>:<interface>/<operation>`. Every producer writes it, and every reader reads it, with no second form and no strip step.
- The version of a package is a fact of the package coordinate. No operation id, grant, route entry, declaration key, statement set key or history row repeats it.
- A release holds one version of each package (`catalog.effective_release_packages` is keyed by the package id). So an operation id is unique in a release without a version.
- The contract, the route entry, the declaration entry, the client bindings and the host's statement set follow the new id. Each one keys on the same string.
- The WIT export name of an application operation is its operation id, as today. The host looks up the export by that name (`crates/platform/engine/src/operation/native_call.rs:195`), and admission compares the two (`crates/platform/engine/src/component_admission.rs`). So an application WIT package carries no version either. WIT permits a package without a version, and the generated world `<package>:generated` has none today.
- Platform WIT packages keep their versions: `wamn:node@0.1.0`, `wamn:postgres@0.2.0`, `wasi:*` and `wasmcloud:*`. They are interface versions of the platform, not operation ids.
- A validator refuses an operation id that contains `@`.
- The digest test of `wamn-iowb.3` makes sure that nothing else moves (section 4.5).

## 3. Current state

Measured on `main` at `e1d076e43` on 2026-09-29.

| Place | Today |
| --- | --- |
| Id builder | `canonical_operation_identity` writes `<prefix><interface>/<operation>@<version>` (`crates/schema/generator/src/manifest.rs:2232-2247`). The generated route and declaration entries build the id again with their own `format!` (`crates/schema/generator/src/generate/publication.rs:51`). |
| Contracts | The members `operation`, `grant` and `dependency.participant` hold the versioned id (`generate/contracts.rs:214-274`, `:740`). The `pre_commit` slot splits the id at `@` and panics without a version (`generate/contracts.rs:283-289`). |
| Write log | `log_operation` strips the version (`crates/schema/generator/src/write_log.rs:59-63`). The column `app_system.write_log.operation` holds the id without it. |
| Generated WIT | `package <package>:<group>@<version>;` in each `generated/wit/deps/*/package.wit` (`generate/wit.rs:102-104`, `:1763`, `:1896`). Each `export` line of the generated world (`generate/component.rs:253-256`), the pre-commit slot (`generate/wit.rs:1681`) and each participant `use` line (`generate/wit.rs:1829`, `:1877`) name the version. |
| Clients | The client IR copies the contract `operation` (`client_ir.rs:329-330`, `:403`). The TypeScript client writes it as `operation: "<id>"` (`client_ts.rs:525`). `operation_stem`, `link_label` and `local_name` split at `@` and also accept an id without a version. |
| Catalog model | `validate_canonical_operation` refuses an id without a version (`crates/catalog/model/src/package.rs:8-35`). `validate_canonical_operation_for_package` requires the suffix `@<package version>` (`package.rs:37-52`). The serving manifest and the component library call both (`serving_manifest.rs:960`, `:1284-1308`, `component_library.rs:741-825`). |
| Grants | `operation_grants.rs` removes a stale grant of a package only when the grant ends in `@<version>` (`crates/control/provision/src/operation_grants.rs:210`, `:295-297`). |
| Control | Publish builds a dependency id from the dependency version (`crates/control/lib/src/publish_release/components.rs:74`), an event-handler target (`publish_release.rs:1695`), and a projection check (`push_component.rs:949`). |
| Host and runtime | The host keys the statement set on the id (`crates/execution/host/src/operation.rs:585-596`, `operation/native_policy.rs:244-260`) and binds it as `app.operation` (`native_policy.rs:197`). Record history copies `app.operation` into its `operation` column (`deploy/sql/record-history.sql:200`). The engine grant check compares the id with the stored grants (`crates/platform/engine/src/router_delivery.rs:554-579`). None of these parse the id. |
| Composer | The composer plugs exports by the dependency id, the pre-commit slot and the participant name, read from the declarations (`crates/platform/component-composer/src/lib.rs:108-196`). |
| Database | The versioned id is data in `catalog.release_components.route_operation`, the keys of `catalog.component_library.operations`, the node `operation` in `catalog.wirings.graph_json`, the release manifest snapshots, and `app_system.permissions.permission`. No CHECK reads the id. |
| Authored files | 1,416 versioned ids under `apps/`, most of them generated. The authored ones are WIT `export` lines, `publication/attachments.json`, the `*.json.in` declarations, `web/src/routes.tsx`, tests, and a few Rust sources such as `apps/wamn_receiving/data/src/read.rs:228`. Outside `apps/`, the ids are mostly test data. |
| Web | `web/runtime` and `web/shell` treat the id as an opaque string. |

## 4. Design

### 4.1 One builder

`canonical_operation_identity` writes `<prefix><interface>/<operation>`. It takes the package identity for its prefix only. `publication.rs` calls it and loses its own `format!`. `log_operation` goes, and the write log takes the operation id as it is. The pre-commit slot is `<prefix><interface>/<operation>-pre-commit`.

### 4.2 Validators

`validate_canonical_operation` refuses an id that contains `@`, with a message that names the package coordinate as the place of the version. `validate_canonical_operation_for_package` makes sure that the id starts with the package prefix. It no longer reads a version. Its callers do not change.

### 4.3 WIT

A generated `package.wit` header is `package <package>:<group>;`. Each generated `export`, slot and `use` line loses its version. An authored `export` or `import` of an application operation loses its version too. Authored world packages such as `wamn:receiving-component@0.1.0` and platform imports keep theirs. They are not operation ids.

### 4.4 Grants

The grant prune removes a stale grant by the package prefix alone: a grant of the package that the desired set does not hold. The suffix match goes. A grant is the operation id, so a new package version keeps every grant whose operation stays.

### 4.5 The digest test

`the_fixture_publishes_what_it_published_before_generated_route_entries` (`crates/control/lib/src/publish_release/attachments.rs:422-447`) pins three values for the platform fixture:

1. The 12 route ids and their definition hashes. A definition holds no operation id, so these values do not change.
2. `FIXTURE_ATTACHMENTS_DIGEST`, over the merged attachment map. Its `operation` and `registered-operation` members change.
3. `FIXTURE_DECLARATION_DIGEST`, over the rendered declaration. Its keys, `registered-operation` and `pre-commit` change.

The issue that changes the ids takes the committed fixture documents from before the change and removes `@1.0.0` from each operation id with a script. It makes sure that the digests of the results equal the new values 2 and 3, and it records the script and both digests in the issue notes. The test then pins the new values. Values 1 do not change. Together these make sure that the id is the only thing that moved.

### 4.6 What moves

Removing the version moves every value that is computed over an operation id: the two digests above, every contract digest, the serving manifest digest and its mint vector (`crates/catalog/model/tests/serving_manifest_digest.rs`), each `projection_hash`, each wiring hash whose nodes name an application operation, and every component byte digest, because the export names are compiled in. The committed pins of base components (the overlay pin of `platform_fixture`, the Acme pin of `wamn_receiving`) take the new digests. Nothing else changes.

## 5. Issues

One branch, one agent (the routes agent). Each issue lands with its tests. No stop between them.

1. The builder and the validators. `canonical_operation_identity`, the pre-commit slot, `publication.rs`, `log_operation`, the two catalog validators and the grant prune take the new id. The catalog and generator unit tests change their expected ids, and one validator test refuses an id with `@`. Regenerate all six packages. Record the script result of section 4.5 and pin the new digests.
2. The WIT and the components. Generated WIT headers, exports, slots and `use` lines lose the version. Authored WIT, component sources and the data sources that spell an id change. Rebuild the components and move the committed base pins. The application publication tests and the Receiving and WMS live tests run as they are.
3. The authored publication and the clients. `publication/attachments.json`, the `*.json.in` declarations, `web/src/routes.tsx`, wirings, the tests under `apps/` and the test data outside `apps/` take the new id. Regenerate the clients. The web checks run as they are.
4. Closeout. `docs/architecture` states the id grammar and the place of the version. `wamn-cuqc` closes with the file count that a version change now renames, which is zero. Workspace test run, log path in the close reason, cluster stages noted pending, merge to main.

## 6. Out of scope

- An upgrade of a deployed database. Stored grants, route rows, library keys and history rows hold the old id. No rewrite of record history happens. The kind clusters are disposable, and the Google Cloud dev environment is provisioned again after this change.
- Platform WIT interface versions. `wamn:node@0.1.0` and `wamn:postgres@0.2.0` stay versioned. A platform interface change is a WIT change and goes to the owner.
- Two versions of one package in one release. A release holds one version of each package today, and this change depends on that rule.

## 7. Questions for review

1. The WIT export name follows the operation id, so application WIT packages lose their version (section 4.3). The other choice is a map from the unversioned id to a versioned export name in the host, admission and the composer. Is the version-free WIT package the choice?
2. A deployed database keeps the old ids (section 6), and the Google Cloud dev environment is provisioned again. Is that acceptable while `wamn-wq26` runs there?
