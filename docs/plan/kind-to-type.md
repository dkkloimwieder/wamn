# Platform `kind` to `type` migration

Status: Draft. Section 1 accepted 2026-09-29 (wamn-sfea.1). Section 2 accepted 2026-09-30 (wamn-sfea.2). Section 3 accepted 2026-09-30 (wamn-sfea.3). Section 4 accepted 2026-09-30 (wamn-sfea.4). Section 5 accepted 2026-09-30 (wamn-sfea.5). Section 6 holds an owner ruling (wamn-sfea.6).

Measured on `main` at `1045b4fad`.

Context: [platform-ui.md §0](platform-ui.md) makes `type` the platform word and `kind` a retired one. This file is the precursor specification that §0 requires.

## 1. Surfaces

This section lists every class of `kind` occurrence in tracked files. It changes no code.

Scope rules:

- All counts are over tracked files at the measured commit.
- `.beads/` is out of scope. It holds 5170 matches of `kind` in any case, all in issue text and exports.
- `docs/history/` is out of scope. It holds 33 matches. These are frozen records and stay as written.
- `docs/archive/` does not exist at this commit. It was deleted at `ff04842e`.
- All other tracked files hold 9834 case-insensitive matches of `kind` inside a word.

Column meanings:

- **Owned**: `yes` if WAMN defines the name. `no` if a third party defines it. `ask` if the owner must decide.
- **Serialized**: `yes` if the name reaches bytes that leave the process (a file, a hash input, a wire body, a column). `value` if only the values of a `*Kind` type are serialized and the word `kind` is not. `no` if the name stays in source.
- **Hashed**: whether the key or the value feeds a definition hash, a manifest digest or a schema identity. wamn-sfea.2 resolved every former `tbd` cell. §2 gives the evidence.
- **Count**: matches from the commands in §1.7. A count is a match count unless the row says otherwise.

### 1.1 Serialized authored surfaces

| # | Surface | Key and values | Owned | Serialized | Hashed | Cite | Count |
|---|---|---|---|---|---|---|---|
| A1 | `apps/*/wamn.json` custom operations | `custom_operations.*.kind`: `command`, `projection`, `event_handler`. Parsed into `CustomOperationDeclaration.kind: CustomOperationKind` | yes | yes | yes. The `wamn.json` bytes are the `manifest_sha256` that seals the package coordinate (§2) | `apps/wamn_receiving/wamn.json:377`, `crates/schema/generator/src/manifest.rs:58` | 21 |
| A2 | `apps/*/publication/attachments.json` | Top-level `kind` and `definition.kind`, value `http` | yes | yes | yes. `definition.kind` is inside the canonical definition that `definition-hash` covers. The top-level `kind` enters the serving manifest digest (see G8) | `apps/wamn_receiving/publication/attachments.json:3`, `:10`. Hash check at `crates/control/lib/src/publish_release/attachments.rs:238` and `crates/schema/generator/src/route_schema.rs:219`. Hash function at `apps/platform/execution/contract/src/lib.rs:54` | 36 (G1 holds the other 68) |
| A3 | Test fixture packages | Same keys as A1 and A2 in fixture copies | yes | yes | yes, at test time only. The same hash code reads them. No sealed coordinate holds them (§2) | `crates/control/lib/tests/fixtures/observer_package/wamn.json:32`, `services/ctl/tests/fixtures/ui_scaffold/publication/attachments.json:3` | 6 |
| A4 | Client TUI classification fixture | `kind` holds an operation kind | yes | yes | no | `crates/client/tui/tests/data/classification-cases.json:31` | 6 |
| A5 | Application column named `kind` in `wamn.json` | The record-history column `kind` (see P9), named as a projection path and a row field. Becomes `type` (§1.8) | yes | yes | yes. Same as A1. The column itself moves the digests of P9 (§2) | `apps/wamn_receiving/wamn.json:1087` | 3 |
| A6 | Operator recovery CRD fixture | `resource_kind` holds a Kubernetes resource kind such as `Host`. Stays (§1.8 rule) | no | yes | no | `tests/integration/fixtures/operator-recovery/deployment-crds-001/crd-inventory.json:11` | 15 |

No authored TOML file carries a `kind` key.

### 1.2 Generated serialized surfaces

| # | Surface | Producer | Owned | Serialized | Hashed | Cite | Count |
|---|---|---|---|---|---|---|---|
| G1 | `apps/*/generated/publication/attachments.json` | `json!` with top-level `kind` and `definition.kind` | yes | yes | yes. Same as A2 | `crates/schema/generator/src/generate/publication.rs:61`, `:70` | 68 |
| G2 | `generated/contracts/*/*.operation.json` | `"kind"` from `OperationKind` or action name | yes | yes | yes. It becomes `ServingRoute.kind` (G8), inside the manifest digest | `crates/schema/generator/src/generate/contracts.rs:246`, `:837` | 57 |
| G3 | `generated/contracts/*/query.input.json` | `pagination.kind`, value `keyset` | yes | yes | no. Only the client IR reads it, and no client output carries it (§2) | `crates/schema/generator/src/generate/contracts.rs:1172` | 10 |
| G4 | `generated/source-map/*.json` | `"kind"` from the custom operation kind | yes | yes | no. Only SQLx metadata preparation reads source maps (§2) | `crates/schema/generator/src/generate/contracts.rs:188` | 8 |
| G5 | `generated/package-weld.json` | `#[serde(tag = "kind")]` on `ConstraintKind`, `ColumnDefault`, `ColumnGeneration` | yes | yes | yes. The schema IR feeds `verified_schema_state_id` | `crates/schema/introspection/src/ir.rs:316`, `:227`, `:283`. Hash at `crates/schema/generator/src/generate.rs:415` | 72 |
| G6 | Client IR | `OperationIr.kind: String`, serde, kebab-case | yes | yes | no. The IR lives in memory. Only tests compare its canonical bytes (§2) | `crates/schema/generator/src/client_ir.rs:329` | 1 field |
| G7 | Screen plan and generated clients | `SuppliedField.kind: SuppliedKind` in the plan. Emitted as `kind:` in generated TypeScript and TUI source | yes | yes, as generated source | no hash. It moves client UI bytes (§2) | `crates/schema/generator/src/client_plan.rs:128`, `crates/schema/generator/src/client_ts.rs:567`, `crates/schema/generator/src/client_tui.rs:469` | TS 35, TUI 148 |
| G8 | Serving (release) manifest | `kind` field on `ServingRoute` (`OperationKind`), `ServingAttachment`, `AttachmentWire`, `RouteAttachment`, `WiringAttachment` (`AttachmentKind`) | yes | yes | yes. `ServingManifest::digest` covers the canonical bytes | `crates/catalog/model/src/serving_manifest.rs:390`, `:441`, `:456`, `:639`, `:655`. Digest at `:886` | 5 fields |
| G9 | Catalog canonical frames | `("kind", ...)` frame in `Source` and attachment definition identities | yes | yes | yes. The frames are the canonical bytes | `crates/catalog/model/src/lib.rs:623`, `:723`, `:875` | 3 |
| G10 | Publish-release package source contract | `Contract.kind: OperationKind`, serde | yes | yes | yes. It is the reader of G2 and feeds G8 | `crates/control/lib/src/publish_release/package_sources.rs:147` | 1 field |
| G11 | Generated app code | `StatementErrorKind` use in `generated/data/error.rs`. History `kind` column in generated WIT codec, accessor and client | yes | yes, as generated source | yes for the history codec and WIT, which compile into the `receiving` component digest. No for the `error.rs` local names (§2) | `apps/wamn_receiving/generated/data/error.rs:154`, `apps/wamn_receiving/generated/wit/receiving_load_purchase_order_history_codec.rs:55` | 30 and 17 |
| G12 | Stored caller outcome | `StoredCallerOutcome.kind: String`, serde, kebab-case | yes | yes | no | `crates/execution/run-state/src/transitions.rs:57` | 1 field |

### 1.3 Wire surfaces

| # | Surface | Name | Owned | Serialized | Cite | Count |
|---|---|---|---|---|---|---|
| W1 | Authoring HTTP API | `#[serde(tag = "kind")]` on `AuthoringCommand`, `AuthoringQuery`, `GateRefusal`, `PublishRefusal`, `GetReportRefusal` | yes | yes | `crates/authoring/model/src/lib.rs:189`, `:210`, `:465`, `:516`, `:552` | 5 |
| W2 | Management refusal bodies | JSON `{"kind": ...}`, such as `authorization-denied` and `unsupported-contract-version`. `ctl dev` renders them | yes | yes | `services/scenario-worker/src/management.rs:728`, `services/ctl/src/dev/tui.rs:635` | 25 in scenario-worker |
| W3 | Router delivery WIT | `enum failure-kind` and `delivery-failure.kind`. Lowered from `FailureKind` | yes | yes | `crates/execution/host/wit/deps/wamn-router-delivery-0.2/package.wit:46`, `:60`, `crates/execution/workflow/src/wiring_delivery.rs:429` | 2 |
| W4 | Generated application WIT | `load-purchase-order-history-row.kind: string`, the history column (P9). Becomes `type` | yes | yes | `apps/wamn_receiving/generated/wit/deps/wamn-receiving-receiving/package.wit:35` | 1 |
| W5 | JetStream router tap | `source-kind` / `source_kind` on `RouterTapWire` and `RouterTapRecord` | yes | yes | `crates/platform/runtime/src/plugins/wamn_jetstream.rs:251`, `:277` | 37 |
| W6 | Connection generation identity | `credential-kind` identity label. `CredentialKind` values | yes | yes | `crates/platform/runtime/src/connection_generation.rs:230` | 1 |
| W7 | Event advisory | Already `type` on the wire through `#[serde(rename = "type")]`. Only the Rust names `kind` and `DeliveryAdvisoryKind` remain. The Rust names follow, nothing else | yes | no (name) | `apps/platform/events/wire/src/lib.rs:285` | 1 |
| W8 | Web runtime | `OperationContract.kind` read from generated clients. `SuppliedField.kind` values | yes | yes | `web/runtime/src/wire.ts:41`, `web/runtime/src/transport.ts:382`, `web/runtime/src/supplied.ts:45` | 7 |
| W9 | Web table filters | `SetFilter.kind` discriminant. Values appear in URLs, the key does not. Becomes `type`, a rename only | yes | value | `web/ui/src/table/column-filter.tsx:26`, `web/ui/src/table/set-view.ts:41` | 34 |
| W10 | Web tests and gallery | Fixtures of W8 | yes | no | `web/runtime/test/classification.test.ts:42`, `web/components/gallery/memory.tsx:28` | 12 |
| W11 | Registry JSON | `#[serde(tag = "kind")]` on `Placement`. Not hashed | yes | yes | `crates/control/registry/src/types.rs:286` | 1 |
| W12 | Trace attribute | `wamn.caller_credential_kind` | yes | yes | `crates/execution/host/src/operation.rs:191` | 1 |

`*ErrorKind` values do not appear on the wire under a `kind` key. They leave the process as `code` strings. wamn-sfea.6 should confirm this per family.

### 1.4 Persisted surfaces

Database names: `wamn_system` is the control database (`SYSTEM_SCHEMA_SQL` and `CONTROL_PORTABLE_STORE_SQL` in `crates/control/provision/src/lib.rs:151`). Project-env is each provisioned project database (`CATALOG_SCHEMA_SQL` in `crates/catalog/model/src/lib.rs:77`, plus `run-state.sql` and `app-schema.sql`).

| # | Database | Table | Column and constraints | Owned | Cite |
|---|---|---|---|---|---|
| P1 | wamn_system | `registry.orgs` | `placement_kind`, `orgs_placement_kind_check`, pool check | yes | `deploy/sql/system-schema.sql:147` |
| P2 | wamn_system | `identity.principals` | `kind`, `principals_kind_check`, `UNIQUE (id, kind)`, `UNIQUE (kind, subject)`, three checks that read `kind` | yes | `deploy/sql/system-schema.sql:259` |
| P3 | wamn_system | `identity.pats` | `principal_kind`, `pats_principal_kind_check`, FK `(principal_id, principal_kind)` to `principals (id, kind)` | yes | `deploy/sql/system-schema.sql:346` |
| P4 | wamn_system | `identity.password_credentials`, `identity.password_tokens`, `identity.password_logins`, `identity.project_env_memberships` | `principal_kind` with an inline check and FK to `principals (id, kind)` | yes | `deploy/sql/system-schema.sql:384`, `:402`, `:425`, `:580` |
| P5 | wamn_system | `provisioning.sagas` | `kind`, `sagas_kind_check` | yes | `deploy/sql/system-schema.sql:731` |
| P6 | wamn_system | `provisioning.copy_sagas` | `kind`, `copy_sagas_kind_check` | yes | `deploy/sql/ops-schema.sql:30` |
| P7 | wamn_system | `catalog.authoring_command_audit` | `command_kind`, `principal_kind`, inline checks | yes | `deploy/sql/control-portable-store.sql:270` |
| P8 | project-env | `catalog.package_definition_owners` | `definition_kind`, inline check, part of the key | yes | `deploy/sql/catalog-schema.sql:59` |
| P9 | project-env and wamn_system | Every `<relation>_history` table | `kind` (`insert`, `update`, `delete`), `<history>_kind_check`, column grant. Becomes `type` and `<history>_type_check` in both databases | yes | `deploy/sql/record-history.sql:117`, `:135`, `deploy/sql/app-schema.sql:525`, `apps/platform/data/record-history/src/lib.rs:55` |
| P10 | project-env | `wamn_run.runs` | `caller_outcome_kind`, `fail_kind`, `runs_caller_outcome_kind_check`, `runs_fail_kind_check` | yes | `deploy/sql/run-state.sql:377`, `:387`, `crates/schema/control/src/run_plane/declarations.rs:104`, `:116` |
| P11 | project-env | `wamn_run.effect_attempts` | `generation_fact_kind`, inline checks | yes | `deploy/sql/run-state.sql:603` |
| P12 | project-env | `wamn_run.operator_run_actions` | `action_kind`, `principal_kind`, `operator_run_actions_kind_check`, `operator_run_actions_principal_kind_check` | yes | `deploy/sql/run-state.sql:795`, `crates/schema/control/src/run_plane/declarations.rs:377`, `:401` |
| P13 | edge SQLite | intent store | `outcome_kind` with a check. Retired upstream at `78d02a6d8` (wamn-4afx.1), which removed the column. No longer in scope | yes | `crates/execution/run-state-sqlite/src/lib.rs:30` |
| P14 | none | Query result aliases | `AS relation_kind`, `AS object_kind` and similar. These are not persisted but are WAMN names | yes | `crates/schema/introspection/src/postgres.rs:305`, `crates/control/provision/src/sql/database_grants.rs:68` |
| P15 | Kubernetes Secrets | PAT Secrets of `provision-project-env` | Annotation `wamn.io/principal-kind` (value `service`). Installed Secrets carry it. Becomes `wamn.io/principal-type` (§4.9 ruling 4) | yes | `crates/control/lib/src/provision_project_env/pat_secrets.rs:267`, read by `deploy/mvp/bootstrap.sh:118` |

Counts: matching lines in `deploy/sql` are 79 (system-schema 43, run-state 17, catalog-schema 5, record-history 5, control-portable-store 3, ops-schema 3, app-schema 2, postgres-init 1). The postgres-init line is prose about the kind cluster. Rust code names these persisted columns 222 times. P14 has 20 aliases. Re-measured for wamn-sfea.4 at `2d9700969`: the Rust count is 218, because `crates/schema/control/src/run_plane/tests.rs` lost 4 references. The `deploy/sql` line counts and the P14 count are unchanged.

No PostgreSQL enum type and no index is named `*kind*`. The unique constraints in P2 and P8 are the only keys that include a `kind` column.

### 1.5 Rust type families

Counts come from the R1 to R5 commands in §1.7. A family is a declared `enum`, `struct` or `type` whose name contains `Kind`. Distinct types count unique declared names. References count every word match of those names in tracked `.rs` files.

| # | Family | Distinct types | References | Owned | Serialized | Examples |
|---|---|---|---|---|---|---|
| R1 | Non-error `*Kind`, serde-serialized | 12 | 407 | yes | yes (value, plus key where G and W rows say so) | `AttachmentKind` `crates/catalog/model/src/lib.rs:660`, `OperationKind` `crates/catalog/model/src/serving_manifest.rs:352`, `FailKind` `crates/execution/run-state/src/status.rs:157`, `ConstraintKind`, `CustomOperationKind`, `SourceKind`, `WiringFailureKind`, `RouterTapSourceKind`, `DeliveryAdvisoryKind`, `AuthoringCommandKind`, `AuthoringQueryKind`, `GenerationFactKind` |
| R2 | Non-error `*Kind`, serialized by hand (`as_str`, WIT lowering, SQL) | 6 | 312 | yes | value | `PrincipalKind` `crates/identity/platform/src/lib.rs:142`, `FailureKind` `crates/execution/workflow/router/src/walk.rs:55`, `CredentialKind` `crates/platform/engine/src/flow_http_routing.rs:378`, `DefinitionKind`, `WorkloadRoleScopeKind`, `UnreadableRegistrationsKind` |
| R3 | Non-error `*Kind`, internal only | 16 | 584 | yes | no | `RunPlaneActionKind` `crates/schema/control/src/run_plane.rs:148` (225 refs), `SuppliedKind`, `InputKind`, `ValueKind`, `OperationRefusalKind`, `CandidateExecutionRefusalKind`, `DeliveryFailureKind`, `CredentialConnectionKind`, `WorkloadSecretBodyKind`, `StatusKind`, `OutcomeKind`, `EntryKind`, `ProfileKind`, `InvalidBareSchemaNameKind`, `CredentialKindSnapshot`, record-history `Kind` |
| R4 | Test-support types for Kubernetes and the kind tool | 6 | 31 | no | yes | `KindCluster` `test-support/infrastructure/rendering.rs:792`, `KindNode`, `ClusterKind`, `WorkloadKind`, `CertificateKind`, cargo `DependencyKind` `crates/control/lib/src/dev/native_tui.rs:473` |
| R5 | `*ErrorKind` | 74 | 2860 | yes | value (as `code`) | `GenerateErrorKind` `crates/schema/generator/src/error.rs:6` (344 refs), `AccessErrorKind` (5 generated copies), `StatementErrorKind`, `MintManifestErrorKind`. Only `NodeErrorKind` `crates/execution/run-state/src/status.rs:254` derives serde |
| R6 | Rust field, variable, function and test names in lowercase (`kind`, `fail_kind`, `kinds`, ...) | n/a | 3938 | mixed | only via G, W, P rows | 3211 are bare `kind`. 72 are `relkind` (not owned, see N6) |

R1 through R4 total 40 distinct non-error types and 1334 references. The R5 reference count includes 78 matches of the bare name `ErrorKind`. One WAMN enum has that name, in `crates/platform/runtime/src/plugins/connection_http/transport.rs:315`. It is in scope and goes with the `*ErrorKind` decision (wamn-sfea.6). Most of the 78 are `std::io::ErrorKind` (N7). R5 leaves out 11 matches of four async-nats error kinds (`ConsumerInfoErrorKind`, `GetStreamErrorKind`, `SubscribeErrorKind`, `RawMessageErrorKind`), which N9 does not count either.

### 1.6 Not WAMN-owned, stays as is

| # | Class | Owned | Count |
|---|---|---|---|
| N1 | Kubernetes `kind:` in YAML (charts, CRDs, manifests), 50 files | no | 246 |
| N2 | Kubernetes `listKind` in CRDs | no | 37 |
| N3 | Kubernetes `"kind": "<Kind>"` in JSON fixtures | no | 39 |
| N4 | Kubernetes `"kind": "<Kind>"` built in Rust (`json!`, test-support) | no | 48 |
| N5 | The `kind` cluster tool (`kind create`, `kind-gate`, `KindCluster`, node names) | no | 75 |
| N6 | PostgreSQL catalog columns `relkind`, `prokind` | no | 81 |
| N7 | `std::io::ErrorKind` | no | 47 |
| N8 | OpenTelemetry `SPAN_KIND_*` | no | 32 |
| N9 | Third-party Rust APIs: `KeyEventKind` (crossterm), `TypeDefKind` (wit-parser), `AckKind` (async-nats), `MeterKind` (wash-runtime), cargo `dep_kinds` | no | 51 |
| N10 | English prose in Markdown (outside `docs/history`) | no | 115 |
| N11 | English prose in Rust comments (lines) | no | 168 |

The CRD names that Kubernetes `kind:` carries, such as `Host` and `WorkloadDeployment`, are WAMN-owned values under a Kubernetes key. The key stays.

### 1.7 Reproduce the counts

Run from the repository root. Each count is `git grep -h -o ... | wc -l` unless noted.

