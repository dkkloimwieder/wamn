# R6 scope at `6b4db53e1`

**F** marks source evidence. **I** marks proposed replacements and inferred impact. Rust counts are distinct files containing the exact spelling in `crates`, `services`, `apps/crates`, `test-support`, and `tests`, including references and incidental text. Shared spellings such as `Application` have one count across their owners. SQL counts are qualified occurrences in those Rust files. R6 calls for one pass that changes the public wire and generated clients. `docs/plan/review-restructure.md:146`

## 1. Crates and modules

| F current name and source | I replacement |
|---|---|
| `crates/application`, package `wamn-application`, Rust `wamn_application` — 14 Cargo manifests, 63 Rust files. `Cargo.toml:250`, `crates/application/Cargo.toml:2`, `Cargo.lock:6191` | `crates/app`, `wamn-app`, `wamn_app` |
| `crates/environment`, package `wamn-environment`, Rust `wamn_environment` — 10 Cargo manifests including `xtask`, 31 Rust files in scope. `Cargo.toml:247`, `crates/environment/Cargo.toml:2`, `Cargo.lock:6655` | `crates/env`, `wamn-env`, `wamn_env` |
| `application` — four module files. `crates/engine/src/application.rs`, `crates/identity/src/identity/application.rs`, `crates/identity/src/identity/application/service.rs`, `services/edge/src/application.rs` | `app.rs` and `identity/app/`. Move `identity/application/user.rs` with its sibling. |
| `environment` — two modules. `crates/environment/src/lib.rs:8`, `crates/environment/src/environment/mod.rs`, `crates/platform-install/src/environment.rs` | `env/` and `env.rs`. Move all descendants of `crates/environment/src/environment/`. |
| `application_chart` — one module with three child files. `crates/environment/src/lib.rs:3`, `crates/environment/src/application_chart.rs`, `crates/environment/src/application_chart/plan.rs` | `app_chart.rs` and `app_chart/` |
| `application_config` — one module. `crates/application/src/lib.rs:6`, `crates/application/src/application_config.rs:8` | `app_config.rs` |
| `application_member` — one module. `crates/component-policy/src/lib.rs:26`, `crates/component-policy/src/application_member.rs:5` | `app_member.rs` |
| `application_route` — one module. `services/host/src/host.rs:25`, `services/host/src/host/application_route.rs:40` | `app_route.rs` |
| `application_verbs` — one module. `services/cli/src/lib.rs:3`, `services/cli/src/application_verbs.rs:14` | `app_verbs.rs` |
| `environment_application` — one module. `crates/registry/src/lib.rs:5`, `crates/registry/src/environment_application.rs:22` | `env_app.rs` |
| `environment_configuration` — one module. `crates/environment/src/lib.rs:9`, `crates/environment/src/environment_configuration.rs:25` | `env_configuration.rs` |
| `environment_fill` — one module and one integration test target. `crates/environment/src/lib.rs:10`, `crates/environment/src/environment_fill.rs`, `crates/environment/tests/environment_fill.rs:25` | `env_fill.rs` |
| `environment_names` — one module. `crates/environment/src/lib.rs:11`, `crates/environment/src/environment_names.rs:4` | `env_names.rs` |
| `environment_org` — one module. `crates/environment/src/lib.rs:12`, `crates/environment/src/environment_org.rs:47` | `env_org.rs` |
| `environment_registry` — one module. `crates/environment/src/lib.rs:13`, `crates/environment/src/platform_migrations.rs:49` | `env_registry.rs` |
| `environment_requests` — one module. `services/cli/src/lib.rs:21`, `services/cli/src/environment_requests.rs:9` | `env_requests.rs` |
| `environment_schema` — one module. `crates/environment/src/lib.rs:14`, `test-support/infrastructure/database.rs:317` | `env_schema.rs` |
| `environment_sql` — one module. `crates/environment/src/lib.rs:15`, `crates/environment/src/environment_sql.rs:55` | `env_sql.rs` |
| `environment_sql_effects` — one module. `crates/environment/src/lib.rs:16`, `crates/environment/src/environment_sql_effects.rs:1148` | `env_sql_effects.rs` |
| `environment_status` — one module. `services/control/src/lib.rs:5`, `services/control/src/environment_status.rs:17` | `env_status.rs` |
| `platform_environment` — two modules. `crates/environment/src/lib.rs:23`, `crates/kubernetes/src/lib.rs:9` | `platform_env.rs` in each crate |
| `application_composition`, `application_routes_kind`, `environment_lock_live` — one test target each. `crates/oci/tests/application_composition.rs`, `tests/journeys/tests/application_routes_kind.rs`, `crates/environment/tests/environment_lock_live.rs` | `app_composition`, `app_routes_kind`, `env_lock_live` |

**F** The directory or filename changes also affect literal paths to delivered inputs, including the file package migration, the env JSON Schema and the env chart. **I** Update these references with the moves. `crates/codegen/tests/file_reference_live.rs:54`, `xtask/src/repo_policy.rs:32`, `crates/platform-install/src/release_chart/bundle.rs:213`

## 2. Rust types, traits, and variants

