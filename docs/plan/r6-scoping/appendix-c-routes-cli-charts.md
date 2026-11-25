# R6 scope at `6b4db53e1`

F marks observed names and uses. I marks proposed names and inferred effects. Counts are matching source lines unless identified as declarations, generated forms, or doc files. **Public wire** marks HTTP, browser URL, and generated API changes. **Cluster wire** marks installed chart, workload, and Kubernetes contracts. R6 calls for one rename pass after Epic 3 and before Epic 4. `docs/plan/review-restructure.md:140` `docs/plan/review-restructure.md:148`

## HTTP routes and operation codes

F. The route table derives the HTTP path, sealed operation ID, stored reference, attachment ID, and OpenAPI `operationId` from the same dotted operation name. HTTP paths retain underscores. Sealed IDs and attachment IDs use hyphens. The web proxy also accepts `/api/wamn_control/...` and strips `/api` on that forwarding path. `crates/routes/src/host_route.rs:1741` `crates/package/src/route_identity.rs:7` `crates/routes/src/route_identity.rs:4` `services/web/src/lib.rs:296`

| F old | I new | F count and evidence | Impact |
|---|---|---|---|
| `environment.configure` · `POST /wamn_control/environment/configure` | `env.configure` · `POST /wamn_control/env/configure` | 6 operation hits. `crates/routes/src/host_route.rs:273` `services/cli/src/environment_requests.rs:62` `crates/environment/src/environment/worker.rs:232` | **Public wire** |
| `environment.delete` · `POST /wamn_control/environment/delete` | `env_project.deactivate` · `POST /wamn_control/env_project/deactivate` (unsure: R6 leaves the spelling open) | 5 operation hits. `crates/routes/src/host_route.rs:288` `services/control/src/execution.rs:27` `crates/environment/src/environment/worker.rs:227` | **Public wire** |
| `environment.operation_status` · `GET /wamn_control/environment/operation_status` | `env.operation_status` · `GET /wamn_control/env/operation_status` | 3 operation hits. `crates/routes/src/host_route.rs:303` `services/cli/src/operation_verbs.rs:66` `services/control/src/execution.rs:27` | **Public wire** |
| `environment.list` · `GET /wamn_control/environment/list` | `env.list` · `GET /wamn_control/env/list` | 15 operation hits. `crates/routes/src/host_route.rs:1453` `apps/platform_ui/src/control-client.ts:64` `services/control/src/host_route/tests.rs:142` | **Public wire** |
| `environment.status` · `GET /wamn_control/environment/status` | `env.status` · `GET /wamn_control/env/status` | 13 operation hits. `crates/routes/src/host_route.rs:1463` `services/cli/src/application_verbs.rs:116` `apps/platform_ui/src/control-client.ts:65` | **Public wire** |
| `environment.activate` · `POST /wamn_control/environment/activate` | `env.activate` · `POST /wamn_control/env/activate` | 6 operation hits. `crates/routes/src/host_route.rs:1596` `apps/platform_ui/src/control-client.ts:62` `apps/platform_ui/src/environment.tsx:316` | **Public wire** |
| `environment.inactivate` · `POST /wamn_control/environment/inactivate` | `env.inactivate` · `POST /wamn_control/env/inactivate` | 6 operation hits. `crates/routes/src/host_route.rs:1619` `apps/platform_ui/src/control-client.ts:63` `apps/platform_ui/src/environment.tsx:338` | **Public wire** |
| `application.stop` · `POST /wamn_control/application/stop` | `app.stop` · `POST /wamn_control/app/stop` | 11 operation hits. `crates/routes/src/host_route.rs:540` `crates/environment/src/environment/worker.rs:45` `apps/platform_ui/src/control-client.ts:52` | **Public wire** |
| `application.deploy` · `POST /wamn_control/application/deploy` | `app.deploy` · `POST /wamn_control/app/deploy` | 8 operation hits. `crates/routes/src/host_route.rs:555` `services/cli/src/application_verbs.rs:82` `apps/platform_ui/src/control-client.ts:10` | **Public wire** |
| `application.rollback` · `POST /wamn_control/application/rollback` | `app.rollback` · `POST /wamn_control/app/rollback` | 8 operation hits. `crates/routes/src/host_route.rs:570` `services/cli/src/application_verbs.rs:94` `apps/platform_ui/src/control-client.ts:15` | **Public wire** |
| `connection.delete` · `POST /wamn_control/connection/delete` | `connection.deactivate` · `POST /wamn_control/connection/deactivate` (unsure: `connection.disable` already exists, and R6 specifies credential removal for the current delete path) | 8 operation hits. `crates/routes/src/host_route.rs:869` `crates/routes/src/host_route.rs:921` `crates/connection/src/administration.rs:534` | **Public wire** |
| `role.delete` · `POST /wamn_control/role/delete` | `role.deactivate` · `POST /wamn_control/role/deactivate` (unsure: R6 specifies behavior and leaves this spelling open) | 3 exact operation hits. `crates/routes/src/host_route.rs:1049` `services/cli/src/role_verbs.rs:84` `apps/platform_ui/src/application-client.ts:33` | **Public wire** |
| `/api/<application>/files` | `/api/<app>/files` | 1 registration, including child paths. `services/host/src/host/platform_http.rs:130` `crates/plugin/src/blobstore/file_route.rs:6` `services/web/src/lib.rs:296` | Placeholder only. The segment is the app name on the wire. |
| `/api/<application>/files/<id>` | `/api/<app>/files/<id>` | 1 child path form. `crates/plugin/src/blobstore/file_route.rs:8` `services/host/src/host/platform_http.rs:130` | Placeholder only |
| `/api/<application>/flow` | `/api/<app>/flow` | 1 prefix constructor. `crates/routes/src/lib.rs:25` `services/host/src/host/platform_http.rs:131` `services/web/src/lib.rs:296` | Placeholder only |
| `wamn-workflow:<application>/<workflow_id>` | `wamn-workflow:<app>/<workflow_id>` | 1 permission constructor. `crates/routes/src/lib.rs:30` `docs/architecture/execution.md:515` | Placeholder only |
| `projects/:project/environments` | `projects/:project/envs` (unsure: `envs` is a valid env slug and the UI also has `projects/:project/:env`) | 6 route, navigation, and test hits. `apps/platform_ui/src/projects.tsx:448` `apps/platform_ui/src/control-navigation.tsx:212` `apps/platform_ui/test/navigation.test.tsx:162` | **Public wire**, browser URL |
| `environment_apply` | `env_apply` | 4 source hits. `crates/platform-install/src/platform_event.rs:28` `deploy/sql/system-schema.sql:624` `services/control/src/platform_event/tests.rs:83` | Persisted event code |
| `environment_delete` | `env_project_deactivate` (unsure: follows the selected operation name) | 2 literal hits. `crates/platform-install/src/platform_event.rs:29` `deploy/sql/system-schema.sql:625` `services/control/src/platform_event.rs:10` | Persisted event code |
| `environment_not_found` | `env_not_found` | 31 source hits. `crates/routes/src/host_route.rs:280` `services/control/src/control_route.rs:121` `apps/platform_ui/src/environment.tsx:295` | **Public wire** |
| `application_write_incomplete` | `app_write_incomplete` | 26 source hits. `crates/routes/src/host_route.rs:653` `crates/routes/src/host_wire/refusals.rs:19` `services/control/src/control_route.rs:1079` | **Public wire** |
| `environment-inactive` | `env-inactive` | 4 source hits. `crates/application/src/ingress.rs:56` `crates/registry/src/types.rs:46` `deploy/sql/app-schema.sql:243` | **Public wire**, ingress refusal |
| `environment_target_missing` | `env_target_missing` | 1 literal. `crates/environment/src/environment/bindings.rs:67` | Worker refusal (unsure: whether callers treat the Job diagnostic as public wire) |

