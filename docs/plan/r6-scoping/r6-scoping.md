# R6 scoping report

Scope: R6 in `docs/plan/review-restructure.md`, read at commit `6b4db53e1`. This report changes no code. F means read in the code at that commit. I means inference or proposal. The appendices in this directory hold the full inventories with every `path:line`:

- `appendix-a-sql-names.md`: every SQL table, column, derived name, history table and SQL string.
- `appendix-b-rust-and-wire.md`: crates, modules, types, functions, constants, wire fields and generated clients.
- `appendix-c-routes-cli-charts.md`: routes, operation codes, CLI, charts, annotations, files and live docs.
- `appendix-d-deactivation.md`: every read path for the seven entities, with "must filter" or "need not filter".
- `appendix-e-cascades-installs-images.md`: foreign keys, `DELETE FROM` paths, cascade tests, installs and image contents.

Two facts in R6 are wrong (F). The schema has 57 tables and 25 plural names, not 54 and 24. The R6 roster itself lists 25.

## 1. Proposed beads

| Bead | Content | Depends on |
| --- | --- | --- |
| R6.1 | Rename pass, one lane, three commits in order: (a) SQL names, (b) Rust crates, modules and types, (c) wire, routes, CLI, charts, generated clients and docs. All three touch the same files, so one lane does them in sequence. | Epic 3 merged |
| R6.2 | Project in env: deactivate instead of delete, the command rename, the capacity count, the new suffix on re-add, membership handling, and the drop of `registry.retired_project_envs`. | R6.1 |
| R6.3 | App: env apply sets `active = false` on a missing app and `true` on an app that comes back. | R6.1 |
| R6.4 | Org and project: the `active` columns, the admission filters, and the org and project commands. | R6.1 |
| R6.5 | Role: deactivate instead of drop, and every permission check ignores an inactive role. | R6.1 |
| R6.6 | Connection: deactivate instead of delete, the credential removal, and refusal of calls through an inactive connection. | R6.1 |
| R6.7 | The 26 cascades become `RESTRICT`, and the cascade tests assert refusal. | R6.2 to R6.6 |
| R6.8 | Epic close: one host image build, then the kind journeys, after your approval. | R6.7 |

R6.2 to R6.6 run in parallel in five lanes after R6.1 merges into the epic branch. Each one owns a different entity. They share some files, for example `crates/environment/src/registry/configuration.rs`, and root resolves those overlaps at merge. R6.7 comes last, because a `RESTRICT` breaks each delete path until its entity bead replaces that path.

## 2. Rename mapping

Names marked (unsure) need a ruling or a second look. Question 1 decides whether the abbreviation list in 2.9 is in scope.

### 2.1 SQL tables (F: current names)

| Old | New |
| --- | --- |
| `registry.orgs` | `registry.org` |
| `registry.projects` | `registry.project` |
| `registry.environments` | `registry.env` |
| `registry.environment_projects` | `registry.project_env` (unsure: Question 2, the alternative is `registry.env_project`) |
| `registry.environment_applications` | `registry.env_app` |
| `registry.environment_integrations` | `registry.env_integration` |
| `registry.environment_application_connections` | `registry.env_app_connection`, which now gets a history table (50-byte derived names, inside the limit) |
| `registry.event_readers` | `registry.event_reader` |
| `registry.retired_project_envs` | dropped (R6) |
| `identity.principals` | `identity.principal` |
| `identity.org_memberships` | `identity.org_membership` |
| `identity.org_roles` | `identity.org_role` |
| `identity.project_roles` | `identity.project_role` |
| `identity.project_env_memberships` | `identity.project_env_membership` |
| `identity.password_credentials` | `identity.password_credential` |
| `identity.password_logins` | `identity.password_login` |
| `identity.password_tokens` | `identity.password_token` |
| `identity.password_attempts` | `identity.password_attempt` |
| `identity.renewal_credentials` | `identity.renewal_credential` |
| `identity.session_keys` | `identity.session_key` |
| `identity.nats_keys` | `identity.nats_key` |
| `catalog.packages` | `catalog.package` |
| `catalog.workflows` | `catalog.workflow` |
| `wamn_run.runs` | `wamn_run.run` |
| `wamn_history.pruning_watermarks` | `wamn_history.pruning_watermark` |
| `platform.environment` (singular, long form) | `platform.env` |
| `<model_schema>.wamn_entities` | `<model_schema>.wamn_entity` |
| `<model_schema>.wamn_cdc_exclusions` | `<model_schema>.wamn_cdc_exclusion` (unsure: `cdc`, Question 1) |
| `wamn_history.migrations_<40 hex>` | `wamn_history.migration_<40 hex>` |
| `registry._sqlx_migrations`, `platform._sqlx_migrations` | unchanged (unsure: SQLx owns this name and its default, Question 1) |
| Edge SQLite `claims`, `samples` | `claim`, `sample` (unsure: Question 1, the device store keeps old files) |
| Test fixture tables `app.orders`, schema `application` in `crates/codegen/tests/catalog_live.rs` | `app."order"`, schema `app` (unsure: test fixtures) |