| F spelling | Files | I replacement | F evidence |
|---|---:|---|---|
| `ActiveEnvironment` | 1 | `ActiveEnv` | `crates/authentication/src/authorization/store.rs:43`, `crates/authentication/src/authorization/store.rs:155` |
| `Application` | 44 | `App` | Operation enum and journey struct at `crates/environment/src/environment/operation.rs:36`, `tests/journeys/tests/common/mod.rs:331`. Variants at `crates/routes/src/host_route.rs:106`, `crates/routes/src/host_wire/types.rs:37`, `crates/blobstore/src/file_route.rs:17`, `crates/authority/src/effect_span.rs:138`. |
| `ApplicationBinding` | 1 | `AppBinding` | `crates/plugin/src/bound_http.rs:49` |
| `ApplicationComponent` | 4 | `AppComponent` | `crates/application/src/application_config.rs:26`, `crates/environment/src/application_chart/plan.rs:73` |
| `ApplicationConfig` | 31 | `AppConfig` | `crates/application/src/application_config.rs:13`, `crates/environment/src/status.rs:7` |
| `ApplicationConfiguration` | 3 | `AppConfiguration` | `crates/registry/src/configuration.rs:37`, `crates/environment/src/registry/configuration.rs:6` |
| `ApplicationConnection` | 3 | `AppConnection` | `crates/application/src/application_config.rs:99`, `crates/connection/src/connection.rs:4` |
| `ApplicationDataChange` | 2 | `AppDataChange` | `crates/application/src/application_config.rs:42`, `crates/environment/src/application_chart/plan.rs:73` |
| `ApplicationDefinition` | 4 | `AppDefinition` | `crates/registry/src/environment_application.rs:120`, `crates/environment/src/environment/write/plan.rs:69` |
| `ApplicationDeployRequest` | 2 | `AppDeployRequest` | `crates/routes/src/host_wire/types.rs:281`, `crates/routes/src/host_route.rs:555` |
| `ApplicationDeployResult` | 2 | `AppDeployResult` | `crates/routes/src/host_wire/types.rs:292`, `crates/routes/src/host_route.rs:559` |
| `ApplicationFiles` | 1 | `AppFiles` | `services/host/src/host/files.rs:26` |
| `ApplicationName` | 55 | `AppName` | `crates/registry/src/environment_application.rs:22`, `services/cli/src/environment_requests.rs:17` |
| `ApplicationOperation` | 4 | `AppOperation` | `crates/application/src/application_config.rs:61`, `crates/plugin/src/operation.rs:11` |
| `ApplicationOperationType` | 4 | `AppOperationType` | `crates/application/src/application_config.rs:77`, `crates/environment/src/application_chart/plan.rs:74` |
| `ApplicationRollbackRequest` | 2 | `AppRollbackRequest` | `crates/routes/src/host_wire/types.rs:298`, `crates/routes/src/host_route.rs:570` |
| `ApplicationRoutes` | 2 | `AppRoutes` | `services/host/src/host/application_route.rs:40`, `services/host/src/host/host_route.rs:22` |
| `ApplicationSelection` | 3 | `AppSelection` | `crates/environment/src/environment/document/plan.rs:31`, `crates/environment/src/environment/document/file.rs:76` |
| `ApplicationStopRequest` | 2 | `AppStopRequest` | `crates/routes/src/host_wire/types.rs:309`, `crates/routes/src/host_route.rs:540` |
| `ApplicationTarget` | 9 | `AppTarget` | `crates/authentication/src/administration.rs:133`, `services/control/src/control_route.rs:842` |
| `ApplicationUser` | 3 | `AppUser` | `crates/authentication/src/administration.rs:394`, `crates/identity/src/identity/application/user.rs:11` |
| `ApplicationUserList` | 2 | `AppUserList` | `crates/routes/src/host_route.rs:126`, `services/host/src/host/application_route.rs:162` |
| `ApplicationUserListResult` | 2 | `AppUserListResult` | `crates/routes/src/host_wire/types.rs:186`, `crates/routes/src/host_route.rs:15` |
| `ApplicationUserListResultUser` | 1 | `AppUserListResultUser` | `crates/routes/src/host_wire/types.rs:168`, `crates/routes/src/host_wire/types.rs:187` |
| `ApplicationUserRow` | 2 | `AppUserRow` | `crates/authentication/src/administration.rs:124`, `crates/authentication/src/administration/sql.rs:3` |
| `ApplicationWriteIncomplete` | 1 | `AppWriteIncomplete` | `crates/routes/src/host_wire/refusals.rs:19` |
| `BoundApplication` | 1 | `BoundApp` | `crates/engine/src/application.rs:14`, `crates/engine/src/application.rs:47` |
| `EdgeApplication` | 4 | `EdgeApp` | `services/edge/src/application.rs:23`, `services/edge/src/serve.rs:39` |
| `Environment` | 27 | `Env` | Registry and journey structs at `crates/registry/src/types.rs:34`, `tests/journeys/tests/common/mod.rs:296`. Variants at `crates/authentication/src/authorization/store.rs:27`, `crates/authentication/src/paging.rs:18`, `crates/connection-type/src/lib.rs:98`, `crates/environment/src/environment/worker.rs:173`. |
| `EnvironmentActivate` | 2 | `EnvActivate` | `crates/routes/src/host_route.rs:187`, `services/control/src/control_route.rs:640` |
| `EnvironmentApplications` | 6 | `EnvApps` | `crates/registry/src/environment_application.rs:128`, `crates/environment/src/application_chart/plan.rs:552` |
| `EnvironmentApply` | 2 | `EnvApply` | `crates/platform-install/src/platform_event.rs:15`, `services/control/src/platform_event.rs:9` |
| `EnvironmentArgs` | 1 | `HostSessionArgs` (unsure: semantic name proposed because it contains host URL and token) | `services/cli/src/role_verbs.rs:6`, `services/cli/src/role_verbs.rs:21` |
| `EnvironmentConfiguration` | 4 | `EnvConfiguration` | `crates/registry/src/configuration.rs:25`, `crates/environment/src/environment_sql_effects.rs:26` |
| `EnvironmentConfigureRequest` | 2 | `EnvConfigureRequest` | `crates/routes/src/host_wire/types.rs:428`, `crates/routes/src/host_route.rs:277` |
| `EnvironmentDelete` | 2 | `ProjectEnvInactivate` | `crates/platform-install/src/platform_event.rs:16`, `services/control/src/platform_event.rs:10` |
| `EnvironmentDeleteRequest` | 2 | `ProjectEnvInactivateRequest` | `crates/routes/src/host_wire/types.rs:440`, `crates/routes/src/host_route.rs:292` |
| `EnvironmentDocument` | 20 | `EnvDocument` | `crates/environment/src/environment/document/plan.rs:8`, `crates/environment/src/environment/bindings.rs:15` |
| `EnvironmentFile` | 10 | `EnvFile` | `crates/environment/src/environment/document/file.rs:20`, `services/cli/src/environment_requests.rs:55` |
| `EnvironmentInactivate` | 2 | `EnvInactivate` | `crates/routes/src/host_route.rs:189`, `services/control/src/control_route.rs:640` |
| `EnvironmentList` | 2 | `EnvList` | `crates/routes/src/host_route.rs:167`, `services/control/src/control_route.rs:434` |
| `EnvironmentListRequest` | 2 | `EnvListRequest` | `crates/routes/src/host_wire/types.rs:448`, `crates/routes/src/host_route.rs:1457` |
| `EnvironmentListResult` | 2 | `EnvListResult` | `crates/routes/src/host_wire/types.rs:476`, `crates/routes/src/host_route.rs:1457` |
| `EnvironmentListResultEnvironment` | 1 | `EnvListResultEnv` | `crates/routes/src/host_wire/types.rs:468`, `crates/routes/src/host_wire/types.rs:740` |
| `EnvironmentListResultEnvironmentState` | 1 | `EnvListResultEnvState` | `crates/routes/src/host_wire/types.rs:459`, `crates/routes/src/host_wire/types.rs:471` |
| `EnvironmentNameError` | 1 | `EnvNameError` | `crates/platform-install/src/environment.rs:5`, `crates/platform-install/src/environment.rs:34` |
| `EnvironmentNotFound` | 5 | `EnvNotFound` | `crates/authentication/src/identity_error.rs:35`, `crates/routes/src/host_wire/refusals.rs:40`, `crates/identity/src/identity/org.rs:132` |
| `EnvironmentOperationStatusRequest` | 2 | `EnvOperationStatusRequest` | `crates/routes/src/host_wire/types.rs:486`, `crates/routes/src/host_route.rs:307` |
| `EnvironmentOperationStatusResult` | 2 | `EnvOperationStatusResult` | `crates/routes/src/host_wire/types.rs:494`, `crates/routes/src/host_route.rs:307` |
| `EnvironmentRequest` | 4 | `EnvRequest` | `crates/routes/src/host_wire.rs:82`, `services/control/src/control_route.rs:459` |
| `EnvironmentRow` | 4 | `EnvRow` | `crates/registry/src/types.rs:62`, `crates/environment/src/environment_sql_effects.rs:25` |
| `EnvironmentSelectedHttpHeader` | 1 | `EnvSelectedHttpHeader` | `crates/connection-type/src/lib.rs:130`, `crates/connection-type/src/lib.rs:466` |
| `EnvironmentSettings` | 5 | `EnvSettings` | `crates/registry/src/configuration.rs:10`, `crates/environment/src/environment/document/file.rs:12` |
| `EnvironmentStatus` | 12 | `EnvStatus` for the trait, registry enum, status result record and handler variant | `crates/application/src/ingress.rs:211`, `crates/registry/src/types.rs:42`, `crates/routes/src/host_wire.rs:106`, `crates/routes/src/host_route.rs:171` |
| `EnvironmentStatusReader` | 2 | `EnvStatusReader` | `crates/authentication/src/route_authentication.rs:33`, `services/host/src/host.rs:597` |
| `EnvironmentStatusResult` | 4 | `EnvStatusResult` | `crates/routes/src/host_wire/types.rs:539`, `services/control/src/environment_status.rs:17` |
| `EnvironmentStatusResultRollout` | 2 | `EnvStatusResultRollout` | `crates/routes/src/host_wire/types.rs:504`, `crates/environment/src/status.rs:10` |
| `EnvironmentStatusResultWorkload` | 2 | `EnvStatusResultWorkload` | `crates/routes/src/host_wire/types.rs:522`, `crates/environment/src/status.rs:10` |
| `EnvironmentStatusResultWorkloadCondition` | 2 | `EnvStatusResultWorkloadCondition` | `crates/routes/src/host_wire/types.rs:513`, `crates/environment/src/status.rs:11` |
| `EnvironmentSummary` | 2 | `EnvSummary` | `crates/identity-protocol/src/records.rs:37`, `crates/identity/src/identity/org.rs:12` |
| `NativeApplication` | 4 | `NativeApp` | `services/edge/src/native_operation/native_workload.rs:139`, `services/edge/src/application.rs:11` |
| `OtherEnvironment` | 3 | `OtherEnv` | `tests/journeys/tests/common/request.rs:49`, `tests/journeys/tests/isolation.rs:66` |
| `ParseApplicationNameError` | 2 | `ParseAppNameError` | `crates/registry/src/environment_application.rs:26`, `crates/registry/src/lib.rs:9` |
| `ProjectEnvironment` | 2 | `ProjectEnv` for both identity enums | `crates/identity-protocol/src/workload_role.rs:120`, `crates/identity-protocol/src/workload_role.rs:144`, `crates/platform-install/tests/common/grants.rs:64` |
| `WaitForEnvironment` | 3 | `WaitForEnv` | `crates/environment/src/environment/database.rs:11`, `crates/environment/src/environment/worker.rs:268` |

**F** The ingress guest has another local `Application` struct outside the Rust directory scope. It decodes the app workload config key. **I** Its reader moves with `wamn.application` below. `apps/guests/ingress/src/guest/data_change.rs:9`, `apps/guests/ingress/src/guest/data_change.rs:55`

## 3. Functions, fields, constants, and embedded SQL

**I** Mechanical identifier rule for the following lists: replace whole naming segments `application(s)` → `app(s)`, `environment(s)` → `env(s)`, and `organization` → `org`, with equivalent uppercase forms. Keep collection plurals. Check bindings when an existing `app` or `env` already has a different meaning. **F** For example, the app reference in `workflow` is named `app` and its name field is `application`. `docs/plan/review-restructure.md:146`, `crates/plugin/src/workflow.rs:85`, `services/host/src/host/workflow/broker.rs:46`

### F Functions and members covered by the mechanical rule