F. Identity dispatch uses `/users`, `/invitations`, `/users/disable`, the three NATS paths, and the password routes. The NATS request fields already use `org` and `env`. The live docs explicitly say `/password/environments` was removed. `services/identity-service/src/lib.rs:233` `services/identity-service/src/lib.rs:270` `crates/identity-protocol/src/nats.rs:7` `crates/identity-protocol/src/nats.rs:24` `docs/architecture/execution.md:1039`

F. Control classifies worker Jobs with `starts_with("environment.")`. It stamps the worker kind into the `wamn.dev/operation` annotation and uses the same kind when polling. I. The proposed `env_project.deactivate` needs explicit dispatch and must remain observable through the env Job status route. `services/control/src/platform.rs:53` `services/control/src/platform.rs:70` `services/control/src/platform.rs:85` `services/control/src/platform.rs:104`

F. The bound HTTP paths require an active connection fact, as do bound integration and file calls. The reader maps the persisted `active` boolean to `Active` or `Disabled`. `crates/plugin/src/bound_http.rs:135` `crates/plugin/src/bound_http.rs:210` `crates/plugin/src/integrations.rs:170` `crates/plugin/src/file_connections.rs:42` `crates/connection/src/connection_credentials.rs:361`

## Route fields, schema names, and installed payloads

