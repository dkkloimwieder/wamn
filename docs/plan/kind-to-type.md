# Platform `kind` to `type` migration

Status: Draft. Section 1 from wamn-sfea.1; sections 2–6 pending wamn-sfea.2–.6.

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
- **Hashed**: whether the key or the value feeds a definition hash, a manifest digest or a schema identity. `tbd` means wamn-sfea.2 must decide.
- **Count**: matches from the commands in §1.7. A count is a match count unless the row says otherwise.

### 1.1 Serialized authored surfaces

| # | Surface | Key and values | Owned | Serialized | Hashed | Cite | Count |
|---|---|---|---|---|---|---|---|
| A1 | `apps/*/wamn.json` custom operations | `custom_operations.*.kind`: `command`, `projection`, `event_handler`. Parsed into `CustomOperationDeclaration.kind: CustomOperationKind` | yes | yes | tbd. The manifest is package content | `apps/wamn_receiving/wamn.json:377`, `crates/schema/generator/src/manifest.rs:58` | 21 |
| A2 | `apps/*/publication/attachments.json` | Top-level `kind` and `definition.kind`, value `http` | yes | yes | yes. `definition.kind` is inside the canonical definition that `definition-hash` covers. The top-level `kind` enters the serving manifest digest (see G8) | `apps/wamn_receiving/publication/attachments.json:3`, `:10`. Hash check at `crates/control/lib/src/publish_release/attachments.rs:238` and `crates/schema/generator/src/route_schema.rs:219`. Hash function at `apps/platform/execution/contract/src/lib.rs:54` | 104 |
| A3 | Test fixture packages | Same keys as A1 and A2 in fixture copies | yes | yes | tbd | `crates/control/lib/tests/fixtures/observer_package/wamn.json:32`, `services/ctl/tests/fixtures/ui_scaffold/publication/attachments.json:3` | 6 |
| A4 | Client TUI classification fixture | `kind` holds an operation kind | yes | yes | no | `crates/client/tui/tests/data/classification-cases.json:31` | 6 |
| A5 | Application column named `kind` in `wamn.json` | The record-history column `kind` (see P9), named as a projection path and a row field | ask | yes | tbd | `apps/wamn_receiving/wamn.json:1087` | 3 |
| A6 | Operator recovery CRD fixture | `resource_kind` holds a Kubernetes resource kind such as `Host` | ask | yes | no | `tests/integration/fixtures/operator-recovery/deployment-crds-001/crd-inventory.json:11` | 15 |

No authored TOML file carries a `kind` key.

### 1.2 Generated serialized surfaces

| # | Surface | Producer | Owned | Serialized | Hashed | Cite | Count |
|---|---|---|---|---|---|---|---|
| G1 | `apps/*/generated/publication/attachments.json` | `json!` with top-level `kind` and `definition.kind` | yes | yes | yes. Same as A2 | `crates/schema/generator/src/generate/publication.rs:61`, `:70` | 68 |
| G2 | `generated/contracts/*/*.operation.json` | `"kind"` from `OperationKind` or action name | yes | yes | tbd. It becomes `ServingRoute.kind` (G8) | `crates/schema/generator/src/generate/contracts.rs:246`, `:837` | 57 |
| G3 | `generated/contracts/*/query.input.json` | `pagination.kind`, value `keyset` | yes | yes | tbd | `crates/schema/generator/src/generate/contracts.rs:1172` | 10 |
| G4 | `generated/source-map/*.json` | `"kind"` from the custom operation kind | yes | yes | tbd | `crates/schema/generator/src/generate/contracts.rs:188` | 8 |
| G5 | `generated/package-weld.json` | `#[serde(tag = "kind")]` on `ConstraintKind`, `ColumnDefault`, `ColumnGeneration` | yes | yes | yes. The schema IR feeds `verified_schema_state_id` | `crates/schema/introspection/src/ir.rs:316`, `:227`, `:283`. Hash at `crates/schema/generator/src/generate.rs:415` | 72 |
| G6 | Client IR | `OperationIr.kind: String`, serde, kebab-case | yes | yes | tbd | `crates/schema/generator/src/client_ir.rs:329` | 1 field |
| G7 | Screen plan and generated clients | `SuppliedField.kind: SuppliedKind` in the plan. Emitted as `kind:` in generated TypeScript and TUI source | yes | yes, as generated source | tbd | `crates/schema/generator/src/client_plan.rs:128`, `crates/schema/generator/src/client_ts.rs:567`, `crates/schema/generator/src/client_tui.rs:469` | TS 35, TUI 148 |
| G8 | Serving (release) manifest | `kind` field on `ServingRoute` (`OperationKind`), `ServingAttachment`, `AttachmentWire`, `RouteAttachment`, `WiringAttachment` (`AttachmentKind`) | yes | yes | yes. `ServingManifest::digest` covers the canonical bytes | `crates/catalog/model/src/serving_manifest.rs:390`, `:441`, `:456`, `:639`, `:655`. Digest at `:886` | 5 fields |
| G9 | Catalog canonical frames | `("kind", ...)` frame in `Source` and attachment definition identities | yes | yes | yes. The frames are the canonical bytes | `crates/catalog/model/src/lib.rs:623`, `:723`, `:875` | 3 |
| G10 | Publish-release package source contract | `Contract.kind: OperationKind`, serde | yes | yes | tbd | `crates/control/lib/src/publish_release/package_sources.rs:147` | 1 field |
| G11 | Generated app code | `StatementErrorKind` use in `generated/data/error.rs`. History `kind` column in generated WIT codec, accessor and client | yes | yes, as generated source | tbd | `apps/wamn_receiving/generated/data/error.rs:154`, `apps/wamn_receiving/generated/wit/receiving_load_purchase_order_history_codec.rs:55` | 30 and 17 |
| G12 | Stored caller outcome | `StoredCallerOutcome.kind: String`, serde, kebab-case | yes | yes | no | `crates/execution/run-state/src/transitions.rs:57` | 1 field |