| Spelling | Files | Examples |
|---|---:|---|
| `acquire_application` | 2 | `crates/environment/src/registry/lifecycle_lock.rs:76`, `crates/environment/tests/environment_lock_live.rs:44` |
| `acquire_environment` | 2 | `crates/environment/src/registry/lifecycle_lock.rs:47`, `crates/environment/src/environment_sql_effects.rs:1155` |
| `affected_applications` | 1 | `crates/environment/src/environment/sql/schema.rs:58`, `crates/environment/src/environment/sql/schema.rs:178` |
| `application` | 273 | `crates/environment/src/environment/operation.rs:182`, `crates/application/src/application_config.rs:17`, `crates/postgres/src/transaction_identity.rs:98` |
| `application_chart` | 12 | `crates/platform-install/src/install.rs:16`, `crates/environment/src/lib.rs:3` |
| `application_chart_reference` | 2 | `crates/platform-install/src/platform_install.rs:51`, `services/cli/src/platform_verbs.rs:9` |
| `application_config` | 33 | `crates/engine/src/ingress.rs:42`, `crates/environment/src/environment/operation.rs:365` |
| `application_consumer` | 1 | `crates/broker/src/events.rs:42`, `crates/broker/src/events.rs:240` |
| `application_consumer_config` | 2 | `crates/broker/src/events.rs:66`, `crates/broker/src/streams/plan.rs:50` |
| `application_counts` | 2 | `services/control/src/control_route.rs:929`, `services/control/src/control_sql.rs:97` |
| `application_dependency_permissions` | 3 | `crates/release/src/publish_release/plan.rs:152`, `crates/environment/src/environment_sql_effects.rs:375` |
| `application_event_consumer` | 2 | `crates/registry/src/resource_names.rs:28`, `crates/broker/src/events.rs:60` |
| `application_for_package` | 3 | `crates/registry/src/environment_application.rs:131`, `crates/environment/src/environment/stage/plan.rs:16` |
| `application_get` | 2 | `tests/journeys/tests/common/host.rs:252`, `tests/journeys/tests/host_contracts.rs:391` |
| `application_host` | 6 | `crates/registry/src/name.rs:18`, `crates/environment/src/application_chart/plan.rs:455` |
| `application_id` | 1 | `services/edge/src/native_operation/native_workload.rs:339`, `services/edge/src/native_operation/native_workload.rs:352` |
| `application_label_reason` | 2 | `crates/registry/src/identifiers.rs:29`, `crates/environment/src/environment/document/plan.rs:236` |
| `application_names` | 2 | `crates/environment/src/environment_sql_effects.rs:115`, `crates/environment/src/environment/analyse.rs:84` |
| `application_objects` | 3 | `tests/journeys/tests/common/host.rs:181`, `tests/journeys/tests/host_contracts.rs:374` |
| `application_project` | 5 | `crates/environment/src/environment/document/file.rs:156`, `services/cli/src/environment_requests.rs:57` |
| `application_recovery_revision` | 2 | `crates/kubernetes/src/helm/plan.rs:14`, `crates/kubernetes/src/helm.rs:16` |
| `application_release_filter` | 1 | `crates/registry/src/name.rs:368` |
| `application_release_name` | 6 | `crates/registry/src/resource_names.rs:14`, `crates/environment/src/application_chart/plan.rs:29` |
| `application_releases` | 3 | `crates/environment/src/environment/analyse.rs:79`, `crates/environment/src/environment/lifecycle.rs:240` |
| `application_repository` | 1 | `crates/environment/src/environment/write/tests.rs:15`, `crates/environment/src/environment/write/tests.rs:127` |
| `application_routes` | 1 | `services/host/src/host/files.rs:106`, `services/host/src/host/files.rs:119` |
| `application_runtime_schema` | 2 | `crates/release/src/publish_release/plan.rs:49`, `crates/release/src/publish_release.rs:594` |
| `application_schemas` | 3 | `crates/codegen/src/materialize.rs:260`, `crates/codegen/src/catalog.rs:539` |
| `application_serves` | 2 | `crates/kubernetes/src/deployment_components.rs:256`, `crates/kubernetes/src/helm.rs:533` |
| `application_subject` | 2 | `crates/registry/src/resource_names.rs:6`, `crates/environment/src/environment_sql_effects.rs:310` |
| `application_users` | 4 | `crates/authentication/src/administration/sql.rs:92`, `services/host/src/host/application_route.rs:166` |
| `application_workloads` | 1 | `crates/kubernetes/src/deployment_components.rs:261` |
| `applications` | 43 | `crates/kubernetes/src/deployment_components.rs:411`, `crates/registry/src/configuration.rs:31`, `crates/environment/src/environment/document/file.rs:76` |
| `applications_for_delete` | 1 | `services/control/src/environment_status.rs:100`, `services/control/src/environment_status.rs:152` |
| `apply_application` | 2 | `crates/platform-install/src/deployment.rs:45`, `crates/environment/src/environment/operation.rs:274` |
| `bind_application` | 3 | `crates/authentication/src/authorization/store.rs:270`, `crates/connection/src/connection_facts.rs:36` |
| `bind_application_config` | 4 | `crates/engine/src/application.rs:19`, `crates/engine/src/ingress.rs:128` |
| `bound_application_config` | 3 | `crates/engine/src/application.rs:47`, `crates/engine/src/ingress.rs:43` |
| `composing_application` | 1 | `services/edge/document/src/serving_manifest.rs:468`, `services/edge/document/src/serving_manifest.rs:1133` |
| `configure_application` | 4 | `crates/authentication/src/authorization/store.rs:130`, `crates/plugin/src/lib.rs:314` |
| `ensure_environment` | 4 | `crates/identity/src/identity/nats_store.rs:78`, `services/identity-service/src/nats.rs:252` |
| `environment` | 254 | `services/control/src/platform.rs:244`, `services/cli/src/operation_verbs.rs:58`, `crates/application/src/application_config.rs:16`. Exceptions below. |
| `environment_account` | 2 | `crates/identity/src/nats.rs:299`, `crates/identity/src/identity/nats.rs:128` |
| `environment_connections` | 2 | `crates/connection/src/project_connections/mod.rs:88`, `crates/connection/src/connection_credentials.rs:427` |
| `environment_event_subjects` | 2 | `crates/registry/src/name.rs:236`, `crates/environment/src/environment/ensure.rs:226` |
| `environment_host_name` | 11 | `crates/registry/src/name.rs:290`, `crates/environment/src/status.rs:71` |
| `environment_inactive` | 3 | `crates/authentication/src/authorization/store.rs:149`, `crates/authentication/src/route_authentication.rs:59` |
| `environment_is_disposable` | 2 | `crates/release/src/publish_release.rs:42`, `crates/release/tests/effective_release_live.rs:176` |
| `environment_key` | 3 | `crates/registry/src/name.rs:65`, `crates/identity/src/workload_identity.rs:95` |
| `environment_limits` | 2 | `crates/identity/src/nats.rs:282`, `crates/identity/src/identity/nats.rs:112` |
| `environment_nats_account` | 2 | `crates/registry/src/name.rs:37`, `crates/identity/src/nats.rs:310` |
| `environment_nats_secret` | 3 | `crates/registry/src/name.rs:47`, `crates/environment/src/environment/broker.rs:36` |
| `environment_permissions` | 2 | `crates/identity/src/workload_identity.rs:43`, `services/identity-service/src/nats.rs:299` |
| `environment_projects` | 17 | `services/control/src/control_sql.rs:27`, `crates/environment/src/registry/deployment.rs:8`. The Rust collection function can become `env_projects` while the table becomes singular. |
| `environment_resource_name` | 11 | `crates/registry/src/name.rs:279`, `crates/environment/src/environment_names.rs:4` |
| `environment_row` | 3 | `crates/environment/src/environment_sql_effects.rs:1183`, `crates/environment/src/environment/stage.rs:25` |
| `environment_service_account` | 6 | `crates/registry/src/name.rs:302`, `crates/environment/src/environment/ensure.rs:210` |
| `environment_signer` | 2 | `crates/identity/src/identity/nats_store.rs:207`, `crates/identity/src/identity/nats.rs:157` |
| `environment_target` | 2 | `crates/environment/src/environment_sql_effects.rs:1090`, `crates/environment/src/environment/bindings.rs:64` |
| `environment_user` | 3 | `crates/identity/src/identity/nats.rs:149`, `services/identity-service/src/nats.rs:305` |
| `environment_users` | 1 | `crates/environment/src/environment/broker.rs:12`, `crates/environment/src/environment/broker.rs:83` |
| `environments` | 33 | `services/control/src/control_route.rs:808`, `crates/identity-protocol/src/records.rs:58`, `crates/environment/src/registry/deployment.rs:22` |
| `for_environment` | 2 | `crates/release/src/publish_release.rs:71`, `crates/release/src/publish_release/plan.rs:265` |
| `into_application` | 4 | `crates/environment/src/environment/document/file.rs:192`, `crates/environment/src/environment/lifecycle.rs:38` |
| `is_application_operation_import` | 2 | `crates/engine/src/component_admission.rs:431`, `crates/component-policy/src/component_facts.rs:600` |
| `is_denied_to_applications` | 1 | `crates/component-policy/src/lib.rs:309`, `crates/component-policy/src/lib.rs:837` |
| `load_native_application` | 2 | `services/edge/src/native_operation/native_workload.rs:174`, `services/edge/src/application.rs:11` |
| `lock_environment` | 4 | `crates/environment/src/environment_sql_effects.rs:1148`, `crates/environment/src/environment/lifecycle.rs:24` |
| `materialize_application_principals` | 2 | `crates/environment/src/environment_sql_effects.rs:288`, `crates/environment/src/environment/operation.rs:266` |
| `only_application` | 3 | `crates/environment/src/environment/document/file.rs:165`, `services/cli/src/schema_verbs.rs:105` |
| `org_environments` | 3 | `crates/environment/src/environment_org.rs:47`, `services/control/src/control_route.rs:814` |
| `organization` | 19 lexical, 2 function/caller files | `crates/identity/src/workload_identity.rs:16`, `services/identity-service/src/workload.rs:30` |
| `project_environment` | 1 | `crates/authentication/src/authorization/store.rs:36`, `crates/authentication/src/authorization/store.rs:123` |
| `read_applications` | 1 | `crates/environment/src/environment/document/plan.rs:20`, `crates/environment/src/environment/document/plan.rs:62` |
| `read_environment_configuration` | 2 | `crates/environment/src/environment_sql_effects.rs:1176`, `crates/environment/src/environment/document.rs:43` |
| `read_environment_inactive` | 2 | `crates/authentication/src/authorization/project_reads.rs:215`, `crates/authentication/src/authorization/store.rs:21` |
| `refuse_application_operation_imports` | 1 | `crates/component-policy/src/component_facts.rs:567` |
| `refuse_application_references` | 2 | `crates/connection/src/references.rs:8`, `crates/connection/src/administration.rs:194` |
| `refuse_environment_references` | 4 | `crates/connection/src/project_connections/references.rs:17`, `crates/connection/src/project_connections/administration.rs:338` |
| `released_application` | 2 | `services/edge/src/application.rs:62`, `services/edge/src/native_operation.rs:80` |
| `require_applications` | 2 | `crates/environment/src/environment/document/plan.rs:230`, `crates/environment/src/environment/document/file.rs:143` |
| `require_counter_application` | 1 | `services/host/src/host/application_route.rs:402`, `services/host/src/host/application_route.rs:403` |
| `retire_environment` | 3 | `crates/identity/src/identity/nats_store.rs:181`, `services/identity-service/src/nats.rs:225` |
| `select_application` | 3 | `crates/environment/src/environment/analyse.rs:30`, `services/cli/src/application_verbs.rs:61` |
| `selected_application` | 1 | `crates/build/src/source_deploy.rs:107` |
| `set_environment_status` | 3 | `crates/environment/src/environment_org.rs:124`, `services/control/src/control_route.rs:722` |
| `status_environments` | 2 | `crates/environment/src/environment_org.rs:74`, `services/control/src/control_route.rs:716` |
| `try_environment_lock` | 3 | `crates/postgres/src/advisory_lock.rs:10`, `services/control/src/control_route/integration.rs:85` |
| `validate_application_member` | 4 | `crates/component-policy/src/application_member.rs:5`, `crates/build/src/composer.rs:232` |
| `wait_application` | 1 | `crates/kubernetes/src/helm.rs:426` |
| `wait_application_ready` | 2 | `crates/kubernetes/src/deployment_components.rs:248`, `crates/kubernetes/src/helm.rs:434` |
| `with_environment_status` | 2 | `crates/engine/src/ingress.rs:48`, `services/host/src/host.rs:598` |
| `workflow_application` | 2 | `crates/plugin/src/operation.rs:1122`, `services/host/src/host/workflow.rs:212` |
| `write_application` | 1 | `services/control/src/control_route.rs:847`, `services/control/src/control_route.rs:943` |
| `write_applications` | 2 | `services/control/src/control_route.rs:820`, `services/control/src/control_route/users.rs:144` |
| `write_environment` | 2 | `crates/environment/src/registry/configuration.rs:216`, `crates/environment/src/environment/lifecycle.rs:117` |
| `write_environment_status` | 3 | `crates/authentication/src/administration/sql.rs:51`, `services/control/src/control_route.rs:999` |