| F old | I new | F count and evidence | Impact |
|---|---|---|---|
| `application` HTTP field | `app` | 11 route schema fields, including file, deploy, configure, workload status, and counter inputs or results. `crates/routes/src/host_wire/types.rs:21` `crates/routes/src/host_wire/types.rs:287` `crates/routes/src/host_wire/types.rs:527` | **Public wire** |
| `application` in `invalid_input.detail.field` | `app` | 3 explicit sites. `services/control/src/control_route/operations.rs:136` `services/control/src/control_route/operations.rs:141` `services/host/src/host/application_route.rs:410` | **Public wire** |
| `environment` HTTP field | `env` | 2 field declarations, the `member.list` query and incomplete-write detail. The control cursor and refusal field name also carry the old spelling. `crates/routes/src/host_wire/types.rs:619` `crates/routes/src/host_wire/refusals.rs:20` `services/control/src/control_route.rs:1026` | **Public wire** |
| `environments` HTTP field | `envs` | 2 response schema fields. `crates/routes/src/host_wire/types.rs:477` `crates/routes/src/host_wire/types.rs:639` `services/control/src/control_route.rs:456` | **Public wire** |
| `exposure = "application"` | `exposure = "app"` | 1 route enum declaration and corresponding blobstore exposure. `crates/routes/src/host_wire/types.rs:33` `crates/blobstore/src/file_route.rs:15` `docs/architecture/data-access.md:1061` | **Public wire**, stored exposure |
| `deleted` on `role.delete` result | `deactivated` (unsure: result field naming is unspecified) | 2 contract sites. `crates/routes/src/host_wire/types.rs:155` `services/host/src/host/application_route.rs:198` | **Public wire** |
| `HostRouteSet::Application` / `host-routes: ["application"]` | `HostRouteSet::App` / `host-routes: ["app"]` | 1 serialized enum declaration. `crates/routes/src/host_route.rs:101` `services/edge/document/src/serving_manifest.rs:369` `crates/release/src/publish_release.rs:452` | Edge artifact wire |
| `composed-entry.application` | `composed-entry.app` | 1 field, with explicit validation. `services/edge/document/src/serving_manifest.rs:348` `services/edge/document/src/serving_manifest.rs:558` | Edge artifact wire |
| `Task::Environment` / `"type":"environment"` | `Task::Env` / `"type":"env"` | 1 reader and 1 producer. `crates/environment/src/environment/worker.rs:167` `services/control/src/platform.rs:265` | Worker wire |
| `application` worker request field | `app` | 2 worker inputs. `crates/environment/src/environment/worker.rs:19` `crates/environment/src/environment/worker.rs:178` `services/control/src/platform.rs:265` | Worker wire |
| `wamn.dev/operation: environment` | `wamn.dev/operation: env` | 1 value source, used to stamp and observe Jobs. `services/control/src/platform.rs:70` `services/control/src/platform.rs:85` `services/control/src/platform.rs:104` | **Cluster wire**, Kubernetes annotation **value** |
| `project-environment` in workload role identity diagnostics | `project-env` (unsure: diagnostic terminology may be frozen) | 1 literal definition. `crates/identity-protocol/src/workload_role.rs:124` `crates/identity-protocol/src/workload_role.rs:197` | Diagnostic |
| `ApplicationName` | `AppName` | 1 type declaration. `crates/registry/src/environment_application.rs:22` `crates/routes/src/lib.rs:25` | Rust API |
| `ApplicationDeployRequest` | `AppDeployRequest` | 1 declaration. `crates/routes/src/host_wire/types.rs:281` `crates/routes/src/host_route.rs:555` | Generated API |
| `ApplicationDeployResult` | `AppDeployResult` | 1 declaration. `crates/routes/src/host_wire/types.rs:292` `crates/routes/src/host_route.rs:555` | Generated API |
| `ApplicationRollbackRequest` | `AppRollbackRequest` | 1 declaration. `crates/routes/src/host_wire/types.rs:298` `crates/routes/src/host_route.rs:570` | Generated API |
| `ApplicationStopRequest` | `AppStopRequest` | 1 declaration. `crates/routes/src/host_wire/types.rs:309` `crates/routes/src/host_route.rs:540` | Generated API |
| `ApplicationUserListResult` | `AppUserListResult` | 1 declaration. `crates/routes/src/host_wire/types.rs:186` `crates/routes/src/host_route.rs:1010` | Generated API |
| `ApplicationUserListResultUser` | `AppUserListResultUser` | 1 declaration. `crates/routes/src/host_wire/types.rs:168` `crates/routes/src/host_wire/types.rs:187` | Generated API |
| `EnvironmentConfigureRequest` | `EnvConfigureRequest` | 1 declaration. `crates/routes/src/host_wire/types.rs:428` `crates/routes/src/host_route.rs:273` | Generated API |
| `EnvironmentDeleteRequest` | `EnvProjectDeactivateRequest` (unsure: follows the selected operation name) | 1 declaration. `crates/routes/src/host_wire/types.rs:440` `crates/routes/src/host_route.rs:288` | Generated API |
| `EnvironmentListRequest` | `EnvListRequest` | 1 declaration. `crates/routes/src/host_wire/types.rs:448` `crates/routes/src/host_route.rs:1453` | Generated API |
| `EnvironmentListResultEnvironmentState` | `EnvListResultEnvState` | 1 declaration. `crates/routes/src/host_wire/types.rs:459` `crates/routes/src/host_wire/types.rs:471` | Generated API |
| `EnvironmentListResultEnvironment` | `EnvListResultEnv` | 1 declaration. `crates/routes/src/host_wire/types.rs:468` `crates/routes/src/host_wire/types.rs:477` | Generated API |
| `EnvironmentListResult` | `EnvListResult` | 1 declaration. `crates/routes/src/host_wire/types.rs:476` `crates/routes/src/host_route.rs:1453` | Generated API |
| `EnvironmentOperationStatusRequest` | `EnvOperationStatusRequest` | 1 declaration. `crates/routes/src/host_wire/types.rs:486` `crates/routes/src/host_route.rs:303` | Generated API |
| `EnvironmentOperationStatusResult` | `EnvOperationStatusResult` | 1 declaration. `crates/routes/src/host_wire/types.rs:494` `crates/routes/src/host_route.rs:303` | Generated API |
| `EnvironmentRequest` | `EnvRequest` | 1 declaration. `crates/routes/src/host_wire.rs:82` `crates/routes/src/host_wire/types.rs:820` | Generated API |
| `EnvironmentStatus` | `EnvStatus` | 1 declaration. `crates/routes/src/host_wire.rs:106` `crates/routes/src/host_route.rs:1596` | Generated API |
| `EnvironmentStatusResultRollout` | `EnvStatusResultRollout` | 1 declaration. `crates/routes/src/host_wire/types.rs:504` `crates/routes/src/host_wire/types.rs:541` | Generated API |
| `EnvironmentStatusResultWorkloadCondition` | `EnvStatusResultWorkloadCondition` | 1 declaration. `crates/routes/src/host_wire/types.rs:513` `crates/routes/src/host_wire/types.rs:534` | Generated API |
| `EnvironmentStatusResultWorkload` | `EnvStatusResultWorkload` | 1 declaration. `crates/routes/src/host_wire/types.rs:522` `crates/routes/src/host_wire/types.rs:543` | Generated API |
| `EnvironmentStatusResult` | `EnvStatusResult` | 1 declaration. `crates/routes/src/host_wire/types.rs:539` `crates/routes/src/host_route.rs:1463` | Generated API |
| `ConnectionDeleteResult` | `ConnectionDeactivateResult` (unsure: follows the selected operation name) | 1 declaration. `crates/routes/src/host_wire/types.rs:348` `crates/routes/src/host_route.rs:921` | Generated API |
| `RoleDeleteResult` | `RoleDeactivateResult` (unsure: follows the selected operation name) | 1 declaration. `crates/routes/src/host_wire/types.rs:155` `crates/routes/src/host_route.rs:1049` | Generated API |
| Schema title `Application` | `App` (unsure: display title, with possible generated name effects) | 5 declarations. `crates/routes/src/host_wire/types.rs:286` `crates/routes/src/host_wire/types.rs:526` `crates/routes/src/host_wire/types.rs:752` | Generated API |
| Schema title `Environment` | `Env` (unsure: display title on the already-short `env` field) | 1 declaration. `crates/routes/src/host_wire/types.rs:586` | Generated API |
| `HostHandler::ApplicationUserList` | `HostHandler::AppUserList` | 1 variant. `crates/routes/src/host_route.rs:126` `crates/routes/src/host_route.rs:1010` | Rust |
| `HostHandler::EnvironmentList` | `HostHandler::EnvList` | 1 variant. `crates/routes/src/host_route.rs:167` `crates/routes/src/host_route.rs:1453` | Rust |
| `HostHandler::EnvironmentStatus` | `HostHandler::EnvStatus` | 1 variant. `crates/routes/src/host_route.rs:171` `crates/routes/src/host_route.rs:1463` | Rust |
| `HostHandler::EnvironmentActivate` | `HostHandler::EnvActivate` | 1 variant. `crates/routes/src/host_route.rs:187` `services/control/src/control_route.rs:642` | Rust |
| `HostHandler::EnvironmentInactivate` | `HostHandler::EnvInactivate` | 1 variant. `crates/routes/src/host_route.rs:189` `crates/routes/src/host_route.rs:1619` | Rust |
| `ApplicationWriteIncomplete` | `AppWriteIncomplete` | 1 refusal variant. `crates/routes/src/host_wire/refusals.rs:19` `services/control/src/control_route.rs:1079` | **Public wire** via serialization |
| `EnvironmentNotFound` | `EnvNotFound` | 1 route refusal variant. `crates/routes/src/host_wire/refusals.rs:40` `services/control/src/control_route.rs:121` | **Public wire** via serialization |
| `application_route` | `app_route` | 1 host module. `services/host/src/host.rs:25` `services/host/src/host/host_route.rs:22` | Rust |
| `ApplicationRoutes` | `AppRoutes` | 1 host route type. `services/host/src/host/application_route.rs:40` `services/host/src/host/host_route.rs:22` | Rust |
| `ApplicationFiles` | `AppFiles` | 1 host route type. `services/host/src/host/files.rs:26` `services/host/src/host/files.rs:67` | Rust |
| `environment_status` | `env_status` | 1 control module. `services/control/src/lib.rs:5` `services/control/src/environment_status.rs:22` | Rust |

## CLI and service inputs