```sh
# Scope totals
git grep -h -o -i -E '[a-z0-9_]*kind[a-z0-9_]*' -- ':!.beads' ':!docs/history' ':!docs/archive' | wc -l   # 9834
git grep -h -o -i -E '[a-z_]*kind[a-z_]*' -- .beads | wc -l                                           # 5170
git grep -h -o -i -E '[a-z_]*kind[a-z_]*' -- docs/history | wc -l                                     # 33

# 1.1 Authored
git grep -h -o -E '"kind": *"(command|projection|event_handler)"' -- 'apps/*/wamn.json' | wc -l      # A1 21
git grep -h -o -E '"kind": *"' -- 'apps/*/publication/attachments.json' ':!apps/*/generated/*' | wc -l  # A2 36
git grep -h -o -E '"kind": *"' -- 'crates/*wamn.json' 'services/*wamn.json' 'services/ctl/tests/fixtures/*attachments.json' | wc -l  # A3 6
git grep -h -o '"kind":' -- crates/client/tui/tests/data/classification-cases.json | wc -l          # A4 6
git grep -h -E '"(path|name)": *"kind"|^ *"kind",?$' -- 'apps/*/wamn.json' | wc -l                 # A5 3
git grep -h -o 'resource_kind' -- ':!.beads' ':!docs/history' | wc -l                              # A6 15

# 1.2 Generated
git grep -h -o '"kind":"' -- 'apps/*/generated/publication/attachments.json' | wc -l              # G1 68
git grep -h -o '"kind":"' -- '*.operation.json' | wc -l                                           # G2 57
git grep -h -o '"kind":"' -- '*query.input.json' | wc -l                                          # G3 10
git grep -h -o '"kind":"' -- '*/generated/source-map/*.json' | wc -l                              # G4 8
git grep -h -o '"kind":"' -- '*/package-weld.json' | wc -l                                        # G5 72
git grep -h -o -w 'kind:' -- '*/generated/client-ts/*' | wc -l                                    # G7 35
git grep -h -o -w 'kind:' -- '*/generated/*-tui/*' | wc -l                                        # G7 148
git grep -h -o -w 'kind' -- '*/generated/data/error.rs' | wc -l                                   # G11 30
git grep -h -o -w 'kind' -- '*/generated/wit/*' '*/generated/wamn/*' '*/generated/native-verifier/*' '*/generated/client/*' | wc -l  # G11 17

# 1.3 Wire
git grep -h -o 'tag = "kind"' -- 'crates/authoring/*.rs' | wc -l                                   # W1 5
git grep -h -o '"kind"' -- 'services/scenario-worker/*' | wc -l                                    # W2 25
git grep -n -w -E 'kind|failure-kind' -- '*.wit' | grep -v '///'                                  # W3, W4
git grep -h -o -E 'source[-_]kind' -- '*.rs' | wc -l                                              # W5 37
git grep -h -o -w kind -- 'web/runtime/src/*' | wc -l                                             # W8 7
git grep -h -o -w kind -- 'web/ui/src/*' | wc -l                                                  # W9 34
git grep -h -o -w kind -- 'web/*' | wc -l                                                         # all web 53

# 1.4 Persisted
git grep -c -w -E '[a-z_]*kind[a-z_]*' -- 'deploy/sql/*.sql'                                      # lines per file
git grep -h -o -w -E 'principal_kind|placement_kind|fail_kind|caller_outcome_kind|generation_fact_kind|action_kind|command_kind|definition_kind' -- '*.rs' | wc -l  # 222
git grep -h -o -i -E 'AS (relation_kind|routine_kind|type_kind|element_relation_kind|object_kind|identity_kind|generated_kind|dependency_kind|constraint_kind|outcome_kind|definition_kind|kind)\b' -- '*.rs' | wc -l  # P14 20

# 1.5 Rust. For each of R1 to R4 the first line counts distinct types and the second counts references.
git grep -h -o -E '(enum|struct|type) (AttachmentKind|OperationKind|FailKind|ConstraintKind|CustomOperationKind|SourceKind|WiringFailureKind|RouterTapSourceKind|DeliveryAdvisoryKind|AuthoringCommandKind|AuthoringQueryKind|GenerationFactKind)\b' -- '*.rs' | sed -E 's/^(enum|struct|type) //' | sort -u | wc -l  # R1 12
git grep -h -o -w -E 'AttachmentKind|OperationKind|FailKind|ConstraintKind|CustomOperationKind|SourceKind|WiringFailureKind|RouterTapSourceKind|DeliveryAdvisoryKind|AuthoringCommandKind|AuthoringQueryKind|GenerationFactKind' -- '*.rs' | wc -l  # R1 407
git grep -h -o -E '(enum|struct|type) (PrincipalKind|FailureKind|CredentialKind|DefinitionKind|WorkloadRoleScopeKind|UnreadableRegistrationsKind)\b' -- '*.rs' | sed -E 's/^(enum|struct|type) //' | sort -u | wc -l  # R2 6
git grep -h -o -w -E 'PrincipalKind|FailureKind|CredentialKind|DefinitionKind|WorkloadRoleScopeKind|UnreadableRegistrationsKind' -- '*.rs' | wc -l  # R2 312
git grep -h -o -E '(enum|struct|type) (RunPlaneActionKind|SuppliedKind|InputKind|ValueKind|OperationRefusalKind|CandidateExecutionRefusalKind|DeliveryFailureKind|CredentialConnectionKind|WorkloadSecretBodyKind|StatusKind|OutcomeKind|EntryKind|ProfileKind|InvalidBareSchemaNameKind|CredentialKindSnapshot|Kind)\b' -- '*.rs' | sed -E 's/^(enum|struct|type) //' | sort -u | wc -l  # R3 16
git grep -h -o -w -E 'RunPlaneActionKind|SuppliedKind|InputKind|ValueKind|OperationRefusalKind|CandidateExecutionRefusalKind|DeliveryFailureKind|CredentialConnectionKind|WorkloadSecretBodyKind|StatusKind|OutcomeKind|EntryKind|ProfileKind|InvalidBareSchemaNameKind|CredentialKindSnapshot|Kind' -- '*.rs' | wc -l  # R3 584
git grep -h -o -E '(enum|struct|type) (KindCluster|KindNode|ClusterKind|WorkloadKind|CertificateKind|DependencyKind)\b' -- '*.rs' | sed -E 's/^(enum|struct|type) //' | sort -u | wc -l  # R4 6
git grep -h -o -w -E 'KindCluster|KindNode|ClusterKind|WorkloadKind|CertificateKind|DependencyKind' -- '*.rs' | wc -l  # R4 31
git grep -h -o -E '(enum|struct|type) [A-Za-z0-9_]*ErrorKind\b' -- '*.rs' | sed -E 's/^(enum|struct|type) //' | sort -u | wc -l  # R5 74
git grep -h -o -E '\b[A-Za-z0-9_]*ErrorKind\b' -- '*.rs' | grep -v -x -E 'ConsumerInfoErrorKind|GetStreamErrorKind|SubscribeErrorKind|RawMessageErrorKind' | wc -l  # R5 2860
git grep -h -o -w -E '[a-z_]*kind[a-z_]*' -- '*.rs' | wc -l                                       # R6 3938

# 1.6 Not owned
git grep -h -E '^\s*-?\s*kind:\s' -- '*.yaml' '*.yml' | wc -l                                     # N1 246
git grep -h -o -w listKind -- ':!.beads' | wc -l                                                  # N2 37
git grep -h -o -E '"kind": *"[A-Z][A-Za-z]*"' -- '*.json' | wc -l                                 # N3 39
git grep -h -o -E '"kind": *"[A-Z][A-Za-z]*"' -- '*.rs' | wc -l                                   # N4 48
git grep -h -o -i -E '\bkind (create|delete|load|get|export)\b|kind-gate|kind_gate|KindCluster|KindNode|kind cluster|kind-control-plane|kind-worker|kind-wamn|kindest' -- ':!.beads' ':!docs/history' ':!docs/archive' | wc -l  # N5 75
git grep -h -o -w -E 'relkind|prokind|attidentity' -- ':!.beads' ':!docs/history' | wc -l         # N6 81
git grep -h -o -E '\bio::ErrorKind\b' -- '*.rs' | wc -l                                           # N7 47
git grep -h -o -E 'SPAN_KIND_[A-Z]+' -- ':!.beads' | wc -l                                        # N8 32
git grep -h -o -w -E 'KeyEventKind|TypeDefKind|AckKind|MeterKind|dep_kinds' -- ':!.beads' ':!docs/history' | wc -l  # N9 51
git grep -h -o -i -w -E 'kinds?' -- '*.md' ':!.beads' ':!docs/history' ':!docs/archive' | wc -l   # N10 115
git grep -h -E '//.*\bkinds?\b' -- '*.rs' | wc -l                                                 # N11 168
```

The R1 to R4 name lists are a manual classification of every declared non-error `*Kind` type. `git grep -h -o -E '(enum|struct|type) [A-Za-z0-9_]*Kind[A-Za-z0-9_]*\b' -- '*.rs' | grep -v 'ErrorKind' | sed -E 's/^(enum|struct|type) //' | sort -u` lists the 40 names that the four lists cover.

### 1.8 Owner rulings, 2026-09-29

Rule: a name that quotes another system's term keeps that term. `resource_kind` (A6) and the R4 test-support types stay for this reason.

- The record-history column becomes `type`, with the constraint `_type_check`. This applies to every history table in both databases, the `wamn.json` paths and the generated WIT field (A5, W4, P9). The wamn-dev hand statements go under `wamn-o8b9`.
- The WAMN enum `ErrorKind` in `connection_http/transport.rs` is in scope. It goes with the `*ErrorKind` decision in wamn-sfea.6.
- `SetFilter.kind` (W9) becomes `type`. It is a rename only, because the word is not on the wire.
- `DeliveryAdvisory` (W7) already sends `type`. Only its Rust names change.

## 2. Digest and package-version consequences

Draft from wamn-sfea.2. Measured on `worktree-table` at `f348aa1e4`. This section changes no code.

### 2.1 What each identity hashes

| Identity | Input bytes | Where it is compared or kept | Cite |
|---|---|---|---|
| Package manifest (`manifest_sha256`) | The raw `wamn.json` bytes. Nothing under `generated/`, no contract, no source map and no `package-weld.json` | `catalog.packages`. A coordinate that is already recorded with other bytes refuses with `package-coordinate-content-conflict` | `crates/schema/control/src/package_migrations.rs:342`, `:265`, `crates/control/lib/src/apply_package/package_version.rs:64` |
| Package migrations | Each file under `migrations/`, by path and sha256 | `catalog.package_migrations`. A new version must keep the old stream as a byte-identical prefix | `crates/schema/control/src/package_migrations.rs:790`, `docs/architecture/data-access.md:13` |
| Attachment `definition-hash` | `canonical_json_sha256` of the attachment `definition` object | Checked at release mint and at route-schema resolve. Stored in the serving manifest | `apps/platform/execution/contract/src/lib.rs:54`, `crates/control/lib/src/publish_release/attachments.rs:238`, `crates/schema/generator/src/route_schema.rs:227` |
| `verified_schema_state_id` | Canonical JSON of the catalog IR. The catalog reader skips every `_history` table | `generated/package-weld.json`. The dev loop uses it only to decide if SQLx metadata is current | `crates/schema/generator/src/generate.rs:415`, `crates/schema/introspection/src/postgres.rs:261`, `crates/control/lib/src/dev/coordinator.rs:1733` |
| `application_sql_corpus_identity` | The authored and generated SQL files | `package-weld.json`. `push-component` refuses a corpus that differs | `crates/schema/generator/src/generate.rs:421`, `crates/control/lib/src/push_component.rs:754` |
| Component digest | sha256 of the component wasm bytes | `catalog.component_library`, keyed by package coordinate and component. Overlays pin the base digest in `base_dependencies[*].digest` | `deploy/sql/catalog-schema.sql:191`, `crates/control/lib/src/component_declaration.rs:122` |
| Serving manifest digest | Canonical bytes of the whole release manifest: routes, attachments with their definitions and hashes, component digests, wirings | Release records, run admission pins, edge bundles, the web client upload path | `crates/catalog/model/src/serving_manifest.rs:886`, `crates/control/lib/src/dev/edge_bundle.rs`, `docs/operations/deployment.md:207` |
| Authoring request hash | Canonical JSON of the whole authoring request, with its `kind` tags | `catalog.authoring_command_audit.request_hash`. A retry with the same `command-id` replays only on an equal hash | `services/scenario-worker/src/management.rs:394`, `:259`, `deploy/sql/control-portable-store.sql:280` |

A package version is part of every sealed operation id, `<package>:<interface>/<operation>@<version>`. WIT packages, contracts, every file under `generated/`, the clients and the grants carry it (`docs/architecture/data-access.md:153`). So a package that takes a new version regenerates all of its generated files and rebuilds all of its components, whatever its `kind` count is.

### 2.2 Effect of each serialized row

Columns: **Def** is the attachment definition hash. **Pkg** is the package manifest hash. **Schema** is `verified_schema_state_id`. **Comp** is a component digest. **Release** is the serving manifest digest and release bytes. **Client** is generated client source and the web bundle. `-` means no effect.

| Row | Def | Pkg | Schema | Comp | Release | Client | Other effect and evidence |
|---|---|---|---|---|---|---|---|
| A1 | - | yes | - | - | - | - | `wamn.json` of six packages. The data-access overlay also records `manifest_sha256` (`crates/schema/generator/src/data_access.rs:779`) |
| A2 | yes | - | - | - | yes | - | The authored `definition-hash` value must be rewritten in the file. All 52 attachment definitions recompute to their stored hash, and all 52 move when `definition.kind` becomes `type` (proof 1) |
| A3 | yes | yes | - | - | - | - | Test fixtures only. No sealed coordinate. Regenerate |
| A4 | - | - | - | - | - | - | Test data only |
| A5 | - | yes | - | - | - | - | `wamn_receiving/wamn.json` only. Its column moves the rows of P9 |
| G1 | yes | - | - | - | yes | - | Same as A2 for generated route attachments |
| G2 | - | - | - | - | yes | yes | Read into `ServingRoute.kind` (`crates/control/lib/src/publish_release/package_sources.rs:124`) and into the client IR (`crates/schema/generator/src/client_ir.rs:1155`) |
| G3 | - | - | - | - | - | - | Only the client IR reads it (`crates/schema/generator/src/client_ir.rs:1354`). No generated client names `keyset` (`git grep -c keyset -- 'apps/*/generated/client-ts/*' 'apps/*/generated/*-tui/*'` is empty). Generated file bytes only |
| G4 | - | - | - | - | - | - | Only SQLx metadata preparation reads source maps (`crates/schema/generator/src/sqlx_metadata.rs:170`). Generated file bytes only |
| G5 | - | - | yes | - | - | - | The `kind` tags of `ConstraintKind`, `ColumnDefault` and `ColumnGeneration` are inside the catalog IR. The weld also copies `ConstraintKind` into `required_schema_contract` (`crates/schema/generator/src/generate/contracts.rs:1431`). Every package with a table moves. `edge_device` has no table and does not |
| G6 | - | - | - | - | - | yes | In memory only. Its canonical bytes are compared only in tests (`crates/schema/generator/tests/support/platform_client_ir.rs:337`) |
| G7 | - | - | - | - | - | yes | Generated TypeScript and TUI source. The web bundle upload path is keyed by the release digest, so old bundles keep their path |
| G8 | yes | - | - | - | yes | - | Proof 2. The reader admits format 3 only, with no dual-version tolerance (`crates/catalog/model/src/serving_manifest.rs:33`, `:898`). Edge bundles carry the same manifest |
| G9 | yes | - | - | - | - | - | Only tests call `Attachment::resolve` (`crates/catalog/model/tests/identity.rs:35`, `tests/conformance/src/catalog.rs:26`). The pinned baseline at `crates/catalog/model/tests/identity.rs:156` moves if the frame tag renames |
| G10 | - | - | - | - | yes | - | The reader of G2 |
| G11 | - | - | - | yes | yes | yes | The history codec and WIT compile into the `receiving` component (`apps/wamn_receiving/component/src/reads.rs:89`). The component digest is in the release. The `error.rs` matches are local names and moves nothing by itself |
| G12 | - | - | - | - | - | - | Built from row columns (`crates/execution/run-state/src/transitions.rs:109`, `:307`). Not hashed |
| W1 | - | - | - | - | - | - | Moves the stored authoring request hash and the stored `outcome_bytes`. An old command retried after the rename hashes differently and refuses instead of replaying (`services/scenario-worker/src/management.rs:262`). The body shape moves, so the authoring contract version moves from `0.1` to `0.2`. The decoder reads the version first (§5.4 ruling 5) |
| W2 | - | - | - | - | - | - | Wire only. Refusal bodies carry `type`. An old body gets the named refusal `unsupported-contract-version` with a body, not a bare 400 (§5.4 ruling 5) |
| W3 | - | - | - | yes | - | - | `http-route` and `materializer` import `wamn:router-delivery/delivery@0.2.0` (`apps/platform/ingress/http-route/wit/world.wit:5`, `apps/platform/execution/materializer/wit/world.wit:21`), and so does the engine world (`crates/platform/engine/wit/world.wit:10`). The pin `apps/platform/ingress/http-route/http_route.wasm.sha256` must be rewritten. The host binary changes too |
| W4 | - | - | - | yes | yes | yes | The `receiving` component and the generated Rust and TypeScript clients (`apps/wamn_receiving/generated/client/receiving.rs:172`, `apps/wamn_receiving/generated/client-ts/receiving.ts:46`) |
| W5 | - | - | - | - | - | - | JetStream stream bytes. `RouterTapWire` carries `format_version` (`crates/platform/runtime/src/plugins/wamn_jetstream.rs:262`). The field becomes `source-type` and the tap format moves to 3. Nothing migrates, because the records live 5 minutes in memory (§5.4 ruling 6) |
| W6 | - | - | - | - | - | - | An error label (`crates/platform/runtime/src/connection_generation.rs:230`). The `CredentialKind` values stay |
| W7 | - | - | - | - | - | - | Rust names only |
| W8 | - | - | - | - | - | yes | Web runtime bundle |
| W9 | - | - | - | - | - | yes | Web bundle only. The key is not on the wire |
| W10 | - | - | - | - | - | - | Tests and gallery |
| P9 | - | - | - | yes | yes | yes | History DDL is platform-owned (`deploy/sql/record-history.sql:117`), so no package migration changes. The one authored read `apps/wamn_receiving/query/load_purchase_order_history.sql:12` changes. That moves the `receiving` SQL corpus identity, its statement digest constants and its SQLx query file `apps/wamn_receiving/tests/.sqlx/query-04951d1d….json`. The history columns also appear in the generated data-access overlays of `wamn_receiving`, `wamn_wms` and `platform_fixture`. A change to `record-history.sql` marks the SQLx metadata of every package stale in the dev loop (`crates/control/lib/src/dev/coordinator.rs:1710`) |
| P1 to P8, P10 to P14 | - | - | - | - | - | - | Database columns only. §4 migrates them |
| R1 to R3, R5, R6 | - | - | - | see note | - | - | Serialized values keep their spelling. A type rename moves no hashed byte |

Note on component bytes. Release components build with `strip = true` (`apps/Cargo.toml:141`). So a pure Rust type or local name leaves no trace in a released component. Three things do stay in the bytes: WIT names, serde keys, and the names that a derived `Debug` prints. A scan of the release builds in `apps/target` (dated 2026-09-26, some stale) found `failure-kind` in `http_route.wasm` and `materializer.wasm`, and `StatementError` with its field `kind` in `receiving.wasm`. That field is `apps/platform/data/postgres-statements/src/lib.rs:88`. The palette components (`http_request`, `transform`, `label_render`, `blob_put`) hold no WAMN `kind` string. The `kind` strings in `jsonata_expression.wasm` and `sqlx_command.wasm` come from third-party crates and `std`.

Proofs. Each is a throwaway Python script that reads tracked files and writes nothing. It hashes `json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)` with sha256, which reproduces the platform canonical bytes for these documents.

1. Every `definition-hash` in `apps/*/publication/attachments.json` and `apps/*/generated/publication/attachments.json`, recomputed over its `definition`. Result: 52 attachments, 52 equal to the stored hash, 52 different after `definition.kind` becomes `type`.
2. The release manifest vector in `crates/catalog/model/tests/fixtures/release_manifest_mint_vector.rs`. As is, it hashes to the pinned `sha256:bc7a8268…`. With its 5 `"kind":` keys renamed, it hashes to `sha256:0baa4d79…`.

### 2.3 Package versions

Every application package must take a new version. Each new `wamn.json` must declare the current version as `predecessor_version`. Registration refuses a new version whose predecessor is not the current leaf (`crates/schema/control/src/package_migrations.rs:287`). No migration file changes, so each new stream keeps the old one as its prefix.

| Package | Current coordinate | Declared at | Changes | Why |
|---|---|---|---|---|
| `wamn_receiving` | 1.0.0 | `apps/wamn_receiving/wamn.json:4` | yes | A1 (4 keys) and A5 (3) move its manifest hash. P9, W4 and G11 move its `receiving` component |
| `wamn_wms` | 1.0.0 | `apps/wamn_wms/wamn.json:4` | yes | A1 (5 keys). The palette components that it admits under its coordinate (`label-render`, `blob-put`, `jsonata`) keep their bytes but are admitted again under the new coordinate. The new `catalog.component_library` key allows that (§5.4 ruling 1) |
| `client_acme_receiving` | 3.0.0, pins `wamn_receiving` 1.0.0 | `apps/client_acme_receiving/wamn.json:4`, `:9` | yes | A1 (5 keys). Its base pin moves to the new `wamn_receiving` version and `receiving` digest |
| `platform_fixture` | 1.0.0 | `apps/platform_fixture/wamn.json:4` | yes | A1 (4 keys) |
| `platform_fixture_overlay` | 1.0.0, pins `platform_fixture` 1.0.0 | `apps/platform_fixture_overlay/wamn.json:4`, `:9` | yes | Its `wamn.json` holds no `kind`. Its base pin must move with `platform_fixture` |
| `edge_samples` | 1.0.0 | `apps/edge_samples/wamn.json:4` | yes | A1 (2 keys) |
| `edge_device` | 1.0.0 | `apps/edge_device/wamn.json:4` | yes | A1 (1 key) |

Platform components under `apps/platform/` have no package coordinate. Their crates are all 0.1.0.

| Component | Changes | Why |
|---|---|---|
| `http-route` | yes, new digest | W3. Rewrite `http_route.wasm.sha256` |
| `materializer` (execution) | yes, new digest | W3 |
| `events/wire` | Rust names only | W7 |
| `http-request`, `transform`, `label-render`, `label-template`, `blob-put`, `jsonata`, `events/materializer`, `events/registration`, `fixtures/*` | no | No serialized WAMN `kind`. See the question on `Debug` field names below |

### 2.4 What stays and what is regenerated

Stays, byte for byte:

- Every sealed coordinate above, in every environment where it is recorded: its `catalog.packages` row, its migrations, its `catalog.component_library` rows and its component bytes. Applying new bytes under an old coordinate refuses, so the new vocabulary can only arrive under new versions.
- Every minted release manifest and its digest, every edge bundle and every uploaded web client path.
- Authoring audit rows and gate reports.
- `crates/schema/control/src/package_migrations.rs:796`. Its pinned manifest holds no `kind`.
- `docs/history/` and the dated digest table at `docs/operations/gcp.md:1184`.

Regenerated for the new vocabulary:

| Evidence | Row |
|---|---|
| Every file under `apps/*/generated/` | all app rows, and the version bump itself |
| `apps/wamn_receiving/tests/.sqlx/query-04951d1d….json` | P9 |
| `apps/platform/ingress/http-route/http_route.wasm.sha256` | W3 |
| `crates/catalog/model/tests/fixtures/release_manifest_mint_vector.rs`, `crates/catalog/model/tests/serving_manifest_digest.rs` | G8 |
| Manifest constants in `crates/execution/host/src/router_delivery.rs:959` and `crates/platform/engine/src/router_delivery.rs:625` | G8 |
| `crates/catalog/model/tests/identity.rs:156` | G9, if the frame tag renames |
| `crates/control/lib/tests/fixtures/observer_package/wamn.json`, `crates/control/lib/tests/fixtures/apply_package/overlay/wamn.json`, `services/ctl/tests/fixtures/ui_scaffold/` | A3 |
| `crates/client/tui/tests/data/classification-cases.json` | A4 |
| Router tap test bodies in `crates/platform/runtime/src/plugins/wamn_jetstream.rs:2190` | W5 |

### 2.5 Owner rulings, 2026-09-30

1. Every package takes a major version step, because a serialized field of the package contract changes. Each package goes to 2.0.0, and `client_acme_receiving` goes to 4.0.0.
2. `wamn-lt7y` retired `wamn:router-delivery@0.1.0` at `288e65032`. `failure-kind` is in `wamn:router-delivery@0.2.0` (`crates/execution/host/wit/deps/wamn-router-delivery-0.2/package.wit:46`, `:60`). This epic renames it. The rename is a break, so the package becomes 0.3.0.
3. The serving manifest moves to format 4, and the reader accepts format 4 only. There is no dual read. Old release rows keep their format 3 bytes, and a format 4 router never reads them. The wamn-dev republish order says which release is read when.
4. The `("kind", ...)` frame label in G9 becomes `type`. The test vector follows.
5. Consequence: an authoring command sent before the rename and retried after it is refused as a conflict. It is not replayed (W1).
6. The `kind` fields of error structs are renamed. This moves component bytes, see §6.

The inventory gaps found here are now rows W11 and W12 of §1, and the A2 count is corrected to 36.

## 3. Regeneration and republish order

Draft from wamn-sfea.3. Measured on `worktree-table` at `0dc04ec4e`. This section changes no code, no database and no cluster.

Sources of the rules that this section follows:

- Every package takes 2.0.0, and `client_acme_receiving` takes 4.0.0 (§2.5 ruling 1).
- `wamn:router-delivery` goes from 0.2.0 to 0.3.0 (§2.5 ruling 2).
- The serving manifest goes to format 4. The reader accepts format 4 only. Old release rows keep their format 3 bytes, and a format 4 reader never reads them (§2.5 ruling 3, bead wamn-sfea.3 notes).
- The `("kind", ...)` frame label becomes `type` (§2.5 ruling 4).
- All 74 `*ErrorKind` families and the `kind` fields of error structs rename in one commit, before the package rebuild (§6).
- The stop drains first. Ingress goes off, open runs finish within a stated bound, `terminalize-effect-uncertain` settles what stays uncertain, and only then do the workloads stop. Nothing is stranded (§5.4 ruling 2).
- On wamn-dev there is no mixed window. The order is drain, stop, wamn_system script, project-env script, new `reconcile-run-plane`, PAT re-annotation, then new binaries and the republish (§4.7).

Part 3.1 is the repository order. Part 3.2 is the wamn-dev order. Part 3.3 says which release is read when. Part 3.4 is rollback. Part 3.5 holds the owner rulings on its questions.

### 3.1 Repository order