### 1.3 Wire surfaces

| # | Surface | Name | Owned | Serialized | Cite | Count |
|---|---|---|---|---|---|---|
| W1 | Authoring HTTP API | `#[serde(tag = "kind")]` on `AuthoringCommand`, `AuthoringQuery`, `GateRefusal`, `PublishRefusal`, `GetReportRefusal` | yes | yes | `crates/authoring/model/src/lib.rs:189`, `:210`, `:465`, `:516`, `:552` | 5 |
| W2 | Management refusal bodies | JSON `{"kind": ...}`, such as `authorization-denied` and `unsupported-contract-version`. `ctl dev` renders them | yes | yes | `services/scenario-worker/src/management.rs:728`, `services/ctl/src/dev/tui.rs:635` | 25 in scenario-worker |
| W3 | Router delivery WIT | `enum failure-kind` and `delivery-failure.kind`. Lowered from `FailureKind` | yes | yes | `crates/execution/host/wit/deps/wamn-router-delivery/package.wit:41`, `:55`, `crates/execution/workflow/src/wiring_delivery.rs:429` | 2 |
| W4 | Generated application WIT | `load-purchase-order-history-row.kind: string`, the history column (P9) | ask | yes | `apps/wamn_receiving/generated/wit/deps/wamn-receiving-receiving/package.wit:35` | 1 |
| W5 | JetStream router tap | `source-kind` / `source_kind` on `RouterTapWire` and `RouterTapRecord` | yes | yes | `crates/platform/runtime/src/plugins/wamn_jetstream.rs:251`, `:277` | 37 |
| W6 | Connection generation identity | `credential-kind` identity label. `CredentialKind` values | yes | yes | `crates/platform/runtime/src/connection_generation.rs:230` | 1 |
| W7 | Event advisory | Already `type` on the wire through `#[serde(rename = "type")]`. Only the Rust names `kind` and `DeliveryAdvisoryKind` remain | yes | no (name) | `apps/platform/events/wire/src/lib.rs:285` | 1 |
| W8 | Web runtime | `OperationContract.kind` read from generated clients. `SuppliedField.kind` values | yes | yes | `web/runtime/src/wire.ts:41`, `web/runtime/src/transport.ts:382`, `web/runtime/src/supplied.ts:45` | 7 |
| W9 | Web table filters | `SetFilter.kind` discriminant. Values appear in URLs, the key does not | yes | value | `web/ui/src/table/column-filter.tsx:26`, `web/ui/src/table/set-view.ts:41` | 34 |
| W10 | Web tests and gallery | Fixtures of W8 | yes | no | `web/runtime/test/classification.test.ts:42`, `web/components/gallery/memory.tsx:28` | 12 |

`*ErrorKind` values do not appear on the wire under a `kind` key. They leave the process as `code` strings. wamn-sfea.6 should confirm this per family.

### 1.4 Persisted surfaces