| F old | I new | F count and evidence | Impact |
|---|---|---|---|
| `--application` | `--app` | 2 flag declarations, `wamn deploy` and `wamn env apply`. `services/cli/src/application_verbs.rs:21` `services/cli/src/environment_requests.rs:14` `services/cli/src/bin/wamn.rs:167` | CLI |
| Positional Clap ID `application` | `app` | 3 declarations, rollback, status, and the shared schema args. `services/cli/src/application_verbs.rs:33` `services/cli/src/application_verbs.rs:45` `services/cli/src/schema_verbs.rs:46` | CLI help and argument ID |
| `wamn env delete` | `wamn env deactivate-project` (unsure: command spelling open, and the existing `--data`/`--confirm` behavior needs to agree with R6 teardown) | 1 subcommand. `services/cli/src/environment_requests.rs:9` `services/cli/src/environment_requests.rs:22` `docs/plan/review-restructure.md:155` | CLI |
| `wamn connection delete` | `wamn connection deactivate` (unsure: existing `disable` is a separate verb) | 1 subcommand. `services/cli/src/component_verbs.rs:36` `services/cli/src/component_verbs.rs:170` | CLI |
| `wamn delete-role` | `wamn deactivate-role` (unsure: follows the selected route name) | 1 subcommand. `services/cli/src/bin/wamn.rs:33` `services/cli/src/role_verbs.rs:79` | CLI |
| `wamn-host serve --environment` | Preserve pending operator contract check (unsure: this flag carries the operator-supplied pod namespace and is checked against `settings.org`; `--env` would name the wrong coordinate) | 1 declaration and 1 use. `services/host/src/host.rs:94` `services/host/src/host.rs:472` `deploy/charts/environment/templates/host.yaml:78` | CLI and operator integration |
| `target.environment` | `target.env` | 1 edge TOML field. The override table supplies `WAMN_ORG` and `WAMN_PROJECT`; this field has no override. `services/edge/src/config.rs:5` `services/edge/src/config.rs:117` `services/edge/edge.example.toml:11` | Operator config |
| `application` in `wamn publish` stdout JSON | `app` | 1 output site. `services/cli/src/schema_verbs.rs:121` `services/cli/src/schema_verbs.rs:125` | CLI JSON |
| `application_verbs` | `app_verbs` | 1 module. `services/cli/src/lib.rs:3` `services/cli/src/bin/wamn.rs:48` | Rust |
| `environment_requests` | `env_requests` | 1 module. `services/cli/src/lib.rs:21` `services/cli/src/bin/wamn.rs:24` | Rust |
| `EnvironmentArgs` | `AppHostArgs` (unsure: `EnvArgs` is the mechanical spelling, but the type carries an app host URL and token) | 1 type. `services/cli/src/role_verbs.rs:8` `services/cli/src/role_verbs.rs:21` | Rust and flattened args |
| `ControlHost::environment` | `ControlHost::env` | 1 method. `services/cli/src/operation_verbs.rs:58` `services/cli/src/environment_requests.rs:91` | Rust |
| `cargo xtask environment-schema` | `cargo xtask env-schema` | 1 auxiliary command. `xtask/src/lib.rs:119` `xtask/src/lib.rs:237` `docs/operations/building.md:227` | Repository CLI |

F. `wamn env`, `wamn org`, and the CLI `--env` arguments already use the short forms. The production service argument and override declarations yielded no long-form `WAMN_*` variable. `WAMN_APPLICATION_COMPONENTS` appeared in the benchmark README, with no matching service reader found (unsure: stale benchmark input). `services/cli/src/bin/wamn.rs:24` `services/cli/src/bin/wamn.rs:60` `services/cli/src/application_verbs.rs:26` `services/cli/src/identity_verbs.rs:64` `services/edge/src/config.rs:30` `web/sync-bench/README.md:61`

## Helm charts, labels, annotations, and workload configuration

| F old | I new | F count and evidence | Impact |
|---|---|---|---|
| Chart `name: application` | Chart `name: app` | 1 declaration. `deploy/charts/application/Chart.yaml:2` `xtask/src/platform.rs:396` `xtask/src/platform.rs:408` | **Cluster wire**, published chart identity |
| Chart `name: wamn-environment` | Chart `name: wamn-env` | 1 declaration. `deploy/charts/environment/Chart.yaml:2` `crates/platform-install/src/deployment.rs:24` | **Cluster wire**, chart identity |
| App chart value `environment` | `env` | 3 value sites, including the producer. This value is `org/env/project`. `deploy/charts/application/values.yaml:18` `deploy/charts/application/templates/_helpers.tpl:3` `crates/environment/src/application_chart/plan.rs:465` | **Cluster wire** |
| App chart value `applications` | `apps` | 5 value sites including the producer. `deploy/charts/application/values.yaml:22` `deploy/charts/application/templates/web-release.yaml:8` `crates/environment/src/application_chart/plan.rs:491` | **Cluster wire** |
| Env chart value `environmentNats` | `envNats` | 4 source lines. `deploy/charts/environment/values.yaml:16` `deploy/charts/environment/templates/host.yaml:50` `crates/environment/src/environment/ensure.rs:221` | **Cluster wire** |
| Platform chart and `wamn-org-inputs` key `applicationChart` | `appChart` | 9 source lines. `deploy/charts/platform/values.schema.json:317` `deploy/charts/platform/templates/org-provisioning.yaml:18` `crates/platform-install/src/org.rs:89` | **Cluster wire**, ConfigMap data key |
| `application.annotations` | `app.annotations` | 6 lines. `deploy/charts/application/templates/_helpers.tpl:2` `deploy/charts/application/templates/workloads.yaml:10` `deploy/charts/application/templates/web-release.yaml:24` | Template helper |
| `application.hostSelector` | `app.hostSelector` | 2 lines. `deploy/charts/application/templates/_helpers.tpl:12` `deploy/charts/application/templates/workloads.yaml:21` | Template helper |
| `application.requireImageSet` | `app.requireImageSet` | 2 lines. `deploy/charts/application/templates/_helpers.tpl:17` `deploy/charts/application/templates/workloads.yaml:1` | Template helper |
| `application.namespace` | `app.namespace` | 8 lines. `deploy/charts/application/templates/_helpers.tpl:22` `deploy/charts/application/templates/services.yaml:6` `deploy/charts/application/templates/workloads.yaml:22` | Template helper |
| `environment.namespace` | `env.namespace` | 7 lines. `deploy/charts/environment/templates/_helpers.tpl:1` `deploy/charts/environment/templates/host.yaml:2` `deploy/charts/environment/templates/credential-access.yaml:11` | Template helper |
| `environment.httpPort` | `env.httpPort` | 3 lines. `deploy/charts/environment/templates/_helpers.tpl:6` `deploy/charts/environment/templates/host.yaml:100` `deploy/charts/environment/templates/network-policies.yaml:19` | Template helper |
| `application.manifestDigest` | `release.manifestDigest` in the diagnostic (unsure: the current message names a key that the template does not read) | 1 diagnostic. `deploy/charts/application/templates/_helpers.tpl:4` `crates/environment/src/application_chart/plan.rs:462` | Diagnostic |
| `wamn.environment` | `wamn.env` (unsure: the new spelling already exists as a label, and the annotation carries a full coordinate while workload config carries a slug) | 10 source lines across annotations, workload config, and telemetry. `deploy/charts/application/templates/_helpers.tpl:3` `deploy/charts/project/templates/cdc-reader.yaml:22` `crates/environment/src/application_chart/plan.rs:314` | **Cluster wire**, Kubernetes **annotation**, workload config, telemetry |
| `wamn.application` | `wamn.app` | 20 source lines across annotations, workload config, and diagnostics. `deploy/charts/application/templates/_helpers.tpl:7` `crates/environment/src/status.rs:95` `apps/guests/ingress/src/guest/data_change.rs:55` | **Cluster wire**, Kubernetes **annotation** and host/guest config |
| `environment` inside `wamn.application` JSON | `env` | 1 schema field. `crates/application/src/application_config.rs:13` `crates/environment/src/application_chart/plan.rs:302` | **Cluster wire**, host/guest config |
| `application` inside `wamn.application` JSON | `app` | 1 schema field. `crates/application/src/application_config.rs:17` `crates/environment/src/application_chart/plan.rs:303` | **Cluster wire**, host/guest config |