These are the commits of the P1 implementation epic, in this order. Each commit leaves the tree green. Each commit carries its bead id in its subject.

Two rules hold for every commit:

- **Regeneration.** A commit that changes generator output also regenerates every `apps/*/generated/` tree that the change moves, at the versions that the tree holds at that commit. Otherwise `materialize_package check` fails on that commit (`docs/operations/running-tests.md` "Capture a run"). The command for one package is in step A9. No commit is red (§3.5 ruling Q4).
- **Router pin.** A commit that changes the bytes of `http_route.wasm` writes the new digest to `apps/platform/ingress/http-route/http_route.wasm.sha256`. `tools/build-components` refuses a guest that does not match its pin (`tools/build-components:464` to `:469`, `docs/operations/building.md:44` to `:46`).

The general proof of each commit is:

```bash
tools/test-changes run --base HEAD~1
tools/repo-lint run
tools/build-components all
```

`tools/test-changes` selects the packages that the change set touches and their dependents (`docs/operations/running-tests.md` "Select the relevant tests"). `tools/repo-lint run` includes the repository policy lints, which require one version of each WAMN WIT package (commit `288e65032` message). The build checks the router pin.

**A1. Router delivery WIT 0.3.0.**

- Changes: rename `crates/execution/host/wit/deps/wamn-router-delivery-0.2/` to `wamn-router-delivery-0.3/` and declare `package wamn:router-delivery@0.3.0` (`package.wit:1`). `enum failure-kind` becomes `enum failure-type`, and the field `delivery-failure.kind` becomes `failure-type` (`package.wit:46`, `:60`). Neither needs a `%` escape (§3.5 ruling Q3). Follow every importer. The precedent commit `288e65032` (0.1.0 retired) touched the same set. The guests and the engine: `apps/platform/ingress/http-route/wit/world.wit`, `apps/platform/ingress/http-route/src/guest.rs`, `apps/platform/execution/materializer/wit/world.wit`, `apps/platform/execution/materializer/src/main.rs`, `crates/platform/engine/wit/world.wit`, `crates/platform/engine/src/route_bindings.rs`, `crates/platform/engine/src/router_delivery.rs`, `crates/execution/host/src/router_delivery.rs`, `services/edge/src/serve.rs`. The tools and tests: `deploy/platform/http-route-workload.example.yaml`, `tools/build-components` (the `shared_wit_roots` list), `tools/test-changes`, `tests/conformance/tests/profile_selectors.rs`, `tests/conformance/tests/test_changes.rs`. Also `crates/execution/workflow/src/wiring_delivery.rs:429`, which lowers `FailureKind`.
- Regenerates: the router pin `http_route.wasm.sha256` (§2.4).
- Proof: the general proof. `tools/repo-lint run` shows one version of `wamn:router-delivery`. `git grep -n 'router-delivery@0.2\|router-delivery-0.2'` returns nothing outside `docs/history`.

**A2. Serving manifest format 4 and the publish reader.**

- Changes: `SERVING_MANIFEST_FORMAT_VERSION` becomes 4 (`crates/catalog/model/src/serving_manifest.rs:33`). The five `kind` fields of G8 serialize as `type` (`serving_manifest.rs:390`, `:441`, `:456`, `:639`, `:655`). `Contract.kind` of G10 reads `type` (`crates/control/lib/src/publish_release/package_sources.rs:147`). The mint refusal text "format-3" at `crates/control/lib/src/publish_release.rs:1371` follows.
- Regenerates, in the same commit, because the tests read them: `crates/catalog/model/tests/fixtures/release_manifest_mint_vector.rs` (bytes and `DIGEST`), `crates/catalog/model/tests/serving_manifest_digest.rs`, the manifest constants at `crates/execution/host/src/router_delivery.rs:959` and `crates/platform/engine/src/router_delivery.rs:625`, and the other format 3 byte fixtures: `crates/control/lib/src/push_release_manifest.rs` (its `CANONICAL_MANIFEST` tests), `crates/control/lib/tests/push_release_manifest_live.rs`, `crates/platform/runtime/tests/release_manifest_source.rs`, `services/ctl/src/delivery_verbs.rs`. The new vector digest comes from the method of §2.2 proof 2, and the digest test checks it.
- Proof: `cargo test --locked --offline -p wamn-catalog -p wamn-control -p wamn-engine -p wamn-execution-host`. `git grep -n -E '"format-version": ?3' -- ':!.beads' ':!docs/history' ':!crates/catalog/model/tests/fixtures/*'` returns nothing. The excluded directory holds the frozen format-3 fixture that §5.3 keeps. The snapshot table rename is part of A7 (§5.4 ruling 3).

**A3. Catalog frame label.**

- Changes: the `("kind", ...)` frames in `crates/catalog/model/src/lib.rs:623`, `:723`, `:875` become `("type", ...)`.
- Regenerates: the pinned baseline at `crates/catalog/model/tests/identity.rs:156` (§2.4).
- Proof: `cargo test --locked --offline -p wamn-catalog --test identity`, and `tests/conformance/src/catalog.rs` through `-p wamn-conformance-tests`.

**A4. Authoring model and management wire.**

- Changes: the five `#[serde(tag = "kind")]` of W1 (`crates/authoring/model/src/lib.rs:189`, `:210`, `:465`, `:516`, `:552`). The W2 refusal bodies (`services/scenario-worker/src/management.rs:728`, `:767`, `services/ctl/src/dev/tui.rs:635`).
- Changes, contract version (§5.4 ruling 5): `SCHEMA_VERSION` moves from `0.1` to `0.2` (`crates/authoring/model/src/lib.rs:19`). `decode_document` reads `schema-version` from the raw JSON before it decodes the body. Today it decodes the whole document first and compares the version after (`lib.rs:572` to `:585`). So an old body fails the strict decode and gets HTTP 400 with no body (`services/scenario-worker/src/management.rs:774`). After the change, any body whose version is not `0.2` gets HTTP 400 with the body `{"type":"unsupported-contract-version","requested":"0.1","supported":"0.2"}` (`management.rs:763` to `:771`, with `type` for `kind`).
- Proof: `cargo test --locked --offline -p wamn-authoring-model -p wamn-scenario-worker -p wamn-ctl`, with the tests of §5.3.

**A5. Other wire surfaces.** One commit per surface.

- W5 router tap (§5.4 ruling 6): `source_kind` becomes `source_type` on `RouterTapWire` and `RouterTapRecord`, with the key `source-type` (`crates/platform/runtime/src/plugins/wamn_jetstream.rs:251`, `:277`). The writer writes format 3 (`wamn_jetstream.rs:577`). The reader accepts 3 only. Formats 1 and 2 refuse with `unsupported router-tap format-version` (`wamn_jetstream.rs:186` to `:190`). The `ctl dev` observation decoder follows (`crates/control/lib/src/dev/observations.rs:437`).
- `wamn web upload` (§5.4 ruling 4): the upload writes every object create-only, with no switch. It reads the head of the environment, `catalog.effective_release_heads` in the project-env database (`deploy/sql/catalog-schema.sql:178`). `delivery select` writes that row on the project-env connection (`crates/control/lib/src/delivery/deployment.rs:22`, `:69` to `:83`), and `promote` writes it too (`crates/control/lib/src/promote.rs:75`). The control copy of the table (`deploy/sql/control-portable-store.sql:144`) is not what `delivery select` writes, so the upload does not read it. It refuses a `--release` that is not that head, before the build (`services/ctl/src/web.rs:56`, `:125`). An existence check is not enough, because it lets an old client be published over the current one. `push-release-manifest` keeps its existence check (`crates/control/lib/src/push_release_manifest.rs:231` to `:246`), because pushing a manifest is not serving it.
- W6, W8, W9, W11, W12 and G12.
- Proof: `cargo test --locked --offline -p wamn-runtime -p wamn-control -p wamn-ctl`, `cd web/runtime && pnpm test`, with the tests of §5.3.

**A6. Generator serde names and the record-history column.**

- Changes: the G2, G3 and G4 names (`crates/schema/generator/src/generate/contracts.rs:246`, `:837`, `:1172`, `:188`). The G5 tags (`crates/schema/introspection/src/ir.rs:316`, `:227`, `:283`). The G6 and G7 names (`crates/schema/generator/src/client_ir.rs:329`, `client_plan.rs:128`, `client_ts.rs:567`, `client_tui.rs:469`). G1 (`generate/publication.rs:61`, `:70`). The P9 column in `apps/platform/data/record-history/src/lib.rs:55`, `:88`, and everything that derives from `HISTORY_COLUMNS` (§4.5 row "Record history"). The web runtime (`web/runtime/src/wire.ts:41`, `transport.ts:382`, `supplied.ts:45`) reads the generated contracts, so it follows in this commit. The generated WIT field of W4 is the bare `%type`, because its column is `type` and its JSON key is `"type"`. The Rust name is whatever bindgen makes of it, and no hand-written name copies it (§3.5 ruling Q3).
- Changes, authored package inputs: `custom_operations.*.kind` (A1), the history paths (A5), and `kind` and `definition.kind` in every `apps/*/publication/attachments.json` (A2). Each authored `definition-hash` is recomputed with `canonical_json_sha256` over the new `definition` (`apps/platform/execution/contract/src/lib.rs:54`), by the method of §2.2 proof 1. The authored read `apps/wamn_receiving/query/load_purchase_order_history.sql:12` follows P9. The package versions do not change yet.
- Regenerates: every `apps/*/generated/` tree (step A9 commands), and `apps/wamn_receiving/tests/.sqlx/query-04951d1d….json` (step A9 SQLx command).
- Proof: `materialize_package check` passes for all seven packages. The attachment hash check runs inside it (`crates/schema/generator/src/route_schema.rs:227`). `cargo test --locked --offline -p wamn-schema-generator`. `cargo run --locked --offline -p wamn-schema-generator --example check_client_ts` and `--example check_client_components` (`docs/operations/running-tests.md` "Generated TypeScript bindings" and "Generated components"). `cd web/runtime && pnpm test`. The regenerated A4 table `crates/client/tui/tests/data/classification-cases.json` is read by both `cargo test -p wamn-client-tui` and the web runtime tests (`running-tests.md` "Web runtime").

**A7. SQL sources and the `reconcile-run-plane` cutover.** One commit, or one per §4.5 group, each with its DDL and its readers.

- Changes: the fresh-install DDL of §4.2 in `deploy/sql/*.sql`. The Rust groups of §4.5 (registry and provisioning, identity, management audit, package ownership, run plane, grants). The `TypeColumnCutover` action and its detection of §4.3.2. The PAT annotation of §4.3.5 (`crates/control/lib/src/provision_project_env/pat_secrets.rs:267`, `deploy/mvp/bootstrap.sh:118` and their tests). `tools/identity-jwks-journey-run:428`.
- Changes, component key (§5.4 ruling 1): the DDL of §4.3.6 in `deploy/sql/catalog-schema.sql` and `deploy/sql/control-portable-store.sql`, with the §4.5 group "Component key".
- Changes, snapshot table (§5.4 ruling 3): `catalog.release_manifest_v3_snapshots` becomes `catalog.release_manifest_snapshots` in `deploy/sql/catalog-schema.sql` and in the 21 other files of the §4.5 group "Snapshot table".
- Proof: the unit plan case in `crates/schema/control/src/run_plane/tests.rs` and the live case in `crates/control/lib/tests/run_plane_live/` (§4.3.2 "Tests"). `cargo test --locked --offline -p wamn-control-provision --test deploy_sql_authority`. `deploy/mvp/tests/bootstrap.sh`. The check query of §4.3 on a fresh install returns no row.

**A8. The `*ErrorKind` commit (§6).**

- Changes: all 74 families, `NodeErrorKind`, the enum `ErrorKind` in `crates/platform/runtime/src/plugins/connection_http/transport.rs:315`, and the `kind` fields of error structs such as `StatementError.kind` (`apps/platform/data/postgres-statements/src/lib.rs:88`). The generator emits `AccessErrorKind` and `StatementErrorKind` into `generated/data/error.rs` (§1.2 G11), so the generator change and the regenerated trees land here too.
- Regenerates: every `apps/*/generated/` tree. When the router bytes move, it also writes the router pin.
- Proof: the general proof. `git grep -h -o -E '(enum|struct|type) [A-Za-z0-9_]*ErrorKind\b' -- '*.rs'` counts 0 WAMN declarations. No `*ErrorKind` of async-nats or `std::io` is touched (§1.5, N7).

After A8, no later commit changes platform code that compiles into a guest. So the component bytes built in A9 to A12 are the bytes that wamn-dev receives. This is the "built once" rule of §6.

**A9. Base packages: `wamn_receiving`, `wamn_wms`, `platform_fixture`, `edge_samples`, `edge_device` to 2.0.0.** One commit per package. The base commits come before any overlay commit.

- Changes: in `wamn.json`, `package.version` becomes `2.0.0` and `package.predecessor_version` becomes `1.0.0`. Registration refuses a new version whose predecessor is not the current leaf (`crates/schema/control/src/package_migrations.rs:287`). A fresh database has no leaf and admits it (same function, `:286`). No migration file changes (§2.3).
- Regenerate the package. Receiving:

  ```bash
  cargo run --locked --offline -p wamn-test-infrastructure --bin wamn-test-postgres -- \
    --database wamn_receiving --schema receiving \
    --migration-dir apps/wamn_receiving/migrations \
    --history-manifest apps/wamn_receiving/wamn.json \
    --url-env DATABASE_URL -- \
    cargo run --locked --offline -p wamn-schema-generator --example materialize_package \
    -- write apps/wamn_receiving
  ```

  This is the command of `running-tests.md` "Capture a run" with `write` in place of `check`. The other packages take the arguments that the same section gives: WMS `--schema wms`, its migrations and its manifest. Platform fixture `--schema inventory`, its migrations and its manifest. `edge_samples` takes `--schema edge_samples` (`apps/edge_samples/wamn.json:12`), `--migration-dir apps/edge_samples/migrations` and `--history-manifest apps/edge_samples/wamn.json`. `edge_device` has no schema and no migration (`docs/plan/edge.md:76`), so it takes only `--database edge_device --url-env DATABASE_URL`. The runner makes `--schema` and `--migration-dir` optional (`test-support/infrastructure/postgres-run.rs:42` to `:64`).
- Prepare SQLx metadata for each package with SQL, with the same runner arguments and `--example sqlx_metadata -- prepare apps/<package>` (`running-tests.md` "Capture a run"). Then run it with `check`.
- Build the component: `tools/build-components app apps/<package>` (`docs/operations/building.md:28`). Record the sha256 of `apps/target/virtualized/std-empty-environment/<component>.wasm`. The overlay commits need the `receiving` and `platform_fixture` digests.
- Sweep the tests. 78 Rust files hold the literal `"1.0.0"` and 7 files hold a `<package>@1.0.0` coordinate outside `generated/` (`git grep -l -E '"1\.0\.0"' -- 'crates/*.rs' 'services/*.rs' 'tests/*.rs' 'apps/*.rs'`). Each one that names one of these packages moves to 2.0.0. `docs/operations/gcp.md` stays as a dated record.
- Proof: `materialize_package check` for the package. `sqlx_metadata check`. `cargo test --locked --offline -p wamn-receiving-tests -p wamn-wms-tests --lib committed_sqlx_metadata_compiles_offline -- --exact` (`running-tests.md` "Select the relevant tests"). `cargo test --locked --offline -p wamn-schema-generator --test platform_generation`. The general proof.

**A10. Overlays: `client_acme_receiving` to 4.0.0 and `platform_fixture_overlay` to 2.0.0.** One commit each, after its base.

- Changes: `package.version` becomes `4.0.0` (Acme) or `2.0.0` (fixture overlay). `package.predecessor_version` becomes `3.0.0` or `1.0.0`. `base_dependencies.*.version` becomes `2.0.0`, and `base_dependencies.*.digest` becomes the base component digest recorded in A9 (`apps/client_acme_receiving/wamn.json:9`, `:10`, `apps/platform_fixture_overlay/wamn.json:9`, `:10`). That field is the one authored site of the base pin (`crates/control/lib/src/component_declaration.rs:112` to `:122`).
- Regenerate with the base migrations first, then the overlay migrations, and the overlay manifest (`running-tests.md` "Capture a run", the Acme and fixture overlay paragraphs). Prepare SQLx metadata the same way.
- Build: `tools/build-components app apps/wamn_receiving apps/client_acme_receiving` (`building.md:29`). The fixture overlay takes `apps/platform_fixture apps/platform_fixture_overlay`. An overlay build without its base fails (`building.md:35`, `:36`).
- Proof: `cargo test --locked --offline -p wamn-client-acme-receiving-tests`, which runs `apps/client_acme_receiving/tests/acme_overlay_publication.rs` against the authored base pin. `materialize_package check` and `sqlx_metadata check` for the overlay. The general proof.

**A11. Test fixtures that are not package trees.** Most fixtures land in their owning commits. The rest land here: A3 `crates/control/lib/tests/fixtures/observer_package/wamn.json`, `crates/control/lib/tests/fixtures/apply_package/overlay/wamn.json`, `services/ctl/tests/fixtures/ui_scaffold/`. W5 tap bodies at `crates/platform/runtime/src/plugins/wamn_jetstream.rs:2190`. The gate command id of `test-support/infrastructure/examples/gate_request.rs:37` becomes `gate-<package>-<version>-<wiring>`, for example `gate-wamn_wms-2.0.0-inventory_move_and_label` (§3.5 ruling Q6). Proof: `tools/test-changes run --base HEAD~1`.

**A12. Final build and pins.** On the final commit, run `tools/build-components all` once. It must pass the router pin with no edit. Record the sha256 of every guest that wamn-dev takes: `receiving.wasm`, `wms.wasm`, `label_render.wasm`, `blob_put.wasm`, `jsonata_expression.wasm`, `http_route.wasm`, `materializer.wasm` (`docs/operations/gcp.md` §3.9, §3.20, §5.3). Then check the tree:

```bash
git grep -n -w -E 'failure-kind|definition_kind|principal_kind|fail_kind' -- ':!.beads' ':!docs/history' ':!tests/sweeps'
git grep -h -o '"kind":"' -- 'apps/*/generated/*'
```

Both print nothing. The §1.7 counts of the renamed rows are 0, and the rows that stay (§1.6, §1.8) keep their counts.

**Frozen evidence that stays.** `docs/history/`, the dated digest tables of `docs/operations/gcp.md` (§2.4), and `tests/sweeps/*.log` (§4.5) are not edited. New records go below the old ones.

### 3.2 wamn-dev order

**Installed environments.** wamn-dev has one control database and two project environments. No overlay, fixture or edge package is installed there. No edge device runs there (§4.4).

| Environment | Tenant | Database | Package | Host group | Route host | Release now | Cite |
|---|---|---|---|---|---|---|---|
| wamn_system | none | `wamn_system` | none | none | none | none | `gcp.md` §3.6 |
| Receiving | `dev` | `wamn-db-dkk--receiving--dev--4pqjfmli` | `wamn_receiving@1.0.0` | `default` | `receiving.wamn.dev` | 1, `sha256:900d35fb…` | `gcp.md` §3.7, §3.8, §6.7 table |
| WMS | `wms` | `wamn-db-dkk--wms--dev--0nk1lrpr` | `wamn_wms@1.0.0` | `wms` | `wms.wamn.dev` | 1, `sha256:3d6b13f9…` | `gcp.md` §5.2, §5.3, §6.7 table |

The hosts select a release by `--release-manifest-digest` (`deploy/gcp/values-host.yaml:110`, `:250`). The edge serves each web client from a bucket path keyed by the release digest (`deploy/gcp/values-edge.yaml:13`, `:16`, `deploy/gcp/url-map.yaml:47` to `:93`).

The base-then-overlay rule has nothing to order on wamn-dev, because each environment holds one base package. Receiving runs before WMS in the steps below. The two environments share no package, so that order is for the record only.

Run everything in one session. The daily guard sets pool `main` to 0 nodes at 03:00 New York time (`gcp.md:762`). Start with pool `main` at 2 nodes (`gcp.md` §3.17). Keep the port-forward, `WAMN_SYSTEM_ADMIN_URL` and `PW` of `gcp.md` §3.6. `T` is the superuser URL of the environment's database (`gcp.md` §3.8). `SYS` is the control database URL of `gcp.md` §5.3.

**B0. Before the stop. Nothing here changes installed state.**

1. Check out the final commit of 3.1. Build the programs: `cargo build -p wamn-ctl`, `cargo build -p wamn-ctl --bin wamn`, `cargo build -p wamn-scenario-worker` (`gcp.md` §3.6, §4.2, §5.3).
2. Build the guests with `tools/build-components all` (`gcp.md` §3.20). Each sha256 must equal the value recorded in A12.
3. Build and push the host and identity images by source identity (`gcp.md:384`, `:385` and the push lines of §3.4). Record both digests. The CDC reader image does not change, because the CDC readers keep running (§4.6, §4.7 step 1).
4. Push the four workload guests with `wash`, as `gcp.md` §3.20 and §5.5 do: `flow-http` and `wms-flow-http` from `http_route.wasm`, `materializer` and `wms-materializer` from `materializer.wasm`. The running workloads name their guests by digest (`gcp.md:868`), so a new tag does not move them. Record the four pushed digests.
5. Run the ops-schema query of §4.3.4 on wamn_system and the PAT Secret listing of §4.3.5. Record both answers.
6. Run the check query of §4.3 on the three databases. Record the result. It must equal the §4.1 names for that database plus the function bodies of §4.3.

**Drain. Before B1 (§5.4 ruling 2).** The old binaries finish every open run under release 1. Nothing is stranded.

A run is open while its `wamn_run.runs.status` is `dispatched`, `running` or `effect-uncertain` (`deploy/sql/run-state.sql:329` to `:331`). `effect-uncertain` is not terminal (`run-state.sql:240` to `:242`). A run keeps its `effective_release_id` for life (`run-state.sql:319`, pinned at `:203`). A run waiting for a host has a row in `wamn_run.run_queue` (`deploy/sql/run-queue.sql:51`), with `available_at` and `lease_expires_at` (`run-queue.sql:55`, `:65`). A format 4 host never claims a release 1 run (§5.2 row R10), so every open run must close here.

This is the open-run query. Run it as the `postgres` superuser, which reads past row security:

```sql
SELECT r.effective_release_id, r.status, count(*) AS runs,
       count(q.run_id) AS queued, min(q.available_at) AS next_available,
       max(q.lease_expires_at) AS last_lease
  FROM wamn_run.runs AS r
  LEFT JOIN wamn_run.run_queue AS q
    ON q.tenant_id = r.tenant_id AND q.run_id = r.run_id
 WHERE r.status IN ('dispatched', 'running', 'effect-uncertain')
 GROUP BY 1, 2
UNION ALL
SELECT NULL, 'queue-without-open-run', count(*), count(*), min(q.available_at), max(q.lease_expires_at)
  FROM wamn_run.run_queue AS q
  JOIN wamn_run.runs AS r ON r.tenant_id = q.tenant_id AND r.run_id = q.run_id
 WHERE r.status NOT IN ('dispatched', 'running', 'effect-uncertain')
HAVING count(*) > 0
ORDER BY 1, 2;
```

1. Turn ingress off. The hosts and identity keep running, so they can finish the open runs:

   ```bash
   kubectl -n edge scale deploy/wamn-edge --replicas=0
   kubectl delete -f deploy/gcp/materializer.yaml -f deploy/gcp/wms-materializer.yaml
   ```

   The URL map sends `/api/` and `/password/` to the edge and serves the rest from the bucket (`deploy/gcp/url-map.yaml:4`, `:5`), so the edge is the only way to reach a host. Its chart names the Deployment after the release (`deploy/platform/edge/templates/deployment.yaml:6`). The materializers admit the event runs. Their JetStream consumers stay, so events wait for B10 as in §3.3. Run no `wamn-ctl workflow start` and no scenario-worker. The `flow-http` workloads and their Services stay, because B9 needs the Services.