### F Constants covered by the mechanical rule

| Spelling | Files | Examples |
|---|---:|---|
| `APPLICATION` | 8 | `tests/journeys/tests/waits.rs:26`, `crates/environment/tests/support/files.rs:15` |
| `APPLICATION_CONFIG` | 7 | `crates/application/src/application_config.rs:8`, `crates/environment/src/status.rs:7` |
| `APPLICATION_DENIED_INTERFACES` | 1 | `crates/component-policy/src/lib.rs:306`, `crates/component-policy/src/lib.rs:309` |
| `CONTROL_ENVIRONMENT_SRC_ENVIRONMENT_ANALYSE_READ_ROW` | 2 | `crates/environment/src/environment_sql.rs:67`, `crates/environment/src/environment_sql_effects.rs:66` |
| `CONTROL_ENVIRONMENT_SRC_ENVIRONMENT_DELETE_DELETE_LOCKED` | 2 | `crates/environment/src/environment_sql.rs:71`, `crates/environment/src/environment_sql_effects.rs:795` |
| `CONTROL_ENVIRONMENT_SRC_ENVIRONMENT_DELETE_ROWS_DELETE_REGISTRY_ROWS` | 2 | `crates/environment/src/environment_sql.rs:75`, `crates/environment/src/environment_sql_effects.rs:621`. **I** Re-scope the statement name with the deactivation change. |
| `ENVIRONMENT` | 4 | `test-support/grant-tests/tests/system.rs:176`, `services/edge/tests/forward.rs:32` |
| `ENVIRONMENT_INACTIVE_CODE` | 1 | `crates/application/src/ingress.rs:56`, `crates/application/src/ingress.rs:263` |
| `ENVIRONMENT_INACTIVE_SQL` | 1 | `crates/authentication/src/authorization/project_reads.rs:18`, `crates/authentication/src/authorization/project_reads.rs:215` |
| `ENVIRONMENT_INACTIVE_STATUS` | 1 | `crates/application/src/ingress.rs:55`, `crates/application/src/ingress.rs:262` |
| `ENVIRONMENT_LOCK_SQL` | 4 | `crates/postgres/src/lib.rs:54`, `crates/environment/src/registry/lifecycle_lock.rs:10` |
| `ENVIRONMENT_PROJECTS` | 1 | `services/control/src/control_sql.rs:9`, `services/control/src/control_sql.rs:33` |
| `ENVIRONMENT_SHARED_LOCK_SQL` | 1 | `crates/environment/src/registry/lifecycle_lock.rs:17`, `crates/environment/src/registry/lifecycle_lock.rs:102` |
| `ENVIRONMENT_STATUS_ROW_SQL` | 1 | `crates/authentication/src/administration/sql.rs:25`, `crates/authentication/src/administration/sql.rs:51` |
| `ENVIRONMENT_STATUS_SQL` | 1 | `crates/environment/src/environment_org.rs:119`, `crates/environment/src/environment_org.rs:131` |
| `ENVIRONMENT_TRANSACTION_LOCK_SQL` | 2 | `crates/postgres/src/lib.rs:58`, `crates/plugin/src/workflow.rs:18` |
| `IDENTITY_PLATFORM_SRC_ORG_ENVIRONMENT_STATUS_SQL` | 2 | `crates/environment/src/environment_sql.rs:55`, `crates/environment/src/environment_org.rs:120` |
| `IDENTITY_PLATFORM_SRC_ORG_ORG_ENVIRONMENTS_SQL` | 2 | `crates/environment/src/environment_sql.rs:59`, `crates/environment/src/environment_org.rs:44` |
| `MAX_APPLICATION_LABEL_LEN` | 2 | `crates/registry/src/identifiers.rs:26`, `crates/environment/src/environment/document/plan.rs:36` |
| `ORG_ENVIRONMENTS_SQL` | 1 | `crates/environment/src/environment_org.rs:43`, `crates/environment/src/environment_org.rs:52` |

### F Test function spellings covered by the same rule

Each is in one file.