Each renamed table also renames its history table (`<table>_history`), its explicit constraints, indexes, triggers and catalog policies. Appendix A lists the 55 explicit derived names, for example `orgs_id_charset_check` → `org_id_charset_check`, `packages_pkey` → `package_pkey` and `runs_idempotency` → `run_idempotency`. No proposed derived name exceeds 63 bytes (F, computed).

### 2.2 SQL columns, parameters, functions and stored values

| Old | New |
| --- | --- |
| `application` column in `registry.environment_applications`, `registry.environment_application_connections`, `provisioning.platform_event`, `catalog.workflows`, `platform.file`, `platform.number_sequence`, `platform.tx_identity` | `app` |
| `environment` column in `catalog.workflows`, `wamn_run.runs` | `env` |
| Parameter `environment` of `identity.has_project_access` | `env_name` (unsure: plain `env` is ambiguous with the columns in that function) |
| `wamn_authority.current_application()` | `wamn_authority.current_app()` |
| `registry.keep_environment_target()`, trigger `environment_target_immutable`, message `environment_target_changed` | `registry.keep_env_target()`, `env_target_immutable`, `env_target_changed` |
| Event codes `environment_apply`, `environment_delete` | `env_apply`, and the code that Question 3 names |
| Event labels `Create organization`, `Apply environment`, `Delete environment`, `Deploy application`, `Roll back application` | `Create org`, `Apply env`, the Question 3 label, `Deploy app`, `Roll back app` |
| NATS key kinds `environment`, `environment-signing` | `env`, `env-signing` |
| File exposure value `application` (default, check and writer) | `app` |
| Setting `target_type = 'application'` | `app` |
| Generated role prefix `application-` | `app-` |
| Advisory lock input `wamn-application/` | `wamn-app/` |
| JetStream durable prefix `application_` | `app_` (a new durable identity, so fresh install only) |

### 2.3 Rust crates, modules and types

| Old | New |
| --- | --- |
| Crate `crates/application`, package `wamn-application` | `crates/app`, `wamn-app` |
| Crate `crates/environment`, package `wamn-environment` | `crates/env`, `wamn-env` |
| Modules `application`, `environment`, `application_chart`, `application_config`, `application_member`, `application_route`, `application_verbs`, `environment_application`, `environment_configuration`, `environment_fill`, `environment_names`, `environment_org`, `environment_registry`, `environment_requests`, `environment_schema`, `environment_sql`, `environment_sql_effects`, `environment_status`, `platform_environment` | Same names with `app` and `env`, for example `env_sql_effects`, `app_chart`, `env_app` |
| 63 types, for example `ApplicationName`, `ApplicationConfig`, `EnvironmentDocument`, `EnvironmentFile`, `EnvironmentStatus`, `EnvironmentApplications` | `AppName`, `AppConfig`, `EnvDocument`, `EnvFile`, `EnvStatus`, `EnvApps` |
| `EnvironmentArgs` in `services/cli/src/role_verbs.rs` (holds a host URL and a token) | `EnvArgs` (unsure: the type does not hold an env, a name such as `HostSessionArgs` fits better) |
| About 90 functions and 20 constants, for example `lock_environment`, `application_release_name`, `ENVIRONMENT_LOCK_SQL` | One rule: replace the whole word `application(s)` with `app(s)`, `environment(s)` with `env(s)`, `organization` with `org`. Appendix B lists every name and the exceptions. |
| 47 test function names | Same rule |