2. Wait for the open runs to finish. The bound is 15 minutes from step 1. It comes from the queue. A lease lasts 30 seconds (`DEFAULT_QUEUE_LEASE_TTL_MS`, `crates/execution/workflow/src/queue.rs:30`). A run whose holder dies is claimed again when its lease expires, and after 20 such claims (`max_attempts`, `run-queue.sql:70`) the janitor marks it `infrastructure-failure` (`run-queue.sql:45` to `:47`). So a run that no live host finishes closes within 20 × 30 seconds, which is 10 minutes. The 5 minutes above that is margin. No `wamn_receiving` or `wamn_wms` attachment declares a run deadline or a retry (`git grep -n -i 'deadline\|retry' -- 'apps/wamn_receiving/publication/*' 'apps/wamn_wms/publication/*'` is empty). Observe it with the open-run query on both databases once a minute, and record each answer:

   ```bash
   for i in $(seq 1 15); do
     for db in wamn-db-dkk--receiving--dev--4pqjfmli wamn-db-dkk--wms--dev--0nk1lrpr; do
       echo "$(date -u +%T) $db"
       kubectl -n platform exec -i wamn-pg-1 -c postgres -- psql -U postgres -d "$db" -At < $P/open-runs.sql
     done
     sleep 60
   done
   ```

   `$P/open-runs.sql` holds the query above. Stop the loop early when both databases print only `effect-uncertain` rows or nothing.

3. Settle what stays uncertain. For each run that is still `effect-uncertain`, run the verb once. `T` names the database and `<tenant>` is `dev` or `wms`:

   ```bash
   kubectl -n platform exec wamn-pg-1 -c postgres -- psql -U postgres -d "$db" -Atc \
     "SELECT tenant_id, run_id FROM wamn_run.runs WHERE status = 'effect-uncertain' ORDER BY 1, 2"
   target/debug/wamn-ctl terminalize-effect-uncertain --admin-database-url "$T" --tenant <tenant> --run <run id> \
     --basis operator-judgment --evidence-ref kind-to-type-drain --correlation-id kind-to-type-<run id>
   ```

   The verb takes one run and refuses a run that is not `effect-uncertain` (`crates/control/lib/src/terminalize_effect_uncertain.rs:236` to `:241`, flags at `services/ctl/src/release_verbs.rs:172` to `:200`). Use `--basis external-evidence` when the effect target shows the outcome. Record each run id and its answer.

4. If a `dispatched` or `running` run is still open at the bound, do not go on. Turn ingress back on (§3.4 row R1) and report the run. The old binaries still serve release 1, so the run is not stranded.

The open-run query must then print nothing on both databases. Record both empty answers. Only then does B1 start.

**B1. Stop (§4.7 step 1).**

```bash
kubectl -n hosts scale deploy/hostgroup-default deploy/hostgroup-wms --replicas=0
kubectl -n identity scale deploy/identity --replicas=0
kubectl -n hosts get pods
kubectl -n identity get pods
```

The scale form is the one of `gcp.md:1646`. The hosts run the HTTP and materializer workloads, so both stop with them. The scenario-worker is not deployed on wamn-dev. It runs on the operator machine only for a gate (`gcp.md:1278`), so make sure none runs. The CDC readers keep running (§4.6). Run no `bootstrap.sh`. wamn-dev never runs it (`gcp.md` has no `bootstrap.sh` step), so the rule of §4.3.5 holds by default.

Then run the open-run query of the drain once more on each project-env database. Ingress is off, so it must still print nothing. A run pins its release id and manifest digest (`run-state.sql:190` to `:227`). A new host reads the snapshot of that release (`crates/platform/runtime/src/plugins/wamn_postgres/wiring_resolution.rs:17` to `:30`), and a format 4 reader cannot read a release 1 snapshot. If a query prints a row, stop here and restart the old binaries (3.4 row R1).

**B2 to B5. Schema and annotations (§4.7 steps 2 to 5).** Run §4.8 exactly:

- B2. The wamn_system script `$P/kind-to-type-system.sql` (§4.3.4, §4.8).
- B3. The project-env script `$P/kind-to-type-project-env.sql` on both databases (§4.3.4, §4.8).
- B4. The new `reconcile-run-plane` for Receiving (`--project receiving --tenant dev`) and for WMS (`--project wms --tenant wms`), twice each. The second run reports no action (§4.3.2, §4.8, `gcp.md:534`).
- B5. One `kubectl annotate` per listed PAT Secret (§4.3.5).

Proof: the check query of §4.3 returns no row in any of the three databases (§4.8).

**B6. Identity (§4.7 step 6, first part).** Write the new identity digest into `deploy/gcp/values-identity.yaml:7`. Then:

```bash
helm upgrade identity deploy/platform/identity -n identity -f deploy/gcp/values-identity.yaml
kubectl -n identity rollout status deploy/identity --timeout=300s
```

The command is the one of `gcp.md:413`. The chart sets `replicas: 1` (`deploy/platform/identity/templates/deployment.yaml:19`). If `kubectl -n identity get deploy identity` still shows 0 replicas, run `kubectl -n identity scale deploy/identity --replicas=1`. Proof: the TLS check of `gcp.md` §3.10 answers, and the key set still lists the active `kid` (`gcp.md` §3.15).

The hosts stay stopped until B10. At this point the host values name only format 3 digests, and a new host refuses them (`crates/catalog/model/src/serving_manifest.rs:1073` to `:1076`). The republish in B7 to B9 runs from the operator machine and needs no host.

**B7. Receiving republish.** `T` names the Receiving database.

1. `target/debug/wamn-ctl apply-package --package apps/wamn_receiving --database-url "$T" --tenant dev` (`gcp.md:536`). It records `wamn_receiving@2.0.0` with predecessor `1.0.0`, and it writes `definition_type`.
2. `target/debug/wamn-ctl reconcile-package-data-access --package apps/wamn_receiving --database-url "$T" --tenant dev` (`gcp.md:537`).
3. `push-component` of `receiving.wasm` with the declaration template, as `gcp.md` §3.9 (`gcp.md:566`). The printed digest equals the local sha256 (`gcp.md:574`).
4. `publish-release` with `--org dkk --project receiving --tenant dev --environment dev --effective-release-id 2 --verified-publisher-principal wamn-management-author-dkk--receiving--dev --run-schema wamn_run --package wamn_receiving@2.0.0 --attachments apps/wamn_receiving/publication/attachments.json --route-host receiving.wamn.dev --package-manifest apps/wamn_receiving/wamn.json` and no `--wiring` (`docs/operations/deployment.md:123` to `:134`, `gcp.md:1300`). Release 1 exists, so release 2 is the next id (`deploy/sql/control-portable-store.sql:50` to `:62`).
5. `push-release-manifest` with `--effective-release-id 2 --artifact-base us-central1-docker.pkg.dev/wamn-dev/wamn/releases` (`gcp.md:1296`, `deployment.md:154`).
6. `print-release-env --effective-release-id 2` (`deployment.md:143`). Record the manifest digest.

Proof: the new snapshot holds format 4:

```bash
kubectl -n platform exec wamn-pg-1 -c postgres -- psql -U postgres -d wamn-db-dkk--receiving--dev--4pqjfmli -Atc \
  "select effective_release_id, manifest_digest, convert_from(canonical_bytes, 'UTF8')::jsonb ->> 'format-version' from catalog.release_manifest_snapshots order by 1"
```

It prints release 1 with format 3 and release 2 with format 4. The table name holds no format. Each row states its own format in the `format-version` of its bytes (§5.4 ruling 3).

**B8. WMS republish.** `T` names the WMS database.

1. `apply-package`, `reconcile-package-data-access` and `reconcile-replica-identity` with `--package apps/wamn_wms --tenant wms` (`gcp.md` §5.2, `deployment.md:54` to `:59`).
2. Render the three platform declarations with `s/__PACKAGE_VERSION__/2.0.0/g` in the `sed` of `gcp.md:1254`. Push `wms`, `label-render`, `blob-put` and `jsonata` under `wamn_wms` 2.0.0 with the admitted packages of the `gcp.md` §5.3 table. The palette bytes do not change, but they are admitted again under the new coordinate (§2.3). Each push writes a new `catalog.component_library` row keyed by `(wamn_wms, 2.0.0, digest)` in the WMS database and in wamn_system. The owner row of each palette digest already names `wamn_wms`, so the push is admitted. This works only with the key of §4.3.6, which B2 and B3 install. Under the old key the push refuses `component-fact-conflict` (`crates/control/lib/src/push_component.rs:1985`). Proof: the component listing of §5.3 shows each palette digest under both 1.0.0 and 2.0.0, and `SELECT component_digest, package_id FROM catalog.component_digest_owners` shows one row per digest.
3. Gate the wiring. Run the new scenario-worker as `gcp.md:1275` to `:1278` does. Make the request with `cargo run -p wamn-test-infrastructure --example gate_request -- wamn_wms 2.0.0 wms dev apps/wamn_wms/publication/wirings/inventory_move_and_label.json` (`gcp.md:1279`). Post it with the WMS management-author PAT. The reply has `body.outcome.status` `completed`. Stop the service. The example derives the command id from the package id and the wiring id only (`test-support/infrastructure/examples/gate_request.rs:37`). The 1.0.0 gate already holds that id in `catalog.authoring_command_audit` for this principal (`control-portable-store.sql:287`). A new request under the old id is refused (§2.5 ruling 5, `services/scenario-worker/src/management.rs:984`). A11 puts the package version into that id, so the 2.0.0 gate uses `gate-wamn_wms-2.0.0-inventory_move_and_label`. The 1.0.0 audit row stays where it is (§3.5 ruling Q6).
4. `author-wiring` with `--package-version 2.0.0` (`gcp.md:1286`). Record the wiring version `<V>` that it prints.
5. `publish-release` with `--effective-release-id 2 --package wamn_wms@2.0.0 --wiring "wamn_wms@2.0.0::inventory_move_and_label=<V>"` and the other arguments of `gcp.md:1288` to `:1291`.
6. `bind-connection` with `--effective-release-id 2` and the `blob-put` digest (`gcp.md:1293`).
7. `push-release-manifest` and `print-release-env` with `--effective-release-id 2`. Record the manifest digest.

Proof: the snapshot query of B7 on the WMS database prints release 1 with format 3 and release 2 with format 4.

The event users and consumers need no change. The consumer names come from the package id and the registration name (`gcp.md:1314`), and neither changes. As a check, run `event_broker_files` into a scratch directory (`gcp.md` §5.4) and compare `consumers.jsonl` with the one in `$P/evt`. They are equal.

**B9. Web clients and edge. The hosts are still stopped.**

0. Set the head of each environment to release 2 with `wamn-ctl delivery select` (`services/ctl/src/delivery_verbs.rs:298`, `crates/control/lib/src/delivery/deployment.rs:62`). This writes `catalog.effective_release_heads` in each project-env database. wamn-dev writes heads from this cutover on. Proof: `SELECT tenant_id, environment, effective_release_id FROM catalog.effective_release_heads` prints release 2 on the Receiving database and on the WMS database.

1. Upload both clients of release 2 (`gcp.md:922`, `:1383`):

   ```bash
   target/debug/wamn web upload apps/wamn_receiving --release <Receiving release 2 digest> --bucket gs://wamn-dev-web/clients --org dkk \
     --database-url <Receiving database URL> --tenant dev --environment dev
   target/debug/wamn web upload apps/wamn_wms --release <WMS release 2 digest> --bucket gs://wamn-dev-web/clients --org dkk \
     --database-url <WMS database URL> --tenant wms --environment dev
   ```

   The upload is create-only and refuses a digest that is not the head of the environment (A5, §5.4 ruling 4). Release 2 is the head after step 0, and its path is new, so both uploads pass. `--database-url` names the project-env database that holds the head. A5 fixes the exact flags.

2. Write the two new digest hex values into `deploy/gcp/values-edge.yaml:13`, `:16` and the six rewrites of `deploy/gcp/url-map.yaml:47` to `:93`.
3. `helm upgrade wamn-edge deploy/platform/edge -n edge -f deploy/gcp/values-edge.yaml --wait --timeout 3m` and `gcloud compute url-maps import wamn-edge --global --project wamn-dev --source deploy/gcp/url-map.yaml --quiet` (`gcp.md:1391`, `:1392`). The Services `hosts/flow-http` and `hosts/wms-flow-http` still exist, so the edge starts (`gcp.md` §4.1).

The switch comes before the hosts start. So no old client ever calls a new host, and no new client ever calls an old host.

**B10. Hosts and workloads (the router switch).**

1. Write the new host image into `HOST_IMAGE` (`test-support/infrastructure/examples/host_values_files.rs:34`). Render the values with both release 2 digests (`gcp.md:1347`):

   ```bash
   cargo run -p wamn-test-infrastructure --example host_values_files -- deploy/gcp \
     us-central1-docker.pkg.dev/wamn-dev/wamn/releases <Receiving release 2 digest> <WMS release 2 digest>
   ```

2. Render the four workloads with the digests of B0 step 4 (`gcp.md:1356`).
3. `helm upgrade wamn-host oci://ghcr.io/wasmcloud/charts/runtime-operator --version 2.10.0 -n hosts -f deploy/gcp/values-host-base.yaml -f deploy/gcp/values-host.yaml --wait --timeout 10m` (`gcp.md:1349`, the timeout of `gcp.md` §6.7). The values set one replica per host group (`deploy/gcp/values-host.yaml:4`, `:144`). If `kubectl -n hosts get deploy` shows 0 for a host group, scale it to 1.
4. `kubectl apply -f deploy/gcp/flow-http.yaml -f deploy/gcp/materializer.yaml -f deploy/gcp/wms-flow-http.yaml -f deploy/gcp/wms-materializer.yaml`, then the two `kubectl -n hosts wait` lines of `gcp.md:870` and `:1359`. Before this apply, the operator can place the old `flow-http` guest on a new host. That guest imports `wamn:router-delivery@0.2.0`, so it does not link. No traffic reaches it, because the apply follows at once.

Proof: the serve check of `gcp.md` §3.21 for both route hosts answers 401 on a released route and 404 on an unknown path. The host log says that each host loaded release 2 (`gcp.md:1367` shows the form of that line).

**B11. End-to-end check.** The owner signs in at both hosts. At Receiving the lists answer 200. At WMS the owner moves one pallet, and the label object appears (`gcp.md` §5.7). One history row of the move is readable through `load_purchase_order_history` or the WMS history table. This shows the renamed `type` column from write to read.

**B12. Record.** Add the §4.8 entry to `gcp.md` §7. Add the new images to the table of `gcp.md` §3.13. Add the new digests to the component tables of §3.20 and §5.3. Add a release table like the one of §6.7. Commit `deploy/gcp/values-identity.yaml`, `values-host.yaml`, `values-host-base.yaml`, the four workload files, `values-edge.yaml`, `url-map.yaml`, `host_values_files.rs` and `gcp.md` together (`deployment.md:156`, `:157`).

**Gated follow-up. Immutable tags on the `wamn` registry (§5.4 ruling 8).** Not part of this cutover. The deployment agent turns immutable tags on only after `wamn-r9lz` closes. Until component bytes are reproducible, a rebuild of one source identity can produce a second digest, and an immutable tag would refuse it for the wrong reason. The repository was created without `--immutable-tags` (`docs/operations/gcp.md:132`). The command is:

```bash
gcloud artifacts repositories update wamn --project wamn-dev --location us-central1 --immutable-tags
```

Before it runs, every push into the repository must use a tag that names one content. Component tags are the digest hex (`crates/control/lib/src/push_component.rs:523`). Release tags derive from the manifest digest (`crates/control/lib/src/push_release_manifest.rs:1` to `:5`). Host and identity images are tagged by source identity (`gcp.md:384`, `:385`). The workload guests are pushed under fixed tags such as `flow-http` and `materializer` (`gcp.md` §3.20). An immutable tag refuses the second push of such a tag, so those pushes need content tags first. The record of the change goes into `gcp.md` §7.

### 3.3 Which release is read when

| Time | Receiving and WMS hosts | Release read | Web client served |
|---|---|---|---|
| Before the drain | old binaries | release 1, format 3, by `--release-manifest-digest` (`values-host.yaml:110`, `:250`) | release 1 client |
| Drain | old binaries, ingress off | release 1, for the open runs only. Every API call fails, because the edge has no replica. No event run starts, because the materializers are gone. The events wait in JetStream (§4.6) | release 1 client, served from the bucket. Its API calls fail |
| B1 to B9 | stopped | none. Every API call fails at the edge, because no workload serves it. Sign-in fails until B6, because identity is stopped. The CDC readers keep writing change events into JetStream, and the events wait for the materializer consumers (§4.6) | release 1 client until B9 step 3, then release 2 client |
| B7 and B8 | stopped | The new `wamn-ctl` reads only the release 2 bytes it has just minted (`crates/control/lib/src/publish_release.rs:627`, `:1366`). Never pass `--effective-release-id 1` to a new verb. `print-release-env`, `push-release-manifest` and `promote` refuse the format 3 bytes (`crates/control/lib/src/print_release_env.rs:79`, `push_release_manifest.rs:227`, `promote.rs:391`) | as above |
| From B10 step 3 | new binaries | release 2, format 4. This is the moment the router switches | release 2 client |
| After B10 | new binaries | release 2 only. Release 1 rows stay in `catalog.release_manifest_snapshots` and in the registry under their digests. No reader opens them, because no open run pins release 1 (the drain query) and no host names its digest | release 2 client. The release 1 client stays in the bucket at its own path (`deployment.md:207`) |

The events captured during the stop are delivered after B10 to the new materializer. The new host runs them under release 2.

### 3.4 Rollback

Each row says what state the environment is in and how to go back. `deployment.md:340` to `:343` states the general rule: revert the configuration and select the previous release, and a code rollback does not reverse database changes.

| Row | Point reached | Can roll back | How | Cannot roll back |
|---|---|---|---|---|
| R0 | Any commit of 3.1, before the drain | everything | `git revert` of the commit. wamn-dev is untouched | nothing |
| R1 | Drain (ingress off) or B1 (stopped) | everything | `kubectl -n edge scale deploy/wamn-edge --replicas=1` and `kubectl apply -f deploy/gcp/materializer.yaml -f deploy/gcp/wms-materializer.yaml`. After B1 also `helm upgrade` of identity and the host with the unchanged values, or `kubectl scale` back to 1 | The operator actions of drain step 3. Each settled run stays terminal (`deploy/sql/run-state.sql:790`) |
| R2 | B2 to B5 | everything | Run the rollback of §4.7. It runs the hand statements with the names swapped, with the old `deploy/sql/record-history.sql`. It runs the §4.3.2 renames swapped for P8 and P10 to P12. It swaps the annotation keys back. It renames the snapshot table back and restores the old component key (§4.3.6). All of it is metadata-only, except the restore of `component_library_digest_key`, which builds one index and fails if a digest already has two rows. Before B8 no digest has two rows. Then R1 | nothing |
| R3 | B6 (new identity) | everything | Old `values-identity.yaml` (`git revert`) and `helm upgrade`, then R2 | nothing |
| R4 | B7 or B8 started | serving and schema | R3. The old host serves release 1 again, because its values still name the release 1 digest | The rows of `wamn_receiving@2.0.0` and `wamn_wms@2.0.0` in `catalog.packages`, their migrations, `catalog.component_library`, the gate report and the audit row, `catalog.effective_releases` 2 and its snapshot. The triggers `<table>_immutable` refuse their removal (`gcp.md:1676` to `:1684`). The pushed registry artifacts also stay. They are harmless to release 1. After B8 step 2 each palette digest has two `catalog.component_library` rows, so the old key `component_library_digest_key` cannot return. The new key stays. Run no old `push-component` against it, because the old verb writes no owner row. A later attempt must present the same 2.0.0 bytes, because a sealed coordinate refuses other bytes (§2.1, `package-coordinate-content-conflict`). A fix that keeps the contract is a patch step such as 2.0.1. A contract break is a major step. A teardown is not a fix path (§3.5 ruling Q5) |
| R5 | B9 (edge switched) | the edge | Old `values-edge.yaml` and `url-map.yaml`, `helm upgrade wamn-edge`, `gcloud compute url-maps import`. The release 1 client is still in the bucket | nothing beyond R4 |
| R6 | B10 and after (new hosts serve) | serving and schema | Old host values and workload files, `helm upgrade`, `kubectl apply`, then R5 and R2. Rows written under the new names keep their data, because every rename is metadata-only (§4.3) | Everything of R4. Runs admitted under release 2 that are still open cannot be read by an old host, because it reads format 3 only. The JetStream acknowledgements of the new materializer stand, so events handled under release 2 are not replayed. Authoring commands sent after the switch hash differently from the same commands sent before it (§2.5 ruling 5) |

### 3.5 Owner rulings, 2026-09-30

- **Q1. Snapshot table name.** Answered by §5.4 ruling 3. `catalog.release_manifest_v3_snapshots` becomes `catalog.release_manifest_snapshots` (A7, §4.3.6).
- **Q2. Open runs at the stop.** Answered by §5.4 ruling 2. The stop drains first, with a bound of 15 minutes, and settles what stays uncertain with `terminalize-effect-uncertain` (3.2 "Drain").
- **Q3. WIT names.** No `%` escape where a plain name exists. The escape is used only where the bare word is the thing. The router-delivery enum is `failure-type`, and the `delivery-failure` field is `failure-type` too (A1). The generated history field is the bare `%type`, because its column is `type`, its JSON key is `"type"`, and the WIT name must match both (A6). The Rust side is whatever bindgen makes of it, and no hand-written name copies it.
- **Q4. Regeneration.** Every commit stays green. A commit regenerates the trees it changes. No commit between is red (3.1 "Regeneration").
- **Q5. A defect after 2.0.0 is sealed on wamn-dev.** It is fixed as 2.0.1. A fix that keeps the contract is a patch step. A contract break is a major step. A teardown is not a fix path (3.4 row R4).
- **Q6. Gate command id.** Confirmed. The id becomes `gate-<package>-<version>-<wiring>`, a code change at `test-support/infrastructure/examples/gate_request.rs:37` (A11). The 1.0.0 audit row stays where it is (B8 step 3).

Still open:

- **Q7.** The current Acme base pin `sha256:4bc28f01…` (`apps/client_acme_receiving/wamn.json:10`) is not the `receiving` digest of wamn-dev release 1 (`sha256:1d034a09…`, `gcp.md` §6.7 table). A10 sets the pin to the new A9 build. No action is needed unless the owner wants the pin checked against a deployed digest.

## 4. Database schema changes

Draft from wamn-sfea.4. Measured on `worktree-table` at `31eac2d05`, re-verified at `2d9700969` after the rebase onto `0dad0cb9b`. This section changes no code and no database.

### 4.1 Names

One pattern holds everywhere (§4.9 ruling 1). A column `kind` becomes `type`. A column `<x>_kind` becomes `<x>_type`. Every constraint name follows its column.

PostgreSQL 18 runs every wamn-dev database (`deploy/gcp/cnpg-cluster.yaml:14`). PostgreSQL 18 records each `NOT NULL` as a named constraint, `<table>_<column>_not_null` by default. A column rename keeps every generated name. An installed database must match a fresh install name for name (§4.9 ruling 2). So every path of §4.3 also renames each generated name that holds the old column name: `_not_null`, `_key`, `_fkey` and one-column `_check`.

The constraint column lists the old names. Each new name is the old name with `kind` replaced by `type`. The **Path** column says which part of §4.3 applies the row.