- `a_base_operation_reached_through_the_composed_entry_runs_as_the_composing_application` — `services/edge/document/src/serving_manifest.rs:1116`
- `a_package_has_one_declared_application_without_a_fallback` — `crates/registry/src/environment_application.rs:148`
- `a_route_host_must_be_the_control_or_an_application_host_of_its_namespace` — `crates/routes/src/route_host.rs:33`
- `an_application_component_importing_client_is_admitted` — `crates/engine/src/component_admission.rs:1099`
- `an_application_component_may_import_client_with_a_postgres_grant` — `crates/component-policy/src/lib.rs:839`
- `an_application_operation_import_is_refused` — `crates/component-policy/src/component_facts.rs:990`
- `an_unpinned_base_and_an_application_import_refuse` — `crates/engine/src/component_admission.rs:1177`
- `another_environments_credential_is_refused` — `tests/journeys/tests/request_path.rs:25`
- `application_byte_admission_grants_modbus_but_refuses_private_binding` — `crates/engine/src/component_admission.rs:775`
- `application_consumers_accept_application_names_and_refuse_invalid_source_tokens` — `crates/broker/src/events.rs:240`
- `application_file_writes_are_refused_and_history_uses_the_standard_contract` — `crates/environment/tests/files_live.rs:16`
- `application_history_keeps_bound_actor_and_records_scale_changes` — `crates/environment/tests/project_schema.rs:632`
- `application_members_cannot_import_operation_authority_or_export_http` — `crates/build/src/composer.rs:329`
- `application_mutations_require_row_security_in_each_database` — `crates/platform-install/tests/protected_relations_live.rs:8`
- `application_recovery_excludes_the_deployed_revision_that_does_not_serve` — `crates/kubernetes/src/helm.rs:733`
- `application_recovery_restores_the_last_success_after_an_interrupted_rollback` — `crates/kubernetes/src/helm.rs:713`
- `application_recovery_uninstalls_a_failed_first_install` — `crates/kubernetes/src/helm.rs:725`
- `application_stop_leaves_its_host` — `tests/journeys/tests/host_contracts.rs:222`
- `application_upgrade_requests_rollback_with_the_watcher` — `crates/kubernetes/src/helm.rs:660`
- `application_upgrade_reserves_rollback_time_and_requires_a_minimum_budget` — `crates/kubernetes/src/helm.rs:685`
- `collection_refuses_objects_from_another_environment_on_the_same_store` — `crates/blobstore/src/files.rs:785`
- `composed_entry_requires_a_named_application_in_its_members` — `services/edge/document/src/serving_manifest.rs:1219`
- `configured_connections_keep_each_application_declaration_owner` — `crates/release/src/connection_requirement.rs:40`
- `connection_delete_refuses_an_unready_workload_in_the_selected_environment` — `services/control/src/environment_status.rs:127`
- `credentials_are_inaccessible_to_application_roles` — `crates/platform-install/tests/platform_grants_live.rs:41`
- `each_environment_has_one_account_and_no_traffic_crosses_accounts` — `crates/identity/tests/nats_isolation.rs:302`
- `env_apply_stops_an_unlisted_application_and_keeps_its_data` — `tests/journeys/tests/host_contracts.rs:318`
- `environment_account_limits_refuse_the_next_connection_and_subscription` — `crates/identity/tests/nats_isolation.rs:397`
- `environment_data_deletion_confirmation_without_terminal` — `services/cli/tests/control_operations.rs:164`. **I** Align the test name with the project-env command and the actual data-teardown confirmation.
- `environments_in_one_org_are_isolated` — `tests/journeys/tests/isolation.rs:59`
- `environments_in_two_orgs_are_isolated` — `tests/journeys/tests/isolation.rs:75`
- `format_eleven_requires_composed_entry_application` — `services/edge/document/src/serving_manifest.rs:1230`. **I** The format number also needs to follow the serving manifest change.
- `generated_application_client_live` — `services/host/src/host/host_route/tests.rs:31`
- `member_list_cursor_binds_the_environment` — `services/control/src/control_route.rs:1206`
- `missing_corrupt_and_application_artifacts_write_nothing` — `crates/release/tests/web_artifact_upload.rs:12`
- `native_ingress_preserves_refusals_bind_transitions_and_application_404` — `crates/engine/src/expected_router.rs:170`
- `one_application_release_has_one_digest_across_environments` — `crates/release/src/publish_release.rs:600`
- `platform_schema_refuses_a_system_environment` — `services/cli/src/bin/wamn.rs:235`
- `project_configuration_keeps_applied_environment_timings` — `crates/environment/tests/registry_deployment_live.rs:140`
- `project_environment_membership_round_trip` — `crates/identity/tests/identity_live.rs:247`
- `provisioning_disables_and_reactivates_environment_member_rows` — `crates/environment/tests/environment_fill.rs:25`
- `runtime_schema_follows_relations_instead_of_the_application_name` — `crates/release/src/publish_release.rs:1046`
- `runtime_schema_is_pinned_and_consistent_within_an_application` — `services/edge/document/src/serving_manifest.rs:1251`
- `the_owner_reaches_an_application_and_control_through_web` — `tests/journeys/tests/smoke.rs:17`
- `the_projected_environment_marker_selects_the_dependency_rule` — `crates/release/src/publish_release.rs:1069`
- `two_environments_with_the_same_web_digest_share_one_completed_upload` — `crates/environment/src/web_artifact_tests.rs:8`

### Non-mechanical Rust and SQL names

- **F** The env CLI uses `Command::Delete` and `Delete`; the lifecycle module exports `delete` and has `delete_locked`; `delete_environment` occurs in three journey files. **I** Use `InactivateProject`, `inactivate_project`, `inactivate_project_locked`, and `inactivate_project_env` respectively. `services/cli/src/environment_requests.rs:9`, `crates/environment/src/environment/delete.rs:40`, `tests/journeys/tests/common/failure.rs:103`
- **F** `EnvironmentArgs` is flattened into three role argument types under `environment`. **I** If renamed to `HostSessionArgs`, change those fields to `session` (unsure: semantic name proposed for flattened host URL and token). `services/cli/src/role_verbs.rs:8`, `services/cli/src/role_verbs.rs:21`, `services/cli/src/role_verbs.rs:43`
- **F** `current_application` appears in SQL embedded in four Rust files. `application_role` is one SQL dollar-quote delimiter, `parent_environment` is a CTE alias in two files, and `environment_history` is a derived relation name referenced in one Rust file. **I** Use `current_app`, `app_role`, `parent_env`, and `env_history`. `apps/crates/guest/src/postgres_statements/numbering.rs:13`, `crates/environment/src/project_setup.rs:74`, `crates/codegen/src/build_database.rs:322`, `crates/identity/tests/password_live.rs:55`, `crates/environment/tests/project_authority.rs:351`
- **F** `application_history` is one test database name. **I** `app_history` (unsure: disposable fixture name, separate from a relation or wire contract). `crates/environment/tests/project_schema.rs:637`

### Embedded table references

| F old name | Rust hits | I replacement | F examples |
|---|---:|---|---|
| `registry.orgs` | 15 | `registry.org` | `crates/platform-install/src/provision_org.rs:235`, `test-support/grant-tests/tests/system.rs:567` |
| `registry.projects` | 16 | `registry.project` | `services/control/src/host_route/tests.rs:207`, `test-support/grant-tests/tests/system.rs:280` |
| `registry.environments` | 27 | `registry.env` | `crates/environment/src/environment_sql_effects.rs:1095`, `test-support/grant-tests/tests/system.rs:291` |
| `registry.environment_projects` | 33 | `registry.project_env` (unsure: R6 leaves relation word order open, proposed to match `ProjectEnvKey`) | `services/control/src/control_sql.rs:57`, `test-support/grant-tests/tests/system.rs:307`, `crates/registry/src/project_env_key.rs:9` |
| `registry.environment_applications` | 5 | `registry.env_app` | `crates/environment/src/registry/configuration.rs:177`, `test-support/grant-tests/tests/system.rs:324` |
| `registry.environment_integrations` | 5 | `registry.env_integration` | `crates/environment/src/registry/configuration.rs:247`, `crates/connection/src/project_connections/administration.rs:441` |
| `registry.environment_application_connections` | 4 | `registry.env_app_connection` | `crates/environment/src/registry/configuration.rs:204`, `test-support/grant-tests/tests/system.rs:328` |
| `registry.event_readers` | 5 | `registry.event_reader` | `crates/environment/src/environment_sql.rs:23`, `tests/journeys/tests/installed_digest.rs:131` |
| `registry.retired_project_envs` | 1 | Drop per R6 | `test-support/grant-tests/tests/system.rs:354`, `docs/plan/review-restructure.md:157` |
| `identity.principals` | 39 | `identity.principal` | `services/identity-service/src/password.rs:680`, `test-support/grant-tests/tests/system.rs:255` |
| `identity.org_memberships` | 18 | `identity.org_membership` | `services/identity-service/src/password.rs:509`, `test-support/grant-tests/tests/system.rs:679` |
| `identity.org_roles` | 10 | `identity.org_role` | `services/control/src/control_sql.rs:185`, `test-support/grant-tests/tests/system.rs:273` |
| `identity.project_roles` | 12 | `identity.project_role` | `crates/platform-install/src/sql.rs:27`, `test-support/grant-tests/tests/system.rs:282` |
| `identity.project_env_memberships` | 11 | `identity.project_env_membership` | `services/control/src/control_sql.rs:83`, `crates/identity/tests/identity_live.rs:554` |
| `identity.password_credentials` | 9 | `identity.password_credential` | `crates/identity/src/identity/password.rs:282`, `crates/identity/tests/password_live.rs:314` |
| `identity.password_logins` | 17 | `identity.password_login` | `crates/platform-install/src/identity_role.rs:210`, `crates/identity/tests/password_login_live.rs:204` |
| `identity.password_tokens` | 13 | `identity.password_token` | `crates/identity/src/identity/password.rs:268`, `services/identity-service/src/users.rs:319` |
| `identity.password_attempts` | 9 | `identity.password_attempt` | `crates/identity/src/identity/issuer.rs:88`, `crates/identity/tests/issuer_admission.rs:37` |
| `identity.renewal_credentials` | 10 | `identity.renewal_credential` | `crates/identity/src/identity/password_login.rs:209`, `crates/identity/tests/password_login_live.rs:228` |
| `identity.session_keys` | 11 | `identity.session_key` | `crates/identity/src/identity/session_keys.rs:309`, `crates/platform-install/src/identity_role.rs:177` |
| `identity.nats_keys` | 11 | `identity.nats_key` | `crates/identity/src/identity/nats_store.rs:254`, `crates/platform-install/src/identity_role.rs:179` |
| `catalog.packages` | 10 | `catalog.package` | `crates/environment/src/apply_package/package_version.rs:48`, `tests/journeys/tests/installed_digest.rs:223` |
| `catalog.workflows` | 18 | `catalog.workflow` | `crates/plugin/src/workflow.rs:294`, `crates/plugin/src/workflow/admission.rs:215` |
| `wamn_run.runs` | 5 | `wamn_run.run` | `crates/environment/src/environment_sql_effects.rs:155`, `tests/journeys/tests/workflows.rs:198` |
| `wamn_history.pruning_watermarks` | 4 | `wamn_history.pruning_watermark` | `crates/codegen/src/sql.rs:534`, `crates/environment/src/prune_record_history/plan.rs:18` |
| `platform.environment` | 13 | `platform.env` | `deploy/sql/app-schema.sql:247`, `crates/authentication/src/administration/sql.rs:28`, `crates/authentication/src/route_authentication.rs:31` |