Database names: `wamn_system` is the control database (`SYSTEM_SCHEMA_SQL` and `CONTROL_PORTABLE_STORE_SQL` in `crates/control/provision/src/lib.rs:151`). Project-env is each provisioned project database (`CATALOG_SCHEMA_SQL` in `crates/catalog/model/src/lib.rs:76`, plus `run-state.sql` and `app-schema.sql`).

| # | Database | Table | Column and constraints | Owned | Cite |
|---|---|---|---|---|---|
| P1 | wamn_system | `registry.orgs` | `placement_kind`, `orgs_placement_kind_check`, pool check | yes | `deploy/sql/system-schema.sql:147` |
| P2 | wamn_system | `identity.principals` | `kind`, `principals_kind_check`, `UNIQUE (id, kind)`, `UNIQUE (kind, subject)`, three checks that read `kind` | yes | `deploy/sql/system-schema.sql:259` |
| P3 | wamn_system | `identity.pats` | `principal_kind`, `pats_principal_kind_check`, FK `(principal_id, principal_kind)` to `principals (id, kind)` | yes | `deploy/sql/system-schema.sql:346` |
| P4 | wamn_system | `identity.password_credentials`, `identity.password_tokens`, `identity.password_logins`, `identity.project_env_memberships` | `principal_kind` with an inline check and FK to `principals (id, kind)` | yes | `deploy/sql/system-schema.sql:384`, `:402`, `:425`, `:580` |
| P5 | wamn_system | `provisioning.sagas` | `kind`, `sagas_kind_check` | yes | `deploy/sql/system-schema.sql:706` |
| P6 | wamn_system | `provisioning.copy_sagas` | `kind`, `copy_sagas_kind_check` | yes | `deploy/sql/ops-schema.sql:30` |
| P7 | wamn_system | `catalog.authoring_command_audit` | `command_kind`, `principal_kind`, inline checks | yes | `deploy/sql/control-portable-store.sql:270` |
| P8 | project-env | `catalog.package_definition_owners` | `definition_kind`, inline check, part of the key | yes | `deploy/sql/catalog-schema.sql:59` |
| P9 | project-env and wamn_system | Every `<relation>_history` table | `kind` (`insert`, `update`, `delete`), `<history>_kind_check`, column grant | yes | `deploy/sql/record-history.sql:117`, `:135`, `deploy/sql/app-schema.sql:525`, `apps/platform/data/record-history/src/lib.rs:55` |
| P10 | project-env | `wamn_run.runs` | `caller_outcome_kind`, `fail_kind`, `runs_caller_outcome_kind_check`, `runs_fail_kind_check` | yes | `deploy/sql/run-state.sql:377`, `:387`, `crates/schema/control/src/run_plane/declarations.rs:104`, `:116` |
| P11 | project-env | `wamn_run.effect_attempts` | `generation_fact_kind`, inline checks | yes | `deploy/sql/run-state.sql:603` |
| P12 | project-env | `wamn_run.operator_run_actions` | `action_kind`, `principal_kind`, `operator_run_actions_kind_check`, `operator_run_actions_principal_kind_check` | yes | `deploy/sql/run-state.sql:795`, `crates/schema/control/src/run_plane/declarations.rs:377`, `:401` |
| P13 | edge SQLite | intent store | `outcome_kind` with a check | yes | `crates/execution/run-state-sqlite/src/lib.rs:41` |
| P14 | none | Query result aliases | `AS relation_kind`, `AS object_kind` and similar. These are not persisted but are WAMN names | yes | `crates/schema/introspection/src/postgres.rs:305`, `crates/control/provision/src/sql/database_grants.rs:68` |

Counts: matching lines in `deploy/sql` are 79 (system-schema 43, run-state 17, catalog-schema 5, record-history 5, control-portable-store 3, ops-schema 3, app-schema 2, postgres-init 1). The postgres-init line is prose about the kind cluster. Rust code names these persisted columns 222 times. P14 has 20 aliases.

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

R1 through R4 total 40 distinct non-error types and 1334 references. The R5 reference count includes 78 matches of the bare name `ErrorKind`. One WAMN enum has that name, in `crates/platform/runtime/src/plugins/connection_http/transport.rs:315`. Most of the 78 are `std::io::ErrorKind` (N7). R5 leaves out 11 matches of four async-nats error kinds (`ConsumerInfoErrorKind`, `GetStreamErrorKind`, `SubscribeErrorKind`, `RawMessageErrorKind`), which N9 does not count either.

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
git grep -h -o -E '"kind": *"' -- 'apps/*/publication/attachments.json' | wc -l                     # A2 104
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