| Row | Database | Table | Column | Constraints renamed | Path |
|---|---|---|---|---|---|
| P1 | wamn_system | `registry.orgs` | `placement_kind` → `placement_type` | `orgs_placement_kind_check`, `orgs_placement_kind_not_null` | hand |
| P2 | wamn_system | `identity.principals` | `kind` → `type` | `principals_kind_check`, `principals_kind_not_null`, `principals_id_kind_key`, `principals_kind_subject_key` | hand |
| P3 | wamn_system | `identity.pats` | `principal_kind` → `principal_type` | `pats_principal_kind_check`, `pats_principal_kind_not_null`, `pats_principal_id_principal_kind_fkey` | hand |
| P4 | wamn_system | `identity.password_credentials`, `identity.password_tokens`, `identity.password_logins` | `principal_kind` → `principal_type` | per table: `<table>_principal_kind_check`, `<table>_principal_kind_not_null`, `<table>_principal_id_principal_kind_fkey` | hand |
| P4 | wamn_system | `identity.project_env_memberships` | `principal_kind` → `principal_type` | `project_env_memberships_principal_kind_not_null`, `project_env_memberships_principal_id_principal_kind_fkey`. `project_env_memberships_human_check` keeps its name | hand |
| P5 | wamn_system | `provisioning.sagas` | `kind` → `type` | `sagas_kind_check`, `sagas_kind_not_null` | hand |
| P6 | wamn_system | `provisioning.copy_sagas` | `kind` → `type` | `copy_sagas_kind_check`, `copy_sagas_kind_not_null` | hand |
| P7 | wamn_system | `catalog.authoring_command_audit` | `command_kind` → `command_type`, `principal_kind` → `principal_type` | `authoring_command_audit_command_kind_check`, `authoring_command_audit_command_kind_not_null`, `authoring_command_audit_principal_kind_check`, `authoring_command_audit_principal_kind_not_null` | hand |
| P8 | project-env | `catalog.package_definition_owners` | `definition_kind` → `definition_type` | `package_definition_owners_definition_kind_check`, `package_definition_owners_definition_kind_not_null`. The primary key keeps its name | `reconcile-run-plane` |
| P9 | project-env | every `<relation>_history` | `kind` → `type` | `<history>_kind_check`, `<history>_kind_not_null` | hand |
| P10 | project-env | `wamn_run.runs` | `caller_outcome_kind` → `caller_outcome_type`, `fail_kind` → `fail_type` | `runs_caller_outcome_kind_check`, `runs_fail_kind_check`. Both columns are nullable, so no `NOT NULL` name | `reconcile-run-plane` |
| P11 | project-env | `wamn_run.effect_attempts` | `generation_fact_kind` → `generation_fact_type` | `effect_attempts_generation_fact_kind_not_null`. `effect_attempts_generation_fact_check` and `effect_attempts_generation_values_check` keep their names | `reconcile-run-plane` |
| P12 | project-env | `wamn_run.operator_run_actions` | `action_kind` → `action_type`, `principal_kind` → `principal_type` | `operator_run_actions_kind_check` (new name `operator_run_actions_type_check`), `operator_run_actions_action_kind_not_null`, `operator_run_actions_principal_kind_check`, `operator_run_actions_principal_kind_not_null` | `reconcile-run-plane` |

Every other constraint that reads one of these columns keeps its name. Examples are `orgs_pool_cluster_check`, `principals_subject_check`, `principals_email_check`, `principals_platform_principal_check`, `package_definition_owners_relation_shape_check`, `package_definition_owners_extensibility_check` and `runs_check6` to `runs_check8`.

The auto-generated names in the table follow the PostgreSQL rules: `<table>_<column>_check` for a check that reads one column, `<table>_<columns>_key` for a unique key, `<table>_<columns>_fkey` for a foreign key and `<table>_<column>_not_null` for a `NOT NULL`. The check query in §4.3 confirms them on each database before anything runs. The table is complete. No index outside a unique key holds a renamed name (§1.4). Every other check on these tables either has an explicit name without `kind` or reads more than one column, so its generated name holds no column name.

P14 aliases are not persisted and need no DDL. `AS outcome_kind` (`crates/execution/run-state/src/transitions.rs:178`) and `AS definition_kind` (`crates/control/lib/src/apply_package/definition_ownership.rs:48`) name WAMN columns and follow them. The aliases in `crates/schema/introspection/src/postgres.rs` quote PostgreSQL catalog columns such as `relkind` and `contype`, so the §1.8 rule keeps them.

W11, the `Placement` registry JSON, is not stored in any database. `registry.orgs` keeps the placement as the two columns `placement_kind` and `pool_cluster` (`crates/control/registry/src/sql.rs:18`). `Registry::to_json` and `Registry::from_json` (`crates/control/registry/src/types.rs:536`) have no caller outside tests, and no registry JSON file is tracked. So W11 needs no schema change.

### 4.2 Fresh-install DDL

The composed constants `SYSTEM_SCHEMA_SQL`, `CONTROL_PORTABLE_STORE_SQL`, `OPS_SCHEMA_SQL`, `APP_SCHEMA_SQL` (`crates/control/provision/src/lib.rs:151`) and `CATALOG_SCHEMA_SQL` (`crates/catalog/model/src/lib.rs:77`) take the files by `include_str!`. They change with the files and need no edit of their own.

| Row | File and line | Edit |
|---|---|---|
| P1 | `deploy/sql/system-schema.sql:147` | `placement_kind text NOT NULL` → `placement_type text NOT NULL` |
| P1 | `deploy/sql/system-schema.sql:149`, `:150` | `CONSTRAINT orgs_placement_type_check CHECK (placement_type IN ('pooled', 'dedicated'))` |
| P1 | `deploy/sql/system-schema.sql:152` | `orgs_pool_cluster_check` reads `placement_type` |
| P2 | `deploy/sql/system-schema.sql:259` | `kind text NOT NULL` → `type text NOT NULL` |
| P2 | `deploy/sql/system-schema.sql:269`, `:270` | `UNIQUE (id, type)`, `UNIQUE (type, subject)` |
| P2 | `deploy/sql/system-schema.sql:272`, `:273` | `CONSTRAINT principals_type_check CHECK (type IN ('human', 'service', 'platform'))` |
| P2 | `deploy/sql/system-schema.sql:275`, `:279`, `:290` | `principals_subject_check`, `principals_email_check` and `principals_platform_principal_check` read `type` |
| P2 | `deploy/sql/system-schema.sql:305` | The `wamn:provisioning` seed inserts `(id, type, subject, display_name)` |
| P2 | `deploy/sql/system-schema.sql:752` | `identity.lock_password_principal` reads `status = 'active' AND type = 'human'` |
| P3 | `deploy/sql/system-schema.sql:346` | `principal_type text NOT NULL` |
| P3 | `deploy/sql/system-schema.sql:356`, `:357` | `FOREIGN KEY (principal_id, principal_type) REFERENCES identity.principals (id, type)` |
| P3 | `deploy/sql/system-schema.sql:358`, `:359` | `CONSTRAINT pats_principal_type_check CHECK (principal_type IN ('human', 'service'))` |
| P4 | `deploy/sql/system-schema.sql:384`, `:402`, `:425` | `principal_type text NOT NULL DEFAULT 'human' CHECK (principal_type = 'human')` |
| P4 | `deploy/sql/system-schema.sql:391`, `:392`, `:410`, `:411`, `:436`, `:437` | `FOREIGN KEY (principal_id, principal_type) REFERENCES identity.principals (id, type)` |
| P4 | `deploy/sql/system-schema.sql:580` | `principal_type text NOT NULL DEFAULT 'human'` |
| P4 | `deploy/sql/system-schema.sql:589`, `:590`, `:594` | The foreign key as above, and `project_env_memberships_human_check` reads `principal_type` |
| P5 | `deploy/sql/system-schema.sql:731`, `:739`, `:740` | `type text NOT NULL`, `CONSTRAINT sagas_type_check CHECK (type IN ('provision-org', 'provision-project-env'))` |
| P6 | `deploy/sql/ops-schema.sql:30`, `:38` | `type text NOT NULL`, `CONSTRAINT copy_sagas_type_check CHECK (type = 'copy')` |
| P7 | `deploy/sql/control-portable-store.sql:270` | `command_type text NOT NULL CHECK (command_type IN ('gate', 'publish'))` |
| P7 | `deploy/sql/control-portable-store.sql:272` | `principal_type text NOT NULL CHECK (principal_type IN ('human', 'service'))` |
| P8 | `deploy/sql/catalog-schema.sql:59`, `:60` | `definition_type text NOT NULL CHECK (definition_type IN ('relation', 'field', 'constraint'))` |
| P8 | `deploy/sql/catalog-schema.sql:67`, `:70`, `:73` | The primary key and the two named checks read `definition_type` |
| P9 | `deploy/sql/record-history.sql:117` | `(5, 'type', 'text')` |
| P9 | `deploy/sql/record-history.sql:126`, `:135`, `:139` | `history \|\| '_type_check'` in the name list and in the `CREATE TABLE`, and `CHECK (type IN (''insert'', ''update'', ''delete''))` |
| P9 | `deploy/sql/record-history.sql:240` | The log function writes `row_key, type, operation, ...` |
| P9 | `deploy/sql/app-schema.sql:525` | `GRANT INSERT (tenant_id, row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON app_system.configurations_history TO wamn_app` |
| P9 | `apps/platform/data/record-history/src/lib.rs:55` | `("type", "text")` in `HISTORY_COLUMNS` |
| P9 | `apps/platform/data/record-history/src/lib.rs:88` | `format!("{history}_type_check")` in `history_object_names_fit` |
| P10 | `deploy/sql/run-state.sql:377`, `:378` | `caller_outcome_type text CHECK (caller_outcome_type IN ('responded', 'failed'))` |
| P10 | `deploy/sql/run-state.sql:387` | `fail_type text CHECK (fail_type IN (...))` |
| P10 | `deploy/sql/run-state.sql:419`, `:421`, `:423` | `runs_check6`, `runs_check7` and `runs_check8` read `caller_outcome_type` |
| P10 | `crates/schema/control/src/run_plane/declarations.rs:104` to `:106`, `:116` to `:118` | Names `runs_caller_outcome_type_check` and `runs_fail_type_check`, their definitions, and `CheckOrigin::Inline("caller_outcome_type")` and `CheckOrigin::Inline("fail_type")` |
| P10 | `crates/schema/control/src/run_plane/declarations.rs:147`, `:162`, `:168` | The `runs_check6` to `runs_check8` definitions read `caller_outcome_type` |
| P11 | `deploy/sql/run-state.sql:603`, `:633`, `:635`, `:639` | `generation_fact_type text NOT NULL`, and both named checks read it |
| P11 | `crates/schema/control/src/run_plane/declarations.rs:276`, `:282` | Both check definitions read `generation_fact_type` |
| P12 | `deploy/sql/run-state.sql:795`, `:799` | `action_type text NOT NULL`, `principal_type text NOT NULL` |
| P12 | `deploy/sql/run-state.sql:809`, `:810` | `CONSTRAINT operator_run_actions_type_check CHECK (action_type = 'terminalize-effect-uncertain')` |
| P12 | `deploy/sql/run-state.sql:816`, `:817` | `CONSTRAINT operator_run_actions_principal_type_check CHECK (principal_type = 'database-role')` |
| P12 | `crates/schema/control/src/run_plane/declarations.rs:377`, `:378`, `:401`, `:402` | The two names and definitions as in `run-state.sql` |
| Component key | `deploy/sql/catalog-schema.sql:191` to `:227` | The new table `catalog.component_digest_owners` before `component_library`. `component_library_digest_key` goes. `component_library_digest_owner_fkey` comes. `connection_requirements_component_fkey` references the owner table. The exact text is in §4.3.6 |
| Component key | `deploy/sql/catalog-schema.sql:489` to `:497`, `:522` to `:530` | `component_digest_owners` joins the tenant floor list and the immutable list |
| Component key | `deploy/sql/control-portable-store.sql:171` to `:215`, `:340` to `:347`, `:370` to `:375`, `:465` to `:470` | The same change keyed by `environment_instance`. The table joins the policy list, the immutable list and the catalog inventory check |
| Snapshot table | `deploy/sql/catalog-schema.sql:432` to `:444`, `:456`, `:495`, `:529`, `:553` | `catalog.release_manifest_v3_snapshots` becomes `catalog.release_manifest_snapshots`, with `release_manifest_snapshots_pkey`, `_release_fkey` and `_exact_hash`. `catalog.guard_release_component_insert` reads the new name |

The declarations in `declarations.rs` are the text that `pg_get_constraintdef` renders. `reconcile-run-plane` compares them with the installed checks. So they must match `run-state.sql` in the same commit.

Comments that name a column follow it: `system-schema.sql:127`, `:228`, `:341`, `run-state.sql:304`, `record-history.sql` and `apps/platform/data/record-history/src/lib.rs:80`.

Column grants name columns in Rust. They change with the DDL:

- `crates/control/provision/src/identity_issuer.rs:26`, `:37`. The issuer reads `principals.type` and inserts `pats.principal_type`.
- `crates/control/provision/src/sql.rs:242`, `:247`. The executor updates `runs.fail_type` and `runs.caller_outcome_type`.

### 4.3 Installed databases

Four paths change installed state. A verb carries every change that a verb already knows how to make. Hand statements carry only what no verb can do (§4.9 ruling 3).

| Path | Rows | Where |
|---|---|---|
| `reconcile-run-plane` | P8, P10, P11, P12 | Each project-env database, by the new verb |
| Hand statements | P1 to P7, and the P9 functions | wamn_system |
| Hand statements | P9 tables and functions | Each project-env database |
| `kubectl annotate` | P15 | Each installed PAT Secret |
| Hand statements | Component key and snapshot table (§4.3.6) | Each project-env database. The component key also in wamn_system |

Every database change here is metadata-only in PostgreSQL. `ALTER TABLE ... RENAME COLUMN` and `ALTER TABLE ... RENAME CONSTRAINT` change catalog rows only. No table is rewritten and no row changes. `CREATE OR REPLACE FUNCTION` changes the stored function text only. Each `ALTER TABLE` takes an `ACCESS EXCLUSIVE` lock until its transaction commits, so the changes run with the workloads stopped (§4.7).

What follows a rename by itself, because PostgreSQL stores it by column number and not by name:

- check expressions, foreign keys, unique keys and their indexes, and column defaults
- column grants, such as the issuer grants of §4.2, the executor grants at `crates/control/provision/src/sql.rs:242` and the column grant at `app-schema.sql:525`
- row security policies. No policy reads a renamed column (`git grep -n -i 'CREATE POLICY' -- deploy/sql` finds none that names one)
- trigger `WHEN` clauses and `UPDATE OF` column lists. None names a renamed column
- publications. The CDC publication is `FOR TABLES IN SCHEMA <schema>` with no column list (`crates/control/provision/src/sql/cdc.rs:82`)

What does not follow a rename, because PostgreSQL stores it as text:

- PL/pgSQL function bodies. Three name a renamed column: `wamn_history.create_history_table` (`record-history.sql:117`, `:135`), `wamn_history.log_row_change` (`record-history.sql:240`) and `identity.lock_password_principal` (`system-schema.sql:752`). The hand statements replace all three in the same transaction as the renames. Without that, every write to a logged relation fails, and every password login fails.

No view, materialized view, rule, statistics object or event trigger exists in `deploy/sql` or in SQL that Rust creates. The other functions in `deploy/sql` and in `crates/schema/control/src/run_plane/declarations.rs:431` to `:527` name no renamed column.

**Check query.** Run it on each database before and after. Before, the result must be exactly the names of §4.1 for that database, plus the function bodies above. After, it must be empty.

```sql
SELECT 'column' AS object, n.nspname, c.relname, a.attname AS name
  FROM pg_catalog.pg_attribute AS a
  JOIN pg_catalog.pg_class AS c ON c.oid = a.attrelid
  JOIN pg_catalog.pg_namespace AS n ON n.oid = c.relnamespace
 WHERE a.attname LIKE '%kind%' AND a.attnum > 0 AND NOT a.attisdropped
   AND c.relkind IN ('r', 'p') AND n.nspname NOT IN ('pg_catalog', 'information_schema')
UNION ALL
SELECT 'constraint', n.nspname, c.relname, con.conname
  FROM pg_catalog.pg_constraint AS con
  JOIN pg_catalog.pg_class AS c ON c.oid = con.conrelid
  JOIN pg_catalog.pg_namespace AS n ON n.oid = c.relnamespace
 WHERE con.conname LIKE '%kind%'
UNION ALL
SELECT 'function', n.nspname, p.proname, p.oid::regprocedure::text
  FROM pg_catalog.pg_proc AS p
  JOIN pg_catalog.pg_namespace AS n ON n.oid = p.pronamespace
 WHERE p.prosrc ~ '\mkind\M' AND n.nspname NOT IN ('pg_catalog', 'information_schema')
ORDER BY 1, 2, 3, 4;
```

#### 4.3.1 What `reconcile-run-plane` changes today

The verb already changes installed schema in code. Its scope is the `--schema` project-env schema plus the `catalog` schema of the same database (`crates/control/lib/src/reconcile_run_plane.rs:41`). It observes the live tables, columns, checks and foreign keys (`crates/schema/control/src/run_plane/observation_sql.rs:350`, `:360`) and the `catalog` columns (`crates/control/lib/src/reconcile_run_plane.rs:1140` to `:1153`). It plans against the schema of record, which is `deploy/sql` itself (`crates/schema/control/src/run_plane.rs:18`). The changes it makes to installed schema today:

- It adds a missing record column (`crates/schema/control/src/run_plane/plan.rs:651`, `:910`).
- It drops and re-adds a drifted check, and drops a check that the record does not have (`plan.rs:1052`, `:1076`).
- It runs named cutovers that remove retired columns and indexes, for example the rerun-lineage cutover (`crates/schema/control/src/run_plane/schema_changes.rs:1006`, `:1014`).
- It alters the installed `catalog.release_components` in place (`schema_changes.rs:24`, planned at `plan.rs:487` from the probe at `reconcile_run_plane.rs:1152`).

It applies as a role with `SUPERUSER` or `BYPASSRLS` (`reconcile_run_plane.rs:795` to `:810`). Each action is one `batch_execute` (`reconcile_run_plane.rs:824`, `:832`). PostgreSQL runs a multi-statement batch without its own `BEGIN` as one implicit transaction, so each action is atomic.

The verb has no rename today. Without one, the new verb does the wrong thing on an unrenamed database. It adds `fail_type` and `caller_outcome_type` as new empty columns and reports `fail_kind` and `caller_outcome_kind` as unknown extra columns (`plan.rs:960`). It drops `runs_fail_kind_check` and the other old checks as extra (`plan.rs:1076`). Its `ADD COLUMN generation_fact_type text NOT NULL` fails on a table with rows. So the rename is a new cutover that runs before every other action.

#### 4.3.2 Code changes in `reconcile-run-plane`

P8, P10, P11 and P12 move into the verb. Nothing of them stays a hand statement. P8 is in the `catalog` schema, which is inside the verb's scope, and the verb already alters an installed `catalog` table.

- **Observation.** The verb already reads the columns of `runs`, `effect_attempts` and `operator_run_actions`, and the `catalog` columns. Add one flag beside `release_components_without_routes` (`reconcile_run_plane.rs:1152`, `crates/schema/control/src/run_plane/observation.rs:139`): whether `catalog.package_definition_owners` has `definition_kind`.
- **Detection.** If any old column of §4.1 rows P8, P10, P11 or P12 exists, the plan needs the cutover. If a table has both the old and the new column, or an old check of §4.1 is missing, the cutover refuses with SQLSTATE 55000. Those states have no safe rename.
- **Action.** A new action in `RunPlaneActionKind` (`crates/schema/control/src/run_plane.rs:146`), working name `TypeColumnCutover`, is the first action of the plan. Its SQL is one batch, so it is one transaction. The SQL is below, with `wamn_run` rewritten to `--schema` by `rewrite_schema` as for every run-plane section.
- **Planning after the cutover.** The planner plans the other actions against the observation with the new names applied. So the same run adds no column and drops no check for the renamed objects. The existing cutovers set this precedent: the planner already skips the objects that a cutover owns (`plan.rs:916` to `:959`, `:1063` to `:1074`). A second run then plans nothing, which is the verb's idempotence rule (`crates/schema/control/src/run_plane.rs:235`).
- **Records.** `declarations.rs` and `run-state.sql` take the new names (§4.2). `CheckOrigin::Inline` names the new columns.
- **Tests.** A unit plan case in `crates/schema/control/src/run_plane/tests.rs`. A live case in `crates/control/lib/tests/run_plane_live/` builds a database from the old `run-state.sql` and `catalog-schema.sql`, reconciles it, and shows that the check query of §4.3 is empty for these tables and that a second plan is a no-op.

The verb does not observe `NOT NULL` constraints. It reads `contype` `c` and `f` only (`observation_sql.rs:350`, `:360`). So the cutover renames the `NOT NULL` names by name, inside a guard. The checks are renamed in plain statements, because the verb observes them and the detection refuses before the batch when one is missing.

```sql
LOCK TABLE wamn_run.runs, wamn_run.effect_attempts, wamn_run.operator_run_actions,
           catalog.package_definition_owners IN ACCESS EXCLUSIVE MODE;
-- P8
ALTER TABLE catalog.package_definition_owners RENAME COLUMN definition_kind TO definition_type;
ALTER TABLE catalog.package_definition_owners RENAME CONSTRAINT package_definition_owners_definition_kind_check TO package_definition_owners_definition_type_check;
-- P10
ALTER TABLE wamn_run.runs RENAME COLUMN caller_outcome_kind TO caller_outcome_type;
ALTER TABLE wamn_run.runs RENAME CONSTRAINT runs_caller_outcome_kind_check TO runs_caller_outcome_type_check;
ALTER TABLE wamn_run.runs RENAME COLUMN fail_kind TO fail_type;
ALTER TABLE wamn_run.runs RENAME CONSTRAINT runs_fail_kind_check TO runs_fail_type_check;
-- P11
ALTER TABLE wamn_run.effect_attempts RENAME COLUMN generation_fact_kind TO generation_fact_type;
-- P12
ALTER TABLE wamn_run.operator_run_actions RENAME COLUMN action_kind TO action_type;
ALTER TABLE wamn_run.operator_run_actions RENAME CONSTRAINT operator_run_actions_kind_check TO operator_run_actions_type_check;
ALTER TABLE wamn_run.operator_run_actions RENAME COLUMN principal_kind TO principal_type;
ALTER TABLE wamn_run.operator_run_actions RENAME CONSTRAINT operator_run_actions_principal_kind_check TO operator_run_actions_principal_type_check;
-- The NOT NULL names of P8, P11 and P12.
DO $type_column_not_null$
DECLARE
    renamed record;
BEGIN
    FOR renamed IN
        SELECT * FROM (VALUES
            ('catalog', 'package_definition_owners', 'package_definition_owners_definition_kind_not_null', 'package_definition_owners_definition_type_not_null'),
            ('wamn_run', 'effect_attempts', 'effect_attempts_generation_fact_kind_not_null', 'effect_attempts_generation_fact_type_not_null'),
            ('wamn_run', 'operator_run_actions', 'operator_run_actions_action_kind_not_null', 'operator_run_actions_action_type_not_null'),
            ('wamn_run', 'operator_run_actions', 'operator_run_actions_principal_kind_not_null', 'operator_run_actions_principal_type_not_null')
        ) AS names (schema_name, table_name, old_name, new_name)
    LOOP
        IF EXISTS (SELECT FROM pg_catalog.pg_constraint AS con
                     JOIN pg_catalog.pg_class AS c ON c.oid = con.conrelid
                     JOIN pg_catalog.pg_namespace AS n ON n.oid = c.relnamespace
                    WHERE n.nspname = renamed.schema_name AND c.relname = renamed.table_name
                      AND con.conname = renamed.old_name) THEN
            EXECUTE format('ALTER TABLE %I.%I RENAME CONSTRAINT %I TO %I',
                           renamed.schema_name, renamed.table_name,
                           renamed.old_name, renamed.new_name);
        END IF;
    END LOOP;
END
$type_column_not_null$;
```

#### 4.3.3 Why the rest stays by hand