F. Status and deployment discovery read the old annotations by exact key. Web discovery instead selects `wamn.route=true` and reads `route.json` and `release.json`. The workload and NetworkPolicy host selectors use `hostgroup` and `wasmcloud.com/hostgroup`. I. Change the annotation readers with their writers. The observed rename candidates are annotations, while the observed selector keys are listed below and retain their spelling. `crates/kubernetes/src/deployment_components.rs:422` `services/control/src/environment_status.rs:55` `crates/kubernetes/src/web_release.rs:59` `crates/kubernetes/src/web_release.rs:72` `deploy/charts/application/templates/_helpers.tpl:12` `deploy/charts/application/templates/network-policy.yaml:20`

## KCL, environment YAML, and on-disk names

F. There are 14 tracked `.k` files, comprising 12 app `wamn.k` files, the manifest schema, and one journey fixture. The schema and tracked manifests yielded **zero KCL keys** containing a long form. The long-form hits in the manifests are comments. `wamn.k` is compiled through `compile_authored_manifest` and `compile_package`. The live env file is YAML and has a separate parser and schema. `crates/codegen/kcl/manifest/manifest.k:308` `crates/codegen/kcl/manifest/manifest.k:320` `apps/platform_fixture/wamn.k:1` `apps/wamn_receiving/wamn.k:1` `tests/journeys/fixtures/installed_digest/model.k:1` `crates/codegen/src/authoring.rs:113` `crates/codegen/src/kcl.rs:27` `crates/environment/src/environment/document/file.rs:79`

| F old | I new | F count and evidence | Impact |
|---|---|---|---|
| `projects[].applications` | `projects[].apps` | 1 authored YAML field, also in the generated JSON Schema. `crates/environment/src/environment/document/file.rs:76` `crates/environment/environment.schema.json:199` `docs/operations/deployment.md:329` | Operator config and **public wire** when the CLI submits `document` |
| `applications` in selected env and recorded config | `apps` | 2 serialized field declarations. `crates/environment/src/environment/document/plan.rs:21` `crates/registry/src/configuration.rs:31` `crates/environment/src/environment/document.rs:62` | Worker and persisted config |
| `EnvironmentFile` | `EnvFile` | 1 type declaration and generated schema title. `crates/environment/src/environment/document/file.rs:20` `crates/environment/environment.schema.json:3` | Rust and schema |
| `ApplicationSelection` | `AppSelection` | 1 type declaration and generated schema definition. `crates/environment/src/environment/document/plan.rs:31` `crates/environment/environment.schema.json:63` `crates/environment/environment.schema.json:202` | Rust and schema |
| `EnvironmentDocument` | `EnvDocument` | 1 type declaration. `crates/environment/src/environment/document/plan.rs:8` `crates/environment/src/environment/document/file.rs:181` | Rust |
| `environment.yaml` | `env.yaml` | 4 code hits. The CLI reads the passed path, so this is an example and fixture filename. `services/cli/tests/control_operations.rs:68` `xtask/src/preflight.rs:293` `crates/environment/src/environment/document/file.rs:80` | Examples and fixtures |
| `environment.k` | Remove stale uses or update them to `env.yaml` (unsure: CLI parse fixtures can name any path, and the actual reader expects YAML) | 8 code hits. `crates/environment/src/environment/stage/plan.rs:7` `crates/registry/src/environment_application.rs:4` `services/cli/src/bin/wamn.rs:170` | Comments and fixtures |
| `environment.schema.json` | `env.schema.json` | 1 tracked schema and 1 explicit file consumer. `crates/environment/environment.schema.json:3` `xtask/src/repo_policy.rs:32` | Generated schema filename |
| `deploy/charts/application` | `deploy/charts/app` | 1 directory, 9 source reference lines. `deploy/charts/application/Chart.yaml:2` `xtask/src/platform.rs:62` `crates/environment/tests/release_chart_render.rs:46` | Repository layout |
| `deploy/charts/environment` | `deploy/charts/env` | 1 directory, 2 source reference lines. `deploy/charts/environment/Chart.yaml:2` `crates/platform-install/src/release_chart/bundle.rs:213` `crates/environment/tests/platform_chart.rs:189` | Repository layout |
| `platform/application` | `platform/app` | 1 packaged leaf, with a bundle check and 2 full-path source references. `crates/platform-install/src/release_chart/bundle.rs:119` `crates/platform-install/src/release_chart/bundle.rs:203` `crates/platform-install/src/deployment.rs:65` | Release bundle |
| `platform/environment` | `platform/env` | 1 packaged leaf, with a bundle check and 1 full-path source reference. `crates/platform-install/src/release_chart/bundle.rs:117` `crates/platform-install/src/release_chart/bundle.rs:213` `crates/platform-install/src/deployment.rs:24` | Release bundle |
| `application.yaml` | `app.yaml` | 2 temporary Helm values filenames. `crates/platform-install/src/deployment.rs:54` `xtask/src/helm_size.rs:196` | On disk |
| `application-values.yaml` | `app-values.yaml` | 1 preflight values filename. `xtask/src/preflight.rs:441` `xtask/src/preflight.rs:451` | On disk |
| `application-{version}.tgz` | `app-{version}.tgz` | 1 expected archive name, derived from the Helm chart name. `xtask/src/platform.rs:385` `xtask/src/platform.rs:396` | Published archive |
| OCI repository `<registry>/wamn/charts/application` | `<registry>/wamn/charts/app` | 1 publisher path. `xtask/src/platform.rs:408` `crates/platform-install/src/org.rs:89` | Published artifact coordinate |
| `application.openapi.json` | `app.openapi.json` | 4 source lines. `xtask/src/platform.rs:44` `apps/platform_ui/openapi-ts.config.ts:6` `apps/platform_ui/src/application-client.ts:1` | Generated artifact and imports |
| `e-<slug>-<hash>` | Decision needed (unsure: `e` may be a disallowed env abbreviation, but this names a project deployment and is a persisted Helm/resource identity) | 1 constructor. `crates/registry/src/name.rs:273` `crates/registry/src/name.rs:286` `services/control/src/platform.rs:56` | **Cluster wire** if renamed |

