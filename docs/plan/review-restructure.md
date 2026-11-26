# wamn review: worktree-restructure

This document is the work list for wamn, from the crate cleanup to the workflow design. It states the target, the current code, and how each change is checked. Epic 1 is done and merged: `main` is at `31703d9bb`. The code facts were read before Epic 1. The cited files and symbols, and the behaviour claims of Epic 2 (R2 to R5, C2), were checked again at `31703d9bb`. Before acting on a fact, check the cited file and symbol again.

## How to read this document

| Mark | Meaning |
| --- | --- |
| **F** | Read in the code or in the cited upstream source. |
| **I** | Inferred from the code. Not reproduced. Write one focused test before building a larger remedy on it. |
| **Not verified** | Stated by a source that was not read for this document. Read it before relying on it. |

- Nothing in this document was built, run or measured. A test that exists is not a test that passed.
- A rule in this document is decided. It is not a proposal. Where a rule says "future option", nothing is built for it now.
- Do not add a controller, a policy service, a contract language or a general execution framework to carry out any rule here.

## Fixed constraints

- Orgs are separate clients. Org separation comes first.
- Environment separation comes second. Nothing that runs for dev may stop or read prod.
- Weigh cost with every isolation option.
- wamn is greenfield. Breaking changes are acceptable.
- Org and environment settings change without a platform deploy or a platform restart.
- Use stock wasmCloud and upstream mechanisms where they exist. Never patch upstream code and never maintain a copy of upstream code.
- Runs on kind or on GCP need explicit authorization before they start.
- Every cloud resource that holds tenant data, or grants on it, belongs to one boundary and is permissioned where cloud IAM grants natively: an Artifact Registry repository per org (T8), a backup bucket per environment (T7). Only the provisioning command, run by the platform operator, holds cloud IAM administration. It creates these resources and their top-level grants. A tenant account manages only inside a resource of its own boundary. Org and environment separation never rest on IAM conditions inside a shared resource.

## Order of work

| Epic | Scope | Depends on | Host image rebuild |
| --- | --- | --- | --- |
| 1 | Crate cleanup: R1, in its own document. Done at `31703d9bb`. | — | Yes: `wamn-plugin`, the host and the CDC reader change. |
| 2 | Deletion and API contracts: R2, R3, R4, R5, C2. | 1 | Yes: R2 and R3 touch host crates (`wamn-routes` and `wamn-package` are host crates). |
| 3 | State and lifecycle: C1, L1, L2, L3, L4, L5, L6, L7, L8, L9. | 2. L2 uses the shared Helm helper of R4. | Yes, for L6, L7, L8 and L9, in one rebuild at the end of the epic. |
| Rename | R6. | 3 merged. Runs before 4. | Yes. |
| 4 | Org boundary on kind: T3, T9, T12, and the kind pin and refusal tests of T13. | 1. Can run beside 2 and 3. | Yes: the non-root `USER` in the `Dockerfile`. |
| 5 | Environment boundary: T1, T2, T4, T5, T10, and the T16 items on connections and locking. | 4 | Yes: the host's `--environment` argument. |
| 6 | Public addresses: T11, and client base paths. | 5 | No (I). |
| 7 | Cloud and production: T6, T7, T8, the GKE cluster of T13, and the rest of T16. | A GKE cluster, created only with explicit authorization. | Mostly no (I). |
| 8 and later | Workflow design: W1 to W12. | Every epic above. | Yes. |

- Each item is done when its own check, stated in its section, and its refusal tests pass.
- Org separation (Epic 4) comes before environment separation (Epic 5).
- Epics 4 to 7 are required before any real customer. They are not a precondition for Epics 1 to 3.
- T14 and T15 are accepted risks. No epic builds anything for them.
- Epic 1 adds no abstraction. It deletes traits and moves code to its owner.
- Epic 2 sets no target for crate count, file size or test count, and adds no second installed-state model, schema language or universal serializer.
- Epic 3 adds no pending queue, general reconciler, or resolver that looks in several repositories.
- Nothing in the workflow design is built before Epic 8.

Two items outside the workflow design wait for it: the deletion of the unnamed NATS entry (T10), and the removal of the `ingress` NATS entry and user (I).

Every change records: the commit, the owner removed or moved, the interface exercised, the cases that ran, and any claim left untested. A deletion needs a clean build of the affected packages and the relevant existing checks, nothing more.

## 1. Structure and deletion

### R1. Crate roles, planes and owned I/O