## 4. Wire fields and literals

**F** The native HTTP contract is version `0.9.0`. It derives the URL, attachment ID and sealed operation ID from the same `model.action` entry, and uses the sealed ID as OpenAPI `operationId`. **I** All native HTTP rows below change that versioned contract. The HTTP route prefix stays `/wamn_control`. Sealed IDs use kebab case, so `environment.operation_status` currently yields `wamn-control:environment/operation-status@0.9.0`. `crates/routes/src/host_route.rs:52`, `crates/routes/src/host_route.rs:1742`, `crates/routes/src/host_route.rs:1815`, `crates/package/src/route_identity.rs:7`, `docs/architecture/execution.md:197`

| F old name | I replacement | F owners and contract |
|---|---|---|
| `environment.configure` | `env.configure` | Native operation, route, attachment and sealed ID. `crates/routes/src/host_route.rs:273`, `services/cli/src/environment_requests.rs:64` |
| `environment.delete` | `project_env.inactivate` | Native operation, route, attachment and sealed ID. R6 requires a semantic rename. **I** Route this operation explicitly through env Job handling because it leaves the `environment.` prefix used to select the coordinate and Job type. `crates/routes/src/host_route.rs:288`, `services/control/src/execution.rs:27`, `services/control/src/platform.rs:53` |
| `environment.operation_status` | `env.operation_status` | Native operation and derivatives. `crates/routes/src/host_route.rs:303`, `services/cli/src/operation_verbs.rs:66` |
| `environment.list` | `env.list` | Native operation and derivatives. Its operation name also enters the v1 cursor. `crates/routes/src/host_route.rs:1453`, `services/control/src/control_route.rs:434`, `crates/authentication/src/paging.rs:95` |
| `environment.status` | `env.status` | Native operation and derivatives. `crates/routes/src/host_route.rs:1463`, `apps/platform_ui/src/control-client.ts:46` |
| `environment.activate` | `env.activate` | Native operation and derivatives. `crates/routes/src/host_route.rs:1596`, `apps/platform_ui/src/control-client.ts:62` |
| `environment.inactivate` | `env.inactivate` | Native operation and derivatives. It targets env status, distinct from project-in-env deactivation. `crates/routes/src/host_route.rs:1619`, `services/control/src/control_route.rs:640` |
| `application.stop` | `app.stop` | Native operation and derivatives. `crates/routes/src/host_route.rs:540`, `crates/environment/src/environment/worker.rs:42` |
| `application.deploy` | `app.deploy` | Native operation and derivatives. `crates/routes/src/host_route.rs:555`, `services/control/src/platform_event.rs:11` |
| `application.rollback` | `app.rollback` | Native operation and derivatives. `crates/routes/src/host_route.rs:570`, `services/control/src/platform_event.rs:12` |
| JSON key and field path `application` | `app` | Eleven native schema members. Requests `FileDelete`, `FileExpose`, `FileGet`, `FilePut`, `NumberCounterSet`, `ApplicationDeploy`, `ApplicationRollback`, `ApplicationStop`, `EnvironmentConfigure`, `SchemaDiff`, plus `EnvironmentStatusResultWorkload`. Includes GET parameter names, `required` lists and `invalid_input.detail.field`. Runtime file decoders have three matching request fields, and the Job request and event projection read it too. `crates/routes/src/host_wire/types.rs:21`, `crates/routes/src/host_wire/types.rs:287`, `crates/routes/src/host_wire/types.rs:527`, `crates/blobstore/src/file_route.rs:148`, `services/control/src/control_route/operations.rs:136`, `services/control/src/platform_event.rs:43` |
| JSON key and field path `environment` | `env` | Native `member.list` query, bound cursor input and invalid-input field. Also `application_write_incomplete.detail.environment`, whose value is a project/env coordinate. `crates/routes/src/host_wire/types.rs:619`, `crates/routes/src/host_wire/refusals.rs:20`, `services/control/src/control_route.rs:1026`, `services/control/src/control_route.rs:1081` |
| JSON key `environments` | `envs` | Native env-list result collection, member result collection, and matching `ProjectMember` serialization. `crates/routes/src/host_wire/types.rs:477`, `crates/routes/src/host_wire/types.rs:639`, `crates/identity-protocol/src/records.rs:58` |
| `application_write_incomplete` | `app_write_incomplete` | Native declared refusal code, Serde `snake_case` tag and emitted literal. `crates/routes/src/host_wire/refusals.rs:8`, `crates/routes/src/host_wire/refusals.rs:19`, `services/control/src/control_route.rs:1079` |
| `environment_not_found` | `env_not_found` | Native declared refusal code. Existing `detail.field` is already `env`. `crates/routes/src/host_wire/refusals.rs:40`, `crates/routes/src/host_route.rs:280`, `services/control/src/control_route.rs:121` |
| `environment-inactive` | `env-inactive` | Fixed native ingress refusal literal, emitted before app dispatch. The emitting layer has a status constant and code literal. `crates/application/src/ingress.rs:55`, `crates/application/src/ingress.rs:263`, `crates/registry/src/types.rs:46` |
| Exposure value `application` | `app` | Native file request/result enum, persisted file catalog default and CHECK value, and Rust SQL insert. Changes native v0.9.0 and stored data interpretation. `crates/routes/src/host_wire/types.rs:36`, `crates/blobstore/src/file_route.rs:25`, `deploy/sql/app-schema.sql:371`, `crates/plugin/src/file_catalog.rs:80` |
| Cursor `order: "Environment"` | `order: "Env"` | Versioned token format `v: 1`. The env-list operation identity and `member.list` binding key also change. **I** Bump the cursor format for this pass. `crates/authentication/src/paging.rs:14`, `crates/authentication/src/paging.rs:95`, `crates/authentication/src/paging.rs:113`, `services/control/src/control_route.rs:1043` |
| `composed-entry.application` | `composed-entry.app` | **Versioned serving manifest format 11.** The parser checks the literal key before typed decoding and canonical digest computation. `services/edge/document/src/serving_manifest.rs:42`, `services/edge/document/src/serving_manifest.rs:348`, `services/edge/document/src/serving_manifest.rs:554`, `services/edge/document/src/serving_manifest.rs:1230` |
| `host-routes[]` value `application` | `app` | **Versioned serving manifest format 11.** Explicit Serde rename in `HostRouteSet`, carried in the manifest and inserted at release publication. `crates/routes/src/host_route.rs:105`, `services/edge/document/src/serving_manifest.rs:369`, `crates/release/src/publish_release.rs:452` |
| Connection field `owner: "environment"` | `owner: "env"` | **Versioned connection descriptor `1`.** Implicit kebab-case enum value, included in canonical component requirement bytes. `crates/connection-type/src/lib.rs:60`, `crates/connection-type/src/lib.rs:68`, `crates/connection-type/src/lib.rs:95`, `crates/connection-type/src/lib.rs:437` |
| `environment-selected-http-header` | `env-selected-http-header` | **Versioned connection descriptor `1`.** Implicit kebab-case value under `credential-injection`, selected by the built-in HTTP descriptor. `crates/connection-type/src/lib.rs:128`, `crates/connection-type/src/lib.rs:442`, `crates/connection-type/src/lib.rs:466` |
| Worker `type: "environment"` | `type: "env"` | Fixed internal Job JSON tag, paired producer/consumer. The same current spelling is the `wamn.dev/operation` annotation **value**, checked by Job observation. `services/control/src/platform.rs:71`, `services/control/src/platform.rs:265`, `crates/environment/src/environment/worker.rs:167`, `crates/kubernetes/src/job.rs:411` |
| Workload facts `application`, `environment` | `app`, `env` | Fixed JSON keys in `ApplicationConfig`, carried under `wamn.application`; separate from its native HTTP homonyms. Change the deploy writer, host readers and ingress guest reader together. `crates/application/src/application_config.rs:8`, `crates/application/src/application_config.rs:16`, `crates/environment/src/application_chart/plan.rs:73`, `apps/guests/ingress/src/guest/data_change.rs:55` |
| Env document/config `applications` | `apps` | Three fixed Serde fields across authored YAML `projects[].applications`, the compiled document and registry configuration. Tracked JSON Schema changes the property, its `required` entry and the `ApplicationSelection` definition name. `crates/environment/src/environment/document/file.rs:76`, `crates/environment/src/environment/document/plan.rs:20`, `crates/registry/src/configuration.rs:31`, `crates/environment/environment.schema.json:195` |
| Edge TOML `target.environment` | `target.env` | Fixed `deny_unknown_fields` config field. The example file supplies it. `services/edge/src/config.rs:113`, `services/edge/edge.example.toml:11` |
| `wamn.application` | `wamn.app` | Fixed WAMN key used both for workload component config and a Kubernetes annotation. `crates/application/src/application_config.rs:8`, `deploy/charts/application/templates/_helpers.tpl:7`, `crates/environment/src/status.rs:95` |
| `wamn.environment` | `wamn.env` | Fixed WAMN deployment and CDC annotation plus telemetry attribute. `deploy/charts/application/templates/_helpers.tpl:3`, `deploy/charts/project/templates/cdc-reader.yaml:22`, `crates/engine/src/invocation_trace.rs:62` |
| `applicationChart` | `appChart` | WAMN chart value and org provisioning ConfigMap field. Chart versions govern the delivered values. `deploy/charts/platform/values.schema.json:317`, `deploy/charts/platform/templates/org-provisioning.yaml:18`, `crates/platform-install/src/org.rs:89` |
| `environmentNats` | `envNats` | WAMN versioned env chart value. `crates/environment/src/environment/ensure.rs:221`, `deploy/charts/environment/templates/host.yaml:50`, `deploy/charts/environment/Chart.yaml:4` |
| App chart `environment`, `applications` | `env`, `apps` | Two values in the versioned WAMN app chart. The emitted wasmCloud `spec.template.spec.environment` belongs to the upstream CRD. `deploy/charts/application/values.yaml:18`, `deploy/charts/application/values.yaml:22`, `deploy/charts/application/Chart.yaml:5`, `deploy/charts/application/templates/workloads.yaml:22` |
| Chart names `application`, `wamn-environment`, helper prefix `application.` | `app`, `wamn-env`, `app.` | WAMN-owned chart identities, helper namespace and matching bundle paths. **I** Coordinate the published chart ref with the new names. `deploy/charts/application/Chart.yaml:2`, `deploy/charts/environment/Chart.yaml:2`, `deploy/charts/application/templates/_helpers.tpl:22`, `crates/platform-install/src/deployment.rs:65`, `xtask/src/platform.rs:408` |
| `environment_apply` | `env_apply` | Fixed persisted `registry.platform_event_type` code and Rust Serde enum tag. `deploy/sql/system-schema.sql:624`, `crates/platform-install/src/platform_event.rs:12`, `crates/platform-install/src/platform_event.rs:28` |
| `environment_delete` | `project_env_inactivate` | Fixed persisted event code and Rust Serde enum tag. R6 explicitly ties this to the request and command rename. `deploy/sql/system-schema.sql:625`, `crates/platform-install/src/platform_event.rs:29`, `docs/plan/review-restructure.md:159` |
| NATS key kinds `environment`, `environment-signing` | `env`, `env-signing` | Fixed WAMN DB discriminants in constraints, lookup, issuance and retirement. `deploy/sql/system-schema.sql:518`, `crates/identity/src/identity/nats_store.rs:146`, `crates/identity/src/identity/nats_store.rs:218` |
| JetStream durable prefix `application_` | `app_` | Fixed WAMN consumer-name literal. The entity filter comes from a separate builder. **I** The new prefix creates a different durable identity. `crates/registry/src/resource_names.rs:39`, `crates/broker/src/events.rs:59`, `crates/broker/src/events.rs:87` |
| Generated role prefix `application-` | `app-` | Two formatting sites create and look up the same persisted role name. `crates/environment/src/environment_sql_effects.rs:322`, `crates/environment/src/environment_sql_effects.rs:356` |
| Project setting `target_type = 'application'` | `target_type = 'app'` | Fixed stored discriminant and guest numbering lookup. `deploy/sql/app-schema.sql:446`, `apps/crates/guest/src/postgres_statements/numbering.rs:12` |
| Advisory lock prefix `wamn-application/` | `wamn-app/` | Fixed hash input to app lifecycle locking. **I** Old and new writers would acquire different app locks. `crates/environment/src/registry/lifecycle_lock.rs:76`, `crates/environment/src/registry/lifecycle_lock.rs:86` |
| `project-environment` | `project-env` | Fixed workload identity diagnostic value, formatted in `WorkloadRoleIdentityError`. `crates/identity-protocol/src/workload_role.rs:130`, `crates/identity-protocol/src/workload_role.rs:193` |
| `environment_target_missing` | `env_target_missing` | Fixed binding error code (unsure: external delivery of this particular code was not verified). `crates/environment/src/environment/bindings.rs:67`, `crates/environment/src/environment/bindings/plan.rs:3` |
| `environment_target_changed` | `env_target_changed` | Fixed PostgreSQL exception **message** owned by WAMN. Existing SQLSTATE `55000` is PostgreSQL. `deploy/sql/system-schema.sql:941`, `deploy/sql/system-schema.sql:944` |
| `--application` | `--app` | Two named Clap flags, deploy and env apply. Rollback and status use positional `application` arguments whose Rust names and help metavariables change. `services/cli/src/application_verbs.rs:22`, `services/cli/src/application_verbs.rs:32`, `services/cli/src/environment_requests.rs:16` |
| `wamn env delete` | `wamn env inactivate-project` | CLI command proposed for the same operation as `project_env.inactivate`. `services/cli/src/environment_requests.rs:9`, `services/cli/src/environment_requests.rs:67`, `docs/plan/review-restructure.md:159` |
| WIT `world application` | `world app` | One authored declaration, in unversioned `wamn:modbus-example`; update the `wit_bindgen::generate!` world selector. The long-form hits in the shared WAMN WIT packages are doc comments. `apps/modbus_reader/component/wit/authored.wit:2`, `apps/modbus_reader/component/src/lib.rs:19`, `wit/wamn-connection/package.wit:3`, `wit/wamn-operation-0.1/package.wit:27` |
| `/api/<application>/flow`, `/api/<application>/files`, `wamn-workflow:{application}/{id}` | `/api/<app>/flow`, `/api/<app>/files`, `wamn-workflow:{app}/{id}` in source/docs | **F** Each placeholder is interpolated from the app slug. **I** Shortening the placeholder leaves the actual URL and permission string byte-for-byte identical. `crates/routes/src/lib.rs:25`, `crates/routes/src/lib.rs:31`, `services/host/src/host/platform_http.rs:130` |