- **wamn_system (P1 to P7, the P9 functions).** `provision-system` installs `CONTROL_BOOTSTRAP_SQL` once and refuses a database that already has the schema `registry` (`crates/control/lib/src/provision_system.rs:8`, `:51` to `:63`). No other verb changes installed wamn_system schema. This is the `wamn-o8b9` finding.
- **P9 in project-env.** No verb changes an installed history table or the installed record-history functions. `reconcile-run-plane` never alters application or floor tables (`reconcile_run_plane.rs:41` to `:44`). It installs `app-schema.sql` only when `app_system` is absent (`reconcile_run_plane.rs:628`), and `record-history.sql` only inside `CATALOG_SCHEMA_SQL` when `catalog` is absent (`plan.rs:496`). apply-package skips a history table that exists (`crates/control/lib/src/apply_package/record_history.rs:51`).

#### 4.3.4 Hand statements

Run them as the `postgres` superuser with no `SET ROLE`. `CREATE OR REPLACE FUNCTION` keeps the owner, the `SECURITY DEFINER` setting and the grants of the function. The renames create nothing new, so no ownership question arises. §4.3.6 creates one table and gives it the owner of `catalog.component_library`. Each database takes one transaction, and `ON_ERROR_STOP` rolls it back on the first wrong name. Foreign keys and unique keys follow the rename by column number, so the order inside the transaction does not affect correctness. The statements rename the referenced table first, so that a reader can check them against §4.1 row by row.

`deploy/sql/record-history.sql` is idempotent: `CREATE SCHEMA IF NOT EXISTS`, `CREATE OR REPLACE FUNCTION`, and grants that the database already holds (`record-history.sql:37` to `:286`). It carries no transaction of its own (`record-history.sql:4`). So each script appends the file of the migration commit before `COMMIT`, and the installed functions become the fresh-install functions.

**ops-schema.sql.** `provision-system` installs `CONTROL_BOOTSTRAP_SQL`, which is `SYSTEM_SCHEMA_SQL` and `CONTROL_PORTABLE_STORE_SQL` (`crates/control/provision/src/lib.rs:166`, `crates/control/lib/src/provision_system.rs:84`). It does not install `ops-schema.sql`. Only `wamn-ctl-ops copy-project-env` installs it (`crates/control/lib/src/copy_project_env.rs:488`, `crates/control/lib/src/ops_schema.rs:21`), and `docs/operations/gcp.md` records no run of that verb. So the operations page does not say whether P6 exists on wamn-dev. The deployment agent answers it on wamn_system before the apply, and the record states the answer:

```sql
SELECT to_regclass('provisioning.copy_sagas') IS NOT NULL AS ops_schema_installed;
```

When the answer is false, leave the three P6 lines out of the script. The script names P6 without `IF EXISTS`, so a wrong answer fails loudly.

**wamn_system.** Save as `$P/kind-to-type-system.sql`:

```sql
BEGIN;
-- P1
ALTER TABLE registry.orgs RENAME COLUMN placement_kind TO placement_type;
ALTER TABLE registry.orgs RENAME CONSTRAINT orgs_placement_kind_check TO orgs_placement_type_check;
ALTER TABLE registry.orgs RENAME CONSTRAINT orgs_placement_kind_not_null TO orgs_placement_type_not_null;
-- P2, the table that the P3 and P4 foreign keys reference
ALTER TABLE identity.principals RENAME COLUMN kind TO type;
ALTER TABLE identity.principals RENAME CONSTRAINT principals_kind_check TO principals_type_check;
ALTER TABLE identity.principals RENAME CONSTRAINT principals_kind_not_null TO principals_type_not_null;
ALTER TABLE identity.principals RENAME CONSTRAINT principals_id_kind_key TO principals_id_type_key;
ALTER TABLE identity.principals RENAME CONSTRAINT principals_kind_subject_key TO principals_type_subject_key;
-- P3
ALTER TABLE identity.pats RENAME COLUMN principal_kind TO principal_type;
ALTER TABLE identity.pats RENAME CONSTRAINT pats_principal_kind_check TO pats_principal_type_check;
ALTER TABLE identity.pats RENAME CONSTRAINT pats_principal_kind_not_null TO pats_principal_type_not_null;
ALTER TABLE identity.pats RENAME CONSTRAINT pats_principal_id_principal_kind_fkey TO pats_principal_id_principal_type_fkey;
-- P4
ALTER TABLE identity.password_credentials RENAME COLUMN principal_kind TO principal_type;
ALTER TABLE identity.password_credentials RENAME CONSTRAINT password_credentials_principal_kind_check TO password_credentials_principal_type_check;
ALTER TABLE identity.password_credentials RENAME CONSTRAINT password_credentials_principal_kind_not_null TO password_credentials_principal_type_not_null;
ALTER TABLE identity.password_credentials RENAME CONSTRAINT password_credentials_principal_id_principal_kind_fkey TO password_credentials_principal_id_principal_type_fkey;
ALTER TABLE identity.password_tokens RENAME COLUMN principal_kind TO principal_type;
ALTER TABLE identity.password_tokens RENAME CONSTRAINT password_tokens_principal_kind_check TO password_tokens_principal_type_check;
ALTER TABLE identity.password_tokens RENAME CONSTRAINT password_tokens_principal_kind_not_null TO password_tokens_principal_type_not_null;
ALTER TABLE identity.password_tokens RENAME CONSTRAINT password_tokens_principal_id_principal_kind_fkey TO password_tokens_principal_id_principal_type_fkey;
ALTER TABLE identity.password_logins RENAME COLUMN principal_kind TO principal_type;
ALTER TABLE identity.password_logins RENAME CONSTRAINT password_logins_principal_kind_check TO password_logins_principal_type_check;
ALTER TABLE identity.password_logins RENAME CONSTRAINT password_logins_principal_kind_not_null TO password_logins_principal_type_not_null;
ALTER TABLE identity.password_logins RENAME CONSTRAINT password_logins_principal_id_principal_kind_fkey TO password_logins_principal_id_principal_type_fkey;
ALTER TABLE identity.project_env_memberships RENAME COLUMN principal_kind TO principal_type;
ALTER TABLE identity.project_env_memberships RENAME CONSTRAINT project_env_memberships_principal_kind_not_null TO project_env_memberships_principal_type_not_null;
ALTER TABLE identity.project_env_memberships RENAME CONSTRAINT project_env_memberships_principal_id_principal_kind_fkey TO project_env_memberships_principal_id_principal_type_fkey;
-- P5
ALTER TABLE provisioning.sagas RENAME COLUMN kind TO type;
ALTER TABLE provisioning.sagas RENAME CONSTRAINT sagas_kind_check TO sagas_type_check;
ALTER TABLE provisioning.sagas RENAME CONSTRAINT sagas_kind_not_null TO sagas_type_not_null;
-- P6. Only when the ops-schema query answered true.
ALTER TABLE provisioning.copy_sagas RENAME COLUMN kind TO type;
ALTER TABLE provisioning.copy_sagas RENAME CONSTRAINT copy_sagas_kind_check TO copy_sagas_type_check;
ALTER TABLE provisioning.copy_sagas RENAME CONSTRAINT copy_sagas_kind_not_null TO copy_sagas_type_not_null;
-- P7
ALTER TABLE catalog.authoring_command_audit RENAME COLUMN command_kind TO command_type;
ALTER TABLE catalog.authoring_command_audit RENAME CONSTRAINT authoring_command_audit_command_kind_check TO authoring_command_audit_command_type_check;
ALTER TABLE catalog.authoring_command_audit RENAME CONSTRAINT authoring_command_audit_command_kind_not_null TO authoring_command_audit_command_type_not_null;
ALTER TABLE catalog.authoring_command_audit RENAME COLUMN principal_kind TO principal_type;
ALTER TABLE catalog.authoring_command_audit RENAME CONSTRAINT authoring_command_audit_principal_kind_check TO authoring_command_audit_principal_type_check;
ALTER TABLE catalog.authoring_command_audit RENAME CONSTRAINT authoring_command_audit_principal_kind_not_null TO authoring_command_audit_principal_type_not_null;
-- P2 function body
CREATE OR REPLACE FUNCTION identity.lock_password_principal(principal uuid) RETURNS boolean
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog AS $$
DECLARE eligible boolean;
BEGIN
    SELECT status = 'active' AND type = 'human' INTO eligible
    FROM identity.principals WHERE id = principal FOR UPDATE;
    RETURN COALESCE(eligible, false);
END;
$$;
-- Component key, control copy: append the wamn_system statements of §4.3.6 here.
-- P9 function bodies: append the new deploy/sql/record-history.sql here.
COMMIT;
```

wamn_system holds the record-history functions (`SYSTEM_SCHEMA_SQL`, `crates/control/provision/src/lib.rs:151`) but no history table. Its identity relations carry stamp triggers only (`system-schema.sql:296` and the other `wamn_record_history_stamp` triggers), and `record-history-app-grants.sql:6` says the system database never applies the app grants. So P9 in wamn_system is the function replacement only.

**Each project-env database, P9 only.** In wamn-dev these are `wamn-db-dkk--receiving--dev--4pqjfmli` and `wamn-db-dkk--wms--dev--0nk1lrpr` (`docs/operations/gcp.md` §3.18 and §5.4). `SELECT datname FROM pg_database WHERE datname LIKE 'wamn-db-%'` confirms the list. Save as `$P/kind-to-type-project-env.sql`:

```sql
BEGIN;
-- P9 tables. The set varies by package, so the block finds each history table
-- by the check constraint that create_history_table gave it.
DO $rename_history$
DECLARE
    history record;
BEGIN
    FOR history IN
        SELECT n.nspname AS schema_name, c.relname AS table_name
          FROM pg_catalog.pg_constraint AS con
          JOIN pg_catalog.pg_class AS c ON c.oid = con.conrelid
          JOIN pg_catalog.pg_namespace AS n ON n.oid = c.relnamespace
         WHERE con.contype = 'c' AND c.relkind = 'r'
           AND c.relname LIKE '%\_history'
           AND con.conname = c.relname || '_kind_check'
         ORDER BY 1, 2
    LOOP
        EXECUTE format('ALTER TABLE %I.%I RENAME COLUMN kind TO type',
                       history.schema_name, history.table_name);
        EXECUTE format('ALTER TABLE %I.%I RENAME CONSTRAINT %I TO %I',
                       history.schema_name, history.table_name,
                       history.table_name || '_kind_check', history.table_name || '_type_check');
        EXECUTE format('ALTER TABLE %I.%I RENAME CONSTRAINT %I TO %I',
                       history.schema_name, history.table_name,
                       history.table_name || '_kind_not_null', history.table_name || '_type_not_null');
        RAISE NOTICE 'history renamed: %.%', history.schema_name, history.table_name;
    END LOOP;
END
$rename_history$;
-- Component key and snapshot table: append the project-env statements of §4.3.6 here.
-- P9 function bodies: append the new deploy/sql/record-history.sql here.
COMMIT;
```

`create_history_table` names both constraints, `<history>_kind_check` and `<history>_kind_not_null` (`record-history.sql:106`, `:139`). The block therefore finds every history table, in `app_system` (`app-schema.sql:435`) and in each package schema (`crates/control/lib/src/apply_package/record_history.rs:57`). It prints one notice per table. In wamn-dev the expected tables are the six `app_system` tables in both databases, `receiving.purchase_order_history` and `receiving.purchase_order_line_history` in Receiving, and `wms.packaging_history` in WMS. These come from the generated data-access overlays. The notices are the evidence to record.

No history entry holds a renamed key. The `before` and `after` images copy the columns of the logged relation. No logged relation has a renamed column: the P1 to P8 and P10 to P12 tables carry no log trigger, and no application relation has a `kind` column (the only `kind` in `apps/*/wamn.json` outside A1 is the history column, A5).

#### 4.3.5 PAT Secret annotations (P15)

The code change: `crates/control/lib/src/provision_project_env/pat_secrets.rs:267` writes `wamn.io/principal-type`, and the template at `deploy/mvp/bootstrap.sh:118` reads it. The value stays `service`, which `bootstrap.sh:161` checks. The fixtures in `crates/control/lib/src/provision_project_env/tests.rs:117` and `deploy/mvp/tests/bootstrap.sh` follow.

The installed Secrets carry the label `app.kubernetes.io/component=project-env-pat` (`pat_secrets.rs:259`). List them:

```bash
kubectl get secret --all-namespaces -l app.kubernetes.io/component=project-env-pat \
  -o custom-columns=NAMESPACE:.metadata.namespace,NAME:.metadata.name --no-headers
```

Then run one command per Secret. It sets the new key and removes the old key in one patch:

```bash
kubectl -n <namespace> annotate secret <name> wamn.io/principal-type=service wamn.io/principal-kind-
```

The wamn-dev listing is possibly empty. `docs/operations/gcp.md` §3.16 writes the management-author PAT Secret to a file that is not applied (`gcp.md:758`), and §6.4 copies the operator PAT file to the bench client. The listing answers it, and the record states the count.

`bootstrap.sh` reads one annotation key, so the transition matters. When the principal annotation is missing or is not `service`, it classifies a Secret that has a PAT prefix as `invalid` (`bootstrap.sh:161`, `:176` to `:179`). For an `invalid` Secret it issues a new PAT and revokes the old prefix (`bootstrap.sh:317` to `:322`, `:436`, `:437`). So an old `bootstrap.sh` run against a re-annotated Secret, or a new one run against an old Secret, rotates that PAT without an error. §4.7 therefore allows no `bootstrap.sh` run from the stop of the workloads until the new `bootstrap.sh` is deployed, and requires every Secret to be re-annotated before the new `bootstrap.sh` runs.

#### 4.3.6 Component key and snapshot table (§5.4 rulings 1 and 3)

These are not `kind` renames. They go into the same scripts and the same record, under `wamn-o8b9`, because no verb changes an installed `catalog` key or table name (§4.3.3).

**What the old component key protects.** `catalog.component_library` holds `component_library_digest_key UNIQUE (tenant_id, component_digest)` (`deploy/sql/catalog-schema.sql:210`, `:211`). The control copy holds the same key per environment instance (`deploy/sql/control-portable-store.sql:193`, `:194`). The key is the target of `connection_requirements_component_fkey`, and connection requirements are keyed by digest alone (`catalog-schema.sql:216` to `:227`, `control-portable-store.sql:199` to `:215`). So the key makes one digest one fact row: one package, one version, one component name. Its purpose is that each requirement row belongs to exactly one component owner. A digest under a second package is refused as part of that. A digest under a second version of the same package is refused too, and that is the defect. The append is `ON CONFLICT DO NOTHING` with no target (`crates/control/lib/src/push_component.rs:58` to `:80`). So the key turns a second row into no insert, and the exact check then refuses `component-fact-conflict` (`push_component.rs:1977` to `:1996`). One reader also assumes one row per digest: `promote` reads projection hashes by digest alone (`crates/control/lib/src/promote.rs:62` to `:65`, `:441` to `:467`).

**The new constraint.** A digest belongs to one package for life. Each version of that package may hold it once. Another package may not hold it.

- `component_library_digest_key` goes.
- `component_library_package_digest_key UNIQUE (tenant_id, package_id, package_version, component_digest)` stays (`catalog-schema.sql:212`, `:213`). It is the key `(package, version, digest)`.
- A new table `catalog.component_digest_owners` holds one row per digest with its package. Its primary key `(tenant_id, component_digest)` refuses a second package. It is the new target of `connection_requirements_component_fkey`, so a requirement still belongs to one owner.
- `component_library_digest_owner_fkey FOREIGN KEY (tenant_id, component_digest, package_id)` references `component_digest_owners (tenant_id, component_digest, package_id)`. A library row under another package has no owner row to match.
- The control copy adds `environment_instance` after `tenant_id` in every key above.

Fresh install, `deploy/sql/catalog-schema.sql`, before `catalog.component_library`:

```sql
CREATE TABLE catalog.component_digest_owners (
    tenant_id        text NOT NULL CHECK (tenant_id <> ''),
    component_digest text NOT NULL CHECK (component_digest ~ '^sha256:[0-9a-f]{64}$'),
    package_id       text NOT NULL CHECK (package_id <> ''),
    CONSTRAINT component_digest_owners_pkey
        PRIMARY KEY (tenant_id, component_digest),
    CONSTRAINT component_digest_owners_package_key
        UNIQUE (tenant_id, component_digest, package_id)
);
```

In `catalog.component_library`, `component_library_digest_key` gives way to:

```sql
    CONSTRAINT component_library_digest_owner_fkey
        FOREIGN KEY (tenant_id, component_digest, package_id)
        REFERENCES catalog.component_digest_owners (tenant_id, component_digest, package_id),
```

`connection_requirements_component_fkey` references `catalog.component_digest_owners (tenant_id, component_digest)`. The code change: `append_or_verify_admitted_component_count` first inserts the owner row with `ON CONFLICT DO NOTHING`. When the stored owner names another package, it refuses `component-fact-conflict` and names the owner package. Then it appends the library row as today. `promote` reads projection hashes by package, version and digest.

**Hand statements, each project-env database.** Append to `$P/kind-to-type-project-env.sql` before `COMMIT;`:

```sql
-- Component key
CREATE TABLE catalog.component_digest_owners (
    tenant_id        text NOT NULL CHECK (tenant_id <> ''),
    component_digest text NOT NULL CHECK (component_digest ~ '^sha256:[0-9a-f]{64}$'),
    package_id       text NOT NULL CHECK (package_id <> ''),
    CONSTRAINT component_digest_owners_pkey PRIMARY KEY (tenant_id, component_digest),
    CONSTRAINT component_digest_owners_package_key UNIQUE (tenant_id, component_digest, package_id)
);
DO $owner$
BEGIN
    EXECUTE format('ALTER TABLE catalog.component_digest_owners OWNER TO %s',
                   (SELECT relowner::regrole::text FROM pg_catalog.pg_class
                     WHERE oid = 'catalog.component_library'::regclass));
END
$owner$;
INSERT INTO catalog.component_digest_owners (tenant_id, component_digest, package_id)
SELECT tenant_id, component_digest, package_id FROM catalog.component_library;
ALTER TABLE catalog.component_digest_owners ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.component_digest_owners FORCE ROW LEVEL SECURITY;
CREATE POLICY component_digest_owners_tenant ON catalog.component_digest_owners TO wamn_app
    USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())
    WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key());
CREATE POLICY component_digest_owners_platform ON catalog.component_digest_owners
    AS PERMISSIVE FOR ALL TO wamn_platform USING (true) WITH CHECK (true);
CREATE INDEX component_digest_owners_tkey
    ON catalog.component_digest_owners ((wamn_authority.tenant_key(tenant_id)));
CREATE TRIGGER component_digest_owners_immutable BEFORE UPDATE OR DELETE ON catalog.component_digest_owners
    FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();
REVOKE ALL ON catalog.component_digest_owners FROM PUBLIC;
ALTER TABLE catalog.component_library ADD CONSTRAINT component_library_digest_owner_fkey
    FOREIGN KEY (tenant_id, component_digest, package_id)
    REFERENCES catalog.component_digest_owners (tenant_id, component_digest, package_id);
ALTER TABLE catalog.connection_requirements DROP CONSTRAINT connection_requirements_component_fkey;
ALTER TABLE catalog.connection_requirements ADD CONSTRAINT connection_requirements_component_fkey
    FOREIGN KEY (tenant_id, component_digest)
    REFERENCES catalog.component_digest_owners (tenant_id, component_digest);
ALTER TABLE catalog.component_library DROP CONSTRAINT component_library_digest_key;
-- Snapshot table
ALTER TABLE catalog.release_manifest_v3_snapshots RENAME TO release_manifest_snapshots;
DO $rename_snapshot$
DECLARE
    old_name text;
BEGIN
    FOR old_name IN
        SELECT con.conname FROM pg_catalog.pg_constraint AS con
         WHERE con.conrelid = 'catalog.release_manifest_snapshots'::regclass
           AND con.conname LIKE 'release\_manifest\_v3\_snapshots\_%'
         ORDER BY 1
    LOOP
        EXECUTE format('ALTER TABLE catalog.release_manifest_snapshots RENAME CONSTRAINT %I TO %I',
                       old_name, replace(old_name, 'release_manifest_v3_snapshots', 'release_manifest_snapshots'));
        RAISE NOTICE 'snapshot constraint renamed: %', old_name;
    END LOOP;
END
$rename_snapshot$;
ALTER INDEX catalog.release_manifest_v3_snapshots_tkey RENAME TO release_manifest_snapshots_tkey;
ALTER POLICY release_manifest_v3_snapshots_tenant ON catalog.release_manifest_snapshots
    RENAME TO release_manifest_snapshots_tenant;
ALTER POLICY release_manifest_v3_snapshots_platform ON catalog.release_manifest_snapshots
    RENAME TO release_manifest_snapshots_platform;
ALTER TRIGGER release_manifest_v3_snapshots_immutable ON catalog.release_manifest_snapshots
    RENAME TO release_manifest_snapshots_immutable;
CREATE OR REPLACE FUNCTION catalog.guard_release_component_insert()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    PERFORM 1 FROM catalog.effective_releases
     WHERE tenant_id = NEW.tenant_id
       AND effective_release_id = NEW.effective_release_id
     FOR UPDATE;
    IF EXISTS (
        SELECT 1 FROM catalog.release_manifest_snapshots
         WHERE tenant_id = NEW.tenant_id
           AND effective_release_id = NEW.effective_release_id
    ) THEN
        RAISE EXCEPTION USING ERRCODE = '55000',
            MESSAGE = 'effective-release-snapshot-already-sealed';
    END IF;
    RETURN NEW;
END
$$;
```

The old key allows one row per digest, so the `INSERT` of the owner rows meets no conflict. The constraint loop renames the three named constraints and the generated `_check` and `_not_null` names of the four columns (§4.1 rule on generated names). It prints one notice for each. Renaming `release_manifest_v3_snapshots_pkey` also renames its index. The function body is the one of `deploy/sql/catalog-schema.sql:446` to `:465` with the new table name, because PostgreSQL stores a function body as text (§4.3). Row security, the grants and the trigger follow the table by OID. The grants that `crates/control/provision/src/sql.rs:211`, `:1104` write name the table, so they change with the code of §4.5.

**Hand statements, wamn_system.** Append to `$P/kind-to-type-system.sql` before `COMMIT;`. wamn_system has no snapshot table.

```sql
CREATE TABLE catalog.component_digest_owners (
    tenant_id            text NOT NULL CHECK (tenant_id <> ''),
    environment_instance text NOT NULL,
    component_digest     text NOT NULL CHECK (component_digest ~ '^sha256:[0-9a-f]{64}$'),
    package_id           text NOT NULL CHECK (package_id <> ''),
    CONSTRAINT component_digest_owners_pkey
        PRIMARY KEY (tenant_id, environment_instance, component_digest),
    CONSTRAINT component_digest_owners_package_key
        UNIQUE (tenant_id, environment_instance, component_digest, package_id)
);
DO $owner$
BEGIN
    EXECUTE format('ALTER TABLE catalog.component_digest_owners OWNER TO %s',
                   (SELECT relowner::regrole::text FROM pg_catalog.pg_class
                     WHERE oid = 'catalog.component_library'::regclass));
END
$owner$;
INSERT INTO catalog.component_digest_owners (tenant_id, environment_instance, component_digest, package_id)
SELECT tenant_id, environment_instance, component_digest, package_id FROM catalog.component_library;
ALTER TABLE catalog.component_digest_owners ENABLE ROW LEVEL SECURITY;
ALTER TABLE catalog.component_digest_owners FORCE ROW LEVEL SECURITY;
CREATE POLICY component_digest_owners_tenant ON catalog.component_digest_owners
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
CREATE TRIGGER component_digest_owners_immutable BEFORE UPDATE OR DELETE ON catalog.component_digest_owners
    FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change();
REVOKE ALL ON catalog.component_digest_owners FROM PUBLIC;
ALTER TABLE catalog.component_library ADD CONSTRAINT component_library_digest_owner_fkey
    FOREIGN KEY (tenant_id, environment_instance, component_digest, package_id)
    REFERENCES catalog.component_digest_owners (tenant_id, environment_instance, component_digest, package_id);
ALTER TABLE catalog.connection_requirements DROP CONSTRAINT connection_requirements_component_fkey;
ALTER TABLE catalog.connection_requirements ADD CONSTRAINT connection_requirements_component_fkey
    FOREIGN KEY (tenant_id, environment_instance, component_digest)
    REFERENCES catalog.component_digest_owners (tenant_id, environment_instance, component_digest);
ALTER TABLE catalog.component_library DROP CONSTRAINT component_library_digest_key;
```