F. The observed ConfigMap and Secret object names use generated resource identities or already-short names. The env ConfigMap is `<host-name>-settings`; web ConfigMaps use the release name and `<release>-route`; org inputs use `wamn-org-inputs`; credentials use `wamn-credential-<project_env_key>`; env NATS credentials use `wamn-nats-<kind>-<host-name>`. The spelling change in `wamn-org-inputs` is to its data key `applicationChart`. `deploy/charts/environment/templates/host.yaml:8` `deploy/charts/application/templates/web-release.yaml:6` `deploy/charts/application/templates/web-release.yaml:18` `deploy/charts/platform/templates/org-provisioning.yaml:13` `crates/registry/src/name.rs:47` `crates/registry/src/name.rs:70`

## Live docs

F counts are distinct files in `docs/architecture`, `docs/operations`, `docs/testing`, and tracked READMEs. Counts distinguish literal operation names from paths and sealed IDs. The SQL rows report documentation references, including unqualified names where the schema is clear.

| F old | I new | F files and examples |
|---|---|---|
| `environment.configure` | `env.configure` | 1 `docs/architecture/control.md:79` |
| `environment.delete` | `env_project.deactivate` (unsure: selected operation name) | 1 `docs/architecture/control.md:79` |
| `environment.operation_status` | `env.operation_status` | 1 `docs/operations/deployment.md:376` |
| `environment.list` | `env.list` | 2 `apps/platform_ui/README.md:28` `docs/architecture/execution.md:182` |
| `environment.status` | `env.status` | 2 `apps/platform_ui/README.md:28` `docs/architecture/execution.md:158` |
| `application.deploy` | `app.deploy` | 1 `docs/architecture/execution.md:226` |
| `application.rollback` | `app.rollback` | 1 `docs/architecture/execution.md:226` |
| `application.stop` | `app.stop` | 1 `docs/architecture/execution.md:226` |
| `wamn-control:environment/list@0.9.0` | `wamn-control:env/list@<version>` | 1 `docs/architecture/execution.md:144` |
| `wamn-control:environment/status@0.9.0` | `wamn-control:env/status@<version>` | 1 `docs/architecture/execution.md:145` `docs/architecture/execution.md:1262` |
| `wamn-control:environment/activate@0.9.0` | `wamn-control:env/activate@<version>` | 1 `docs/architecture/execution.md:153` |
| `wamn-control:environment/inactivate@0.9.0` | `wamn-control:env/inactivate@<version>` | 1 `docs/architecture/execution.md:154` |
| `wamn-control:role/delete@0.9.0` | `wamn-control:role/deactivate@<version>` (unsure: selected operation name) | 1 `docs/architecture/execution.md:122` |
| `/wamn_control/application/deploy` | `/wamn_control/app/deploy` | 1 `docs/operations/deployment.md:231` |
| `/wamn_control/application/rollback` | `/wamn_control/app/rollback` | 1 `docs/operations/deployment.md:233` |
| `/api/<application>/files` | `/api/<app>/files` | 2 `docs/architecture/data-access.md:1022` `docs/operations/deployment.md:700` |
| `/api/<application>/flow` | `/api/<app>/flow` | 1 `docs/architecture/execution.md:514` |
| `wamn-workflow:<application>/<workflow_id>` | `wamn-workflow:<app>/<workflow_id>` | 2 `docs/architecture/execution.md:515` `docs/operations/queued-automation.md:21` |
| `/password/environments` | Retired route reference (unsure: retain the old spelling if keeping the historical statement) | 1 `docs/architecture/execution.md:1039` |
| `application_write_incomplete` | `app_write_incomplete` | 1 `docs/architecture/execution.md:213` |
| `environment_not_found` | `env_not_found` | 1 `docs/architecture/execution.md:207` |
| `environment-inactive` | `env-inactive` | 1 `docs/architecture/execution.md:27` |
| `application` HTTP field | `app` | 1 `docs/operations/deployment.md:231` `docs/operations/deployment.md:233` |
| `environment` HTTP field | `env` | 1 `docs/architecture/execution.md:148` `docs/architecture/execution.md:188` |
| `environments` HTTP field | `envs` | 1 `docs/architecture/execution.md:144` `docs/architecture/execution.md:148` |
| `applications` YAML key | `apps` | 3 `docs/architecture/data-access.md:592` `docs/operations/deployment.md:329` `apps/rest_connector/README.md:20` |
| `application` file exposure | `app` | 1 `docs/architecture/data-access.md:1061` |
| `--application` | `--app` | 2 `docs/operations/deployment.md:385` `docs/operations/development-loop.md:70` |
| `env delete` | `env deactivate-project` (unsure: selected command and retention behavior) | 5 `docs/operations/deployment.md:368` `docs/operations/backup-and-recovery.md:135` `deploy/charts/project/README.md:13` |
| `connection delete` | `connection deactivate` (unsure: selected command) | 1 `docs/operations/deployment.md:611` |
| `delete-role` | `deactivate-role` (unsure: selected command) | 1 `docs/operations/deployment.md:804` |
| `environment-schema` | `env-schema` | 1 `docs/operations/building.md:227` `docs/operations/building.md:228` |
| `environment.yaml` | `env.yaml` | 6 `docs/architecture/data-access.md:555` `docs/operations/deployment.md:298` `docs/operations/development-loop.md:11` |
| `environment.schema.json` | `env.schema.json` | 2 `docs/operations/building.md:226` `docs/operations/deployment.md:299` |
| `deploy/charts/application` | `deploy/charts/app` | 3 `docs/architecture/execution.md:357` `docs/architecture/overview.md:94` `docs/operations/building.md:33` |
| `deploy/charts/environment` | `deploy/charts/env` | 1 `docs/architecture/overview.md:92` |
| `platform/application` | `platform/app` | 1 `docs/operations/building.md:35` |
| `applicationChart` | `appChart` | 1 `deploy/charts/org/README.md:18` |
| `wamn.application` | `wamn.app` | 2 `docs/architecture/execution.md:492` `docs/operations/queued-automation.md:20` |
| `wamn.environment` | `wamn.env` (unsure: explain the coordinate separately from the short label and config key) | 1 `docs/architecture/naming.md:137` |
| `/etc/wamn/environment` | Reconcile with the live mount paths (unsure: doc-only hit, host chart mounts current settings under `/etc/wamn/host`) | 1 `docs/architecture/execution.md:726` `deploy/charts/environment/templates/host.yaml:12` |
| `org/env/project/application` | `org/env/project/app` | 1 `docs/architecture/naming.md:134` |
| `<org>--<environment>--<project>` | `<org>--<env>--<project>` | 1 `docs/operations/edge.md:18` |
| `<application>-<entity>-<op>` | `<app>-<entity>-<op>` | 1 `docs/architecture/execution.md:494` |
| `<environment>/_wamn_files/staging/<uuid>` | `<env>/_wamn_files/staging/<uuid>` | 1 `docs/architecture/data-access.md:1005` |
| `<environment>/_wamn_files/live/<uuid>` | `<env>/_wamn_files/live/<uuid>` | 1 `docs/architecture/data-access.md:1006` |
| `<environment>/public/_wamn_files/live/<uuid>` | `<env>/public/_wamn_files/live/<uuid>` | 1 `docs/architecture/data-access.md:1047` |
| `registry.orgs` | `registry.org` | 2 `docs/architecture/execution.md:849` `docs/architecture/data-access.md:1077` |
| `registry.projects` | `registry.project` | 2 `docs/architecture/overview.md:164` `docs/architecture/data-access.md:1077` |
| `registry.environments` | `registry.env` | 4 `docs/architecture/overview.md:163` `docs/architecture/execution.md:217` `docs/operations/deployment.md:421` |
| `registry.environment_projects` | `registry.env_project` | 3 `docs/architecture/data-access.md:1077` `docs/architecture/execution.md:650` `docs/architecture/overview.md:165` |
| `registry.environment_applications` | `registry.env_app` | 1 `docs/architecture/overview.md:167` |
| `registry.environment_application_connections` | `registry.env_app_connection` | 1 `docs/architecture/data-access.md:1082` |
| `registry.event_readers` | `registry.event_reader` | 1 `docs/operations/gcp.md:581` `docs/operations/gcp.md:620` |
| `retired_project_envs` | Remove reference | 1 `docs/architecture/data-access.md:1077` `docs/plan/review-restructure.md:157` |
| `identity.principals` | `identity.principal` | 2 `docs/architecture/data-access.md:1075` `docs/operations/deployment.md:848` |
| `identity.org_memberships` | `identity.org_membership` | 3 `docs/architecture/data-access.md:1075` `docs/architecture/execution.md:211` `docs/architecture/overview.md:183` |
| `identity.org_roles` | `identity.org_role` | 2 `docs/architecture/data-access.md:1075` `docs/architecture/overview.md:191` |
| `identity.project_roles` | `identity.project_role` | 2 `docs/architecture/data-access.md:1075` `docs/architecture/overview.md:191` |
| `identity.project_env_memberships` | `identity.project_env_membership` | 2 `docs/architecture/data-access.md:1075` `docs/architecture/execution.md:161` |
| `identity.password_credentials` | `identity.password_credential` | 1 `docs/architecture/data-access.md:1083` |
| `identity.password_logins` | `identity.password_login` | 1 `docs/operations/deployment.md:859` |
| `identity.password_tokens` | `identity.password_token` | 1 `docs/operations/deployment.md:856` |
| `password_attempts` | `password_attempt` | 1 `docs/architecture/data-access.md:1078` |
| `identity.session_keys` | `identity.session_key` | 2 `docs/architecture/data-access.md:1078` `docs/architecture/execution.md:1078` |
| `catalog.packages` | `catalog.package` | 1 `docs/operations/deployment.md:439` |
| `catalog.workflows` | `catalog.workflow` | 1 `docs/architecture/execution.md:577` |
| `wamn_run.runs` | `wamn_run.run` | 3 `apps/netsuite/README.md:230` `docs/operations/cluster-tests.md:28` `docs/operations/queued-automation.md:36` |
| `wamn_history.pruning_watermarks` | `wamn_history.pruning_watermark` | 1 `docs/architecture/data-access.md:856` |
| `platform.environment` | `platform.env` | 2 `docs/architecture/execution.md:25` `docs/architecture/data-access.md:924` |
| `platform.tx_identity.application` | `platform.tx_identity.app` | 1 `docs/architecture/data-access.md:555` |
| `wamn_authority.current_application()` | `wamn_authority.current_app()` | 2 `docs/architecture/data-access.md:554` `docs/architecture/execution.md:482` |
| `app.application` | `app.app` | 1 `docs/testing/database-tests.md:46` |
| `target_application` | `target_app` if the design lands (unsure: described as separate work) | 1 `docs/architecture/data-access.md:1072` |
| `wamn-environment` Cargo package | `wamn-env` | 4 `docs/architecture/overview.md:41` `apps/rest_connector/README.md:33` `apps/wamn_integration/README.md:25` |
| `crates/environment` path prefix | `crates/env` | 6 `docs/architecture/overview.md:41` `docs/operations/building.md:226` `docs/testing/database-tests.md:42` |
| `environment::apply` | `env::apply` | 2 `docs/architecture/data-access.md:1156` `docs/architecture/execution.md:844` |
| `wamn_environment::application` | Reconcile with the current role handler (unsure: linked source path is stale) | 1 `docs/architecture/execution.md:175` `services/host/src/host/application_route.rs:198` |
| `wamn_environment::environment_org` | `wamn_env::env_org` | 1 `docs/architecture/execution.md:206` |
| `wamn_catalog::ApplicationName` | `wamn_registry::AppName` | 1 `docs/architecture/naming.md:19` `crates/registry/src/environment_application.rs:22` |
| `application_routes_kind` | `app_routes_kind` | 4 `docs/architecture/execution.md:291` `docs/operations/cluster-tests.md:5` `docs/testing/application-tests.md:44` |
| `application_deploy_kind` | `app_deploy_kind` if still applicable (unsure: cited as historical coverage) | 1 `docs/testing/strategy.md:137` |
| `environment_apply_kind` / `environment_apply_kind.rs` | Reconcile the named target before shortening (unsure: README reference) | 1 `apps/netsuite/README.md:234` `apps/netsuite/README.md:241` |
| `apps/platform_ui/src/environment.tsx` | `apps/platform_ui/src/env.tsx` if the UI module moves (unsure: source module rename was outside the route spelling itself) | 1 `docs/architecture/execution.md:1183` |
| `apps/platform_ui/test/environment.test.tsx` | `apps/platform_ui/test/env.test.tsx` if the UI module moves (unsure: test name follows the source decision) | 1 `docs/operations/running-tests.md:280` |
| `solidApplication` | `solidApp` if the shared Vite API is included (unsure: generic Solid app helper) | 1 `web/shell/README.md:7` `web/shell/vite.ts:71` |
| `applicationConfig` | `appConfig` if the shared Vite API is included (unsure: tooling API scope) | 1 `web/shell/README.md:8` `web/shell/vite.ts:89` |
| `WAMN_APPLICATION_COMPONENTS` | Verify benchmark command before changing (unsure: README-only hit in the scanned live consumer paths) | 1 `web/sync-bench/README.md:61` |
| `#environment-names` | `#env-names` if the heading changes (unsure: prose heading linked by a chart) | 1 heading file `docs/architecture/naming.md:114`, chart link `deploy/charts/application/Chart.yaml:3` |
| `#organization-membership-and-administration` | `#org-membership-and-administration` if the heading changes (unsure: prose heading linked by route code) | 1 heading file `docs/architecture/overview.md:180`, code links `services/control/src/control_route.rs:16` `crates/routes/src/host_route.rs:146` |
| `#application-declarations` | `#app-declarations` if the heading changes (unsure: prose heading linked by the KCL reader) | 1 heading file `docs/architecture/data-access.md:80`, code link `crates/codegen/src/authoring.rs:103` |
| `#application-and-schema-operations` | `#app-and-schema-operations` if the heading changes (unsure: prose heading) | 2 `docs/operations/deployment.md:220` `docs/architecture/execution.md:233` |
| `#application-generation-and-sqlx` | `#app-generation-and-sqlx` if the heading changes (unsure: prose heading) | 2 `docs/operations/running-tests.md:409` `docs/architecture/data-access.md:833` |