Exceptions (F): names about process environment variables (`kubernetes::job::environment`, `is_secret_environment` and five engine tests) keep the word, because they do not mean the wamn env.

### 2.4 Wire fields, codes and documents

| Old | New |
| --- | --- |
| JSON field `application` in 11 native schemas, `invalid_input.detail.field` and the worker request | `app` |
| JSON field `environment` (`member.list`, `application_write_incomplete.detail`) | `env` |
| JSON field `environments` (env list, member list, `ProjectMember`) | `envs` |
| Refusal codes `application_write_incomplete`, `environment_not_found`, `environment-inactive`, `environment_target_missing` | `app_write_incomplete`, `env_not_found`, `env-inactive`, `env_target_missing` |
| Request and result types `ApplicationDeployRequest`, `EnvironmentStatusResult`, and 22 more | `AppDeployRequest`, `EnvStatusResult`, and so on |
| Cursor `order: "Environment"` (cursor format `v: 1`) | `order: "Env"`, cursor format 2 (Question 7) |
| Serving manifest `composed-entry.application`, `host-routes` value `application` (format 11) | `composed-entry.app`, `app`, format 12 (Question 7) |
| Connection descriptor `owner: "environment"`, `environment-selected-http-header` (descriptor 1) | `owner: "env"`, `env-selected-http-header`, descriptor 2 (Question 7) |
| Worker task `type: "environment"` | `type: "env"` |
| Workload configuration `wamn.application` with fields `application`, `environment` | `wamn.app` with fields `app`, `env` |
| Env file `projects[].applications`, recorded `applications`, schema `ApplicationSelection` | `projects[].apps`, `apps`, `AppSelection` |
| Edge configuration `target.environment` | `target.env` |
| WIT `world application` in `apps/modbus_reader` | `world app` |
| `wamn publish` output field `application` | `app` |

### 2.5 Routes and operations

The native HTTP contract is version `0.9.0`. Each operation name gives its path, sealed ID, attachment ID and OpenAPI `operationId` (F).

| Old | New |
| --- | --- |
| `environment.configure`, `.list`, `.status`, `.operation_status`, `.activate`, `.inactivate` | `env.configure`, `env.list`, `env.status`, `env.operation_status`, `env.activate`, `env.inactivate` (verb: Question 4) |
| `environment.delete` | Question 3 |
| `application.deploy`, `.rollback`, `.stop` | `app.deploy`, `app.rollback`, `app.stop` |
| `role.delete` | `role.deactivate` (verb: Question 4) |
| `connection.delete` | Question 5 |
| Platform UI route `projects/:project/environments` | `projects/:project/envs` (unsure: `envs` is also a valid env slug in `projects/:project/:env`) |
| Placeholders `/api/<application>/files`, `/api/<application>/flow`, `wamn-workflow:<application>/<id>` | `<app>` in code and docs only. The bytes on the wire do not change. |

### 2.6 CLI flags and commands

| Old | New |
| --- | --- |
| `--application` on `wamn deploy` and `wamn env apply` | `--app` |
| Positional argument `application` on rollback, status and schema commands | `app` |
| `wamn env delete` | Question 3 |
| `wamn delete-role` | `wamn deactivate-role` (Question 4) |
| `wamn connection delete` | Question 5 |
| `cargo xtask environment-schema` | `cargo xtask env-schema` |
| `wamn-host serve --environment` | unchanged in R6 (unsure: Epic 5 replaces this argument, and the value is a pod namespace) |

The `WAMN_*` service variables hold no long form (F).

### 2.7 Chart values, chart names and annotations