**F** On the inactive-connection check, the fact reader maps `active = false` to `Disabled`. Bound HTTP rejects that fact as `Unbound`, and file binding also requires `Active`. The delete implementation at this commit still issues `DELETE FROM platform.connection`. These are source-path checks, including the predicate that consumes the observed fact. `crates/connection/src/connection_credentials.rs:357`, `crates/plugin/src/bound_http.rs:131`, `crates/plugin/src/file_connections.rs:42`, `crates/connection/src/project_connections/administration.rs:385`

## 5. Generated clients and schemas

- **F** `cargo xtask platform-ui-openapi` calls `HostRouteSet::openapi()` and writes `apps/target/wamn/platform_ui/{application,control}.openapi.json`. Platform UI `pnpm run generate:clients` invokes that step, then `@hey-api/openapi-ts` 0.99.0. Output is `apps/platform_ui/src/generated/wamn_control` for app-host routes and `apps/platform_ui/src/generated/wamn_control_org` for control routes. Both output trees are ignored. **I** Regenerate both. The app-host client changes for app fields and file exposure, and the control client changes for the app/env operations and payloads. Rename the app document to `app.openapi.json` and update the config and imports if the pass applies R6 to generated filenames. `xtask/src/lib.rs:205`, `xtask/src/platform.rs:38`, `apps/platform_ui/package.json:10`, `apps/platform_ui/openapi-ts.config.ts:5`, `.gitignore:46`
- **F** Handwritten UI imports reference generated symbols such as `wamnControlApplicationDeploy_090`, `wamnControlEnvironmentActivate_090` and `wamnControlEnvironmentList_090`. Field paths depend on `environments` and the old routes. **I** Regeneration changes these identifiers and generated request and result types. Update the wrappers and tests alongside the wire. `apps/platform_ui/src/control-client.ts:35`, `apps/platform_ui/src/control-client.ts:51`, `apps/platform_ui/src/control-client.ts:62`, `apps/platform_ui/src/application-client.ts:1`
- **F** Application package builds emit `generated/openapi.json` from compiled routes and operation schemas. The build command is `wamn build`; R3 names `@hey-api/openapi-ts` as the consumer of the published OpenAPI. **I** Regenerate at package build/publication for affected schemas. Compile the Modbus guest with the new WIT world selector. `crates/codegen/src/generate/publication.rs:151`, `crates/codegen/src/generate/publication.rs:394`, `crates/codegen/src/build.rs:578`, `services/cli/src/bin/wamn.rs:141`, `docs/plan/review-restructure.md:98`, `apps/modbus_reader/component/src/lib.rs:19`
- **F** `identity-client` is a handwritten HTTPS client with `users.rs` and `nats.rs`; its NATS types already serialize `org` and `env`. **I** No generated identity-client output changes. The separate identity `ProjectMember` record is covered by the native `environments` → `envs` change. `crates/identity-client/src/lib.rs:1`, `crates/identity-client/src/lib.rs:44`, `crates/identity-client/src/nats.rs:40`, `crates/identity-protocol/src/nats.rs:24`, `crates/identity-protocol/src/records.rs:58`
- **F** `cargo xtask environment-schema` emits the tracked `crates/environment/environment.schema.json`; the command uses `environment::document::schema_json()`. **I** Regenerate the `EnvFile`, `AppSelection` and `apps` schema together. Rename the task and path to `env-schema` and `crates/env/env.schema.json` as part of the naming pass. `xtask/src/lib.rs:119`, `xtask/src/repo_policy.rs:28`, `crates/environment/src/environment/document/file.rs:239`, `docs/operations/building.md:226`
- **F** Receiving still imports the removed emitter’s `./generated/wamn_receiving` files, and the sync benchmark describes the old `client-ts` output. WFE’s example `emit.rs` also imports the old emitter, while the platform example documents capturing a pinned client snapshot and generating result schemas from that snapshot. **I** These are separate legacy consumers (unsure: whether the owner intends to refresh the pinned WFE snapshot in R6). The snapshot instructions require existing generated inputs. `apps/wamn_receiving/web/src/forms.tsx:8`, `web/sync-bench/README.md:69`, `web/wfe/examples/wamn/emit.rs:8`, `web/wfe/examples/platform/README.md:75`, `web/wfe/examples/platform/README.md:175`

