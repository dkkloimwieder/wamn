# Platform `kind` to `type` migration

Status: Draft. Section 1 accepted 2026-09-29 (wamn-sfea.1). Section 2 accepted 2026-09-30 (wamn-sfea.2). Section 4 is a draft from wamn-sfea.4. Sections 3 and 5 are pending wamn-sfea.3 and wamn-sfea.5. Section 6 holds an owner ruling (wamn-sfea.6).

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
| P15 | Kubernetes Secrets | PAT Secrets of `provision-project-env` | Annotation `wamn.io/principal-kind` (value `service`). Installed Secrets carry it. Pending an owner ruling (§4.9) | yes | `crates/control/lib/src/provision_project_env/pat_secrets.rs:267`, read by `deploy/mvp/bootstrap.sh:118` |

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
| W1 | - | - | - | - | - | - | Moves the stored authoring request hash and the stored `outcome_bytes`. An old command retried after the rename hashes differently and refuses instead of replaying (`services/scenario-worker/src/management.rs:262`). §5 must decide |
| W2 | - | - | - | - | - | - | Wire only |
| W3 | - | - | - | yes | - | - | `http-route` and `materializer` import `wamn:router-delivery/delivery@0.2.0` (`apps/platform/ingress/http-route/wit/world.wit:5`, `apps/platform/execution/materializer/wit/world.wit:21`), and so does the engine world (`crates/platform/engine/wit/world.wit:10`). The pin `apps/platform/ingress/http-route/http_route.wasm.sha256` must be rewritten. The host binary changes too |
| W4 | - | - | - | yes | yes | yes | The `receiving` component and the generated Rust and TypeScript clients (`apps/wamn_receiving/generated/client/receiving.rs:172`, `apps/wamn_receiving/generated/client-ts/receiving.ts:46`) |
| W5 | - | - | - | - | - | - | JetStream stream bytes. `RouterTapWire` carries `format_version` (`crates/platform/runtime/src/plugins/wamn_jetstream.rs:262`). §5 must decide on a format bump |
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
| `wamn_wms` | 1.0.0 | `apps/wamn_wms/wamn.json:4` | yes | A1 (5 keys). The palette components that it admits under its coordinate (`label-render`, `blob-put`, `jsonata`) keep their bytes but are admitted again under the new coordinate |
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

## 4. Database schema changes

Draft from wamn-sfea.4. Measured on `worktree-table` at `31eac2d05`, re-verified at `2d9700969` after the rebase onto `0dad0cb9b`. This section changes no code and no database.

### 4.1 Names

§1.8 rules the history column only. The other names below apply the same rule, `kind` becomes `type` inside the name. The owner must confirm them (question 1 in §4.9).

PostgreSQL 18 runs every wamn-dev database (`deploy/gcp/cnpg-cluster.yaml:14`). PostgreSQL 18 records each `NOT NULL` as a named constraint, `<table>_<column>_not_null` by default. A column rename keeps every generated name. So the installed statements also rename each generated name that holds the old column name. After that, an installed database and a fresh install have the same names.