| Old | New |
| --- | --- |
| Chart `application` at `deploy/charts/application`, packaged `platform/application`, OCI repository `wamn/charts/application` | `app` at `deploy/charts/app`, `platform/app`, `wamn/charts/app` |
| Chart `wamn-environment` at `deploy/charts/environment`, packaged `platform/environment` | `wamn-env` at `deploy/charts/env`, `platform/env` |
| App chart values `environment`, `applications` | `env`, `apps` |
| Env chart value `environmentNats` | `envNats` |
| Platform chart and `wamn-org-inputs` key `applicationChart` | `appChart` |
| Helpers `application.*`, `environment.*` | `app.*`, `env.*` |
| Annotations `wamn.application`, `wamn.environment` | `wamn.app`, `wamn.env` (unsure: `wamn.env` already exists as a label with a different value, a slug instead of a full coordinate) |
| Job annotation value `wamn.dev/operation: environment` | `env` |
| Generated files `application.openapi.json`, `environment.schema.json`, `application.yaml`, `application-values.yaml` | `app.openapi.json`, `env.schema.json`, `app.yaml`, `app-values.yaml` |
| Resource name prefix `e-<slug>-<hash>` | unchanged (unsure: Question 1, `e` is a hash name prefix, not a word) |

No Kubernetes label or selector changes. The keys that change are annotations and values (F).

### 2.8 wamn.k keys

None. The 14 `.k` files hold no key with a long form. The hits are comments (F).

### 2.9 Other abbreviations (Question 1)

If "no other abbreviation" covers wamn-owned names, these also change: `platform.pat`, `platform.tx_identity` and `prune_tx_identity`, `registry.auth_type` and `auth_type` columns, `registry.meta`, columns `ops`, `ref`, `max_projects`, `max_attempts`, `sub`, `xid`, `kid`, `active_kid`, `start_lsn`, `end_lsn`, `enable_lsn`, and the `_json` suffixes. `pat` is also in the token prefix `wamn_pat_`. `xid`, `kid`, `sub` and `lsn` are PostgreSQL or JWT terms.

## 3. Deactivation changes

F unless marked. Appendix D lists every read path.

Org (`registry.org`). No `active` column and no delete command today. Add `active`. Filter it at: SQL login mapping (`registry.login_org`), the identity org check (`Issuer::registered`), invitation and session issue (`session_exchange::finish_session`), control admission (`ControlRoutes::admits`, `identity.is_project_admin`), and project creation. Lock, idempotency and membership administration reads need not filter. No command sets it (Question 6).

Env (`registry.env`). `active` exists. `environment.inactivate` and `environment.activate` write it and copy it into each project database. The host refuses requests from the copy (`crates/application/src/ingress.rs`). No change except the names.

Project (`registry.project`). No `active` column. Today `project.inactivate` and `project.activate` write the env status of every env of the project (`services/control/src/control_route.rs::set_status`). An env holds many projects, so this also stops the sibling projects (F). Add `active`. Filter it in `identity.is_project_admin`, `identity.has_project_access`, `project.list` state, `control.mine`, project grant writes and host admission (Question 6).

Project in env (`registry.project_env`). The delete command keeps its teardown and sets `active = false` at the end instead of `DELETE` (`crates/environment/src/environment_sql_effects.rs::delete_registry_rows`). These reads must ignore inactive rows: `registry.project_capacity_available` and `reserve_project` (both count all rows today), `deployment::instance_suffix`, `deployment::exists`, `deployment::projects`, `host_projects`, `configuration::read`, `identity.has_project_access`, `PROJECT_MEMBERS_SQL`, `org_environments`, `project_deployments`, `environment.status` counts, the env project count, event reader provision and recovery, and capture gap lookup. On re-add, `reserve_project` must write the new suffix: today its conflict arm keeps the old suffix (F). It must also clear `schema_mode` and the old event reader row. Today the delete cascade removes the project env memberships, and a live test asserts that a re-added project in env grants nothing to the old members (`crates/identity/tests/identity_live.rs:450`). Question 8 decides this.