I. `<version>` in the proposed sealed IDs is unresolved here. The current table uses `0.9.0`, and R6 says the rename changes the public wire and generated clients. `crates/routes/src/host_route.rs:53` `docs/plan/review-restructure.md:148`

## Not renamed

| F name | F reason and evidence |
|---|---|
| `type: application` | Helm chart type, 6 declarations. `deploy/charts/application/Chart.yaml:4` `deploy/charts/environment/Chart.yaml:3` `deploy/charts/platform/Chart.yaml:3` |
| `appVersion` | Helm chart metadata key, 1 declaration. `deploy/charts/application/Chart.yaml:6` `docs/operations/building.md:42` |
| `spec.template.spec.environment` | wasmCloud `WorkloadDeployment` field, 1 chart site. The template supplies the namespace. `deploy/charts/application/templates/workloads.yaml:4` `deploy/charts/application/templates/workloads.yaml:22` |
| Runtime-operator `env` | Child chart input for a list of process environment variables, 1 host template site. `deploy/charts/environment/templates/host.yaml:78` `deploy/charts/environment/templates/host.yaml:102` |
| `app.kubernetes.io/name` | Kubernetes **label and selector**, including host, CDC, and web dependencies. `deploy/charts/environment/templates/network-policies.yaml:18` `deploy/charts/project/templates/cdc-reader.yaml:29` `crates/kubernetes/src/deployment_components.rs:30` |
| `app.kubernetes.io/instance` | Kubernetes **label and selector**. `deploy/charts/project/templates/cdc-reader.yaml:30` `deploy/charts/project/templates/cdc-reader.yaml:35` `deploy/charts/environment/templates/network-policies.yaml:33` |
| `app.kubernetes.io/component` | Kubernetes **label and selector**. `deploy/charts/org/templates/provision-org.yaml:80` `deploy/charts/project/templates/network-policies.yaml:6` `crates/identity-protocol/src/workload_role.rs:106` |
| `wasmcloud.com/hostgroup` | wasmCloud **label and selector**, 6 source lines. `crates/registry/src/name.rs:379` `deploy/charts/application/templates/network-policy.yaml:20` `deploy/charts/project/templates/network-policies.yaml:19` |
| `wamn.env` | Already short Kubernetes **label**, 3 source lines. `deploy/charts/project/templates/publication.yaml:11` `crates/kubernetes/src/kubernetes.rs:76` `crates/kubernetes/src/deployment_components.rs:110` |
| `wamn.org` | Already short Kubernetes **label**, 3 source lines. `deploy/charts/project/templates/publication.yaml:9` `crates/kubernetes/src/kubernetes.rs:76` `crates/kubernetes/src/deployment_components.rs:108` |
| `wamn.dev/org` | Already short Kubernetes **label and selector**, 4 chart lines. `deploy/charts/org/templates/namespace.yaml:6` `deploy/charts/platform/templates/identity-network-policy.yaml:18` |
| `wamn.route` | Kubernetes **label and selector** used by the web proxy. `deploy/charts/application/templates/web-release.yaml:20` `deploy/charts/org/templates/control-route.yaml:19` `crates/kubernetes/src/web_release.rs:59` |
| `guest` and `guest-sql` | Frozen CLI and Kubernetes **label value** stems for the guest-SQL workload role, whose enum already uses `App`. `crates/identity-protocol/src/workload_role.rs:95` `crates/identity-protocol/src/workload_role.rs:106` |
| `application/json` | MIME media type. `crates/routes/src/host_route.rs:1869` `services/identity-service/src/password.rs:148` `services/web/src/lib.rs:181` |
| `application/octet-stream` | MIME media type. `services/web/src/lib.rs:186` |
| `application/wasm` | wasmCloud artifact media type. `docs/architecture/components.md:89` `docs/operations/building.md:351` |
| `application/vnd.wamn.package.v3` | MIME `application` type in the package artifact contract. `docs/operations/building.md:353` |
| `application/vnd.wamn.package.manifest.v1+json` | MIME `application` type in the manifest layer contract. `docs/operations/building.md:360` |
| `application/vnd.wamn.package.v1.tar` | MIME `application` type in the migrations layer contract. `docs/operations/building.md:361` |
| `application/vnd.wamn.web.v1` | MIME `application` type in the web artifact contract. `docs/operations/web-upload.md:43` |
| `application/vnd.wamn.web.v1.tar` | MIME `application` type in the web layer contract. `docs/operations/web-upload.md:44` |
| `application/vnd.oci.image.manifest.v1+json` | OCI media type. `docs/operations/building.md:354` |
| `application/vnd.oci.empty.v1+json` | OCI media type. `docs/operations/building.md:355` |
| `/oauth/callback` | OAuth callback term and WAMN proxy path contain no R6 long form. `services/web/src/lib.rs:308` |
| `CURRENT_USER` and `hashtextextended` | PostgreSQL names. They carry no WAMN domain abbreviation. `crates/authentication/src/administration/sql.rs:65` `docs/architecture/execution.md:728` |
| `wamn.k`, `manifest.k`, `wamn.json` | Authored filename, schema filename, and compiled filename contain no R6 long form. `crates/codegen/src/authoring.rs:23` `crates/codegen/src/authoring.rs:33` `crates/codegen/src/authoring.rs:1` |
| `project_env_key` and `wamn.project_env_key` | Already use `env`. `crates/registry/src/name.rs:22` `crates/environment/src/application_chart/plan.rs:310` `deploy/charts/application/templates/workloads.yaml:18` |
| `WAMN_ORG` and `WAMN_MANAGEMENT_PROJECT_ENV_KEY` | Already use `org` and `env`. `services/edge/src/config.rs:33` `docs/architecture/data-access.md:942` |