The policy and trigger forms are those of `deploy/sql/control-portable-store.sql:335` to `:385`. The control copy must change with the project copy, because `push-component` writes both in one run (`crates/control/lib/src/push_component.rs:499` to `:512`, `:1602`). The ruling names the project-env databases only. Without the wamn_system part, B8 step 2 still refuses in the control copy.

**Check.** After the apply, on each database:

```sql
SELECT conrelid::regclass, conname, pg_get_constraintdef(oid)
  FROM pg_catalog.pg_constraint
 WHERE conrelid IN ('catalog.component_library'::regclass, 'catalog.component_digest_owners'::regclass,
                    'catalog.connection_requirements'::regclass)
   AND contype IN ('p', 'u', 'f')
 ORDER BY 1, 2;
SELECT count(*) FROM pg_catalog.pg_class WHERE relname LIKE '%release\_manifest\_v3%';
```

The first lists `component_library_package_digest_key`, `component_library_digest_owner_fkey`, both owner keys and the new requirement key, and no `component_library_digest_key`. The second answers 0 on each project-env database.

### 4.4 Edge SQLite

P13 is out of scope. `78d02a6d8` (wamn-4afx.1) removed `outcome_kind` from the edge intent table, so the table has no `kind` name left (`crates/execution/run-state-sqlite/src/lib.rs:30`). An `edge.db` made before that commit keeps the old column, because the store runs `CREATE TABLE IF NOT EXISTS` at every open and has no migration step (`lib.rs:110`). That is a consequence of wamn-4afx.1, not of this epic. No edge device runs in wamn-dev: `docs/operations/` has no edge-device section, and the only `edge.db` reference is the default path (`services/edge/src/config.rs:303`).

### 4.5 Code that changes with the DDL

The 218 Rust references of §1.4 (222 at the §1 commit) and the bare `kind` columns fall into these groups. Each group must land in the commit that edits the DDL it reads.

| Group | Columns | Files |
|---|---|---|
| Registry and provisioning | P1, P5, P6 | `crates/control/registry/src/sql.rs`, `crates/control/registry/src/types.rs`, `crates/control/lib/src/provision_org.rs`, `crates/control/lib/src/provision_project_env/registry.rs`, `crates/control/provision/src/saga.rs:5`, `crates/control/provision/src/state.rs:28`, `crates/control/provision/src/copy.rs:82` |
| Identity | P2, P3, P4 | `crates/identity/platform/src/lib.rs` (`PRINCIPAL_COLUMNS` and the PAT queries at `:31` to `:92`), `crates/identity/platform/src/password.rs:349`, `:453`, `crates/identity/platform/src/session_token.rs:154`, `crates/control/lib/src/reconcile_run_plane.rs:407`, `:422`, `crates/control/provision/src/identity_issuer.rs:26`, `:37`, `:462`, `:470` |
| Management audit | P7 | `services/scenario-worker/src/management.rs:61` and its attribution test at `:1669` |
| Package ownership | P8 | `crates/control/lib/src/apply_package/definition_ownership.rs`, `crates/control/lib/src/apply_package/error.rs`, `crates/control/lib/src/apply_package/package_version.rs`, `crates/control/lib/src/apply_package.rs` |
| Record history | P9 | `deploy/sql/record-history.sql`, `deploy/sql/app-schema.sql`, `apps/platform/data/record-history/src/lib.rs` (`HISTORY_COLUMNS`, the name check, and the fold that reads `HistoryRow.kind` at `:118`, `:265`), `apps/wamn_receiving/query/load_purchase_order_history.sql:12`, `apps/wamn_receiving/data/src/read.rs`, and everything the generator derives from `HISTORY_COLUMNS` (`crates/schema/generator/src/data_access.rs:624`, `:749`, `generate/contracts.rs:1341`, `generate/validation.rs:841`, `:974`) |
| Run plane | P10, P11, P12 | `crates/schema/control/src/run_plane/declarations.rs`, `schema_changes.rs`, `crates/execution/run-state/src/transitions.rs`, `run_store.rs`, `sql.rs`, `queue/sql.rs`, `operator_action.rs`, `crates/execution/workflow/src/queue.rs`, `router_action.rs`, `crates/platform/runtime/src/plugins/wamn_postgres/production_claim.rs`, `crates/control/lib/src/terminalize_effect_uncertain.rs`, `crates/control/provision/src/sql.rs:242`, `:247`, `:1107` |
| `reconcile-run-plane` cutover | P8, P10, P11, P12 | `crates/schema/control/src/run_plane.rs` (the new action), `crates/schema/control/src/run_plane/schema_changes.rs` (detection and SQL), `plan.rs` (first action, planning against the renamed observation), `observation.rs` and `crates/control/lib/src/reconcile_run_plane.rs` (the `definition_kind` probe), `crates/schema/control/src/run_plane/tests.rs`, `crates/control/lib/tests/run_plane_live/` (§4.3.2) |
| PAT Secret annotation | P15 | `crates/control/lib/src/provision_project_env/pat_secrets.rs:267`, `crates/control/lib/src/provision_project_env/tests.rs:117`, `deploy/mvp/bootstrap.sh:118`, `deploy/mvp/tests/bootstrap.sh` |
| Regenerated | P9 | Every `apps/*/generated/` file of §2.4, the three `generated/platform-policy/data-access.json` overlays (`wamn_receiving`, `wamn_wms`, `platform_fixture`), and `apps/wamn_receiving/tests/.sqlx/query-04951d1d….json`. That is the only SQLx query file that names a renamed column (`git grep -l -E '_kind\|\bkind\b' -- '*/.sqlx/*'`) |
| Tests and fixtures | all | The live and unit tests that the §1.7 command lists, including `crates/control/provision/tests/deploy_sql_authority.rs`, `control_storage.rs`, `control_portable_store.rs`, `identity_issuer_live.rs`, `crates/identity/platform/tests/`, `crates/identity/project-state/tests/`, `crates/control/lib/tests/run_plane_live/`, `crates/schema/control/src/run_plane/tests.rs`, `tests/conformance/src/schema_drift.rs`, `crates/schema/generator/tests/generation.rs`, `crates/platform/runtime/tests/support/session_fixture.rs:220`, and the `registry.orgs` fixture inserts across `crates/`, `services/` and `tests/integration/` |
| Scripts | P1 | `tools/identity-jwks-journey-run:428` inserts `placement_kind` |
| Component key | §4.3.6 | `deploy/sql/catalog-schema.sql`, `deploy/sql/control-portable-store.sql` (the table, the lists and the inventory check), `crates/control/lib/src/push_component.rs` (the owner row before the library row, `:58` to `:115`, `:1923` to `:1996`), `crates/control/lib/src/promote.rs:62` to `:65`, and the tests of the control store and of the component append: `crates/control/provision/tests/control_portable_store.rs`, the `push_component.rs` tests |
| Snapshot table, DDL | §4.3.6 | `deploy/sql/catalog-schema.sql` |
| Snapshot table, release mint and delivery | §4.3.6 | `crates/control/lib/src/publish_release.rs`, `crates/control/lib/src/publish_release/effective_release_live.rs`, `crates/control/lib/src/push_release_manifest.rs`, `crates/control/lib/src/print_release_env.rs`, `crates/control/lib/src/promote.rs` |
| Snapshot table, grants | §4.3.6 | `crates/control/provision/src/sql.rs:211`, `:1104`, `crates/control/provision/tests/family_denial_matrix.rs`, `crates/control/provision/tests/family_surface_grants.rs` |
| Snapshot table, runtime readers | §4.3.6 | `crates/platform/runtime/src/plugins/wamn_postgres/wiring_resolution.rs`, `crates/execution/workflow/src/contract/postgres.rs` |
| Snapshot table, tests | §4.3.6 | `crates/execution/run-state/tests/admission_live.rs`, `crates/execution/workflow/src/queue/automation_live.rs`, `crates/platform/runtime/tests/executor_platform_surface_live.rs`, `services/scenario-worker/tests/management_live.rs`, `tests/integration/src/local_application/assembly.rs`, `tests/integration/src/trusted_http_route.rs`, `apps/wamn_receiving/tests/route_authentication_live/dev/local_delivery.rs`, `apps/wamn_receiving/tests/route_authentication_live/environment.rs`, `apps/wamn_receiving/tests/route_authentication_live/fresh_only.rs`, `apps/wamn_receiving/tests/route_authentication_live/sessions.rs`, `apps/wamn_wms/tests/environment.rs` |

The five snapshot groups are the 22 tracked files that name the table outside `.beads`, `docs/history` and this file (`git grep -l release_manifest_v3_snapshots -- ':!.beads' ':!docs/history' ':!docs/plan/kind-to-type.md'`). After A7 that command prints nothing.

The `tests/sweeps/*.log` files that name `principal_kind` are records of past runs and stay.

### 4.6 CDC

No change-event payload key changes.

- The publication covers the application data schema only, `FOR TABLES IN SCHEMA <schema>` (`crates/control/provision/src/sql/cdc.rs:82`). In wamn-dev that is `receiving` and `wms` (`docs/operations/gcp.md` §3.18 and §5.4). The P1 to P8 and P10 to P12 tables live in `registry`, `identity`, `provisioning`, `catalog` and `wamn_run`, which no publication covers.
- The package history tables do live in the published schema. But each one is a declared CDC exclusion (`crates/schema/control/src/package_migrations.rs:385`). The reader classifies each relation by OID and drops an excluded relation before it builds a payload (`services/cdc-reader/src/lib.rs:339`, `:398`). A rename keeps the OID, so the classification holds.
- The payload is the column map of the changed row (`services/cdc-reader/src/lib.rs:478`). No published entity relation has a renamed column (§4.3).
- Logical decoding carries no DDL. After the rename, pgoutput sends a new relation message for a history table at its next change, and the reader drops that change as before. The slot and the publication need no change, and the CDC readers may keep running through the apply.

The router tap `source_kind` (W5) is a JetStream body, not a column. It becomes `source-type` in tap format 3 (§5.4 ruling 6).

### 4.7 Ordering with the release cutover

A renamed column breaks every old binary that names it. The old host and runtime read `runs.fail_kind` and `effect_attempts.generation_fact_kind`. The old identity service and the old host PAT check read `principals.kind`. The old scenario-worker writes `authoring_command_audit.command_kind`. The old `receiving` component reads `history.kind`, and the old data-access overlays grant `SELECT (kind)` on history tables. The new binaries fail the same way against the old names. So there is no mixed window.

The constraint that §3 must honor:

1. Drain, then stop every workload that reads wamn_system or a project-env database with the old names. The drain turns ingress off and closes every open run first (§3.2 "Drain", §5.4 ruling 2). Then stop the hosts, the identity service, the scenario-worker and the HTTP and materializer workloads. The CDC readers keep running (§4.6). Run no `bootstrap.sh` from here until step 6 deploys the new one.
2. Apply the wamn_system hand script, with the control component key of §4.3.6. The new `reconcile-run-plane` reads `principals.type` in wamn_system (`crates/control/lib/src/reconcile_run_plane.rs:407`, `:422`), so this step comes before step 4.
3. Apply the project-env hand script (P9, the component key and the snapshot table of §4.3.6) to each project-env database. The component key comes before any 2.0.0 push (§3.2 B8 step 2).
4. Run the new `reconcile-run-plane` for each project-env. Its cutover renames P8, P10, P11 and P12 (§4.3.2). Never run the old verb against a renamed database. The old verb finds the declared checks missing and the new columns unknown.
5. Re-annotate every installed PAT Secret (§4.3.5).
6. Start the new binaries, deploy the new `bootstrap.sh`, and run the republish of the 2.0.0 packages (§2.5). The new apply-package writes `definition_type`, and the new scenario-worker writes `command_type`, so the republish cannot come before steps 2 to 4.

The republish order itself belongs to wamn-sfea.3.

Rollback swaps the names back. It runs the hand statements with the names swapped and appends `deploy/sql/record-history.sql` of the old commit. It renames P8 and P10 to P12 back with the statements of §4.3.2, names swapped, because the old verb has no rename. It swaps the annotation keys back. It renames the snapshot table and its names back and restores the old function body. It restores `component_library_digest_key` and the old requirement key and drops `catalog.component_digest_owners`, which works only while no digest has two library rows (§3.4 rows R2 and R4).

### 4.8 Record for gcp.md §7

`docs/operations/gcp.md:1728` is §7 "Schema changes applied by hand". When the changes run, add this entry to it, in the form of its `registry.capture_gap` entry. The entry records the hand statements, the verb run that applied P8 and P10 to P12, the component key and the snapshot table of §4.3.6, and the annotations. Before the apply, the deployment agent runs the ops-schema query of §4.3.4 on wamn_system and the Secret listing of §4.3.5, and the entry states both answers. The angle-bracket fields are filled in at apply time.

````markdown
On <date> (`wamn-sfea`, `wamn-o8b9`), the `kind` → `type` renames of `docs/plan/kind-to-type.md` §4.3 went into `wamn_system`, `wamn-db-dkk--receiving--dev--4pqjfmli` and `wamn-db-dkk--wms--dev--0nk1lrpr`, with the workloads stopped. On wamn_system, `SELECT to_regclass('provisioning.copy_sagas') IS NOT NULL` answered <true|false>, so the script <kept|left out> the three `provisioning.copy_sagas` lines. Write the wamn_system script of §4.3.4 into `$P/kind-to-type-system.sql` and the project-env script into `$P/kind-to-type-project-env.sql`. Put `deploy/sql/record-history.sql` of commit <commit> before the `COMMIT;` of each. Apply them as the superuser:

```sql
BEGIN;
ALTER TABLE registry.orgs RENAME COLUMN placement_kind TO placement_type;
-- ... the other renames of §4.3.4 ...
CREATE OR REPLACE FUNCTION identity.lock_password_principal(principal uuid) ...;
-- the component key of §4.3.6, one line per copy (owner ruling 2026-09-30):
-- wamn_system control copy: CREATE TABLE catalog.component_digest_owners ... (with environment_instance); DROP CONSTRAINT component_library_digest_key;
-- each project-env copy:    CREATE TABLE catalog.component_digest_owners ...; DROP CONSTRAINT component_library_digest_key;
-- project-env only, the snapshot table of §4.3.6:
-- ALTER TABLE catalog.release_manifest_v3_snapshots RENAME TO release_manifest_snapshots; ...
-- deploy/sql/record-history.sql
COMMIT;
```

```bash
kubectl -n platform exec -i wamn-pg-1 -c postgres -- psql -U postgres -d wamn_system -v ON_ERROR_STOP=1 < $P/kind-to-type-system.sql
for db in wamn-db-dkk--receiving--dev--4pqjfmli wamn-db-dkk--wms--dev--0nk1lrpr; do
  kubectl -n platform exec -i wamn-pg-1 -c postgres -- psql -U postgres -d "$db" -v ON_ERROR_STOP=1 < $P/kind-to-type-project-env.sql
done
```

The applies took <n> seconds. The history notices named <tables>. The owner table took <n> rows in wamn_system, <n> in Receiving and <n> in WMS. The snapshot notices named <constraints>. The check of §4.3.6 listed no `component_library_digest_key` and no `release_manifest_v3` name.

`reconcile-run-plane` of commit <commit> then applied P8 and P10 to P12 by its `TypeColumnCutover` action. Run it for each project-env as in sections 3.8 and 5.2:

```bash
target/debug/wamn-ctl reconcile-run-plane --system-database-url "$WAMN_SYSTEM_ADMIN_URL" --admin-database-url "$T" \
  --org dkk --project <project> --tenant <tenant> --env dev --schema wamn_run
```

It reported <actions> for Receiving and <actions> for WMS, and a second run reported no action. The check query of §4.3 returned no row in any of the three databases afterwards.

The listing of PAT Secrets with the label `app.kubernetes.io/component=project-env-pat` returned <count> Secrets. Each took one command:

```bash
kubectl -n <namespace> annotate secret <name> wamn.io/principal-type=service wamn.io/principal-kind-
```
````

### 4.9 Owner rulings, 2026-09-30

1. One pattern holds everywhere. A column `kind` becomes `type`, a column `<x>_kind` becomes `<x>_type`, and every check follows its column: `principal_type`, `fail_type`, `action_type`, `operator_run_actions_type_check`.
2. Installed databases match a fresh install name for name. The installed changes also rename PostgreSQL's automatic constraint names (`_not_null`, `_key`, `_fkey`, `_check`).
3. Where `reconcile-run-plane` already changes installed run-plane schema in code, P10 to P12 go into that verb, and the operations page records that the verb applied them. Hand statements are only for what no verb can do. §4.3.1 shows what the verb changes today. §4.3.2 moves P8 and P10 to P12 into it. §4.3.3 says why the rest stays by hand.
4. The PAT Secret annotation becomes `wamn.io/principal-type`. Installed Secrets are re-annotated by one recorded `kubectl annotate` per Secret under gcp.md §7 (§4.3.5, §4.8).
5. Whether `ops-schema.sql` is installed on wamn-dev is not guessed. The operations page does not say, so the deployment agent answers it with the query of §4.3.4 before the apply, and the record states the answer.
6. P8 goes into `reconcile-run-plane` with P10 to P12. A table that the verb already changes on an installed database is the verb's to rename.
7. P9 stays a hand statement. A verb that renames history tables is the upgrade path of `wamn-o8b9` built sideways. The cost is recorded under `wamn-o8b9`: 15 history tables in two project-env databases (Receiving 8: six `app_system` and two `receiving`, WMS 7: six `app_system` and one `wms`), plus the record-history functions in wamn_system and both project-env databases.

## 5. Sealed-history compatibility and immutability proofs

Draft from wamn-sfea.5. Measured on `worktree-table` at `0dc04ec4e`. This section changes no code.

The rule of this section: nothing old is read in two spellings. Each old byte has one of three fates. No reader touches it (inert). Or a reader refuses it with a named error. Or only a column name changed, and the reader never sees the word `kind`. §2.5 ruling 3 (format 4 only, no dual read) and ruling 5 (an old authoring command is refused, not replayed) apply throughout, with the rulings of §5.4.

### 5.1 Stored things that carry old `kind` bytes

**Kept** means the stored bytes after the migration. **Reader after** names the new code that still reads the thing, or says that no reader touches it.

| # | Stored thing | Where the old `kind` is | Kept | Reader after | Result |
|---|---|---|---|---|---|
| S1 | Sealed package rows `catalog.packages` | Only `manifest_sha256`, the hash of the old `wamn.json` (A1, A5). The row holds no `wamn.json` bytes | Row unchanged. `BEFORE UPDATE OR DELETE` trigger refuses any change (`deploy/sql/catalog-schema.sql:516` to `:537`, `deploy/sql/control-portable-store.sql:359` to `:384`, message `<table> is immutable` from `deploy/sql/reject-immutable-row-change.sql`) | apply-package reads the row of the current leaf to check `predecessor_version` (`crates/control/lib/src/apply_package/package_version.rs:44` to `:61`). It compares hashes and versions only. It never re-reads old `wamn.json` bytes | Works, no `kind` read |
| S2 | Old `wamn.json` and `generated/` files | A1, A2, A5, G1 to G11 | Git history only. The tree moves to the 2.0.0 files (§2.3) | If an operator feeds an old file to a new verb: apply-package refuses `invalid-manifest` (`crates/schema/control/src/package_migrations.rs:306`, strict `CustomOperationDeclaration` with `deny_unknown_fields` at `crates/schema/generator/src/manifest.rs:56`). publish-release refuses `package-manifest` (`crates/control/lib/src/publish_release/package_sources.rs:35`) | Refused, named |
| S3 | Package migrations `catalog.package_migrations` | None. Migration SQL holds no `kind` (§2.3) | Unchanged. Immutable trigger, and the seal trigger refuses a new migration under a sealed coordinate with `package-version-sealed` (`catalog-schema.sql:131` to `:170`) | apply-package reads them as the byte-identical prefix of each 2.0.0 stream (§2.1) | Works |
| S4 | Definition owners `catalog.package_definition_owners` | Column name `definition_kind` (P8). Values `relation`, `field`, `constraint` | Rows unchanged. `reconcile-run-plane` renames the column only (§4.3.2). A rename fires no row trigger | apply-package reads the predecessor's rows under `definition_type` (`crates/control/lib/src/apply_package/definition_ownership.rs:48`) | Works, values unchanged |
| S5 | Attachment definition hashes | `definition.kind` inside each hashed definition (A2, G1) | Stored only inside old release snapshots (S7). No catalog table holds them | None outside S7 | Inert |
| S6 | Component artifacts in the OCI registry | None in the config blob. The config holds `format-version`, `component-digest`, `imports`, `imports-fingerprint` (`crates/platform/engine/src/component_artifact.rs:83`, `:281`). The wasm of `receiving`, `http-route` and `materializer` holds `kind` strings (§2.2 note) | Unchanged. The tag is the hex of the component digest (`crates/control/lib/src/push_component.rs:523`), and every blob is derived from the wasm bytes. New wasm gets a new tag | A host pulls only the digests its format-4 release names. Unchanged palette components keep their digest and are read as before, which is intended. Their `catalog.component_library` rows under 2.0.0 are new rows under the new key (§4.3.6). The owner row of each digest keeps its package | Old changed digests inert. Unchanged digests work |
| S7 | Release rows `catalog.release_manifest_snapshots`, named `release_manifest_v3_snapshots` before B3 (§5.4 ruling 3) | Format 3 bytes with G8 and G9 `kind` keys | Unchanged. The table rename is metadata-only and touches no row. Immutable trigger. `CHECK (manifest_digest = 'sha256:' \|\| encode(sha256(canonical_bytes), 'hex'))` (`catalog-schema.sql:442`) | Every decoder refuses format 3 (§5.2). `RELEASE_WIRING_SQL` and `RELEASE_COMPONENTS_SQL` read the jsonb without a format check. They bind the carried digest of the host's own release (§5.2 row R9). So a format-3 row never matches | Inert or refused |
| S8 | Release membership `catalog.effective_release_packages`, `catalog.release_components`, `catalog.effective_releases` | None | Unchanged. Immutable trigger. The snapshot seal refuses a new component row after the mint (`catalog-schema.sql:447` to `:468`, message `effective-release-snapshot-already-sealed`) | Mint of a new release under an old id refuses `closure-conflict` (§5.2 row R6) | Inert |
| S9 | Release manifest OCI artifacts (`.../wamn/releases`) | Format 3 bytes | Unchanged. The tag derives from the manifest digest. An existing tag with other bytes refuses `release-manifest-publish-refused` kind `conflict` (`crates/control/lib/src/push_release_manifest.rs:1` to `:5`, `:570`) | A host started with an old digest pulls it and refuses (§5.2 row R1) | Refused, named |
| S10 | Run-state rows `wamn_run.runs`, `effect_attempts`, `operator_run_actions` | Column names only (P10 to P12). Values keep their spelling (§2.2 R rows) | Rows unchanged. The rename is metadata-only (§4.3). Admission pins are immutable (`deploy/sql/run-state.sql:195` to `:235`) | The new host reads them under the new names. `StoredCallerOutcome` is built from columns (`crates/execution/run-state/src/transitions.rs:109`) | Works. No run pinned to release 1 is open after the drain (§3.2 "Drain", §5.4 ruling 2) |
| S11 | Write log `app_system.write_log` | None. `request` is the item input hash (`crates/platform/engine/src/operation/intent.rs:139`). `result` is the operation's answer body | Unchanged | Read across releases on purpose, because the log key drops `@version` (`crates/platform/runtime/src/plugins/wamn_postgres/write_log.rs:11` to `:13`). No application input or command output has a platform `kind` field. The one `kind` output field is the history row of `load_purchase_order_history`, which is a projection and claims no write-log key | Works |
| S12 | Authoring audit `catalog.authoring_command_audit` | `request_hash` over `kind`-tagged JSON. `outcome_bytes` hold `kind` tags (W1). Columns `command_kind`, `principal_kind` (P7) | Unchanged. Immutable trigger (`control-portable-store.sql:371`). Columns renamed by hand (§4.3.4) | The retry path reads `request_hash` and `outcome_bytes` (`services/scenario-worker/src/management.rs:93`). A replay needs an equal hash (`:259`). A new server hashes its own `type`-tagged encoding, so an old hash never matches and old `outcome_bytes` are never sent again | Inert. See §5.2 row R11 |
| S13 | Gate reports `wamn_run.gate_reports` | None. `summary` is `{"cases": n}` (`services/scenario-worker/src/store/admission.rs:854`) | Unchanged. Immutable trigger | `get-report` returns the summary as stored (`services/scenario-worker/src/authoring.rs:393`) | Works |
| S14 | Edge bundles on devices | `manifest.json` in format 3 | Unchanged on the device | An old `wamn-edge` binary keeps serving its old bundle. It needs no database. A new binary refuses the bundle (§5.2 row R4) | Old works, new refuses, named |
| S15 | Web uploads `<prefix>/<package id>/<digest hex>/` | Generated client source (G7, W8) | Unchanged. The upload writes create-only and refuses a `--release` that is not the current release of the environment (§5.4 ruling 4). Today it overwrites (`services/ctl/src/web.rs:125`) | The edge chart serves the path its `bucketPath` names. After the republish it names the new path | Inert, and enforced after A5 |
| S16 | History rows `<relation>_history` | Column name `kind` (P9). Values `insert`, `update`, `delete`. `before` and `after` images hold no renamed key (§4.3.4) | Rows unchanged. Rename is metadata-only | The new `receiving` component reads them under `type` (`apps/wamn_receiving/query/load_purchase_order_history.sql:12`) | Works |
| S17 | Router tap records, stream `WAMN_TAP` | `source-kind` key (W5) | Memory storage, `max_age` 5 minutes (`deploy/gcp/nats-jetstream.yaml:207`, `:210`) | `ctl dev` observations decode with `deny_unknown_fields` and fail the read on an old record (`crates/control/lib/src/dev/observations.rs:437`). The new reader accepts format 3 only | Gone 5 minutes after the old hosts stop. Refused by format if met (§5.4 ruling 6) |
| S18 | Identity rows, provisioning sagas, registry orgs | Column names only (P1 to P6) | Rows unchanged | New services read the new names | Works |
| S19 | PAT Secrets | Annotation key (P15) | Re-annotated (§4.3.5) | New `bootstrap.sh` | Works |