App (`registry.env_app`). Env apply deletes a missing app row today (`crates/environment/src/registry/configuration.rs:171`). It must set `active = false`. These reads must filter: `configuration::read` (the app list and its connection aliases), `application_names` and the live arm of `application_releases`. Number sequences, files, catalog rows and history need not filter. An inactive app keeps its label, and the label is unique in the org (`(org, label)`), so a new app cannot reuse it (F).

Role (`platform.role`). `DROP_ROLE_SQL` deletes the row, and the cascade deletes its grants. It must set `active = false` and keep both grant tables. These checks must ignore an inactive role: `PRINCIPAL_GRANTS_SQL` (sessions, PATs, signed HTTP, `permission.mine`, guest SQL), `workflow_role_permissions`, `is_admin`, `holds_permission`, `mutate_grant`, `role.list`, `user.list` and the control status counts. `Authorization::principal_grants` caches positive grants. A deactivated role grants until that cache entry expires (I). `ROLE_EXISTS_SQL`, the deploy ownership check and the revoke lookups need not filter. Activation needs a command (Question 4).

Connection (`platform.connection`). `active` exists. `connection.disable` and `connection.enable` change only the flag. `connection.delete` deletes the row. A fresh read refuses an inactive connection in bound HTTP, integration `begin`, blobstore and file binding, HMAC and PAT ingress (F). Three gaps exist (F). First, `ConnectionFacts::answer` returns a cached active answer before it reads the row again. Second, `connection.test` uses the credential and ignores `active`. Third, `platform.require_file_store` ignores `active`. R6 says to fix these. The R6 credential removal conflicts with the schema (F, `deploy/sql/catalog-schema.sql:123-159`). `platform.connection_generation` has a foreign key to `platform.credential`. The trigger `credential_immutable` refuses a credential delete while the connection row exists. Question 5 decides this. A connection with `auth_type = 'none'` has no credential, so "activation needs a new credential" needs a case for it (I).

Removing `registry.retired_project_envs` touches these items, and each one exists (F): the table and its DDL, `retired_project_envs_history`, the trigger `project_envs_retire_instance`, the function `registry.retire_project_env` with its `REVOKE`, the grant in `deploy/sql/apply-grants.sql:21`, the generated history grant, the protected relation in `deploy/sql/system-schema.sql:1128`, `test-support/grant-tests/tests/system.rs:354` and `docs/architecture/data-access.md:1077`. Nothing reads it.

## 4. Every ON DELETE CASCADE in deploy/sql (F)

27 clauses. The migration directories are empty. One stays, 26 become `RESTRICT`.