| Row | Database | Table | Column | Constraints renamed (old → new) |
|---|---|---|---|---|
| P1 | wamn_system | `registry.orgs` | `placement_kind` → `placement_type` | `orgs_placement_kind_check`, `orgs_placement_kind_not_null` |
| P2 | wamn_system | `identity.principals` | `kind` → `type` | `principals_kind_check`, `principals_kind_not_null`, `principals_id_kind_key`, `principals_kind_subject_key` |
| P3 | wamn_system | `identity.pats` | `principal_kind` → `principal_type` | `pats_principal_kind_check`, `pats_principal_kind_not_null`, `pats_principal_id_principal_kind_fkey` |
| P4 | wamn_system | `identity.password_credentials`, `identity.password_tokens`, `identity.password_logins` | `principal_kind` → `principal_type` | per table: `<table>_principal_kind_check`, `<table>_principal_kind_not_null`, `<table>_principal_id_principal_kind_fkey` |
| P4 | wamn_system | `identity.project_env_memberships` | `principal_kind` → `principal_type` | `project_env_memberships_principal_kind_not_null`, `project_env_memberships_principal_id_principal_kind_fkey`. `project_env_memberships_human_check` keeps its name |
| P5 | wamn_system | `provisioning.sagas` | `kind` → `type` | `sagas_kind_check`, `sagas_kind_not_null` |
| P6 | wamn_system | `provisioning.copy_sagas` | `kind` → `type` | `copy_sagas_kind_check`, `copy_sagas_kind_not_null` |
| P7 | wamn_system | `catalog.authoring_command_audit` | `command_kind` → `command_type`, `principal_kind` → `principal_type` | `authoring_command_audit_command_kind_check`, `authoring_command_audit_command_kind_not_null`, `authoring_command_audit_principal_kind_check`, `authoring_command_audit_principal_kind_not_null` |
| P8 | project-env | `catalog.package_definition_owners` | `definition_kind` → `definition_type` | `package_definition_owners_definition_kind_check`, `package_definition_owners_definition_kind_not_null`. The primary key keeps its name |
| P9 | project-env | every `<relation>_history` | `kind` → `type` | `<history>_kind_check`, `<history>_kind_not_null` |
| P10 | project-env | `wamn_run.runs` | `caller_outcome_kind` → `caller_outcome_type`, `fail_kind` → `fail_type` | `runs_caller_outcome_kind_check`, `runs_fail_kind_check`. Both columns are nullable, so no `NOT NULL` name |
| P11 | project-env | `wamn_run.effect_attempts` | `generation_fact_kind` → `generation_fact_type` | `effect_attempts_generation_fact_kind_not_null`. `effect_attempts_generation_fact_check` and `effect_attempts_generation_values_check` keep their names |
| P12 | project-env | `wamn_run.operator_run_actions` | `action_kind` → `action_type`, `principal_kind` → `principal_type` | `operator_run_actions_kind_check` → `operator_run_actions_type_check`, `operator_run_actions_action_kind_not_null`, `operator_run_actions_principal_kind_check`, `operator_run_actions_principal_kind_not_null` |

Every other constraint that reads one of these columns keeps its name. Examples are `orgs_pool_cluster_check`, `principals_subject_check`, `principals_email_check`, `principals_platform_principal_check`, `package_definition_owners_relation_shape_check`, `package_definition_owners_extensibility_check` and `runs_check6` to `runs_check8`.

The auto-generated names in the table follow the PostgreSQL rules: `<table>_<column>_check` for a check that reads one column, `<table>_<columns>_key` for a unique key and `<table>_<columns>_fkey` for a foreign key. The pre-check in §4.3 confirms them on each database before any statement runs.

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

The declarations in `declarations.rs` are the text that `pg_get_constraintdef` renders. `reconcile-run-plane` compares them with the installed checks. So they must match `run-state.sql` in the same commit.

Comments that name a column follow it: `system-schema.sql:127`, `:228`, `:341`, `run-state.sql:304`, `record-history.sql` and `apps/platform/data/record-history/src/lib.rs:80`.

Column grants name columns in Rust. They change with the DDL:

- `crates/control/provision/src/identity_issuer.rs:26`, `:37`. The issuer reads `principals.type` and inserts `pats.principal_type`.
- `crates/control/provision/src/sql.rs:242`, `:247`. The executor updates `runs.fail_type` and `runs.caller_outcome_type`.

### 4.3 Installed-database statements

Every statement in this section is metadata-only in PostgreSQL. `ALTER TABLE ... RENAME COLUMN` and `ALTER TABLE ... RENAME CONSTRAINT` change catalog rows only. No table is rewritten and no row changes. `CREATE OR REPLACE FUNCTION` changes the stored function text only. Each `ALTER TABLE` takes an `ACCESS EXCLUSIVE` lock until the transaction commits, so the statements run with the workloads stopped (§4.7).

What follows a rename by itself, because PostgreSQL stores it by column number and not by name:

- check expressions, foreign keys, unique keys and their indexes, and column defaults
- column grants, such as the issuer grants of §4.2 and the column grant at `app-schema.sql:525`
- row security policies. No policy reads a renamed column (`git grep -n -i 'CREATE POLICY' -- deploy/sql` finds none that names one)
- trigger `WHEN` clauses and `UPDATE OF` column lists. None names a renamed column
- publications. The CDC publication is `FOR TABLES IN SCHEMA <schema>` with no column list (`crates/control/provision/src/sql/cdc.rs:82`)

What does not follow a rename, because PostgreSQL stores it as text:

- PL/pgSQL function bodies. Three name a renamed column: `wamn_history.create_history_table` (`record-history.sql:117`, `:135`), `wamn_history.log_row_change` (`record-history.sql:240`) and `identity.lock_password_principal` (`system-schema.sql:752`). The statements replace all three in the same transaction as the renames. Without that, every write to a logged relation fails, and every password login fails.