## 6. Names that stay and exceptions

**F** These already use the standard forms.

- `EnvName`, `ParseEnvNameError`, `env_name`; `OrgId`, `ParseOrgIdError`, `org_id`; `ProjectEnvKey`, `project_env_key`. `crates/registry/src/lib.rs:24`, `crates/registry/src/lib.rs:29`, `crates/registry/src/project_env_key.rs:9`
- `org`, `env` coordinate columns and serialized members, `--env`, `wamn env apply`, `org-admin`, `org_admin`, `org_create`. `deploy/sql/system-schema.sql:601`, `crates/routes/src/host_wire.rs:82`, `crates/identity-protocol/src/nats.rs:24`, `services/cli/src/bin/wamn.rs:169`, `crates/routes/src/host_wire/types.rs:634`, `crates/platform-install/src/platform_event.rs:27`
- `WorkloadRoleFamily::App`, `APP_LOGIN`, `wamn_app_login`, service identity `wamn-app-{key}--{package}`, and the `wamn-env/` advisory lock input. `crates/identity-protocol/src/workload_role.rs:28`, `crates/role/src/lib.rs:16`, `crates/registry/src/resource_names.rs:10`, `crates/postgres/src/lib.rs:54`
- WAMN NATS subjects `evt.{org}.{env}.>` and `evt.{org}.{env}.{project}.…`, and the account name `ENV_{org}_{env}`. The corresponding Rust helper names still shorten. `crates/registry/src/name.rs:37`, `crates/registry/src/name.rs:236`, `crates/registry/src/name.rs:241`
- `platform."user"` retains its quotes. `docs/plan/review-restructure.md:146`, `deploy/sql/app-schema.sql:93`

### Not renamed

- **F** wasmCloud `WorkloadDeployment.spec.template.spec.environment`, `with_environment`, `copy_environment`, and `wasmcloud.dev/hostgroup` are upstream terms. The host’s `--environment` is a separate WAMN-declared argument for the operator-supplied pod namespace (unsure: the supplying operator’s argument contract was not verified in this checkout). Preserve its external spelling pending that check. `deploy/charts/application/templates/workloads.yaml:3`, `deploy/charts/application/templates/workloads.yaml:22`, `services/host/src/host.rs:94`, `services/host/src/host.rs:613`, `services/host/src/host.rs:653`, `crates/registry/src/name.rs:360`
- **F** Kubernetes `apps/v1`, container `env`, and `app.kubernetes.io/*` are upstream names. Helm `.Release`, `.Chart`, and chart metadata keys are upstream as well. The WAMN helper `kubernetes::job::environment` populates process environment variables, outside the WAMN env concept. `xtask/src/journey_cluster.rs:8`, `crates/kubernetes/src/job.rs:27`, `crates/kubernetes/src/job.rs:44`, `crates/kubernetes/src/deployment_components.rs:30`, `deploy/charts/application/templates/workloads.yaml:7`
- **F** PostgreSQL `application_name` is a connection/session setting and a `pg_stat_activity` column. OCI and HTTP `application/*` strings are media types, including WAMN vendor subtypes, and OAuth’s `application/x-www-form-urlencoded` uses the standard MIME top-level type. `crates/environment/tests/environment_lock_live.rs:126`, `crates/environment/tests/history_retention_live.rs:298`, `crates/oci/src/package_artifact/mod.rs:44`, `crates/routes/src/host_route.rs:1869`, `crates/http/src/jwt/tests.rs:92`
- **F** `wasi:cli/environment`, vendored WASI random wording and vendored wasmCloud NATS wording belong to upstream interfaces. `$SYS.REQ.CLAIMS.UPDATE`, `$SYS.REQ.ACCOUNT.*.CLAIMS.LOOKUP`, and `$SYS.REQ.CLAIMS.DELETE` are upstream NATS resolver subjects. `crates/component-policy/src/lib.rs:292`, `apps/guests/ingress/wit/deps/wasi-random/package.wit:70`, `apps/guests/ingress/wit/deps/wasmcloud-nats/world.wit:618`, `crates/identity/src/nats.rs:115`
- **F** `environment_names`, `is_secret_environment`, `environment_marker`, `help_hides_environment_values`, `validate_pooling_capacity_environment`, `capacity_and_unknown_pooling_environment_knobs_fail_closed`, and `residency_and_decommit_environment_knobs_are_permitted` refer to process environment variables. Each is in one source file as a helper or test. The shared name `environment_names` still changes for the WAMN module. `services/cli_help_tests.rs:8`, `services/cli_help_tests.rs:19`, `services/cli_help_tests.rs:45`, `services/cli_help_tests.rs:72`, `crates/engine/src/engine.rs:259`, `crates/engine/src/engine.rs:1074`, `crates/engine/src/engine.rs:1081`
- **F** `x-application-response` has two hits in one forwarding test, where it is a sample header. `Application Default Credentials` is Google terminology. `guest-sql` is explicitly frozen as an operator-facing component label even though the family enum uses `App`. `crates/engine/src/expected_router.rs:193`, `crates/engine/src/expected_router.rs:226`, `crates/storage/src/publication.rs:26`, `crates/identity-protocol/src/workload_role.rs:106`
- **F** `environment.k` and `environment.yaml` are supplied filenames in examples and tests. `generated/components/application.wasm` is a watch fixture path. **I** These may be shortened for repository naming consistency (unsure: sample paths have no required public spelling). `services/cli/src/bin/wamn.rs:170`, `docs/operations/deployment.md:314`, `crates/build/src/watch.rs:839`
- **F** `Application` and `Environment` also appear as Schemars display titles. R3 distinguishes display labels from field identifiers. The in-tree `wamn.k` hits for the long forms are comments. `crates/routes/src/host_wire/types.rs:286`, `crates/routes/src/host_wire/types.rs:586`, `docs/plan/review-restructure.md:105`, `apps/platform_fixture/wamn.k:1`