| FK (child → parent) | Location | Parent delete path today | R6 |
| --- | --- | --- | --- |
| `registry.projects` → `registry.orgs` | `system-schema.sql:188` | none | RESTRICT |
| `identity.org_memberships` → `registry.orgs` | `system-schema.sql:311` | none | RESTRICT |
| `identity.nats_keys` → `registry.orgs` | `system-schema.sql:520` | none | RESTRICT |
| `registry.environments` → `registry.orgs` | `system-schema.sql:602` | none | RESTRICT |
| `identity.project_roles` → `registry.projects` | `system-schema.sql:293` | none | RESTRICT |
| `registry.environment_projects` → `registry.projects` | `system-schema.sql:699` | none | RESTRICT |
| `registry.environment_projects` → `registry.environments` | `system-schema.sql:700` | none (status routes only update) | RESTRICT |
| `registry.environment_integrations` → `registry.environments` | `system-schema.sql:735` | none | RESTRICT |
| `registry.environment_applications` → `registry.environment_projects` | `system-schema.sql:724` | `wamn env delete` → `environment.delete` → `delete_registry_rows` | RESTRICT |
| `identity.project_env_memberships` → `registry.environment_projects` | `system-schema.sql:772` | same | RESTRICT |
| `registry.event_readers` → `registry.environment_projects` | `system-schema.sql:880` | same | RESTRICT |
| `registry.environment_application_connections` → `registry.environment_applications` | `system-schema.sql:747` | `wamn env apply` → `configuration::write` deletes missing apps | RESTRICT |
| `identity.project_roles` → `identity.principals` | `system-schema.sql:283` | none (`disable_principal` updates) | RESTRICT |
| `identity.org_memberships` → `identity.principals` | `system-schema.sql:319` | none | RESTRICT |
| `identity.project_env_memberships` → `identity.principals` | `system-schema.sql:770` | none | RESTRICT |
| `identity.org_roles` → `identity.org_memberships` | `system-schema.sql:368` | none (deactivation updates and deletes grants explicitly) | RESTRICT |
| `registry.capture_gap` → `registry.event_readers` | `system-schema.sql:908` | none directly, only through the project in env cascade | RESTRICT |
| `identity.renewal_credentials` → `identity.password_logins` | `system-schema.sql:450` | `password_login::prune_expired`, called from `Issuer::admit` | keep |
| `platform.user_role` → `platform."user"` | `app-schema.sql:200` | none (`disable_project_user` updates and deletes roles explicitly) | RESTRICT |
| `platform.user_role` → `platform.role` | `app-schema.sql:202` | `role.delete` → `DROP_ROLE_SQL` | RESTRICT |
| `platform.permission` → `platform.role` | `app-schema.sql:227` | same | RESTRICT |
| `platform.number_counter` → `platform.number_sequence` | `app-schema.sql:429` | none (only database teardown) | RESTRICT |
| `platform.credential` → `platform.connection` | `catalog-schema.sql:84` | `connection.delete` → `delete_connection` | RESTRICT |
| `platform.oauth_token` → `platform.connection` | `catalog-schema.sql:94` | same | RESTRICT |
| `platform.oauth_authorization` → `platform.connection` | `catalog-schema.sql:113` | same | RESTRICT |
| `platform.connection_generation` → `platform.connection` | `catalog-schema.sql:124` | same | RESTRICT |
| `wamn_run.run_queue` → `wamn_run.runs` | `run-queue.sql:53` | none (reset truncates, terminal paths delete queue rows directly) | RESTRICT |

The R6 note "keep the cascade for the application connections and integrations that env apply replaces" matches no cascade (F). Env apply deletes those rows with explicit statements, and the integration foreign key has no cascade. The R6 statement "the only API deletes of these parents are app, project in env, role and connection" is true (F).

Six tests depend on a cascade (F): `identity_live.rs::project_delete_removes_its_roles_and_memberships` and `::project_environment_membership_round_trip`, `project_schema.rs::app_schema_applies_and_enforces_isolation_on_postgres` and `::platform_relations_log_every_row_change_on_postgres`, `release_permissions_live.rs::permission_grants_and_revokes_store_rows`, and `password_login_live.rs::inactivity_absolute_expiry_and_bounded_cleanup`, which stays.

## 5. Existing databases

How the schema reaches a database today (F). A fresh system database gets `system-schema.sql` and the other bootstrap files in one transaction. A database that already has the SQLx ledger skips the bootstrap and runs only the numbered system migrations, which are empty today. A project database gets the project schema at provisioning. After that, only numbered project migrations reach it, through `wamn platform schema apply`. So an edit of the bootstrap files changes fresh installs only. An in-place rename needs new forward migrations for the system and every project database, including history tables, stored values, policies and grants.

Installs that hold data today:

- kind `wamn-local` runs now (F, `kind get clusters`). It holds journey orgs and test rows. The docs call it a disposable development platform.
- `wamn-dev` on GCP: no GKE cluster exists (F, `docs/plan/review-restructure.md:396`). Retained disks or buckets can exist (I, not checked).
- No install holds customer data (I, from the docs).

Proposal: fresh install only (Question 9). This follows the greenfield default.

## 6. The host image rebuild

Each of these R6 changes touches a host crate, so each one forces the rebuild (I, from the dependency graph of `services/host`). They are the SQL renames (`wamn-plugin`, `wamn-authentication`, `wamn-connection`), the crate and type renames (`wamn-application`, `wamn-registry`, `wamn-routes`), the wire fields, role deactivation (host grant checks) and connection deactivation (`wamn-connection`, `wamn-plugin`). The control, identity, CDC reader (it ships in the host image), web and edge images also change.