No view, materialized view, rule, statistics object or event trigger exists in `deploy/sql` or in SQL that Rust creates. The other functions in `deploy/sql` and in `crates/schema/control/src/run_plane/declarations.rs:431` to `:527` name no renamed column.

`CREATE OR REPLACE FUNCTION` keeps the owner, the `SECURITY DEFINER` setting and the grants of the function. So the statements run as the `postgres` superuser with no `SET ROLE`. They create no object, so no ownership question arises.

Order. Foreign keys and unique keys follow the rename by column number, so the order inside one transaction does not affect correctness. The statements rename the referenced table first, then the referencing tables, so that a reader can check them against §4.1 row by row. Each database takes one transaction, and `ON_ERROR_STOP` rolls the whole transaction back on the first wrong name.

**Pre-check and post-check.** Run this on each database before and after. Before, the result must be exactly the names of §4.1 for that database. After, it must be empty.

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
-- P6. ops-schema.sql is an optional extension, so the table may be absent.
ALTER TABLE IF EXISTS provisioning.copy_sagas RENAME COLUMN kind TO type;
ALTER TABLE IF EXISTS provisioning.copy_sagas RENAME CONSTRAINT copy_sagas_kind_check TO copy_sagas_type_check;
ALTER TABLE IF EXISTS provisioning.copy_sagas RENAME CONSTRAINT copy_sagas_kind_not_null TO copy_sagas_type_not_null;
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
-- P9 function bodies: append the new deploy/sql/record-history.sql here.
COMMIT;
```

`deploy/sql/record-history.sql` is idempotent: `CREATE SCHEMA IF NOT EXISTS`, `CREATE OR REPLACE FUNCTION`, and grants that the database already holds (`record-history.sql:37` to `:286`). It carries no transaction of its own (`record-history.sql:4`). So the apply appends the file from the migration commit before `COMMIT`, and the installed functions become byte for byte the fresh-install functions.

wamn_system holds the record-history functions (`SYSTEM_SCHEMA_SQL`, `crates/control/provision/src/lib.rs:151`) but no history table. Its identity relations carry stamp triggers only (`system-schema.sql:296` and the other `wamn_record_history_stamp` triggers), and `record-history-app-grants.sql:6` says the system database never applies the app grants. So P9 in wamn_system is the function replacement only. The history DO block below may run there too, and it then renames nothing.

**Each project-env database.** In wamn-dev these are `wamn-db-dkk--receiving--dev--4pqjfmli` and `wamn-db-dkk--wms--dev--0nk1lrpr` (`docs/operations/gcp.md` §3.18 and §5.4). `SELECT datname FROM pg_database WHERE datname LIKE 'wamn-db-%'` confirms the list. Save as `$P/kind-to-type-project-env.sql`:

```sql
BEGIN;
-- P8
ALTER TABLE catalog.package_definition_owners RENAME COLUMN definition_kind TO definition_type;
ALTER TABLE catalog.package_definition_owners RENAME CONSTRAINT package_definition_owners_definition_kind_check TO package_definition_owners_definition_type_check;
ALTER TABLE catalog.package_definition_owners RENAME CONSTRAINT package_definition_owners_definition_kind_not_null TO package_definition_owners_definition_type_not_null;
-- P10
ALTER TABLE wamn_run.runs RENAME COLUMN caller_outcome_kind TO caller_outcome_type;
ALTER TABLE wamn_run.runs RENAME CONSTRAINT runs_caller_outcome_kind_check TO runs_caller_outcome_type_check;
ALTER TABLE wamn_run.runs RENAME COLUMN fail_kind TO fail_type;
ALTER TABLE wamn_run.runs RENAME CONSTRAINT runs_fail_kind_check TO runs_fail_type_check;
-- P11
ALTER TABLE wamn_run.effect_attempts RENAME COLUMN generation_fact_kind TO generation_fact_type;
ALTER TABLE wamn_run.effect_attempts RENAME CONSTRAINT effect_attempts_generation_fact_kind_not_null TO effect_attempts_generation_fact_type_not_null;
-- P12
ALTER TABLE wamn_run.operator_run_actions RENAME COLUMN action_kind TO action_type;
ALTER TABLE wamn_run.operator_run_actions RENAME CONSTRAINT operator_run_actions_kind_check TO operator_run_actions_type_check;
ALTER TABLE wamn_run.operator_run_actions RENAME CONSTRAINT operator_run_actions_action_kind_not_null TO operator_run_actions_action_type_not_null;
ALTER TABLE wamn_run.operator_run_actions RENAME COLUMN principal_kind TO principal_type;
ALTER TABLE wamn_run.operator_run_actions RENAME CONSTRAINT operator_run_actions_principal_kind_check TO operator_run_actions_principal_type_check;
ALTER TABLE wamn_run.operator_run_actions RENAME CONSTRAINT operator_run_actions_principal_kind_not_null TO operator_run_actions_principal_type_not_null;
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
-- P9 function bodies: append the new deploy/sql/record-history.sql here.
COMMIT;
```

`create_history_table` names both constraints, `<history>_kind_check` and `<history>_kind_not_null` (`record-history.sql:106`, `:139`). The block therefore finds every history table, in `app_system` (`app-schema.sql:435`) and in each package schema (`crates/control/lib/src/apply_package/record_history.rs:57`). It prints one notice per table. In wamn-dev the expected tables are the six `app_system` tables in both databases, `receiving.purchase_order_history` and `receiving.purchase_order_line_history` in Receiving, and `wms.packaging_history` in WMS. These come from the generated data-access overlays. The notices are the evidence to record.

apply-package never creates a history table again once it exists (`crates/control/lib/src/apply_package/record_history.rs:51`). So the new platform does not repair an old history table, and the DO block is the only path for installed tables.

No history entry holds a renamed key. The `before` and `after` images copy the columns of the logged relation. No logged relation has a renamed column: the P1 to P8 and P10 to P12 tables carry no log trigger, and no application relation has a `kind` column (the only `kind` in `apps/*/wamn.json` outside A1 is the history column, A5).

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
| Regenerated | P9 | Every `apps/*/generated/` file of §2.4, the three `generated/platform-policy/data-access.json` overlays (`wamn_receiving`, `wamn_wms`, `platform_fixture`), and `apps/wamn_receiving/tests/.sqlx/query-04951d1d….json`. That is the only SQLx query file that names a renamed column (`git grep -l -E '_kind\|\bkind\b' -- '*/.sqlx/*'`) |
| Tests and fixtures | all | The live and unit tests that the §1.7 command lists, including `crates/control/provision/tests/deploy_sql_authority.rs`, `control_storage.rs`, `control_portable_store.rs`, `identity_issuer_live.rs`, `crates/identity/platform/tests/`, `crates/identity/project-state/tests/`, `crates/control/lib/tests/run_plane_live/`, `crates/schema/control/src/run_plane/tests.rs`, `tests/conformance/src/schema_drift.rs`, `crates/schema/generator/tests/generation.rs`, `crates/platform/runtime/tests/support/session_fixture.rs:220`, and the `registry.orgs` fixture inserts across `crates/`, `services/` and `tests/integration/` |
| Scripts | P1 | `tools/identity-jwks-journey-run:428` inserts `placement_kind` |

The `tests/sweeps/*.log` files that name `principal_kind` are records of past runs and stay.

### 4.6 CDC

No change-event payload key changes.

- The publication covers the application data schema only, `FOR TABLES IN SCHEMA <schema>` (`crates/control/provision/src/sql/cdc.rs:82`). In wamn-dev that is `receiving` and `wms` (`docs/operations/gcp.md` §3.18 and §5.4). The P1 to P8 and P10 to P12 tables live in `registry`, `identity`, `provisioning`, `catalog` and `wamn_run`, which no publication covers.
- The package history tables do live in the published schema. But each one is a declared CDC exclusion (`crates/schema/control/src/package_migrations.rs:385`). The reader classifies each relation by OID and drops an excluded relation before it builds a payload (`services/cdc-reader/src/lib.rs:339`, `:398`). A rename keeps the OID, so the classification holds.
- The payload is the column map of the changed row (`services/cdc-reader/src/lib.rs:478`). No published entity relation has a renamed column (§4.3).
- Logical decoding carries no DDL. After the rename, pgoutput sends a new relation message for a history table at its next change, and the reader drops that change as before. The slot and the publication need no change, and the CDC readers may keep running through the apply.

The router tap `source_kind` (W5) is a JetStream body, not a column. §5 decides it.

### 4.7 Ordering with the release cutover

A renamed column breaks every old binary that names it. The old host and runtime read `runs.fail_kind` and `effect_attempts.generation_fact_kind`. The old identity service and the old host PAT check read `principals.kind`. The old scenario-worker writes `authoring_command_audit.command_kind`. The old `receiving` component reads `history.kind`, and the old data-access overlays grant `SELECT (kind)` on history tables. The new binaries fail the same way against the old names. So there is no mixed window.

The constraint that §3 must honor:

1. Stop every workload that reads wamn_system or a project-env database with the old names: the hosts, the identity service, the scenario-worker and the HTTP and materializer workloads. The CDC readers may keep running (§4.6).
2. Apply the wamn_system script and then each project-env script, each in one transaction, before any new binary starts.
3. Start the new binaries and run the republish of the 2.0.0 packages (§2.5) after the statements. The new apply-package writes `definition_type`, and the new scenario-worker writes `command_type`, so the republish cannot come first.
4. Do not run the old `reconcile-run-plane` against a renamed database. It would find the declared checks missing.

The republish order itself belongs to wamn-sfea.3.

Rollback runs the same statements with the names swapped and appends `deploy/sql/record-history.sql` from the old commit. It is also metadata-only.

### 4.8 Record for gcp.md §7

`docs/operations/gcp.md:1728` is §7 "Schema changes applied by hand". When the statements run, add this entry to it, in the form of its `registry.capture_gap` entry. The angle-bracket fields are filled in at apply time.

````markdown
On <date> (`wamn-sfea`, `wamn-o8b9`), the `kind` → `type` renames of `docs/plan/kind-to-type.md` §4.3 went into `wamn_system`, `wamn-db-dkk--receiving--dev--4pqjfmli` and `wamn-db-dkk--wms--dev--0nk1lrpr`, with the workloads stopped. Write the wamn_system script of §4.3 into `$P/kind-to-type-system.sql` and the project-env script into `$P/kind-to-type-project-env.sql`. Put `deploy/sql/record-history.sql` of commit <commit> before the `COMMIT;` of each. Apply them as the superuser:

```sql
BEGIN;
ALTER TABLE registry.orgs RENAME COLUMN placement_kind TO placement_type;
-- ... the other renames of §4.3 ...
CREATE OR REPLACE FUNCTION identity.lock_password_principal(principal uuid) ...;
-- deploy/sql/record-history.sql
COMMIT;
```

```bash
kubectl -n platform exec -i wamn-pg-1 -c postgres -- psql -U postgres -d wamn_system -v ON_ERROR_STOP=1 < $P/kind-to-type-system.sql
for db in wamn-db-dkk--receiving--dev--4pqjfmli wamn-db-dkk--wms--dev--0nk1lrpr; do
  kubectl -n platform exec -i wamn-pg-1 -c postgres -- psql -U postgres -d "$db" -v ON_ERROR_STOP=1 < $P/kind-to-type-project-env.sql
done
```

The applies took <n> seconds. The history notices named <tables>. The check query of §4.3 returned no row in any of the three databases afterwards.
````

### 4.9 Questions for the owner

1. §1.8 rules the history column only. Are the other names of §4.1 right? In particular `principals.type`, `fail_type`, `action_type` and `operator_run_actions_type_check`.
2. Should the installed statements rename the generated names too (`_not_null`, `_key`, `_fkey`, one-column `_check`)? Nothing in the code reads them. Renaming keeps an installed database equal to a fresh install. §4.3 assumes yes.
3. `reconcile-run-plane` already carries in-code changes for retired run-schema structures (`crates/schema/control/src/run_plane/schema_changes.rs:24`). Should P10 to P12 go there instead of into hand statements? §4.3 assumes hand statements under `wamn-o8b9`.
4. P15, the Kubernetes annotation `wamn.io/principal-kind` on PAT Secrets: does it rename to `wamn.io/principal-type`? If yes, `pat_secrets.rs:267` and `deploy/mvp/bootstrap.sh:118` change together, and the installed PAT Secrets need the new annotation.
5. Is `ops-schema.sql` installed in wamn-dev? The P6 statements use `IF EXISTS`, so they work either way.

## 6. `*ErrorKind` families

Owner rulings, 2026-09-29 and 2026-09-30 (wamn-sfea.6, corrected in wamn-sfea.2): the `*ErrorKind` rename is part of this migration epic.

- All 74 families rename in one commit, including `NodeErrorKind` and the WAMN enum `ErrorKind` in `connection_http/transport.rs`.
- `NodeErrorKind` serializes only its values, such as `retryable`, and no field holds the type. Its rename moves no bytes (`crates/execution/run-state/src/status.rs:250`).
- The same commit renames the `kind` fields of error structs, such as `StatementError.kind`. Their `Debug` output compiles into components, so this commit moves component bytes.
- The commit lands before the package rebuild and republish, so each component is built once.