R1 is Epic 1, and it is done (commit `31703d9bb`). Its facts, rule, steps and check are in the separate document [wamn Epic 1: crate cleanup](https://claude.ai/artifact/DibKy7Cx6B1SVEeXoUW9Le). The crate rule is in `docs/structure.md`. Code review enforces it, and no lint checks it.

### R2. Delete unused machinery

**F**, by workspace search for callers.

| Delete | Why | Keep |
| --- | --- | --- |
| `Analysis`, `Authorities`, `ReleaseFacts`, `Notice`, `Observation` (`crates/environment/src/environment/analyse.rs`) and `analyse/plan.rs::refusals` | No caller outside the module. | The live readers: `read_row`, `read_project` (`crates/environment/src/environment_sql_effects.rs`), and the installed-state and schema readers. |
| `ConnectionExecutionClosure` and the `closure` field (`crates/connection/src/connection.rs`) | One variant, `Released`. Both production constructors set it. Nothing reads it. |  |
| `EffectEvidence` (`crates/authority/src/effect_span.rs`), the `effects` field (`crates/connection/src/connection.rs`), its `Option` plumbing in the HTTP and blobstore adapters (`crates/plugin/src/bound_http.rs`, `crates/plugin/src/blobstore/host.rs`), and its own tests | Production sets `effects: None` (`crates/plugin/src/operation.rs`, `crates/connection/src/connection.rs`). Only unit tests construct it. | `EffectOutcomeGuard` and the exported outcome trace. |
| The bind counters and `linker_entry_binds`, `scope_registrations` (`crates/plugin/src/lib.rs`) | No caller. The comments describe a removed driver. | Correct the comments. |
| `DerivedEvent`, `derived_msg_id`, `verified_derived_source_event_id`, the `Derived` case of `SourceEvent`, and their tests (`crates/event/src/lib.rs`) | Nothing outside the file uses them. The host admits only the `Cdc` case. Topic publish replaces them (W7). | The `SourceEvent` wrapper, so the wire format does not change. |
| `project_env_namespace` (`crates/registry/src/name.rs`) and its validators | Left from an older namespace layout. Only tests call it (F). | `project_env_namespace_stem`, which `crates/platform-install/src/environment.rs` uses (F). |
| `affinity` and `topologySpreadConstraints` in `environment/templates/host.yaml` and the environment values | The stock host template renders neither. | `nodeSelector` and `tolerations`. |
| The Role rule `configmaps get, list` in `environment/templates/credential-access.yaml` | No host code uses it. | The Secret read. |
| The `DELETE` grant on `wamn_run.runs` to `wamn_platform`, the trigger `runs_terminal_delete_only` and its function `guard_terminal_run_delete`, and their comments (`deploy/sql/run-state.sql`) | Their one user, `prune_run_history`, was deleted in Epic 1. No code deletes from the table. `wamn_app` holds only `SELECT`, so the comment that it keeps `DELETE` is wrong (F). | The other grants on the table, and `guard_run_admission_pins_immutable`. |
| `application_release_name` (`crates/environment/src/environment/analyse.rs` and `environment_names.rs`) and `render` (`crates/environment/src/environment/write.rs`) | No caller. |  |

Leftovers to remove with the code they belong to: the `roles` loop with one legal value in the application chart. `normalize_explicit_ingress_stop` (`services/host/src/host.rs`) and `WorkloadDrain` (`services/host/src/host/commands.rs`) are live code in the host shutdown (F). They stay until the ingress code goes, and W1 deletes them. `release.manifestDigest` is not a leftover: rollback reads it (`crates/environment/src/environment/operation/rollback.rs::application`), and the chart writes it into the `wamn.package-digest` annotation (F).

**Check.** Recheck callers when editing. Compile the affected packages and their dependents. Run `effect_span::tests::every_effect_outcome_reaches_the_exported_span`. Delete tests whose only subject is the deleted code. Do not add tests that search for absent fields or count enum variants.

### R3. API contracts, field names and JS clients

**F.** A fixed native route passes through synthetic package metadata, authored JSON shapes (`crates/routes/src/host_route/control/shapes/`, read by `contract_files.rs`), separate Rust request handling (`services/control/src/control_route.rs`), a custom OpenAPI translation (`crates/codegen/src/host_openapi.rs`), generated clients, and runtime interpretation in the browser. The platform UI rebuilds routes and snake\_case to camelCase maps at run time and intercepts the generated client (`apps/platform_ui/src/host-client.ts`).

- Native Rust request, result and error types own their serialized shapes.
- A small explicit owner holds path, method, authority, operation identity and transport envelope.
- OpenAPI and clients are generated from those owners. Schemars derives the schema in line with Serde. It does not define routing or authorization.
- The platform UI uses the wire field names. The runtime renaming and the interceptor go. The public wire does not change: paths, methods, snake\_case fields, optional and null meaning, request identity, declared refusals and outcome handling stay.
- Application APIs follow the same rules for field names and labels below.

**Field names.**

- One name per field. The database column, the wire, OpenAPI and TypeScript use the same snake\_case name. Filters, sort keys, refusal detail paths and `required` lists use it too. Nothing renames a field at run time.
- The one fixed mapping is WIT, which requires kebab-case. Codegen turns `_` into `-` (`crates/codegen/src/authored_wit.rs`), and `crates/engine/src/operation/json.rs` turns `-` back into `_`. It stays.
- Delete in `web/runtime/src/wire.ts`: `FieldMap`, `NestedFields`, `fromWire`, `toWire`, the renaming step in `reviveOutcome`, and the `FieldMap` parameters of `OperationBinding`, `callOperation` and `callEach`. Delete `fieldMap` and the interceptors in `apps/platform_ui/src/host-client.ts`. `transport.ts::refusedInputMember` uses the wire path as it is. `web/runtime/test/conversion.test.ts` goes with the code it tests. The `refusedInputMember` cases in `web/runtime/test/helpers.test.ts` follow the change.
- Keep the wire types, `ResponseContract`, the outcomes and the transport.
- The header comment of `wire.ts` names `crates/codegen/src/client_ts.rs`, which does not exist (F). Correct it.
- Every JS client is generated by `@hey-api/openapi-ts` from OpenAPI: the platform UI from the native API documents, and an application from its published OpenAPI (`crates/codegen/src/generate/publication.rs::openapi`). wamn has no TypeScript emitter of its own (F), and none is added. `apps/wamn_receiving/web` still imports the output of the deleted emitter (`src/forms.tsx`, `./generated/...`), so it does not build (F). Moving it to generated clients is a later epic. R3's checks compile the platform UI only.
- This forces a host image rebuild: `wamn-routes` and `wamn-package` are host crates. The schema derives stay on the workspace's schemars 0.8, unless the baseline difference shows a change in the emitted contract.

**UI labels.**

- A field name is never display text. A screen shows the field's label.
- An application field's label and description are authored in `wamn.k` (`crates/package/src/manifest.rs::FieldText`). The default label is the field name with its underscores as spaces. A label changes no path, wire name or generated identifier.
- The operation contract carries the label (`crates/package/src/client_fields.rs`, `FieldIr.label`), but the published OpenAPI drops it: `crates/codegen/src/generate/publication.rs::object_schema` writes type, enum, nullability and required only (F). No client reads labels today.
- OpenAPI carries the label as the JSON Schema `title` keyword and the description as `description`. Both are API documentation, so they belong in the API contract, and external callers see them too. The OpenAPI is 3.1 with inline schemas (`crates/codegen/src/openapi.rs`), so the keywords sit on each property (F).
- Generated clients and UIs read labels from OpenAPI only, not from the release manifest.
- The native API's Rust types set the same keywords with `#[schemars(title = "...")]` (I). Where the platform UI shows a field, its hand-typed label (for example in `apps/platform_ui/src/application-users.tsx`) moves to those types.
- Not designed: translated labels. A label is one string.
- `visible` goes. It says whether generated tables and details show a field, and nothing reads it after codegen (F). Delete it from `FieldText` and `ContractFieldDeclaration` in `crates/package/src/manifest.rs`, from `FieldIr` and its reads in `crates/package/src/client_fields.rs`, from `crates/codegen/src/generate/operations.rs`, and its assertion in `crates/codegen/tests/manifest_authoring.rs`.
- Screen hints such as visibility, order, control type and translations are not API contract. They get a home in the generated UI design when one needs them, not as OpenAPI extension keys.

**Check.** Keep the old emitted contract as a baseline and review the semantic difference. Regenerate the clients and compile their UI consumers. Send literal requests through the native HTTP service: one allowed read and write, an echoed `request_id`, a declared refusal, a member refused an admin route, and a project admin held to its project. State the expected wire examples independently of the new types. For labels, publish a package with one field that has an authored label and one that has none, and assert that the published OpenAPI gives each field a `title`: the authored label, and the default label.

The two live client tests (`apps/platform_ui/test/control-client.live.test.ts`, `control-org-client.live.test.ts`) skip unless `WAMN_CONTROL_CLIENT_*` is set, nothing sets it, and they name the deleted `crates/execution-host` (F). Reconnect their assertions to a small test setup that serves the current native code, and report a nonzero count of executed cases. Do not restore `execution-host`.

### R4. One implementation of construction and Helm calls

**F.**

- Helm execution policy is split between `crates/kubernetes/src/helm.rs` (`upgrade`, `recover_pending`) and `crates/kubernetes/src/deployment_components.rs` (the explicit rollback). Upgrade waits until the Job deadline, explicit rollback uses `600s`, and recovery rollback passes no timeout. Common command, context, wait and deadline policy moves to `crates/kubernetes/src/helm.rs`. Rollback target selection stays in `crates/environment/src/environment/operation/rollback.rs`.
- `xtask/src/preflight.rs` copies private production code instead of calling it (F): `org_create_objects` copies the shapes of `crates/platform-install/src/org.rs::create` and `org_install.rs::org_job`; `api_destinations` copies `org_install.rs::api_destinations`; and it copies `platform_install.rs::registry_egress`, `crates/environment/src/environment/ensure.rs::host_values`, `lifecycle.rs::reader_values` and `operation.rs::use_pod_registry`. Preflight calls the production builders instead, made public where needed. `registry_egress` is already public. `host_values` reads the system database and the image, and `reader_values` reads the image, so each splits into its reads and a builder with no I/O, which preflight calls. The preflight checks stay.

**Check.** For a pure extraction, compile the callers and keep the existing chart render checks. If output changes, compare only meaningful rendered fields: namespace, service account, image and arguments, mounts, policy scope and artifact references. A render shows object shape, not admission, scheduling or rollback.

### R5. Architecture documents describe the current code

**F.** `docs/README.md` calls `docs/architecture` the current system. Correct these:

- `docs/architecture/execution.md` describes a warm pool of reusable instances. The application chart sets no `poolSize` on application components and `poolSize: 0` on `ingress`. Upstream keeps no instance warm when the value is unset. `crates/engine/src/engine.rs` states that allocator capacity is not reusable guest state. The "Trusted component reuse" section of `docs/operations/deployment.md` documents `--trusted-warm-component-digest` and `WAMN_TRUSTED_WARM_COMPONENT_DIGESTS`, and no code reads either (F).
- `docs/architecture/platform-services.md` says the system database supplies connection parameters. Connection rows are in the project database, written in the project transaction (`crates/connection/src/project_connections/administration.rs`). The integration catalogue is a separate, public, system table.
- The same document describes one basic-auth Secret per authority class. The project chart uses certificate logins (`deploy/charts/project/templates/cluster.yaml`, `crates/identity-protocol/src/authority_class.rs`).
- `docs/plan/wamn-restructure.md` says in one place that `ProjectDatabase` does not exist, and in another that it is a trait in `crates/connection/src/database.rs`. It is now a private struct in `crates/connection/src/project_connections/mod.rs` (F). Correct both.
- `docs/testing/` names six test paths that no longer exist, among them `crates/identity/tests/pat_live`.
- Trust wording: the environment is the shared execution trust group. The project is the data, ownership and recovery boundary.

**Check.** Trace each corrected claim to its code and check its links. Do not add source-text tests or another layer of architecture documents.

### R6. Names: singular tables and the short forms org, env and app

**F.** 25 of the 57 tables have plural names (F, at `6b4db53e1`): in registry, orgs, projects, environments, environment_projects, environment_applications, environment_integrations, environment_application_connections, event_readers, retired_project_envs; in identity, principals, org_memberships, org_roles, project_roles, project_env_memberships, password_credentials, password_logins, password_tokens, password_attempts, renewal_credentials, session_keys, nats_keys; in catalog, packages, workflows; wamn_run.runs; wamn_history.pruning_watermarks. The project platform schema is singular. org is always short. Environment is mixed: env columns, wamn env apply and EnvName, but registry.environments, environment_* tables and Environment* types. Application is always long: application columns and tables, ApplicationName, routes `/api/<application>`.

**Rule.**

- Every table name is singular, for example registry.env, identity.principal, wamn_run.run. platform."user" stays quoted, because user is a reserved word.
- org, env and app are the standard short forms, everywhere: SQL tables and columns, Rust types and modules, wire fields, routes, CLI flags, chart values and wamn.k keys. R6 changes no other abbreviation. Standard PostgreSQL, JWT, SQLx and NATS terms stay. The wamn-owned abbreviations are a T16 item. Edge SQLite tables and test fixtures are in scope (owner ruling 2026-11-25, question 1 in [r6-scoping](r6-scoping/r6-scoping.md#7-questions-for-rulings)).
- One rename pass, after Epic 3 merges and before Epic 4. It changes the public wire and the generated clients.
- The pass bumps every versioned contract that it changes, and the product refuses the old versions (question 7).
- Fresh install only: the pass edits the bootstrap SQL and adds no migrations (question 9). The kind run at the end of the epic recreates `wamn-local`. `wamn-dev` on GCP is not touched.
- org, env, project, app, role and connection cannot be deleted by standard means. They can only be deactivated (owner ruling 2026-11-25). Each of the seven has the commands activate and deactivate, role included (question 4).
  - Add `active boolean NOT NULL DEFAULT true` to the org, project and role tables. The env table (L7) and the connection table already have it.
  - Org: `org.deactivate` and `org.activate`, for the platform operator. An inactive org refuses new commands and new logins. R6 does not touch running workloads and tears nothing down (question 6).
  - Project: `project.deactivate` sets only the project's `active` flag. It no longer changes the env status of the sibling projects (question 6).
  - App: when env apply finds an app missing from the env file, it sets `active = false` on the app row (now `registry.environment_applications`, `crates/environment/src/registry/configuration.rs`). It no longer deletes the row. Adding the app back sets `active = true`.
  - Project in env (now `registry.environment_projects`, renamed `registry.project_env`, question 2): the delete command sets `active = false` and still tears down the project database (`crates/environment/src/environment/delete.rs::delete`). The row stays.
  - Deactivating a project in env deletes its project env memberships. The row history keeps them (question 8).
  - `registry.project_capacity_available` counts only rows with `active = true`. An inactive project-in-env row holds no database and no project stream (`crates/environment/src/environment/delete.rs`).
  - A project added back to an env gets a new `instance_suffix` (`crates/environment/src/environment_sql_effects.rs::reserve_project`). Its old suffix stays in the row history (L8).
  - Drop `registry.retired_project_envs` with its history table, the trigger `project_envs_retire_instance` and the function `registry.retire_project_env`. Nothing reads the table (F, at `9e05a8246` only the trigger writes it). Remove their grants in `deploy/sql/apply-grants.sql`, their entry in the protected relations list (`deploy/sql/system-schema.sql`), the entry in `test-support/grant-tests/tests/system.rs` and the mention in `docs/architecture/data-access.md`.
  - Role: the drop command (`crates/authentication/src/administration/sql.rs::DROP_ROLE_SQL`) becomes `role.deactivate` and sets `active = false`. The user-role and role-permission rows stay. An inactive role grants nothing: every permission check ignores it, and reads `active` on each call, with no cached answer (question 10). `role.activate` restores the same grants.
  - Connection: one `connection.deactivate` replaces `connection.delete` (`crates/connection/src/project_connections/administration.rs`) and `connection.disable` (question 5). It sets `active = false`. In the same command it deletes the Kubernetes Secrets that the connection's `platform.credential` rows point to, and deletes its `platform.oauth_token` and `platform.oauth_authorization` rows. The credential rows and `platform.connection_generation` stay. R6 adds no `erased_at` column and does not change the trigger `credential_immutable` (owner ruling 2026-11-25). Activating a connection needs a new credential in a new generation. A call through an inactive connection is refused. Each call reads `active`, with no cached answer (question 10). Check this, record it as F, and fix it if it is not true.
  - Rename the command for removing a project from an env, its request type and its `platform_event_type` code to match what it does (now `environment.delete`, `EnvironmentDeleteRequest`, `environment_delete`). The new names are `project_env.deactivate`, `ProjectEnvDeactivateRequest`, `project_env_deactivate` and the CLI command `wamn env deactivate-project` (question 3). Control routes the operation to the env worker explicitly, because the worker selects env Jobs by the `environment.` prefix today.
- No `ON DELETE CASCADE` where the parent has no delete path through the API. Change each such foreign key to `RESTRICT`. This covers the parents org, project, env, project in env, app, role, connection, identity principal, org membership, event reader, `platform."user"`, number sequence and run.
- Keep the cascade only where the API deletes the parent. Of the 27 cascades in `deploy/sql`, only `identity.renewal_credentials.login_id` to `identity.password_logins` has one: expiry cleanup, `password_login::prune_expired` (F, `deploy/sql/system-schema.sql:450`). Env apply deletes the app connection and integration rows itself, so no cascade serves it (F, `crates/environment/src/registry/configuration.rs:171-247`). [r6-scoping/appendix-e-cascades-installs-images.md](r6-scoping/appendix-e-cascades-installs-images.md) records each cascade as F with its delete path.
- `crates/identity/tests/identity_live.rs` deletes a project to test the cascade. Change it to assert that the delete is refused.
- `connection_event` (Epic 5) can hold a foreign key to the connection, because connections are never deleted.
- At `9cf40e569`, the only API deletes of the parents above are the four this rule changes: app, project in env, role and connection (F, a search for `DELETE FROM` outside tests).
- This forces one host image rebuild, at the kind run at the end of the epic, after the owner's go.

## 2. Consistency fixes

### C1. Open installed packages by their recorded digest

**F** for the paths, **I** for the failure.

- Deploy and reset read `catalog.packages.artifact_digest` and open that digest (`crates/environment/src/package_catalog.rs::installed_package_artifacts`).
- Environment apply reads `package_version` (`analyse.rs`), opens the version tag (`stage.rs::open`, `ensure.rs::installed_manifests`), and builds the CDC publication schemas from that manifest (`lifecycle.rs`, `deploy/charts/project/templates/publication.yaml`).
- Package tags are mutable. A publish under an existing tag runs the migration-prefix check and moves the tag (`crates/release/src/release_package.rs::push_package`). The digest is the identity of a package. The version is a tag.
- Sequence (I): deploy A; publish B under the same version without deploying it; apply the unchanged environment. Environment apply then uses B's schemas while the database records A.

**Rule.** One loader opens installed manifests by recorded digest, shared by deploy and environment apply. Version-tag lookup stays for explicit inspection of a published version. Republishing stays allowed. No new installed-state record. No host image rebuild (I). The recorded digest is the package that migrated the project database (`crates/environment/src/apply_package/package_version.rs::record_package_schema`). Deploy and env apply use it for the CDC publication schemas, and reset to rebuild the schema (F). After a failed rollout of B, B's migrations stay applied, so the record stays B while A serves. Helm records the serving release. An equal digest skips nothing: `ApplyOutcome.changed` is never read (F).

**Check.** One journey test with a test-owned fixture and artifact namespace on the real registry: deploy A; add a migration and model with a second schema and publish B under the same version without deploying; confirm the tag names B and the installed record names A; run `wamn env apply` unchanged; assert that the installed digest, the publication's schemas and the stored CDC schema configuration all still describe A. The test must fail on the old reader and pass on the shared loader. The scripted OCI test server cannot tell a tag from a digest request, so it cannot show this failure. Use the real registry path.

### C2. Keep envelope shaping at its boundary

**F.** `crates/engine/src/operation/json.rs` mixes ordinary recursion with special cases for `encoded-json`, `outcome`, `value`, `page` and `changes`, and the change feed record. A test holds that a text field named `outcome` stays an ordinary field.

- Apply the platform envelope and change feed rules from the operation context. Keep ordinary recursive conversion mechanical.
- No conversion plan registry, manifest flags or universal serializer.

**Check.** Keep the five existing codec tests. Adapt the feed test to call the feed boundary after extraction. If the request path changes, keep the write-log journey's delete response and stored-result assertions. The authored type reader refuses nested WIT variants and results (`crates/codegen/src/authored_wit.rs`), so do not invent such cases.

## 3. Lifecycle changes

### L1. The registry is the one authority for applied environment configuration

**F.**

- `env apply` writes the configuration twice (`crates/environment/src/environment/lifecycle.rs::configure_project`). First Helm applies the project chart, whose ConfigMap `<name>-operations` holds the whole environment file (`deploy/charts/project/templates/operations.yaml`). Last, `save_deployment` writes the registry rows (`crates/environment/src/registry/configuration.rs::write`), after the database, certificate, grant, CDC and stream steps. A failure between them leaves the two at different contents.
- The deploy worker reads the ConfigMap as `/etc/wamn/environment/environment.yaml` (`crates/environment/src/environment/worker.rs::execute`). Control's connection routes read the registry (`crates/connection/src/project_connections/administration.rs::required_connection_version`).
- The registry holds everything a deploy needs except `revocation_seconds` (`environment/document.rs` rebuilds it as `None`; `environment/write/plan.rs` requires it).
- `EnvironmentDocument::read` has no caller.

F. `env apply` saves the applied configuration once per project, at the end of `configure_project` (`lifecycle.rs`, `save_deployment`). `readiness_budget_seconds` and `drain_bound_seconds` are already columns of `registry.environments`, and each project's save writes them again (`registry/configuration.rs::write`). `revocation_seconds` has no column. `env delete` reads the drain bound from the environment row (`delete.rs`, `crates/environment/src/registry/deployment.rs::drain_bound_seconds`). I. After an apply that changes one of these fields and fails partway, the environment row already holds the new value while later projects still run the old one.

**Rule.**

- `readiness_budget_seconds`, `drain_bound_seconds` and `revocation_seconds` live on the environment row (`registry.environments`). `revocation_seconds` gets a column there. Env apply writes the three once, after every project finishes, and no project's save writes them. The host group, the worker and `env delete` read them there. Project rows hold only project facts.
- The worker reads the applied configuration with `EnvironmentDocument::read`.
- The `environment.yaml` copy in the `<name>-operations` ConfigMap and its mount go.
- Database sizing stays operator input, in the file and the project release values.
- "Applied" means: a project's row once that project finishes, and the environment row once every project finishes. A failed apply leaves the environment row at its old values, and `env apply` is run again.
- No host image rebuild (I).

**Check.** Extend an existing env-apply and deploy journey: apply a changed value, run a worker operation, observe the value, and confirm the worker no longer needs the mount. `host_contracts::a_revoked_grant_is_refused_within_the_revocation_lifetime` already changes `revocation_seconds`.

### L2. A failed rollout is rolled back at once

**F.**

- `crates/kubernetes/src/helm.rs::upgrade` does not roll back on failure. `recover_pending` clears only pending releases.
- After a failed rollout the WorkloadDeployment holds the new release while the old replica set keeps serving.
- Three readers treat the new release as serving: the egress NetworkPolicy, built from the new release's ports only (`deploy/charts/application/templates/network-policy.yaml`); `connection.delete`, which takes bindings from the WorkloadDeployment spec (`services/control/src/environment_status.rs::applications`); and `environment.status` with the default rollback target (`operation/rollback.rs::target`).
- Grants are additive only (`crates/environment/src/environment_sql_effects.rs::write_runtime_grants`).
- While a deploy Job runs, the environment lock refuses `connection.delete` (`services/control/src/control_route/integration.rs`).
- wasmCloud v2.12.3 names a replica set from its template alone, so a rollback to the old template makes the serving set current again (upstream `deploymentReplicaSetName`, `TestDeploymentRollbackAdoptsExistingReplicaSet`). F for upstream, I for wamn: wamn's template has no value that changes per Helm revision. Not run.
- The operator deletes every replica set other than current and previous before it checks that the current one is available (upstream `reconcileScale`). A new deploy on top of a failed one therefore stops the serving code. Rolling back at once prevents that state.
- A stock Kubernetes Deployment keeps old pods serving through any number of failed rollouts. This behaviour is the wasmCloud operator's.

**Rule.**

- `helm upgrade` runs with `--rollback-on-failure` in the same apply.
- `recover_pending` also rolls back a `failed` upgrade, for a Job killed before its rollback finished. A failed first install is uninstalled, as a pending install already is (I).
- `connection.delete` refuses while the application's workload is not Ready.
- Helm 4.2.2 gives the rollback the same timeout as the upgrade (`pkg/action/upgrade.go::failRelease`). The upgrade timeout is therefore at most half of the time left before the Job deadline.
- The shared Helm helper of R4 owns this budget.
- If the time left after the SQL and the image checks is below a minimum, the deploy stops before `helm upgrade` starts. The minimum is set after measuring a rollout on kind.
- When the rollback also fails, apply reports the original failure and the rollback failure.
- wamn promises nothing about earlier code on a migrated schema. Schema compatibility is the application developer's decision. Rolling back code does not undo a migration.
- The automatic rollback returns the previous code on the migrated schema. When that code does not work there, the developer migrates the schema forward.
- The developer chooses the rollback target with `wamn rollback --revision`. Without it, rollback picks the newest earlier deploy (`rollback.rs::target`) (F).
- A guide for application developers covers breaking schema changes and how package versions mark them. No such guide exists in `docs/` (F). It is written in Epic 3.
- Deploy and rollback do not report Ready until the new template is live (D15, moved from T16 by the owner, 2026-11-25). Without it, a failed release can look Ready on the old replica set, and no rollback runs. The WorkloadDeployment and WorkloadReplicaSet CRDs of runtime-operator 2.12.3 have no top-level `status.observedGeneration`, so the Helm 4.2.2 watcher can accept the previous release's `Ready=True` (F, `deploy/charts/platform/charts/runtime-operator-2.12.3.tgz`).
- wamn copies no upstream code into wamn, and no upstream naming of replica sets. If the only way to tell that the new template is live needs upstream internals, the work stops and a way is proposed to the owner before any code.
- After an apply ends, the desired release is the serving release, so the policy, the delete check, status and the rollback target need no other change (I).
- No host image rebuild (I).

**Check.** One journey: a healthy release A, then a valid release B that fails after its workload is created. B's migration is one that A works with, chosen by the test. Observe A restored and serving, the failure reported, and A's replica set kept. A later successful release C in the same test shows that nothing was left pending.

### L3. Control copies authority into project databases on a period

**F.**

- A revocation commits in the system database first. Control then copies the user's current authority into each project database, one transaction per environment, and stops at the first failure (`services/control/src/control_route.rs::write_applications`, called by `ProjectAdminRevoke`, `OrgAdminRevoke`, `UserDeactivate` and the grant handlers). The route answers `application_write_incomplete` with the environments that finished.
- Repeating the route works: `crates/identity/src/identity/org.rs::revoke_project_admin` does not fail when nothing is left to delete.
- Control already runs a loop every 60 seconds that connects to each project database of its org (`services/control/src/lib.rs`, `retention::identities`).
- While a project database is down, the host cannot read grants from it either (I).

**Rule.**

- That loop also copies the current system authority into every project database of the org.
- A user row in a project database is never deleted, because created_by and updated_by on other rows name it. When a user loses all access to the project, the row stays with status set to disabled, the user's PATs get revoked_at, and the user's role rows go. A later re-add sets the row active again, with no roles. Today remove_user (`crates/identity/src/identity/application/user.rs`) deletes the row, and the delete fails for a user who holds a PAT, because `platform.pat.user_id` is ON DELETE RESTRICT (F). That foreign key stays.
- No pending table, retry record or status route.
- The route keeps answering `application_write_incomplete` at once.
- While control and a project database are up, a revocation reaches that database within one period, plus `revocation_seconds` for the host cache.
- The period is set after measuring the copy. The copy holds the org membership lock briefly (`crates/environment/src/environment_fill.rs::fill`).
- The copy writes only rows that come from the system database. It never removes a grant of a principal who still has access to the project.
- One unreachable project database does not stop the copy to the others.

**Check.** On real system and project databases: leave one copy behind, change the grant again, run one production pass, and assert that the latest state wins. If the code promises it, check that an unreachable target does not stop the others. A one-pass test does not set a bound for a long outage.

### L4. Two repositories per environment

**F.**

- A deploy builds application component references from `<org_origin>/wamn/components` and integration references from `<platform_origin>/wamn/integrations` (`crates/environment/src/environment/worker.rs::execute`). Control passes `Configuration.registry` as the org origin and `Configuration.platform_registry` as the platform origin (`services/control/src/platform.rs::execute`).
- The deploy package resolver reads packages from `PackageRegistry.artifact_base`, set to `<org_origin>/wamn/packages` from the same `Configuration.registry` (`services/control/src/lib.rs::Configuration::platform`). At deploy, the component path and `artifact_base` are in the same org repository.
- Publication reads one org origin, `WAMN_REGISTRY`, and builds `<org_origin>/wamn/packages` and `<org_origin>/wamn/components` from it (`crates/release/src/release_package.rs::RegistryPackagePublisher::publish`). Publication, the worker and control share one function for the two paths (`crates/registry/src/lib.rs::org_artifact_bases`), so the two bases are always in the same org repository (owner ruling 2026-11-25).
- An integration component's digest comes from the platform catalogue by type and version (`bindings.rs::analyse`, `registry.integration_type_version`). The catalogue has no org column. The version comes from the environment file.
- `ingress` and the workflow service come from the chart's stamped image set (`crates/platform-install/src/release_chart.rs::image_set`).

**Rule.**

- Integrations are public: an integration type and its component are platform code, the same for every org. Connections are private: a connection's row is in the project database and its credential in a Secret of that project environment.
- The org's repository holds what the org publishes. Publish mirrors into it, by digest, every shared artifact that a package pins. The package resolver keeps one base (`PackageRegistry.artifact_base`).
- The platform repository holds integration components and platform components.
- The worker takes the org origin and the platform origin separately.
- In the same change, deploy checks that every resolved image exists before it applies SQL. Today a missing image is found at rollout, after the SQL is in (I).
- Nothing is copied at deploy, at `env apply` or at platform publish.
- The web artifact goes in one of the two repositories. `wamn web push` and `wamn web upload --web` use the caller's `--artifact-base`. The push writes to that base, and the upload reads from it by digest (`services/cli/src/web.rs::run` and `upload`, `crates/release/src/web_artifact.rs::push_web_artifact` and `RegistryWebArtifacts::read_web`). (F)
- No host image rebuild (I).

**Check.** Call `write::release_values` with two distinct repository prefixes and fixed digests. Assert that application and integration references take their owners' prefixes and keep their digests, and that stamped platform references are unchanged. Do not write the expected image references by hand. Cloud pull grants need their own check on GKE.

### L5. wasmCloud v2.12.3

**F.** The pins are in place: wash-runtime v2.12.3 at upstream `8358f1f56`, wasmtime 48.0.10, and runtime-operator chart 2.12.3 in both chart locks and vendored archives. `PlatformHttp` forwards `install_egress_gate` to its ingress (`services/host/src/host/platform_http.rs`), with the unit test `the_platform_handler_checks_egress_addresses`. Connection calls use the stock `EgressAddressPolicy` (`crates/http/src/network_policy.rs`).

Remaining:

- Pin the operator image by digest. The operator image follows the chart's appVersion today, and the restructure plan requires a digest.
- On a cluster that already exists, apply the four changed workload CRDs by hand. Helm does not upgrade CRDs in a chart's `crds/` directory. Fresh kind clusters install them.
- Check two upstream behaviour changes against wamn: a plugin is now bound for a `hostInterfaces` entry that no component imports; and a service that cannot register its ingress again after a restart now spends its restart budget and retries.
- The workarounds X2, X10, X1, X5, C14 and the rollback handling stay. v2.12.3 does not cover them.
- In the Epic 1 kind run at `31703d9bb`, the `runtime-operator` pod restarted 3 times during the platform deploy, because it started before NATS answered, and then became ready (reported by the run, Not verified here). Whether it also happens before Epic 1 is not known. Check it with the operator digest pin.

**Check.** The plan records the v2.12.3 runs as passed on fresh kind clusters: `cargo test --workspace`, the ignored run and every journey test (`docs/plan/wamn-restructure.md`, its v2.12.3 close). Not rerun for this document. The remaining items need the old and new chart render difference and the final operator image reference. The operator image still has no digest (`deploy/charts/platform/values.yaml`, `runtime-operator`) (F).

### L6. Oversized CDC events

F. The CDC reader retries a refused publish without end and does not advance (`crates/event/src/reader.rs::publish_txn`). `max_payload` is 1 MiB. The publication covers whole schemas (`project/templates/publication.yaml`), so one row change over 1 MiB stops capture for its whole project. An unchanged TOAST value is already absent from the event's map (`crates/event/src/lib.rs::Envelope`).

**Rule.**

- The reader leaves out column values until the event fits 1 MiB: the largest first, and values of the old image before any value of the new image. Key and identity columns are never left out.
- The envelope lists its unavailable columns, for both causes: an unchanged TOAST value and a value left out for size. Each entry names the image, `old` or `new`, and the column. An image that is absent stays absent.
- Each trimmed event is logged with its subject, message id, table, omitted columns and size before trimming.
- If the key columns alone do not fit, the reader stops and reports it, as it does for a value that is not UTF-8 (I).
- The reader no longer retries a publish that cannot succeed.
- wamn does not promise the historical value of an unavailable column. A handler that needs it reads the current row.
- The size check is one length comparison on the final bytes, unavailable list included, which the reader already builds (`PreparedMsg`).
- This forces a host image rebuild: the reader ships in the host image (`Dockerfile`, `host` stage).

### L7. Flags are booleans

**Rule.**

- Each of these text flag columns becomes `active boolean NOT NULL DEFAULT true`. In the project database: `platform."user".status`, `platform.environment.status` and `platform.connection.status`. In the system database: `identity.principals.status`, `identity.org_memberships.status`, `registry.environments.status` and `registry.target.status`.
- `identity.principals.disabled_at` goes, with its CHECK. The L8 row history records when a flag changed.
- `'invited'` on `platform."user"` is a separate fact. If any code or SQL sets or reads it, the work stops and reports where. If none does, it goes.
- The schemas are edited in place. No migration.
- Every reader and writer changes, the L3 work included. L3 uses `status = 'disabled'` today. A disabled user becomes `active = false`.
- `wamn_run.runs.status` (the workflow design) and `platform.oauth_token.refresh_status` (Epic 5) do not change.
- This forces a host image rebuild, in the one rebuild at the end of the epic.

### L8. Row history on platform tables

**Rule.**

- Row history uses the existing `wamn_history.create_history_table` and `wamn_history.log_row_change('unlimited')` (`deploy/sql/record-history.sql`).
- In the project database, these tables are logged: `platform.pat`, `platform.connection`, `platform.connection_generation`, `platform.external_system`, `catalog.packages`, `catalog.connection_requirement` and `catalog.workflows`.
- In the system database, these tables are logged: every `registry.*` table, and `identity.principals`, `identity.org_memberships`, `identity.org_roles`, `identity.project_roles`, `identity.project_env_memberships` and `identity.password_credentials`. The record history functions are installed in the system database if they are not there.
- These tables are not logged: `platform.credential` (Epic 5, after the hint fix), `platform.oauth_token`, `platform.oauth_authorization`, `identity.session_keys`, `identity.nats_keys`, `identity.password_tokens`, `identity.password_attempts`, `identity.renewal_credentials`, `identity.session_signing_state`, the write log, `number_counter`, the run queue and the run state.
- Grants follow the existing pattern of the logged project tables.
- `registry.environment_application_connections` has no history until R6 gives it a short singular name. `create_history_table` refuses its 67-byte derived constraint name with `history-name-too-long` (F, `deploy/sql/record-history.sql`). The history naming rule does not change (owner ruling 2026-11-25).
- Schema reset empties every catalog table, the catalog history tables included (`crates/environment/src/environment_sql_effects.rs::reset_database`). The reset itself is recorded as a `schema_reset` row in `platform_event` (L9).
- This forces a host image rebuild, in the one rebuild at the end of the epic.

### L9. Platform events

**Rule.**

- `platform_event` is an immutable table in the system database. A trigger refuses every UPDATE and DELETE. No retention job touches it.
- `event_type` and `outcome` are foreign keys to `registry.platform_event_type` and `registry.platform_event_outcome`. Each has `code text PRIMARY KEY` and `label text NOT NULL`. The outcome table also has `success boolean`, NULL for `started`. The schema seeds both tables.
- Event types: `org_create`, `environment_apply`, `environment_delete`, `deploy`, `rollback`, `schema_apply`, `schema_reset` and `capture_gap_repair`.
- Outcomes: `started`, `succeeded`, `failed`, `rolled_back` and `refused`.
- Columns:
  - `id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY`
  - `occurred_at timestamptz NOT NULL DEFAULT clock_timestamp()`
  - `command_id uuid NOT NULL`
  - `event_type` and `outcome`, the foreign keys above
  - `principal_id uuid NOT NULL`, a foreign key to `identity.principals`
  - `org text NOT NULL`, `env text NULL` and `project text NULL`, with no foreign key. Each has a CHECK on the registry identifier pattern (`crates/registry/src/identifiers.rs`).
  - `application text NULL`, with no foreign key
  - `package_digest text NULL`, in `sha256:` form
  - `helm_revision integer NULL`, `job_namespace text NULL` and `job_name text NULL`
  - `failure_summary text NULL`, at most 500 characters, written by wamn on a failed end row only
- Each command writes two rows with the same `command_id`: outcome `started` when it begins, and its result when it ends.
- A command is recorded even when it fails before, or because, its org, environment or project is registered (owner ruling 2026-11-25).
- The provision-org Job records `org_create` as the seeded principal `wamn:provisioning`. The system schema seeds that principal before any org exists, and the provisioning login binds it (F, `deploy/sql/system-schema.sql`).
- Indexes: `(command_id)`, `(org, env, occurred_at)` and `(org, project, occurred_at)`.
- If any code deletes a `registry.environments` or `registry.projects` row, the work stops and reports it. At `67bda3772` only a test deletes one (`crates/identity/tests/identity_live.rs`) (F).
- This forces a host image rebuild, in the one rebuild at the end of the epic.

## 4. Tenancy and security target

The direction is sound and close to stock wasmCloud: loading, scheduling, linking and routing are stock. The org boundary holds at the API and network level. It does not hold at the node level, and nothing in Kubernetes separates environments inside an org. The rules below close that.

### Findings and their state in code

| # | Severity | Finding | Target | In code |
| --- | --- | --- | --- | --- |
| 1 | High | No pod security on any namespace. The worker can create Deployments and Jobs, so a fault in platform code or tenant input reaching a pod spec could run a privileged pod (I on reach). | T3 | Open. `org/templates/namespace.yaml` sets only `wamn.dev/org`. |
| 2 | High | The host could not read connection Secrets. | The host pod reads them with its own token. | Done: a projected token, read with kube-rs (`crates/kubernetes/src/client.rs::host_credential_secrets`). The Role's ConfigMap rule is deleted by R2. |
| 3 | Low | An unnamed `wasmcloud:nats/jetstream` entry on every workload (`crates/environment/src/application_chart/plan.rs::roles`). Its NATS user cannot publish. | Delete the entry (T10). | Narrowed to the project's stream and subjects. `ingress` needs it for its stock handler export until the workflow design. |
| 4 | High | One org-wide account `wamn-operation-<org>` runs every write Job and can delete the default-deny policy (`org/templates/rbac.yaml`). | T4 | Open. `pods/exec` and `wait_administration` are gone. |
| 5 | High | The system database is one instance with no backup, and holds the NATS operator root and every account key. | T6 | Open. |
| 6 | High | One backup credential for all orgs (`project/templates/cluster.yaml` ObjectStore). | T7 | Open. |
| 7 | Medium | Environments share the org namespace, one quota, one worker. | T1 | Open. |
| 8 | Medium | The quota counts objects only. No LimitRange. Control and the CDC reader set no requests. | T5 | Open. |
| 9 | Medium | In `platform` only identity has an ingress policy. NATS, the system database and the Poolers accept any pod. | T12 | Open. |
| 10 | Medium | The public web proxy lists every ConfigMap in the cluster on every request (`platform/templates/edge-rbac.yaml`, `crates/kubernetes/src/web_release.rs::Releases`, called from `services/web/src/lib.rs`). | T11 | Open. |
| 11 | Medium | NATS accounts had no limits, and creds never expire. | T10 | Partly done: connections 64, subscriptions 1,024, payload 1 MiB per project times the project count. Consumers unlimited, no revocation, `maxProjects` still used. |
| 12 | Medium | Two CA mechanisms. Helm-made CA keys sit in release records (`genCA` and `lookup` in `identity-certificate.yaml`, `system-database.yaml`). | T9 | Open. |
| 13 | Medium | A killed deploy left the release pending. | L2 | Recovery runs under the lifecycle locks. The Helm wait has no margin before the Job deadline. |
| 14 | Medium | A guest `COMMIT; BEGIN; INSERT` can forge `platform.tx_identity` inside its own project database. | Detected, not prevented (T14). | No change needed. |
| 15 | Low | Single-instance project clusters block node drain. | `enablePDB: false` when `instances` is 1. | Open. |
| 16 | Low | Host `affinity` and `topologySpreadConstraints` values are dropped by the stock template. | Delete the keys (R2). | Open. |
| 17 | High if off | NetworkPolicy enforcement is unconfirmed on kind. No GKE cluster exists. | T13 | Open. |

Sound today (F): orgs never share a namespace and `allowSharedHosts: false` is set; default-deny covers ingress and egress in each org namespace; no wildcard, `escalate`, `bind` or `impersonate` in wamn Roles; database logins use certificates with one CA per project cluster; TokenReview checks audience, service account and namespace; NATS has one inbox prefix per user and no imports or exports; no cross-org path was found through control; each host pod has its own scheduler user limited to its own subjects (`services/identity-service/src/nats.rs::sign_host_user`); the default data user cannot publish; NATS and Postgres images are pinned by digest.

### T1. Namespace layout

- `platform` and `hosts` per installation.
- `<org>` per org: the org's control service and its system database login Secrets.
- `<org>--<env>` per environment (`name::environment_key`): the host group, each project's Postgres cluster, CDC reader and certificates, the application workloads, the NATS creds, the quota, the default-deny policy and the pod security label.
- `wamn-helm-<org>--<env>` per environment for Helm release records.
- Control runs once per org, in `<org>`, and is the one actor that spans the org's environments.
- The mapping from a namespace to its org and environment is a row in the system database. Identity and control read it. Neither derives the org from a namespace name.
- Each environment has its own worker login on the system database. A twin of `registry.login_org` reads the environment from the login name, and tables with an environment column check it for worker logins. Control's login stays per org.
- The host reports its pod namespace as its environment through the stock `--environment` argument. Today it reports `settings.org` (`services/host/src/host.rs`).
- Identity checks the environment, not only the org, when a worker asks for NATS creds (`services/identity-service/src/nats.rs`).
- trust-manager copies the CA certificates into each environment namespace, selected by namespace label. CA values leave the org chart's values.
- A new environment changes no platform value and restarts no platform pod.
- With `hostNamespaces: []` and `-host-namespaces=` (`deploy/charts/platform/values.yaml`), the stock operator sends no stop to a deleted host pod in a tenant namespace (F). The plan records, measured on kind, that every `wamn env delete` then waits about 300 s while the host drains, and that the host exits 1 (X5, Not verified here). Its fix is separate design work. It must not list environment namespaces in a platform value, because a new environment changes no platform value (I).

### T2. Environment provisioning

- A CLI command run by the platform operator with the admin kubeconfig creates an environment: the namespace with its labels, the Helm storage namespace, the quota, the environment account and its Role, the default-deny policy, the mapping row, the registry read grants, and the environment's backup bucket with admin on it for the environment account. `wamn org create` creates an org the same way (`crates/platform-install/src/org.rs`). The command name is not set.
- Each environment namespace gets one read grant on each L4 repository, the org repository and the platform repository, for all its service accounts. It covers the host, the environment account and every project account. These are cloud IAM grants, and the provisioning command makes them (moved from L4 by the owner, 2026-11-25).
- `env apply` does not change. The CLI calls the org's control service, which starts a Job in the environment namespace as the environment account.
- No wamn component in the cluster gets a cluster-wide right to create namespaces. The only cluster-wide rights are token review for control and identity, and the web proxy's ConfigMap read until T11 removes it (F).
- An org owner cannot create an environment alone.

### T3. Pod security

- Every tenant namespace, `<org>` and `<org>--<env>`, and `platform` and `hosts`, enforce the `restricted` level, with `warn` at the same level so a preflight dry run shows violations.
- Nodes are shared by default. A node pool is a per-org setting, used when an org needs it.
- The host group and the runtime operator pass with the stock chart's hardened defaults.
- Needs one security context block (non-root user, `fsGroup`, seccomp, no privilege escalation, all capabilities dropped): control, the CDC reader, the Job manifest in `services/control/src/control_route/operations.rs::manifest`, identity, the web proxy, the platform Jobs, NATS and its init container, and the OpenTelemetry collector. wamn runs a separate NATS chart and sets no security context for it (F).
- Control moves off port 80 to a port above 1024 (I).
- Before enforcing on `platform`, check the whole rendered platform chart.
- Not confirmed: that every CloudNativePG pod passes, including init pods and the backup sidecar. The kind run shows it.
- The namespaces of cert-manager and the CloudNativePG operator are owned by their own charts and are not covered.

### T4. Job accounts and the admission policy

- The org-wide account `wamn-operation-<org>` goes. Job accounts and their Roles live in `<org>--<env>`.
- Environment apply and delete run as one environment account per environment namespace, with today's rights and no `pods/exec`.
- Deploy, rollback, stop, schema apply, schema reset and capture-gap run as the project's own account, `<project>-operation`. It names its login Secrets and workloads and holds no Role, Deployment or Job rights.
- Control decides who may deploy which project (`control_route.rs::admits`). The accounts limit what a faulty Job can reach.
- Every project account can list the Helm records of its environment. They hold chart values, not keys (I).
- One ValidatingAdmissionPolicy and one binding, installed once by the platform chart, cover what RBAC cannot limit for accounts named `<project>-operation`:
  - no `secretFrom` and no `configFrom` in a WorkloadDeployment;
  - the egress NetworkPolicy has the chart's exact shape;
  - the Service is `ClusterIP`, with no selector and no external address;
  - a workload refers only to the Service with its own name, `<release>-http`.
- The policy needs no data and has no parameter. The binding selects namespaces by an environment label. It covers create and update of WorkloadDeployment, Service and NetworkPolicy. Delete stays with RBAC.
- The policy runs in Warn mode on kind while it is built, and in Deny mode before any real customer. In Deny mode, a request the policy cannot evaluate is refused. Its CEL expressions use `has()` guards, so a schema change in the upstream wasmCloud resource type makes a check miss rather than fail. Such changes come only with chart upgrades, which go through review and a kind run with the admission refusal test. The account filter is simple text matching that cannot fail.
- Projects of one environment trust each other at run time. The policy does not check the project in workload config, the stream and subject grants, route hosts or object names. A defect in a deploy Job could therefore bind a workload to another project of the same environment.
- Future option: make the plain key `wamn.project` the one source the host reads (`crates/plugin/src/lib.rs::PROJECT_CONFIG_KEY`), drop org, project and environment from the JSON in `wamn.application` (`crates/application/src/application_config.rs`), and add the project check with data. The host reads both today and never compares them. That forces a host image rebuild.
- The policy text is not written.

### T5. Quotas

- A compute quota on every environment namespace: `requests.cpu`, `requests.memory`, `limits.memory`, a LimitRange, and requests on every wamn pod.
- Quotas are data in the system database, applied by the provisioning command. A change needs no chart upgrade and no restart.

### T6. System database and key custody

- The system database gets a backup. Production runs more than one instance. During development it runs one.
- The NATS operator root key stays in the system database. The signing-key layer goes: `operator-signing`, `system-signing`, `platform-signing` and `environment-signing` in `identity.nats_keys`. Revocation entries then clear only when the account key is rotated (T10).
- Identity is trusted with every key. The key table is encrypted under a cloud KMS key on GCP, and that key outlives the cluster so a restore works.
- The system database's `max_connections` is set once, with headroom. `_system-database.tpl systemDatabaseMaxConnections` goes. The Pooler totals stay chart values, and a change is a reload.

### T7. Backups and files

- No org may read or delete another org's backups, and dev may not reach prod's.
- Postgres pods reach the bucket through GKE Workload Identity Federation, with no stored key.
- One backup bucket per environment, with uniform bucket-level access. Each database cluster writes under `<cluster>/wal/`. No managed folders. Each project database is its own cluster, so each prefix belongs to one project in one environment. Managed folders can be added later on the same prefixes with no data move.
- The provisioning command (T2) creates the environment's bucket. Org and environment separation rest on the bucket boundary, not on IAM conditions inside a shared bucket. The system database backup uses the same mechanism under `platform`. Whether env apply makes any cloud IAM change depends on the grant scope below.
- Open in the chart (F): `project/templates/cluster.yaml` still renders `accessKeyId` and `secretAccessKey` and the path `s3://<bucket>/wal/<name>`; `crates/environment/src/platform_environment.rs::database_values` copies endpoint, bucket and Secret from the installation settings.
- A restore runs as a new cluster `<name>-restore` in the same environment and reads the source prefix in the same bucket. Its access follows the grant scope below.
- The pods need an egress rule to the metadata server.
- Needs one backup and one restore on GKE. Fallback if the plugin path fails: a cloud service account and key per environment.
- Grant scope (open, Epic 7). The host calls the metadata server at run time for its KMS key (`services/host/src/host.rs`, `wamn_key::open`) (F), so one grant to every service account of the environment namespace would let host pods, which run tenant code, read and write backups. Options: one bucket grant per database cluster account (env apply then makes one binding per project); a separate namespace for the environment's database clusters, with one grant made by the provisioning command; or one shared CloudNativePG service account, if CloudNativePG allows it (Not verified). Decided with the GKE check.
- On kind, backups stay off.

**Files.** Application files go through a blobstore connection, an integration of type `gcs` or `s3` (`crates/connection-type/src/lib.rs::blobstore_v1`) (F).

- The endpoint, bucket, prefix and credential are environment-owned. The author names only an object key. The host keeps every key under the prefix (`crates/blobstore/src/confinement.rs::resolve_key`) (F).
- A connection is private to its project, and each application binds its own connections in the environment file. The host checks the binding before it opens a store (`crates/blobstore/src/files.rs`) (F).
- To keep two projects' files apart, or two applications' files within one project, give each its own connection. Separation rests on cloud IAM only when each connection's credential reaches only its own bucket or prefix. With one shared credential it rests on the host's prefix check.
- The org supplies the bucket and credential. wamn creates none (I). The application developer guide states one connection per separation need, each with its own scoped credential.
- Future option: platform-made file storage by default for every application, with the same pattern as backups.

### T8. Registry and deploy credentials

- One repository per org, and one shared platform repository (L4). Artifact Registry grants access per project or per repository only.
- The caller's machine pushes with a registry token of about 15 minutes, handed out by the org's control service. The deployer holds only a wamn token.
- A registry token cannot be narrowed after it is issued (Credential Access Boundaries exist for Cloud Storage only). So a separate cloud identity per org, with write on that org's repository only, issues it. Control's own identity is not used, because on GCP it also opens the credential encryption key.
- The org's publish identity can read the platform repository, for the mirror of pinned shared artifacts.
- `wamn org create` makes the org's repository and its publish identity. The read grants are made per environment (T2).
- Git-style deploys, when built, use token exchange: CI presents its platform's identity token, identity checks it against trusted issuers per org, and returns a 15-minute wamn token.
- Until then, a deployer uses a PAT with a short expiry and a deploy-only role. Whether a deploy-only role exists is not verified.

### T9. Certificates and the CSRF check

- cert-manager makes the identity CA and the system database CA: a self-signed Issuer, an `isCA` Certificate and a CA Issuer, as the chart already does for the NATS CA. The `genCA` and `lookup` blocks go. No chart generates a key.
- trust-manager distributes the CA certificates. Its source namespace is `platform` (I).
- The CSRF key, its cookie `__Host-wamn-web-csrf`, its header `x-wamn-csrf`, `platform/templates/edge-csrf-secret.yaml`, `web/runtime/src/csrf.ts` and the HMAC code in `services/web/src/browser_session.rs` go.
- The web proxy checks `Sec-Fetch-Site` on every unsafe request with a session cookie: `same-origin` and `none` pass, `same-site` and `cross-site` are refused. Without that header it compares `Origin` with `Host`. A request with neither header is refused. Callers that are not browsers use bearer tokens, not the session cookie, so the refusal does not reach them.
- These rebuild the web proxy image and the web runtime, not the host image.

### T10. NATS

- One NATS deployment carries scheduler and data traffic. A split is taken only if scheduling is measurably delayed by data traffic. The stock chart keeps it available through a separate data address.
- Each environment's account limits are data with defaults, changed without a redeploy: connections 64, consumers 2,000, subscriptions 10,000, payload 1 MiB, and two streams and 512 MiB of disk per project. None is measured. Identity re-signs and pushes a change.
- `global.maxProjects` goes, with `registry.meta.max_projects`, `registry.project_capacity_available`, the check in `crates/environment/src/environment_sql_effects.rs`, the identity setting (`crates/identity/src/identity/nats_store.rs`), the control argument and the chart arithmetic. The platform value is the NATS store size in bytes, holding two streams per project. Identity refuses a storage limit that would overfill the store.
- Whether NATS 2.12 raises its store size by reload is not verified.
- Identity revokes users through the account's `revocations` field. Deleting a project revokes its users. Deleting an environment removes its whole account. An entry stays until every JWT it covers has expired. Host and CDC reader creds never expire, so their entries stay until the account key is rotated or expiring creds land. Entries accrue only when a project is deleted, so they grow slowly (I). No signing-key layer is kept to clear them: it adds key management that is not worth it before expiring creds.
- The stream-admin and recovery Job users are not stored as Secrets. The Job makes a key in memory and asks identity for a JWT that expires with the Job.
- Host and CDC reader creds do not expire. Expiry with rotation is a future option and needs a host image rebuild, because wamn's scheduler connection and the CDC reader read the creds file once (`crates/broker/src/nats.rs::credentials_options`).
- Identity gives a project's account only its own project's recovery user (I).
- The unnamed `wasmcloud:nats/jetstream` workload entry goes when `ingress` stops handling Data change (W4).
- NATS has no CPU or message-rate limit per account. A flood from one org can slow scheduling and other orgs.
- During a NATS outage the host keeps serving requests that need no NATS (`tests/journeys/tests/nats_outage.rs`) (F). The web edge does not: the stock runtime-operator drops the application and places it on the host again about 3 minutes after the outage begins, so the edge outage outlasts the NATS outage (measured on kind and recorded in the plan, Not verified here). The plan's remedy before any real customer is a NATS cluster of 3.

### T11. Web proxy and public addresses

- The proxy keeps its route table in memory and follows changes with one watch on Services and one on Namespaces, reading only names and labels. Route data moves to annotations on the `<app>-http` Service. The proxy's ClusterRole drops ConfigMaps. The two ConfigMaps per application in `application/templates/web-release.yaml` and `crates/kubernetes/src/web_release.rs::Releases::read` go.
- The prod marker is a label on one environment namespace per org, set by the provisioning command from the registry row, which stays the record. The command refuses a second prod environment in an org. The proxy reads the label from its Namespace watch. Route annotations, which the project account writes, cannot mark prod. The Namespace watch shows the proxy nothing new, because every Service already carries its namespace name (I).
- Prod is `<org>.<domain>`. Every other environment is `<env>.<org>.<domain>`. No public address has two hyphens in a row. Internal names keep the double hyphen.
- The path is `/<project>/<application>`. The API is `<host>/<project>/<application>/api/<route>`. A caller that is not a browser uses the same address with its token.
- The proxy checks the environment from the name: a route in `<org>--<env>` may claim only `<env>.<org>.<domain>`, and only the environment marked prod may claim `<org>.<domain>`. Names may not contain two hyphens in a row (`crates/registry/src/identifiers.rs`), so `<org>--<env>` splits cleanly.
- The label setting goes: the `label` column, `UNIQUE (org, label)`, `application_host` (`crates/registry/src/name.rs`), `MAX_APPLICATION_LABEL_LEN` and the route label in `crates/environment/src/application_chart/plan.rs`.
- The default application is a setting of the user. After login at the org address a user lands on it, or chooses when there are several.
- To design: the landing page and chooser, listing only applications the user may use; the fixed root paths (`/api/`, `/config.json`, `/password/`, `/oauth/callback`, `/wamn_control/`) as names a project may not take; and what a session's active role means across projects. The certificate is decided: `wamn org create` makes one wildcard certificate `*.<org>.<domain>`, with `<org>.<domain>` as a second name, through GCP Certificate Manager and a certificate map, beside the org's repository. On kind, cert-manager's local CA Issuer signs it. No platform redeploy.
- One origin per org and environment: one login covers every application there, and a script fault in one application can act in the others (I).
- Dev cannot act on prod in the browser: a `__Host-` cookie is not shared with a subdomain, and the CSRF check refuses `same-site` requests.

### T12. Network policies

- `platform` gets one default-deny ingress policy, and one allow per service naming its real callers: NATS, the system database, the Poolers, the runtime operator and the trace collector. The allow list comes from the tenant egress rules plus the CloudNativePG operator reaching the system database pods (I). It is not written.
- `hosts` gets one egress policy for the web proxy: host group Services, identity, the API server and DNS only.

### T13. Cluster versions and NetworkPolicy enforcement

- Not pinned today (F, at eec9f3bf4): mise.toml has no kind entry, deploy/kind/kind-config.yaml sets no node image, and docs/operations/deployment.md names kind 0.32. Pin kind 0.33.0 with a checksum in mise.toml, set kindest/node:v1.36.4 by digest in deploy/kind/kind-config.yaml, and name kind 0.33 in docs/operations/deployment.md.
- Kubernetes stays on 1.36. Moving to 1.37 needs Helm 4.3, cert-manager 1.22 and CloudNativePG 1.31.
- The kind smoke run gets a refusal test: from a pod in one org, connect to a host and to a Postgres pod in another org. Both must fail.
- The next GKE cluster enforces NetworkPolicy from creation. Dataplane V2 can be chosen only at creation. The create command goes into `docs/operations/gcp.md`.

### T14. Transaction identity inside a project database

- A forged transaction identity and an early guest commit are detected, not prevented: the host's finish step fails the call and drops the connection.
- A project's own code can falsify who wrote a row, and can leave part of a call stored when the rest fails.
- Future option, not built: only the host sets the identity, through a definer function with a per-connection secret, and a deferred constraint trigger on `platform.tx_identity` fails any commit the host has not marked finished. Not tested.
- Not traced: what an unfinished idempotency record does on a retry after an early commit.

### T15. Host trust and the operator

- A compromised host can report another org's environment in its heartbeat and be given that org's workloads and traffic. The operator copies a host's environment from the heartbeat and places workloads by it (upstream `host_controller.go`, `workload_controller.go::hostEnvironmentFilter`). Per-pod scheduler users do not stop this. This risk is accepted, like shared nodes.
- An operator per org, using the stock `-watch-namespaces` flag, is a per-org setting beside the node pool. It is built and tested when an org needs it. Whether two operators run cleanly in one cluster is not tested.
- Per-pod scheduler users stay. They stop one host taking over another host's id and commands.

### T16. Before any real customer

From the restructure plan's open items (`docs/plan/wamn-restructure.md`) (F for the plan text). Each is designed in its epic.

- Connection budget per org (Epic 5). The Pooler per-login pools and the computed `max_connections` (206) cap the org count near 12 per system database (wamn-44p7o). Identity could not get a database connection under parallel journey load (measured on kind, Not verified here). Decide identity's pool, and whether its key route reads the database per request, with the budget. The budget lands before, or together with, T1's per-environment worker logins, which add logins per org.
- One lock per environment (Epic 5). `ensure_environment` takes a lock shared by every environment of the installation (`crates/identity/src/identity/nats_store.rs`), so env applies are serialized across all orgs. The NATS account revision check must stay under the lock.
- The connection credential hint (Epic 5). crates/connection/src/administration.rs stores the last four characters of the raw credential string as hint, and every connection response returns it (F). For a JSON credential that is "} and two characters of the secret, and a unit test expects it. The hint goes, or becomes a value that shows no secret characters. Connection deactivation (R6) keeps the credential rows, so the hint stays after deactivation (owner ruling 2026-11-25).
- Connection responses (Epic 5). The connection handlers still send to_jsonb(c) plus extra columns (crates/connection/src/project_connections/administration.rs), and wamn_routes::host_wire::ConnectionSummary only describes that JSON (F, at eec9f3bf4). A new column changes the wire with no type change, and the OpenAPI, which allows no other fields, is then wrong. The handlers build ConnectionSummary from the row, so the type owns the shape.
- Connection parameter schemas accept undeclared fields (Epic 5). The api_key, oauth2_client_credentials, oauth2_authorization_code and aws_sigv4 parameter schemas in deploy/sql/system-schema.sql set no additionalProperties: false, so the validator (crates/connection/src/administration.rs, validates) accepts extra keys, and every connection summary returns a secret placed in parameters. These four schemas must reject undeclared fields.
- System schema migrations (Epic 7), before any install that must survive an upgrade. The system schema loads only when it is absent, and no system migration exists.
- A fleet rollout command (Epic 7), run after `wamn platform deploy`. It applies every org and environment again, records each environment's bundle digest, and can resume.
- NATS as a cluster of 3 with three-replica streams (Epic 7).
- A PriorityClass for production environments (Epic 7).
- Readiness probes for control, which needs a health route, and for the CDC reader, which needs a check that its replication session is open (Epic 7).
- X4 (Epic 7): write-log expiry has no production caller.
- CDC session failure classes (Epic 7). `services/cdc-reader/src/replication.rs::classify` searches the error text for "invalidat" before it reads the error type (F). A configuration error that names a column such as `invalidated_at`, for example a value that is not UTF-8, is then treated as a capture gap, not as a fatal error. Match the typed errors first.
- Unread project schema report (Epic 7). `crates/environment/src/environment_sql_effects.rs::read_project` runs `platform_migrations::observe_project` on every project read and stores it in `analyse::ProjectState::schema_report`, which no code reads (F). Delete the field and its query.
- Lost connections on kind (Epic 7, wamn-c1zre, owner ruling 2026-11-25). `application_routes_kind` failed in 4 of 9 runs on the Epic 3 bundle: two 503 answers (host pool checkout timeout) and two 401 answers. A probe in the host pod showed TCP timeouts to the project database on port 5432 while DNS, the Service endpoints, the primary's Ready state and the network policies allowed the connection (F). Every one of 16 probe timeouts fell inside a burst of `"Could not receive message" error="netlink receive: no such file or directory"` from kindnet's network policy engine (kindest/kindnetd:v20260528-9350166c, kind v0.32.0) during its nftables syncs (F). The cause is kind's network (I). The 503 is the correct answer to a lost database connection, so the journey gets no retry. Failures occurred at 2 to 6 and at 6 to 10 orgs, so a full cluster is not needed (F). After R9, every netfilter netlink socket in `/proc/net/netlink` and queue 101 in `/proc/net/netfilter/nfnetlink_queue` showed 0 drops and 0 user drops, on a kindnet pod with 0 restarts (F). The kindnet errors are ENOENT ("no such file or directory"), not ENOBUFS, so larger netlink buffers do not help (F). The likely cause is a verdict sent for a packet that is no longer queued, similar to netfilter bug 1766 (kube-network-policies issue 12, kind issue 3713) (I). The fix is outside wamn: a different network policy engine on kind for journeys (for example Cilium or Calico), or a kernel with a fix.
- A failed signing-key fetch answers 401 (Epic 4). `crates/authentication/src/route_authentication.rs::PlatformRouteAuthenticator::authenticate_session` maps every `verify(token)` error to `unauthorized()` with no log line (F, on main since be8564fe6a). A new host fetches the JWKS on its first session request (`crates/authentication/src/access_token.rs::AccessTokenVerifier::verify`), so a lost fetch answers 401 (I, the probable cause of the two 401 answers above). A failure to fetch the key answers 503, logs a warning and writes an `authentication_event` row. An invalid token stays 401. No `authentication_event` table exists today (F). The fix forces a host image rebuild.
- The host trace sampler (Epic 7, durable traces). The host samples 10% of traces through the constant `TELEMETRY_VARIABLES` in `crates/environment/src/application_chart/plan.rs` (F), which control writes into each host's chart values, so no values file changes it. The sampler becomes a platform setting.
- Hosts send no traces or logs to the kind collector (Epic 7, durable traces). During runs R6 to R9 of wamn-c1zre, the kind collector received only metrics, and its file exporter wrote nothing (F). Find out why.
- wamn-owned abbreviations (epic not set, owner ruling 2026-11-25). R6 keeps `pat`, `tx_identity`, `auth_type`, `meta`, `ops`, `ref`, `max_`, `cdc` and the `e-` prefix. A later pass decides whether each one becomes a full word.
- X5 is in T1.

## 5. Threat model

In scope: the host, control, identity, the web proxy, the CDC reader, the six charts, and NATS and Postgres as wamn configures them. Out of scope: the edge device, `wamn dev`, and wasmCloud's own sandbox.

### Trust decisions

| Question | Answer |
| --- | --- |
| Is component code hostile to other projects of its environment? | No. Projects of one environment trust each other at run time. Inside a project, a forged identity is detected, not prevented. |
| Are orgs hostile to each other at the node? | Yes. Pod security on every tenant namespace. Nodes shared by default, a node pool per org as a setting. |
| Is a host pod inside the control-plane trust? | No. Publish rights are narrowed. The false heartbeat is accepted by default. An operator per org is a setting. |
| Is identity the one point of trust for every key? | Yes. A backup, more than one instance in production, and a KMS key that outlives the cluster. |
| Is the environment a Kubernetes boundary? | Yes. A namespace per org and environment. |

### Security goals

| # | Goal | Today | After the rules | How to check |
| --- | --- | --- | --- | --- |
| 1 | No service account of org A reads or writes an object of org B. | Yes | Yes | `kubectl auth can-i` for every org account against another org's namespaces, and the NetworkPolicy refusal test. |
| 2 | No pod in a tenant namespace runs privileged or mounts a host path. | No | Yes | A server-side dry run of a privileged pod is refused. |
| 3 | No credential of org A opens a database, stream, backup or package of org B. | Database and stream yes. Backup and registry no. | Yes | Use org A's backup and pull credentials against org B's folder and repository. |
| 4 | Only the runtime operator publishes on `runtime.host.>`. | Yes, except each host's own ready subject. | Same | Publish with one host's creds on another host's subject and expect a permission error. |
| 5 | Nothing that runs for dev reads a prod Secret or prod data of the same org. | NATS and database yes. Kubernetes no. | Yes. Control still spans the org's environments. | `kubectl auth can-i get secret` as the account a dev deploy runs under. |
| 6 | A component reaches only its own project's database and stream. | Yes | Yes, resting on the deploy code. | A component importing plain jetstream reads another project's stream and is refused. |
| 7 | Every committed row is attributed to the caller the host authenticated. | No | No, inside one project database. | Not checked. |
| 8 | No private key or password is in a Helm record, argv, environment variable or ConfigMap. | Yes, except the Helm-made CA keys. | Yes | Decode the newest release record of each chart and search for key material. |
| 9 | Losing one volume loses no org's keys or users. | No | Yes | Delete the system database volume on kind and restore. |
| 10 | No org exhausts the CPU, memory or NATS capacity another org needs. | No | Partial. NATS has no CPU or rate limit. | Read the quota and account JWT. Load one org and measure another. |

### Accepted risks

- The environment account reads and writes every Secret in its environment namespace. It runs environment apply and delete only.
- A host can report a false environment in its heartbeat (T15).
- The runtime operator watches all namespaces. Host pods in tenant namespaces have no finalizer, and the web proxy retries.
- The account that runs Helm keeps list and watch on the non-Secret objects of watched charts.
- One NATS deployment carries scheduler and data traffic.
- The org's control login on the system database keeps a password.
- Shared platform components run across all namespaces and are trusted. Identity is trusted with every key.
- Projects of one environment share one host process. A sandbox escape or host defect in one project's component reaches the database logins of the others. A host group per project is a future option, at one more host pod per project and environment.
- Orgs share nodes by default.
- The control service spans every environment of its org.
- Host and CDC reader NATS creds do not expire. Identity revokes them.
- Host pods hold a Kubernetes service account token, against upstream guidance. It can only `get` the credential Secrets of its environment, and the pod already holds every project's database keys and its NATS creds. Mounting the Secrets as files would leave a new credential unreadable for up to about a minute. If the API server is unreachable, an uncached credential cannot be read (I).
- The environment chart includes the stock chart's private template files by path. The chart update review catches a file move.
- The migration appliers stay while the migration directories are empty. System migrations need them.
- Inside one project database, a forged transaction identity and an early guest commit are detected, not prevented (T14).
- During a rollout the old code cannot open new connections on an egress port that the new release dropped. Future option: render the ports of both releases.

### Assumptions about the cluster

| Assumption | How to verify | What fails if false |
| --- | --- | --- |
| The CNI enforces NetworkPolicy. kind uses kindnet. | The refusal test in T13. | Every network rule. Isolation falls back to authentication. |
| No org user holds a Kubernetes credential. | No RoleBinding or ClusterRoleBinding names a user or group subject. | Goals 1, 2 and 5. |
| The wasmtime sandbox holds. | Keep the pinned version current. | Every boundary inside a host pod. |
| cert-manager, CloudNativePG and the runtime operator are trusted cluster-wide. | Read their ClusterRoles after each chart bump. | Goal 1. The operator reads Secrets in every namespace. |
| Components and images are pinned by digest. | The environment chart refuses a host image with no digest. The operator, identity and web proxy images have no digest requirement (F). | A moved tag changes code with no deploy. |
| The API server checks the audience of a service account token. | A token with another audience is refused by identity and control. | The mapping from namespace to org. |

## 6. Workflow design

Built last. Nothing here is built during orders 1 to 4. Workflows are the least important part of the platform and the feature is not complete. Do not build workflow versioning, a durable effect log or new deploy refusals for workflows.

### W1. Two components

- The long-running component keeps only the loops: schedule tick, lease recovery and consumer pull. It is named `trigger`, in `apps/guests/trigger`, and sits in the workload's `service:` field.
- The workflow walk is its own component, started for each run. It is named `workflow`, in `apps/guests/workflow`, matching the WIT package `wamn:workflow`. The host calls it for a Request workflow. `trigger` calls it for queued, scheduled and message-started runs.
- `apps/guests/workflow-service` goes.
- The gain is that a trap in a run's store ends that run only. An ordinary node error or failed run is a handled result and does not stop the loops.
- In text, write "the `trigger` component" when the component is meant, because a workflow also has a trigger.

### W2. One subscribe path

- Every subscriber gets its own durable pull consumer: a workflow, or a handler that a component declares in `wamn.k`. This covers Topic and Data change.
- The host creates consumers. `trigger` pulls them with the stock `open-pull-consumer` and dispatches.
- The consumer names its target, so a topic message cannot be handed over as a data change.
- Two components may listen to the same topic. Each has its own consumer.
- `ingress` keeps HTTP only. `apps/guests/ingress/src/guest/data_change.rs`, the `ingress` NATS entry and the unnamed workload NATS entry go.

### W3. Consumers of declared handlers

- The host creates a declared handler's consumer at workload start (`services/host/src/host/workflow.rs::on_workload_resolved`). It creates what is missing and deletes nothing.
- Each such consumer carries an inactive threshold of 7 days, the stream's age limit. The NATS server removes a consumer nobody pulls for that long. An application stopped longer than that loses its backlog and starts again at new messages.
- A new consumer starts at new messages after the first deploy that declares it. Later deploys do not move it.
- A Data change handler gets one consumer per component and model, with at most three filter subjects, one per op. A topic handler gets one consumer per component.
- When a handler's consumer cannot be created, the host refuses to start that workload. The rollout then does not become available, and the old release keeps running.
- Workflow consumers are created at enable and deleted at disable.
- Building this changes host code and needs a host image rebuild.
- W12's first item is decided before this is built: how a handler's consumer changes when a release changes its subjects.

### W4. Delivery

- Delivery is at least once. The order of Data change delivery is not guaranteed: during a rollout the old and new release pull the same consumer, which allows 64 unacknowledged messages (`crates/broker/src/events.rs::event_consumer_config`). A handler that needs order compares the event with the current row.
- A deploy has no drain for one workload. Teardown marks the workload released, aborts its service, removes its routes and unbinds its plugins (upstream `engine/workload.rs::release`). Work in the aborted service stops mid-run: a Request workflow fails, a queued run waits for its lease to expire, and a handler delivery is sent again after the ack wait.

### W5. Topics

- Topic messages get a second stream per project and environment, separate from the data change stream, with the same limits as environment data: 256 MiB and 7 days, dropping the oldest message when full.
- The subject is `<topic prefix>.<project>.<application>.<name>`.
- Every application workload gets a named `wasmcloud:nats` entry `topic`, which connects as one NATS user per environment that may publish only on the topic prefix. Each workload's `subject-allow` on that entry is narrowed to its own project and application.
- A publish is the stock `jetstream.publish`. It returns once the stream has stored the message and removes duplicates by `Nats-Msg-Id`. A publish never goes through the project database, and it is not part of the database transaction.
- A new topic needs no deploy.
- The body on the wire and in the stream is bytes, passed on unread. The publisher picks the format.
- Three formulas must count two streams per project: the account's stream count and disk storage (`crates/identity/src/nats/claims.rs::account`), and the NATS store and its volume (`deploy/charts/platform/values.yaml`).
- The edge device must support topics. That work is deferred.

### W6. Topic handlers in application code

- `wamn.k` gets a `topic` block on a component, beside `data_change`: `role`, `subjects` and `host_transaction`, on by default.
- A subject is written `<application>.<name>`, with `*` and `>` allowed. Deploy adds the topic prefix and the project. A component may listen to any application in its own project.
- One consumer per component holds all its subjects. NATS refuses overlapping filter subjects in one consumer, so release planning refuses overlapping subjects inside one component.
- The component exports `wamn:handler/topic`: `handle: async func(subject: string, body: list<u8>) -> result<_, string>`.

### W7. Topic entry and publish in workflows

- The Topic entry holds subjects, typed by the author and stored in the workflow graph, and a body setting `json` or `bytes`. A workflow does not depend on any `wamn.k`.
- Its consumer starts at new messages after enable. Authoring refuses overlapping subjects in one entry.
- The workflow input is `{"subject": ..., "body": ...}`. A `bytes` body arrives as base64 text. The node contract does not change.
- A workflow publishes through the platform node component `topic-publish`, built like `blob-put`. It waits for the stream's ack. Its message id is built from `delivery-id`, `node-id` and `occurrence`. Its subject is under the workflow's own application only.

### W8. Chain depth

- Every run carries its own id, the id of the first run in its chain, and a depth (`crates/event/src/lib.rs::Causation`). A message at depth 16 or more starts no run (`crates/event/src/matching.rs`).
- The depth is carried over topics: `topic-publish` writes the run's depth plus one in a header, and `node-context` carries the depth for every node.
- Application code that publishes with the stock call drops the chain.

### W9. Data change conditions and input

- A condition that reads an unavailable column (L6) counts as passed, and the run is admitted. Today an absent column reads as null and the condition turns false without notice (`crates/event/src/matching.rs::event_context`).
- Workflow input gains the unavailable column list and the change's source identity: `identity`, `txid`, `commit_ts` and `entity`. `identity` is unique per change. `txid` alone is not.
- This forces a host image rebuild.

### W10. HTTP node retries

- A request that was not sent is retried for any method.
- An unknown outcome is retried only for GET, HEAD, PUT, DELETE and OPTIONS, or when the node has a setting saying that the receiving API honours the idempotency key. The setting's name is not set.
- Otherwise the node ends with an error saying the outcome is unknown.
- Unknown means a timeout, a lost response, another transport error, or a 5xx answer. 408 and 429 count as not applied and stay retryable (I for 408).
- The host already reports not sent, response lost, timeout and other separately (`crates/http/src/lib.rs`). The node treats them all the same today (`apps/guests/http-request/src/lib.rs`).
- Future option: the integration type declares once that its API honours the key.

### W11. Deploys and old workflows

- An unfinished run of a disabled workflow never stops a deploy. Such a run fails with `unresolvable-name` when it reaches a component that is gone.
- Workflows are migrated like any other change. A component that workflows use is removed in two deploys: first it is marked deprecated and stays, then it is removed.
- wamn has no deprecated mark for a component or export today. Its form is part of this design.
- The deploy check of enabled workflows stays as it is in code (`crates/environment/src/environment_sql_effects.rs::check_workflows`).
- A recovered run starts again from the beginning, on the live release.

### W12. Not decided in the workflow design

- What happens to a handler's consumer when a release changes its subjects. Decided before W3 is built.
- An authoring check refusing a workflow whose publish subject matches its own entry subject.
- A run rate limit per workflow.
- A Request workflow gets a new delivery for each caller request, so a caller's own retry has a new key.

## 7. Edits to `docs/plan/wamn-restructure.md`

Part 1 of the plan must state the rules of this document. These Part 1 statements contradict them and must change:

| Part 1 says | Change to |
| --- | --- |
| The org is a namespace with RBAC, a quota and default-deny | `<org>` holds only control and the org's login Secrets. Quota, default-deny and tenant RBAC move to `<org>--<env>` (T1). |
| Environment: a NATS account and its own host group | Add the namespace `<org>--<env>` with its contents (T1). |
| The project is the trust unit | The environment is the shared execution trust group. The project is the data, ownership and recovery boundary. |
| Jobs that run Helm use the org worker | Environment account and project accounts (T4). |
| Environment account storage is 256 MiB per project; limits scale by project count; the install sizes JetStream from `maxProjects` | Limits per environment as data, store size in bytes (T10). |
| Application hosts use `<label>--<org>.<base>`; labels unique in an org | The public address scheme and path form (T11). |
| The CSRF cookie and header | The `Sec-Fetch-Site` check (T9). |
| One stock `WorkloadDeployment` with one workflow service component | `trigger` and `workflow` (W1). |
| Data change renders a durable subscription per (model, op) aimed at `ingress` | Pull consumers per subscriber (W2, W3). |
| One value each for the NATS, issuer and registry addresses (Part 2, ruling E6) | Two repositories per environment (L4). |
| The environment document has two owners (open item) | The registry owns it (L1). |
| Open item 3.6: nothing in the install fills the `wamn-control-administration-<org>` map that control reads its project logins from | No code reads the map (F). Delete the item. |
| Open items: pod security on org namespaces; backups with one credential per org; narrow the org worker's Role; a web Role per org namespace; org quotas | T3, T7, T4, T11, T5. |
| Open items: `tx_identity` stamps, tamper-evident or definer function | Detected only (T14). |
| Open item: split scheduler NATS from data NATS before the first customer | One NATS. The split is a future option (T10). |
| The environment values already accept `affinity` | The key goes (R2). |

Add as rules: pod security (T3), the admission policy (T4), provisioning by CLI command (T2), the failed rollout rollback (L2), the authority copy (L3), the oversized CDC event (L6), the order of delivery (W4), and the kind pin (T13). Replace "Migrations must work with the previous code" with the L2 rule: schema compatibility is the application developer's decision, and a problem is fixed by migrating forward.

Open items in Part 1 that Epic 1 made stale (F, at `31703d9bb`):

- The four dev-dependency cycles through `wamn-test-infrastructure`: `wamn-provision` is gone, and `wamn-postgres` no longer dev-depends on it. Recount the cycles and restate the item.
- X4: the write-log expiry is now in `crates/environment/src/audit_retention.rs`.
- The crate graph review item: `wamn-registry` and `wamn-package` no longer link sqlx. Delete the item.
- Crate shape: `wamn-postgres` is the kernel, with 7 portable wamn dependencies. Delete the item.
- Org, project and environment written five times in workload config: the path is `crates/environment/src/application_chart/plan.rs::roles`.

## 8. Tests

### Existing checks to keep

| Boundary | Check | What it shows |
| --- | --- | --- |
| Codec | `cargo test -p wamn-engine --lib operation::json::tests::` | Scalar, correlation, JSON and text, feed, and `outcome` contracts. |
| Effect trace | `cargo test -p wamn-authority --lib effect_span::tests::every_effect_outcome_reaches_the_exported_span -- --exact` | Outcome tracing survives R2. |
| Composed imports | `cargo test -p wamn-host --test composed_statements_link an_unnamed_statements_entry_links_every_member_import -- --exact` | The production linker resolves member imports from the artifact. |
| Workflow task state | `cargo test -p wamn-host --test workflow_service_concurrency entry_call_returns_while_cli_run_waits -- --exact` | A concurrent entry call while `wasi:cli/run` waits returns a refusal instead of trapping. Run `eval "$(cargo xtask prepare-tests)"` first: it builds the guest and sets `WAMN_WORKFLOW_SERVICE_COMPONENT`. |
| Number sequences | `cargo test -p wamn-environment --lib environment::stage::tests::sequence_defaults_use_map_names_and_refuse_conflicting_declarations -- --exact` | Default resolution and conflicts. |
| Native API | `cargo xtask platform-ui-openapi`; `pnpm --filter @wamn/platform-ui lint`; `pnpm --filter @wamn/platform-ui test` | Generated documents and consumer compatibility. |
| Client transport | `pnpm --filter @wamn/web-runtime test`; `pnpm --filter @wamn/web-runtime lint` | Response classification, request identity, cancellation, credentials and session. |
| Chart render | `crates/environment/tests/release_chart_render.rs` | Chart object properties. Needs Helm. |
| Platform egress | `the_platform_handler_checks_egress_addresses` in `services/host/src/host/platform_http.rs` | The address check reaches the ingress. |

Structural selection: `cargo tree --locked -e normal -p <consumer>`, and `cargo xtask test-changes --base <commit>` (it prints commands and runs none).

### Journey tests to rerun when their behaviour changes

Run as `cargo test -p wamn-journey-tests --test <target> <name> -- --ignored --exact`. An exact filter can pass with zero matching tests, so record the executed case names and the skipped and ignored counts.

- `application_routes_kind::a_command_numbers_without_a_record_and_with_a_key_map` and `an_overlay_insert_inherits_numbering_and_actor_stamps`: deployment creates the number sequence rows. Fixture setup must not insert them.
- `write_log::a_claim_commits_with_its_work_or_not_at_all`: the delete response and stored result.
- `host_contracts::a_revoked_grant_is_refused_within_the_revocation_lifetime`: the cache lifetime and revocation.
- `host_contracts::application_stop_leaves_its_host` and `env_apply_stops_an_unlisted_application_and_keeps_its_data`.
- `nats_outage::the_host_serves_through_a_nats_outage_and_recovers`: run alone, because it stops the shared NATS. During the outage it calls the host pod through a port-forward and logs the web edge status. After NATS returns it waits for the operator, a newer `lastSeen`, the first application's workloads Ready, a call through the web edge, and a second application Ready. The `lastSeen` sample is still taken before NATS stops (F), so a heartbeat in that gap passes that one check. Take the sample just before NATS restarts, and require a strictly newer value after it returns. The later checks already need a reconnected host (I).

### New tests

- C1: the digest journey.
- L1: a changed value reaches a worker operation without the mount.
- L2: the failed rollout journey.
- L3: one copy pass on real databases.
- L4: `write::release_values` with two prefixes.
- R3: literal native HTTP requests, the live client tests on a current test setup, and labels in the published OpenAPI.
- The kind smoke run: the NetworkPolicy refusal test (T13), and the admission refusal test, in which a project account creates a workload with a `secretFrom` entry and is refused (T4).
- One kind run with `restricted` enforced, to confirm the CloudNativePG pods (T3).
- On GKE, when a cluster exists: one backup and restore through workload identity (T7), and a check that pods other than the CloudNativePG pods cannot use the backup grant. Cloud resources and their grants are made by the provisioning command, never by a controller in the cluster.
- Two runtime operators in one cluster, when an org first needs its own (T15).

Do not build a fault-injection framework, a crash-point matrix, a WIT-wide property suite, source-text tests, or a schema compared with itself as a wire check.

## 9. Not decided

- The name of the environment provisioning command.
- The admission policy text, the `platform` ingress allow list, and the restore cluster's backup grant.
- The copy period of L3.
- Measured values for the NATS limits.
- Identity's shared request budget: password and machine requests share eight permits per identity replica. Not verified and not measured.
- Write-log claims whose outcome is unknown never expire, because the pruning has no caller. Not verified. It is the X4 open item.
- Transaction mode for control's Pooler. Whether control relies on session state is not verified.
- The two meanings of "operation": a platform action run as a Job, and an application's operation (`wit/wamn-operation-0.1`). The account name `<project>-operation` uses the first.
- More efficient message formats for the whole system, covering the data change message, which is always JSON today, and what moves between workflow nodes.

## 10. Sources

- [wasmCloud v2.12.3 source](https://github.com/wasmCloud/wasmCloud/tree/8358f1f56a66c43aedc49f30e2c5d4fa2b4f7ca1): `runtime-operator/internal/controller/runtime/workload_deployment_controller.go`, `deployment_lifecycle_test.go`, `crates/wash-runtime/src/host/http.rs` and `host/mod.rs`.
- [wasmCloud THREAT\_MODEL.md](https://github.com/wasmCloud/wasmCloud/blob/main/THREAT_MODEL.md): hosts are trusted.
- [wasmCloud operator Helm values](https://wasmcloud.com/docs/kubernetes-operator/operator-manual/helm-values/): `allowSharedHosts`.
- [Helm 4.2.2 upgrade failure path](https://github.com/helm/helm/blob/v4.2.2/pkg/action/upgrade.go): the rollback copies the upgrade's timeout and wait settings.
- [Helm CRD practices](https://helm.sh/docs/chart_best_practices/custom_resource_definitions/): CRDs in `crds/` are not upgraded.
- [Cargo feature resolution](https://doc.rust-lang.org/cargo/reference/resolver.html#features).
- [OCI Distribution Specification](https://github.com/opencontainers/distribution-spec/blob/main/spec.md#definitions): digests and tags.
- [Kubernetes multi-tenancy](https://kubernetes.io/docs/concepts/security/multi-tenancy/) and [Pod Security Standards](https://kubernetes.io/docs/concepts/security/pod-security-standards/).
- [Kubernetes ValidatingAdmissionPolicy](https://kubernetes.io/docs/reference/access-authn-authz/validating-admission-policy/).
- [kind releases](https://github.com/kubernetes-sigs/kind/releases): v0.33.0 and the v1.36.4 node image.
- [cert-manager supported releases](https://cert-manager.io/docs/releases/), [Helm version support](https://helm.sh/docs/topics/version_skew/), [CloudNativePG 1.30 supported releases](https://cloudnative-pg.io/docs/1.30/supported_releases).
- [GKE Dataplane V2](https://docs.cloud.google.com/kubernetes-engine/docs/concepts/dataplane-v2): chosen only at cluster creation.
- [Workload Identity Federation for GKE](https://docs.cloud.google.com/kubernetes-engine/docs/concepts/workload-identity), [Cloud Storage managed folders](https://docs.cloud.google.com/storage/docs/managed-folders), [Credential Access Boundaries](https://docs.cloud.google.com/iam/docs/downscoping-short-lived-credentials), [Artifact Registry access control](https://docs.cloud.google.com/artifact-registry/docs/access-control), [Service account credentials](https://docs.cloud.google.com/iam/docs/service-account-creds).
- [NATS ADR-34](https://www.github.com/nats-io/nats-architecture-and-design/blob/main/adr/ADR-34.md): filter subjects of one consumer cannot overlap. [NATS Server 2.9](https://nats.io/blog/nats-server-29-release): inactive threshold on durable consumers. [JetStream streams](https://docs.nats.io/nats-concepts/jetstream/streams): `Nats-Msg-Id` and discard policy.
- [trust-manager](https://cert-manager.io/docs/trust/trust-manager/).
- [MDN Sec-Fetch-Site](https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/Sec-Fetch-Site) and [MDN Set-Cookie](https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/Set-Cookie).
- [RFC 9110, idempotent methods](https://www.rfc-editor.org/rfc/rfc9110#section-9.2.2).
- [Salesforce Apex governor limits](https://developer.salesforce.com/docs/atlas.en-us.apexcode.meta/apexcode/apex_gov_limits.htm): trigger depth 16.