To build it once: all R6 beads merge into one epic branch. No lane builds or publishes an image. Root builds the host image and the other images once, after R6.7, and the kind run at epic close uses them. This is the Epic 3 pattern.

## 7. Questions for rulings

1. Scope of "no other abbreviation".
   - (a) Only the three short forms and singular table names. Standard terms from PostgreSQL, JWT, SQLx and NATS stay, and wamn-owned abbreviations (`pat`, `tx_identity`, `auth_type`, `meta`, `ops`, `ref`, `max_`, `cdc`, the `e-` prefix) stay for now, recorded as a follow-up.
   - (b) Also expand the wamn-owned abbreviations in this pass. `pat` changes a token prefix.
   - (c) Expand everything, including `xid`, `kid`, `sub` and `lsn`.
   - Edge SQLite tables and test fixture names: in scope under every option.
   - Recommendation: (a), because it keeps one pass bounded and leaves standard names readable to outside readers.
2. Name of the project in env table.
   - (a) `registry.project_env`, which matches `ProjectEnvKey` and `identity.project_env_membership`.
   - (b) `registry.env_project`.
   - Recommendation: (a).
3. The command that removes a project from an env.
   - (a) Operation `project_env.deactivate`, request `ProjectEnvDeactivateRequest`, event code `project_env_deactivate`, CLI `wamn env deactivate-project`, label "Deactivate project in env".
   - (b) `project.remove_from_env` and matching names.
   - Recommendation: (a). The control worker selects env Jobs by the `environment.` prefix today, so the new name needs explicit routing to the env worker.
4. One verb pair for all seven entities.
   - (a) `activate` and `deactivate` everywhere. `env.inactivate`, `project.inactivate`, `connection.disable` and `connection.enable` change to match, and role gets `role.activate`.
   - (b) Keep the current verbs and add `deactivate` only where R6 replaces a delete.
   - Recommendation: (a), one word for one meaning.
5. Connection deactivation and the credential.
   - (a) Keep the `platform.credential` rows as history. Destroy the Secret versions they point to, and delete the `oauth_token` and `oauth_authorization` rows. Activation needs a new credential in a new generation.
   - (b) Change the schema so that the credential rows can go: drop the generation foreign key to the credential or let the trigger allow the delete.
   - One operation `connection.deactivate` replaces `connection.delete` and `connection.disable`.
   - Recommendation: (a), because the generation history stays intact and no credential material remains.
6. Org and project commands.
   - (a) Add `org.deactivate` and `org.activate` for the platform operator. `project.deactivate` sets only the project's `active` flag and stops changing the env status of sibling projects.
   - (b) Add the org column with no command. Keep the current project behavior.
   - Recommendation: (a). A column that nothing writes is dead.
7. Versioned contracts. The rename changes the native HTTP contract `0.9.0`, the cursor format `v: 1`, serving manifest format 11, connection descriptor 1 and the chart versions.
   - (a) Bump each one, and refuse the old versions.
   - (b) Rename without a bump.
   - Recommendation: (a), greenfield.
8. Members of a project in env that comes back.
   - (a) Deactivation deletes the project env memberships, as the cascade does today. The no-inheritance test stays.
   - (b) Keep the rows, and bind them to the instance suffix.
   - (c) Keep the rows, and restore the grants on reactivation.
   - Recommendation: (a). It keeps the current contract and adds no column.
9. Existing databases.
   - (a) Fresh install only. Edit the bootstrap SQL. Recreate the platform on `wamn-local` at the epic close kind run, which deletes its current journey data.
   - (b) Forward migrations for the system and project databases.
   - Recommendation: (a). No install holds customer data.
10. Inactive role and connection caches.
    - (a) Read `active` on each new call. This is one indexed read.
    - (b) Keep the caches, and accept the delay until expiry.
    - Recommendation: (a), because R6 requires the refusal.