No session token, OCI config blob or component-library fact carries a WAMN `kind` key. `catalog.component_library` holds `operations`, `imports` and `effects` with no `kind` (`crates/catalog/model/src/component_library.rs`). `catalog.wirings.graph_json` holds none (`git grep '"kind"' -- 'apps/*/publication/wirings/*'` is empty).

### 5.2 Paths that re-read an old release or an old package

Every serving-manifest decoder goes through `ServingManifest::from_canonical_bytes` (`crates/catalog/model/src/serving_manifest.rs:1045`). It reads `format-version` from the raw JSON before it decodes any `kind` field (`:1054`, `:1319`). So format-3 bytes get the typed refusal `CatalogIdentityError::UnsupportedServingManifestVersion` (`crates/catalog/model/src/lib.rs:142`), rendered as `unsupported-serving-manifest-version: requested 3; supported version is 4` (`serving_manifest.rs:100`, `lib.rs:222`). The change to format 4 is one constant, `SERVING_MANIFEST_FORMAT_VERSION` (`serving_manifest.rs:33`).

| # | Path | Entry | What it reads | Outcome on old bytes |
|---|---|---|---|---|
| R1 | Host start from the registry | `services/host/src/host.rs:426` | OCI release artifact by digest | `ReleaseLoadErrorKind::ManifestRejected` carrying the literal (`crates/platform/engine/src/release_manifest.rs:80`, `:176`). The host refuses before it opens a socket |
| R2 | Host start from a local application (`ctl dev`) | `services/host/src/host.rs:719` | Local release directory | Same as R1 |
| R3 | `ctl dev` loop | `crates/control/lib/src/dev/coordinator.rs:1617`, `:2010` | Its kept local target | The package version is in the target structure digest, so the 2.0.0 packages force a new target. The new target mints a new format-4 snapshot. A stale local release directory refuses as R2 |
| R4 | Edge box start | `services/edge/src/release.rs:82` | `manifest.json` of the pinned bundle | `EdgeReleaseErrorKind::Rejected` with the literal |
| R5 | Edge bundle writer | `crates/control/lib/src/dev/edge_bundle.rs:34` | A local release directory | Refused with the literal in the error chain |
| R6 | Re-mint of an existing release id | `crates/control/lib/src/publish_release.rs:1579`, `:2092` to `:2112` | Frozen membership and snapshot | `closure-conflict` (`publish_release.rs:282`). The frozen row is not touched. The wamn-dev republish therefore needs new effective release ids. `docs/operations/gcp.md:1297` uses id 1 today |
| R7 | Rollback by `delivery select` of an older release | `crates/control/lib/src/delivery/deployment.rs:62`, `crates/control/lib/src/delivery/publication.rs:38` | Old snapshot through `print_release_env` (`crates/control/lib/src/print_release_env.rs:79`) | Refused with the literal before the head row moves |
| R8 | `promote` of an old release into another environment | `crates/control/lib/src/promote.rs:391` | Old snapshot | Refused with the literal before anything is copied |
| R9 | Wiring and component resolution for a run or a route | `crates/platform/runtime/src/plugins/wamn_postgres/wiring_resolution.rs:18`, `:135` | Snapshot jsonb, without a format check | Unreachable. Both queries require `snapshot.manifest_digest` to equal the carried digest (`:29`, `:140`). The callers pass the digest of the host's own `LoadedRelease` (`crates/execution/workflow/src/router_driver.rs:784`, `crates/execution/host/src/operation.rs:424`), which is format 4 by R1 |
| R10 | Claim of a queued run | `crates/execution/run-state/src/queue/sql.rs:196` | `runs.effective_release_id` | A host claims only runs of its own release. A run pinned to a format-3 release is never claimed. So the drain closes every such run before B1 (§5.4 ruling 2) |
| R11 | Authoring retry | `services/scenario-worker/src/management.rs:757` to `:775`, `:982`, `:1175` | Stored `request_hash` | Two cases. An old client sends `schema-version` `0.1`. The decoder reads the version first and answers HTTP 400 with the body `unsupported-contract-version`, requested `0.1`, supported `0.2` (§5.4 ruling 5). Nothing runs and nothing replays. Today the strict decode fails first and the answer is a bare 400 (`crates/authoring/model/src/lib.rs:572` to `:585`, `management.rs:774`). A new client that resends the same `command-id` gets `command-id-reuse` (`management.rs:1265`) |
| R12 | `workflow start --effective-release-id <old id>` | `crates/execution/workflow/src/contract/postgres.rs:94` | Old snapshot | `WorkflowErrorKind::Refused` "the release snapshot does not parse", with the literal as source |
| R13 | `push-release-manifest` of old bytes | `crates/control/lib/src/push_release_manifest.rs:227`, `:334` | Old snapshot | `release-manifest-document-refused` |
| R14 | `delivery prepare` of a candidate file | `crates/control/lib/src/delivery.rs:165` | Candidate manifest file | Refused with the literal |
| R15 | apply-package of an old package directory | `package_migrations.rs:306` | Old `wamn.json` | `invalid-manifest` |
| R16 | apply-package of new bytes under an old coordinate | `package_migrations.rs:342` to `:355`, `:265` to `:280` | `catalog.packages` hash | `package-manifest-drift` for the applied coordinate. `package-coordinate-content-conflict` at registration. Exception: a local target lifts both on purpose (`crates/control/lib/src/apply_package/local_target.rs:18`, `catalog-schema.sql:157`). A disposable `ctl dev` target is not a sealed coordinate, so the exception is outside the rules of platform-ui.md §0 (§5.4 ruling 7) |
| R17 | `reconcile-run-plane` | `crates/control/lib/src/reconcile_run_plane.rs` | Table and column shapes only | Reads no release or package bytes |
| R18 | Wiring activation and catalog listing | `crates/catalog/model/src/wiring_activation.rs:91`, `wamn-ctl workflow list` | Heads, membership, run rows | No verb lists or decodes release bytes. These read ids and hashes only |

No path reads a `kind`-keyed manifest or package in a second spelling. Every path either never reads the old bytes (R3, R9, R10, R17, R18) or refuses with a named error (the rest).

### 5.3 Proofs

Tests are run as [running tests](../operations/running-tests.md) describes. Live tests need its database inputs.

**Existing tests to keep.** They already prove a rule and must stay green through the migration.

| Rule | Test | Crate |
|---|---|---|
| A sealed coordinate refuses other bytes, other predecessor, or a non-current predecessor | `registration_preserves_replay_conflicts_and_current_predecessor`, `crates/schema/control/src/package_migrations.rs:825` | `wamn-schema-control` |
| Same, against PostgreSQL, under concurrency | `registration_serializes_replay_conflicts_successors_and_rollback`, `crates/control/lib/src/apply_package/package_version/registration_tests.rs:47` | `wamn-control` |
| Same coordinate with a changed `wamn.json` refuses `package-manifest-drift` | `same_coordinate_manifest_drift_names_coordinate_and_both_hashes`, `package_migrations.rs:1052` | `wamn-schema-control` |
| A new version without the current predecessor refuses `predecessor-not-current` | `exact_runner_commits_once_refuses_drift_and_rolls_back_a_failing_suffix`, `crates/control/lib/tests/apply_package_live.rs:560` (assertions at `:1341`, `:1357`) | `wamn-control` |
| Recorded migration bytes cannot change and a sealed coordinate takes no migration | `package_seal_and_attestation_winner_are_server_enforced`, `crates/control/lib/tests/publish_release_live.rs:220` | `wamn-control` |
| Control facts are immutable (`component_library`, `gate_reports`, `deployment_attestations`) | `control_portable_store_enforces_the_current_record_on_postgres`, `crates/control/provision/tests/control_portable_store.rs:167` | `wamn-control-provision` |
| The decoder refuses other formats with the typed literal | `unsupported_formats_are_typed_refusals_not_compatibility_arms`, `serving_manifest.rs:2070` | `wamn-catalog` |
| The host load keeps the literal | `an_unsupported_format_refuses_with_the_frozen_literal`, `crates/platform/engine/src/release_manifest.rs:413` | `wamn-engine` |
| Release publish refuses other formats and a tag with other bytes | `unsupported_format_and_noncanonical_documents_refuse_before_transport`, `wrong_or_multi_layer_layout_refuses_as_conflict`, `push_release_manifest.rs:630`, `:644` | `wamn-control` |
| A component tag is its digest | `production_publisher_layout_matches_the_puller_contract`, `push_component.rs:3142` | `wamn-control` |
| A claim takes only runs of the carried release | `lease_grant_verifies_release_and_mints_manifest_on_the_existing_write`, `crates/platform/runtime/src/plugins/wamn_postgres/production_claim.rs:1636` | `wamn-runtime` |
| An authoring retry replays only on an equal hash | `retry_classifier_is_exact_hash_or_reuse`, `services/scenario-worker/src/management.rs:1778` | `wamn-scenario-worker` |

**Tests to change.**

- `unsupported_formats_are_typed_refusals_not_compatibility_arms` loops over `[0, 1, 2, 3, 5]` and asserts `requested` for each.
- `the_format_three_preimage_and_digest_are_pinned` (`crates/catalog/model/tests/serving_manifest_digest.rs:193`) becomes the format-4 pin. The format-3 vector file `crates/catalog/model/tests/fixtures/release_manifest_mint_vector.rs` is kept, renamed as a frozen format-3 fixture, and used only by the refusal test below.

**Tests to add.**

| Test | Crate and file | Proves |
|---|---|---|
| `the_frozen_format_three_vector_refuses_with_its_version` | `wamn-catalog`, `crates/catalog/model/tests/serving_manifest_digest.rs` | The real pinned format-3 bytes, with their `kind` keys, refuse with `requested: "3"`. They do not fail as an unknown field |
| `a_format_three_bundle_is_rejected` | `wamn-edge`, `services/edge/tests/release.rs` | R4 is `EdgeReleaseErrorKind::Rejected` and carries the literal |
| `a_frozen_release_refuses_another_closure` | `wamn-control`, `crates/control/lib/src/publish_release/effective_release_live.rs` | R6. Insert a format-3 snapshot, mint the same id, get `closure-conflict`, and read back byte-identical `canonical_bytes` |
| Extend `assert_immutable_rows` | `wamn-control`, `crates/control/lib/tests/publish_release_live.rs:88` | `UPDATE` and `DELETE` on `catalog.packages`, `catalog.release_manifest_snapshots`, `catalog.effective_release_packages`, `catalog.release_components` and `catalog.component_digest_owners` each refuse with SQLSTATE 55000 and `<table> is immutable`. Today only `package_migrations` is asserted |
| `a_start_on_a_format_three_release_refuses` | `wamn-workflow`, `crates/execution/workflow/src/queue/automation_live.rs` | R12 |
| `promote_refuses_a_format_three_source` | `wamn-control`, `crates/control/lib/src/promote/activation_live.rs` | R8, and no row is written in the target |
| `reconcile_renames_without_touching_rows` | `wamn-control`, `crates/control/lib/tests/run_plane_live/` (the §4.3.2 live case) | The digest listing below is equal before and after the `TypeColumnCutover` for `catalog.package_definition_owners` and the run-plane rows |
| `a_digest_keeps_its_owner_across_versions` | `wamn-control`, the live tests of `crates/control/lib/src/push_component.rs` | Ruling 1. On both planes, digest D admitted under P 1.0.0 is admitted again under P 2.0.0. Two library rows and one owner row exist. D under package Q refuses `component-fact-conflict` and names P. A second component name for D in one version still refuses by `component_library_package_digest_key` |
| `promote_copies_the_projection_hash_of_its_own_coordinate` | `wamn-control`, `crates/control/lib/src/promote/activation_live.rs` | Ruling 1. A source that holds D under 1.0.0 and 2.0.0 promotes release 2 with the 2.0.0 projection hash |
| `a_stale_release_is_refused_before_the_build` | `wamn-ctl`, `services/ctl/src/web.rs` | Ruling 4. With a control database whose current release has digest A, `--release B` refuses and writes nothing |
| `an_existing_object_is_never_replaced` | `wamn-ctl`, `services/ctl/src/web.rs` | Ruling 4. A second upload to the same path refuses at its first object, against an in-memory store, and the stored bytes are the first bytes |
| `an_old_contract_version_is_refused_before_decode` | `wamn-authoring-model`, `crates/authoring/model/tests/contract.rs` | Ruling 5. A `0.1` body with `kind` tags gives `UnsupportedContractVersion` with requested `0.1`, not a JSON error |
| Extend the version case at `services/scenario-worker/tests/management_live.rs:1588` | `wamn-scenario-worker` | Ruling 5. The old body gets HTTP 400 with the body `{"type":"unsupported-contract-version","requested":"0.1","supported":"0.2"}` |
| `router_tap_reads_format_three_only` | `wamn-runtime`, `crates/platform/runtime/src/plugins/wamn_jetstream.rs` tests | Ruling 6. Format 1 and 2 bodies refuse with `unsupported router-tap format-version`. A format 3 body with `source-kind` refuses as an unknown field |

**Commands on wamn-dev.** Run each listing before the drain and again after the republish. Save both outputs in `$P`. For each check, `comm -23 before after` must print nothing. Then every old line is still present, byte for byte. The migration only adds lines.

Database digests, run on each project-env database and on wamn_system (the tables that exist in each):

```sql
SELECT 'package', tenant_id, package_id, package_version, coalesce(predecessor_version, '-'), manifest_sha256 FROM catalog.packages
UNION ALL SELECT 'migration', tenant_id, package_id, package_version || '#' || ordinal, relative_path, sha256 FROM catalog.package_migrations
UNION ALL SELECT 'snapshot', tenant_id, effective_release_id::text, manifest_digest, '-', encode(sha256(canonical_bytes), 'hex') FROM catalog.release_manifest_v3_snapshots
UNION ALL SELECT 'member', tenant_id, effective_release_id::text, package_id, package_version, '-' FROM catalog.effective_release_packages
UNION ALL SELECT 'component', tenant_id, package_id, package_version, component || '@' || interface_version, component_digest || ' ' || projection_hash FROM catalog.component_library
ORDER BY 1, 2, 3, 4, 5
```

The snapshot line reads `catalog.release_manifest_v3_snapshots` before B3 and `catalog.release_manifest_snapshots` after it (§4.3.6). The rows are the same. wamn_system has no snapshot table, so leave that line out there. After the republish, also list the owner rows. Each digest has one row, and each palette digest names `wamn_wms`:

```sql
SELECT tenant_id, component_digest, package_id FROM catalog.component_digest_owners ORDER BY 1, 2
```

On wamn_system, add the audit rows:

```sql
SELECT tenant_id, principal_id, command_id, request_hash, encode(sha256(outcome_bytes), 'hex')
  FROM catalog.authoring_command_audit ORDER BY 1, 2, 3
```

Registry digests. The component tag and the manifest digest must both be unchanged for every old line:

```bash
gcloud artifacts docker tags list us-central1-docker.pkg.dev/wamn-dev/wamn/components \
  --format='value(tag.basename(),version.basename())' | sort > $P/components-before.txt
gcloud artifacts docker tags list us-central1-docker.pkg.dev/wamn-dev/wamn/releases \
  --format='value(tag.basename(),version.basename())' | sort > $P/releases-before.txt
```

Immutable tags (ruling 8) are a gated follow-up of §3.2. After `wamn-r9lz` closes and the deployment agent turns them on, `gcloud artifacts repositories describe wamn --project wamn-dev --location us-central1` shows immutable tags enabled. A second push of an existing component tag with other bytes then fails at the registry as well as in code. Until then the proof rests on code: tags derive from content (`push_component.rs:523`), and release pushes refuse a conflict (`push_release_manifest.rs:570`).

Web objects. An overwritten object gets a new generation, so equal generations prove no replacement. After A5 the upload is create-only, so the listing confirms what the code refuses (ruling 4):

```bash
gcloud storage objects list 'gs://wamn-dev-web/**' \
  --format='value(name,generation,md5_hash)' | sort > $P/web-before.txt
```

Predecessors. After the republish, every 2.0.0 row, and `client_acme_receiving` 4.0.0, names the old version:

```sql
SELECT package_id, package_version, predecessor_version FROM catalog.packages
 WHERE package_version IN ('2.0.0', '4.0.0') ORDER BY 1
```

The same fact holds in the tree: `jq -r '.package | [.id, .version, .predecessor_version] | @tsv' apps/*/wamn.json` shows the old version in the third column for every package of §2.3. apply-package enforces it at registration (`predecessor-not-current`), and the unique index `packages_one_successor_per_version` (`catalog-schema.sql:29`) allows one successor per version.

Refusal at runtime. After the new hosts start, start one host process with an old manifest digest, or `delivery select` an old release id. Each must fail with `unsupported-serving-manifest-version: requested 3`. Run `wamn web upload apps/wamn_wms --release <WMS release 1 digest>` with the B9 arguments. It must refuse, because release 1 is not the current release, and the web listing must not change.

Drain (ruling 2). The records of §3.2 "Drain" hold each answer of the open-run query, each settled run id with the answer of `terminalize-effect-uncertain`, and two empty answers before B1. After B10, the open-run query with `effective_release_id = 1` added prints nothing on both databases.

**Fixtures and frozen evidence.** §2.4 lists what is regenerated. The proof is that the regenerating commit changes those files and the tests above pass on it. `docs/history/`, `tests/sweeps/*.log` and the dated digest table at `docs/operations/gcp.md:1184` stay as written.

### 5.4 Owner rulings, 2026-09-30

1. **Component key.** The key `UNIQUE (tenant_id, component_digest)` of `catalog.component_library` (`deploy/sql/catalog-schema.sql:210`) is a defect. It refuses an unchanged component under the next version of its own package with `component-fact-conflict` (`crates/control/lib/src/push_component.rs:1985`). A component that does not change keeps its digest across versions of its package, so the key becomes `(package, version, digest)`. The old key protects one owner per digest, because it is the target of the connection-requirement key. So a digest under a different package stays refused, through the new table `catalog.component_digest_owners`. §4.3.6 states the constraint and the statements, for fresh installs and, under `wamn-o8b9`, for the installed project-env databases and the control copy in wamn_system. §3.2 B8 step 2 depends on it.
2. **The stop drains first.** Ingress goes off. Dispatched and running runs finish within 15 minutes, observed once a minute with the open-run query. `terminalize-effect-uncertain` settles what stays uncertain. Only then do the workloads stop and the cutover run. Nothing is stranded. The query must answer zero open runs before B1 (§3.2 "Drain").
3. **Snapshot table.** `catalog.release_manifest_v3_snapshots` becomes `catalog.release_manifest_snapshots`. The table name holds no format. Each row states its format in the `format-version` of its bytes, and new rows say 4. Fresh-install DDL, a hand statement under `wamn-o8b9` (§4.3.6, §4.8), and the 22 files of the §4.5 snapshot groups, in A7.
4. **Web upload.** `wamn web upload` becomes create-only, with no switch. It refuses a `--release` that is not the head of the environment in `catalog.effective_release_heads` of the project-env database, the row that `delivery select` writes. The cutover sets the head with `delivery select` (§3.2 B9 step 0), so wamn-dev writes heads from then on. `push-release-manifest` keeps its existence check, because pushing a manifest is not serving it. A5 carries it.
5. **Authoring contract version.** The authoring and management decoders check the contract version first and answer the named refusal `unsupported-contract-version` with a body. Today the version is checked only after a full strict decode (`crates/authoring/model/src/lib.rs:572` to `:585`), and an old body gets a bare 400 (`services/scenario-worker/src/management.rs:774`). The body shape moved, so the contract version moves from `0.1` to `0.2` (`lib.rs:19`). A4 carries it (W1, W2).
6. **Router tap.** `source-kind` becomes `source-type`, and the tap format moves to 3. The reader accepts 3 only. The records live 5 minutes in memory, so nothing migrates (S17). A5 carries it (W5).
7. **Local targets.** A disposable `ctl dev` target is not a sealed coordinate, so its local-target exception is outside the rules of platform-ui.md §0 (R16). platform-ui.md §0 says so.
8. **Registry immutable tags.** The deployment agent turns immutable tags on for the `wamn` registry, but only after `wamn-r9lz` closes. Until component bytes are reproducible, a rebuild of one source identity could produce a second digest and be refused for the wrong reason. §3.2 records it as a gated follow-up, and §5.3 states its proof.

## 6. `*ErrorKind` families

Owner rulings, 2026-09-29 and 2026-09-30 (wamn-sfea.6, corrected in wamn-sfea.2): the `*ErrorKind` rename is part of this migration epic.

- All 74 families rename in one commit, including `NodeErrorKind` and the WAMN enum `ErrorKind` in `connection_http/transport.rs`.
- `NodeErrorKind` serializes only its values, such as `retryable`, and no field holds the type. Its rename moves no bytes (`crates/execution/run-state/src/status.rs:250`).
- The same commit renames the `kind` fields of error structs, such as `StatementError.kind`. Their `Debug` output compiles into components, so this commit moves component bytes.
- The commit lands before the package rebuild and republish, so each component is built once.
