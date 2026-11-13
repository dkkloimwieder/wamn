# 62 wamn-environment-control, part 1

Scope: `crates/control/environment` at `epic/dep7` (b94833fd). The crate is 43,549 source lines in 30 public modules, plus 25,946 test lines. This part covers the crate shape, the verb inputs (`Platform`), the environment document and where it is stored, and the `env apply` flow in `environment/mod.rs`. Nothing built or run.

## Earlier work checked first

- Tab 58 finding 6: all Kubernetes access goes through `wamn-kubernetes` (client, `kubectl` and `helm` runners). Tab 58 finding 5: use clap.
- Tab 60 finding 1: one OCI client constructor. Tab 60 finding 4 (schema lifecycle) and tab 59 finding 5 (Kubernetes manifests in Rust) were carried here; they belong to parts 3 and 5.
- Tab 61 finding 2: `env_policies` and `verification_policy` reads move to `wamn-registry`.
- Owner 2026-11-10: hierarchy org, env, project, application; env, project and application held in the platform; a project is declared in `environment.k` (owner leery of a file as the declaration). Owner 2026-11-12: the platform is far too restrictive at apply and deploy.
- Repo rule (CLAUDE.md): no new table, column, flag or environment variable unless a rule names it.

## Files read

- `lib.rs`, `Cargo.toml`, every module header.
- `environment/mod.rs` (in full); `environment/document.rs` (selection logic); `document/plan.rs` (types and checks); `document/store.rs` (non-test part, in full).
- `environment/runtime.rs`: `resolve`, `resolve_target`, `resolve_platform`, `resolve_registry`, `resolve_application`, `host_group`.
- `environment/analyse.rs::read_applied_releases`; heads and function lists of `ensure.rs`, `operation.rs`, `platform/job.rs`, `platform/operations.rs`.
- `deploy/sql/system-schema.sql`: `registry.project_envs`, `registry.projects`, `registry.env_policies`.

## Callers read

- `services/ctl/src/env_verbs.rs` (verb list), `application_verbs.rs` (`deploy`, `rollback`), `schema_verbs.rs`, `platform_verbs.rs`.
- `crates/control/dev/src/source_deploy.rs`, `dev/session.rs`, `dev/coordinator.rs` (document use).
- Every use of the four `Platform` constructors and of `resolve_platform` (grep).

## What the crate holds

Fact: the modules serve at least six purposes.

- Environment lifecycle: `environment/*`, `provision_project_env*`, `enable_cdc_project_env`, `reconcile_*`.
- Platform install: `platform/*`, `platform_build*`, `provision_system`, `provision_org`.
- Workflows: `author_workflow`, `workflow_lifecycle`.
- Operations tasks: `prune_record_history`, `prune_run_history`, `event_advisories`, `capture_gap`.
- Identity: `identity_issuer`, `invite`, `create_user`.
- Kubernetes: `kubectl.rs` (tab 58 finding 6 moves it).

## Crate-wide assessment (re-analysis, 2026-11-12, owner request)

Verdict: the crate does not have a single responsibility, it keeps copies of facts that other places own, and a large part of it is already removed or replaced by earlier rulings.

### Copies of one fact (facts)

| Fact | Places it is kept |
| --- | --- |
| Environment configuration | `environment.k`; ConfigMap `hosts/<environment>`; `registry.project_envs` (`policy_name`, `route_host`) |
| Release an application serves | `environment.k` `current`; ConfigMap `current`/`previous`; Helm history |
| Disposable environment | `registry.project_envs.disposable`; `catalog.tenant_environments.disposable` in the system database (`provision_project_env/registry.rs::project_env_key_environment`, "registry stays the authority"); `environment.k` `schema.mode` overrides it |
| Environment policy | `registry.env_policies`; `<run schema>.environment_policies` in every project database, with a source hash (`verification_policy.rs`, `reconcile_run_plane::converge_environment_policy`) |
| Platform settings (chart, registry transport, artifact bases, event settings, session issuer) | 55 `WAMN_*` variables; Helm values of `wamn`, `c-<org>` and the environment host; `cluster.k`; the operation inputs ConfigMap `wamn-operation-<org>-<hash>` (`platform/operations.rs`) |
| Platform schema of a project database | initialization SQL (digest-pinned); the run-plane reconciler, which rewrites deploy SQL, observes the database and repairs it (`reconcile_run_plane.rs`, `wamn-schema-control::run_plane`); platform migrations (empty, now the SQLx `Migrator`, tab 60 finding 5 B) |
| Application role grants | package declarations, converged to per-column grants with residue revocation (`reconcile_package_data_access.rs`) |

### Already removed or replaced by earlier rulings

| Code | Lines | Ruling | Effect |
| --- | --- | --- | --- |
| `platform_migrations.rs` | 480 | tab 60 finding 5 B | replaced by the SQLx `Migrator` (about 50 lines, inference) |
| `provision_project_env/workload.rs` and `plan.rs`; `identity_issuer.rs` and `plan.rs` | 2,130 | constraint row 7 (one login and Secret per role, restart on change); tab 59 follows | the prepare, authenticate, retire and abort of credential generations go; 319 non-test references to generations (inference: most of the 2,130 lines go) |
| `kubectl.rs` and `plan.rs` | 352 | tab 58 finding 6 | move to `wamn-kubernetes` |
| `env_policies.rs`, `verification_policy.rs` | 111 | tab 61 finding 2 | move to `wamn-registry` |
| package owner roles in `environment/ensure.rs::ensure_package_roles`; `adopted` handling in `sql/schema.rs` | small | tab 60 finding 6 A3 | go |
| `tokio-postgres`, `reqwest`, `wamn-pg-core`, `wamn-execution-contract` uses | throughout | U2, `hyper`, tabs 06 and 01 | rewritten, not deleted |

### In conflict with rulings, not yet ruled (findings 5 to 9 below)

| Code | Lines | Conflicts with |
| --- | --- | --- |
| `reconcile_run_plane.rs` and `wamn-schema-control::run_plane` | 1,167 + 2,229 (non-test) | tab 60 finding 5 B (platform schema on the `Migrator`); use existing tooling |
| environment policy copy in project databases | part of the above, plus `verification_policy.rs` | single source of truth |
| `catalog.tenant_environments` copy | about 100, plus its SQL | single source of truth |
| `reconcile_package_data_access.rs` and `plan.rs` | 1,114 | owner 2026-11-10: apps read and write their own schema; roles govern |
| `platform_build.rs`, `inputs.rs` and plans | 3,868 | owner: build tooling in xtask or just; one build method |
| `invite.rs` | 143 | identity service owns invitations (tabs 55 to 57) |

Inference: about 2,500 lines are already gone by ruling, and about 8,500 more conflict with rulings. Together that is about a quarter of the 43,549 source lines, before the rest of the crate is read.

## Findings

**1. Verb inputs come from 55 environment variables, read through four constructors, and several are overwritten after they are read.**

Fact:

- The crate reads 55 distinct `WAMN_*` environment variables.
- `Platform` has four constructors: `from_env`, `apply_from_env`, `sql_from_env`, `project_schema_from_env`. A `project_schema` flag and empty values select behaviour: an empty `kubeconfig` makes `EnvironmentDocument::read_target` skip the ConfigMap ("SQL Jobs pass the mounted ConfigMap"); `release_artifact_base` and `registry_auth_file` are empty strings in some modes.
- `runtime::resolve_platform` then overwrites `target.namespace` with `"hosts"`, `database_namespace` with `"platform"` and `chart` with the chart recorded in the installed `wamn` Helm values. So `WAMN_RELEASE_NAMESPACE`, `WAMN_DATABASE_NAMESPACE` and `WAMN_RELEASE_CHART` are read and then replaced on every path that calls it (`apply`, `deploy`, `dry_run`, `operation`, `schema_verbs`, dev `source_deploy`). `runtime::resolve` also replaces the three artifact bases from the cluster document.
- The same event settings have two names: `Platform` reads `WAMN_EVENT_STREAM_REPLICAS` and `WAMN_EVENT_DUP_WINDOW_SECS`; `runtime.rs` and `ensure.rs` read and write `WAMN_EVT_STREAM_REPLICAS` and `WAMN_EVT_DUP_WINDOW_SECS`.

Options:

- A: each verb takes one typed input from clap arguments. Settings the installed platform owns (namespaces, chart, registry transport, artifact bases, event settings) are read only from the installed platform; the environment variables that duplicate them go. The SQL Job runs with its own input type, not empty values. One name per setting.

Detail of A:

- Operator inputs that only the caller knows (system database URL, admin URL, kubeconfig and context) become clap arguments on the `ctl` verbs, with `env =` only where a Job passes them.
- One `InstalledPlatform` value is read once from the installed platform (Helm values of `wamn` and `c-<org>`): chart, namespaces, registry transport, artifact bases, event settings. `runtime::resolve_platform`, `resolve_registry` and `resolve` become that one reader.
- The SQL Job takes its own input (mounted document, database URL). `sql_from_env`, `project_schema_from_env` and the empty-value markers go.

Other crates (facts): the repository reads 196 distinct `WAMN_*` names. The same names are read in several crates: `WAMN_REGISTRY_AUTH_FILE` in 8 (dev, environment, package, release, execution host, `ctl`, host service, scenario worker), `WAMN_NATS_URL` in 7, `WAMN_COMPONENT_ARTIFACT_BASE` in 6, `WAMN_EVT_STREAM_REPLICAS` in 4 (including the runtime). NATS settings belong to the one NATS connect function (tab 56 finding 6); registry settings to the one OCI client (tab 60 finding 1). `platform/status.rs` (part 5) reads the same installed Helm values again.

**2. The release an application serves is recorded in three places.**

Fact:

- `environment.k` requires `current` (a digest or `none`) per application and allows `previous`.
- The environment ConfigMap (`hosts/<environment resource name>`, key `environment.json`) stores the whole document with `current` and `previous`. `read_target` prints a warning when the file's values differ (`stale_warnings`).
- Helm history of the application release: `analyse::read_applied_releases` reads up to 50 revisions and derives current and previous. `document::selection_warning` warns when the ConfigMap and Helm differ, and `select_release` refuses a rollback when they disagree ("rollback target is ambiguous").
- `apply_selection` writes the ConfigMap (`persist_selection`) before the Helm write. A failed Helm write leaves the ConfigMap ahead of Helm, which the next run reports.

Options:

- A: Helm history is the only record of what is applied, since Helm applies it. `current` and `previous` leave the document and the ConfigMap. `deploy <digest>` hands the digest to Helm; `rollback` takes Helm's previous; `none` becomes an uninstall action. `selection_warning`, the stale selection warnings, the ambiguity refusal and `AppliedReleases` bookkeeping go.
- B: the ConfigMap is the only record; Helm history is not read. A failed Helm write is reported by the next apply comparing the ConfigMap with the running release.

Detail of A:

- `ApplicationSelection` loses `current` and `previous`. `deploy <digest>` runs the Helm upgrade with that digest. `apply` reapplies Helm's current release. `rollback` uses `read_applied_releases`, which stays as the one reader of Helm history. `none` becomes an explicit stop verb that uninstalls the release.
- Goes: `persist_selection`, `select_release`, `selection_warning`, the selection part of `stale_warnings`, `SelectionChange`, and the ConfigMap's selection fields.
- Inference: rollback depends on Helm keeping at least two distinct digests in its history; `read_history` reads up to 50 revisions.

Other crates: `wamn-release-control::release_chart` runs `helm` (upgrade, `recover_pending`, `uninstall`) and puts the digest in chart values; it moves to `wamn-kubernetes` (tab 58 finding 6). Dev `coordinator.rs` writes `DeclaredRelease::Digest` into its local document in three places; under A it passes the digest to the deploy call instead. `environment/status.rs` (part 2) reports both copies today.

**3. The environment configuration is copied from the file into a ConfigMap and into registry rows, and every later verb still needs the file.**

Fact:

- `env apply` writes the compiled document into the ConfigMap (`provision_document`), retrying on conflict up to four times.
- `registry.project_envs` also stores `policy_name` and `route_host` from the document.
- `deploy`, `rollback`, `status`, `show`, `schema` and the SQL Jobs take `--env <environment.k>`, compile it to find the coordinate, then read the ConfigMap and warn when the file differs. SQL Jobs read the ConfigMap as a mounted file (`JOB_DOCUMENT`).

Options:

- A: the file is the input of `env apply` only. Other verbs take the coordinate (org, project, env) and read the stored copy. The stale configuration warnings go.
- B: as A, and the registry tables hold the applied configuration instead of the ConfigMap (owner 2026-11-10: env, project and application held in the platform). SQL Jobs read it from the registry. The ConfigMap and its conflict retry go.

Detail:

- A: `deploy`, `rollback`, `status`, `show`, `schema`, `operation` and workflow verbs take the coordinate (org, project, env). `env apply` alone reads the file. The ConfigMap stays as the stored copy; the configuration warnings go.
- B: the applied configuration (policy, schema mode, applications with hostnames, packages, clients, connections, integrations) is stored in registry tables. `registry.project_envs` already holds `policy_name` and `route_host`. SQL Jobs read the registry with the database URL they already receive. The ConfigMap, its four-attempt conflict retry and `environment_resource_name` go. Inference: new registry tables for applications and integrations; owner 2026-11-10 (env, project and application held in the platform) would be the rule that names them.

Other crates: `wamn-control-provision::environment_state::environment_resource_name` names the ConfigMap. `provision_project_env/registry.rs` writes `registry.project_envs`; registry SQL moves to `wamn-registry` (tab 02 finding 5). `ctl` verbs and dev `session.rs` compile the file through `wamn-schema-generator::compile_environment` (KCL kept, tab 44).

**4. Namespace names are string literals across the crate.**

Fact: outside tests, `"hosts"` appears 71 times and `"platform"` 127 times (some of those are the `platform` schema). `DEFAULT_NAMESPACE` exists in `environment/mod.rs` but most code does not use it.

Options:

- A: one constant per namespace, beside the other Kubernetes names in `wamn-kubernetes` (tab 58 finding 6). The `"platform"` schema spelling stays separate.

Other crates (non-test literals): `"hosts"` also in dev (2), provision (2), `ctl` (1), identity (1); the `platform` namespace also in dev (1), provision (2), package (2), `ctl` (2), edge (1), identity (2). All use the `wamn-kubernetes` constants under A.

**5. Platform schema is repaired by a hand-written converge engine as well as migrations.**

Fact: `wamn-schema-control::run_plane` "creates each plane from the current deploy SQL" and repairs schema and privileges from observed facts (`plan_run_plane`, `observation_sql.rs`, `declarations.rs`). `reconcile_run_plane.rs` drives it and also creates runtime roles and converges the environment policy copy. Callers: `environment/ensure.rs`, dev `environment.rs`, the workflow queue live test. Tab 60 finding 5 B puts platform scopes on the SQLx `Migrator`.

Options:

- A: platform schema of a project database is the initialization SQL plus project-scope migrations on the `Migrator`. The converge engine (`reconcile_run_plane.rs`, `wamn-schema-control::run_plane`) goes. Runtime roles are created by the initialization SQL or a migration.

* B: keep only the observation as a read-only drift report in `status`; no repairs. Schema changes go through migrations.

Detail: the converge engine reads the deploy SQL files (`RUN_PLANE_FILES`), rewrites their schema name (`BareSchemaName`), observes tables, indexes, grants and triggers, plans repairs, creates runtime roles and writes the environment policy row (finding 6).

Other crates: `wamn-schema-control::run_plane` (2,229 non-test lines, 1,255 test lines); `deploy/sql/run-state.sql` and `run-queue.sql`; `wamn-control-provision::sql` writes grants on run tables again (`sql.rs`, the `environment_policies` and runs grants), a second statement of the same grants as `run_plane/declarations.rs` (inference); dev `environment.rs` calls `reconcile_run_plane` and `ensure_runtime_roles`; the workflow crate's `queue/automation_live.rs` test calls `reconcile`. Tab 59 finding 3 (provision runs its own SQL) and tab 60 finding 5 B meet here.

**6. The environment policy is copied into every project database for one field that has one allowed value.**

Fact: the runtime reads only `durability_class` from `environment_policies` (`event_admission.rs`, `event_batch.rs`). `registry.env_policies.durability_class` has `CHECK (durability_class IN ('standard'))`. The copy carries a source hash, row security and observation checks.

Options:

- A: the copy goes. Durability is a constant until a second class exists; then the host reads it from the registry or its configuration.

* B: the copy goes, but `registry.env_policies.durability_class` and `runs.durability_class` stay for a future second class.

Detail: A deletes `wamn_run.environment_policies`, its grants, row security and history trigger, `verification_policy.rs`, `converge_environment_policy`, the release crate's `read_projected_environment_policy` and `verify_projected_environment_policy`, and `runs.durability_class` (also limited to `'standard'`). The runtime uses the one class.

Other crates (readers of the copy): runtime `wamn_postgres/event_admission.rs` and `event_batch.rs`; release `publish_release.rs` (local publish verifies the copy against the registry); `wamn-control-provision::sql` grants; `wamn-schema-control::run_plane` observation. Tab 61 finding 2 (policy reads move to `wamn-registry`) shrinks to the registry read.

**7. The disposable marker is copied into `catalog.tenant_environments`.**

Fact: `project_env_key_environment` copies the triple, instance suffix and `disposable` from `registry.project_envs` into `catalog.tenant_environments`, probing whether the table exists ("absence means durable"), so the admit path reads it locally.

Options:

- A: the admit path reads `registry.project_envs`; the copy, its probe and its SQL go.

Detail: fact, `catalog.tenant_environments` is created by `deploy/sql/control-portable-store.sql` in the system database, the same database as `registry.project_envs`. It also holds `environment_instance`, which exists nowhere else; `claim_environment_instance` (environment and dev) updates it. Under A, `environment_instance` moves to `registry.project_envs` if it stays.

Other crates: release `publish_release.rs::read_projected_environment_disposable` reads it ("local candidates require a provisioned disposable environment"); `wamn-control-provision::sql` holds its insert, select and update; `wamn-schema-control::environment` holds the instance claim; `environment/delete/rows.rs` deletes it; dev `coordinator.rs` claims the instance.

**8. Application grants are converged per column from package declarations.**

Fact: `reconcile_package_data_access` derives per-relation and per-column grants for the application role from package declarations, revokes undeclared residue across package schemas, and runs at stage, reset, dev and `ctl package`.

Options:

- A: with one migration role (tab 60 finding 6 A3), default privileges give the application role read and write on every table in application schemas, and the read-only login read. The convergence goes. Inference: the generated data access declarations lose their apply-time reader; checked in the generator follow-up.
- B: keep per-column grants.

Detail of A: `ALTER DEFAULT PRIVILEGES FOR ROLE <migration role> GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO wamn_app`, `GRANT SELECT` to the read-only login, `USAGE` on schemas and sequences. Platform columns stay protected by triggers: fact, `wamn_history.stamp_row` replaces any supplied `created_*`/`updated_*` value. Statement admission (`declared_sql.rs`, tab 12 finding 2) is unaffected.

Other crates: `wamn-schema-generator::data_access` (1,047 lines) generates the per-operation authority and an evidence file at build; its header names "prepared App generations", which constraint row 7 removes. Callers of the reconcile: `environment/stage.rs`, `sql/reset.rs`, dev `coordinator.rs` and `target_database.rs`, a dev example, `ctl` `package_verbs.rs`. Under A the generator's data access output loses its reader and goes with it (generator follow-up).

**9. The platform build is in the environment control crate.**

Fact: `platform_build.rs` builds platform images from the root `Dockerfile`, pushes them, stamps digests into the chart, and hashes declared inputs (`inputs.rs`); the generator and component-policy build scripts carry literal directives for it. Its product callers are `ctl` delivery verbs and `platform/operations.rs`.

Options:

- A: it moves to xtask (owner: tooling in xtask or just). `platform/operations.rs` takes the stamped chart as input. Whether the input hashing stays is decided in the scripts and tooling tab.

* B: keep it a product verb, in its own crate.

Detail: it builds images with Docker, pushes them, stamps digests into `deploy/platform/release`, and hashes declared inputs to skip unchanged builds (`inputs.rs`). `platform/operations.rs::worker_chart` calls `copy_package_output` and `prepare_payload`; dev `source_deploy.rs` calls `loopback`.

Other crates: `wamn-schema-generator/build.rs` carries literal directives for `declared_reads`; the `wamn-component-policy` build script also does, but tab 07 already deletes that build script. Tab 44 (tooling to xtask and just) and tab 40 (one component build path; this file also builds the "two role components") meet here.

**10. The crate split.**

Options:

- A: split by purpose: environment lifecycle stays here; platform install (`platform/*`, `provision_system`, `provision_org`) becomes its own crate; workflow authoring and lifecycle move to the workflow crate; retention and event recovery tasks move to their owners (record history, run state, events); `invite.rs` and `create_user.rs` move to identity; `kubectl.rs` to `wamn-kubernetes`; `platform_build` to xtask.
- B: decide after parts 2 to 7.

Destinations under A (existing crates, facts): workflow authoring and lifecycle to `crates/execution/workflow`; `prune_run_history` to `wamn-run-state` (tab 09); `prune_record_history` beside record history (`apply_package/record_history.rs`); `event_advisories` and `capture_gap` beside the event code (`wamn-event-reg`, `services/cdc-reader`); `invite.rs` and `create_user.rs` to `services/identity`; `identity_issuer.rs` goes with the credential generations (row 7).

## Follows from earlier rulings

- Tab 58 finding 6: `kubectl.rs`, `environment/mod.rs::current_context`, the `helm` and `kubectl` processes in `analyse.rs` and `platform/resources.rs`, and the 77 `crate::kubectl` calls move to `wamn-kubernetes`.
- Tab 60 finding 1: `Platform::registry` and `Platform::component_source` use the one OCI client constructor.
- Tab 61 finding 2: `env_policies.rs` and `verification_policy.rs` here become one read in `wamn-registry` and one projection.
- U2 (SQLx): `tokio-postgres` here goes. `hyper` over `reqwest`: `reqwest` here goes. Tab 01: `wamn-execution-contract` uses become `wamn-digest`. Tab 06: `wamn-pg-core` uses go.

## Carried to later parts

- `runtime::host_group` copies named environment variables out of the installed control host's Helm values (`helm get values c-<org>`) and the environment host release into each new host group. Part 5 (platform install), where those values are written.

## Tests deleted, changed or moved

- Finding 2 A: `document.rs` selection tests and the rollback ambiguity tests go; `store.rs` stale selection tests go.
- Finding 3 A or B: `store.rs::stale_input_warns_with_both_configuration_and_target_selection_values` goes; B also removes `stale_resource_version_retries_preserve_both_application_updates` and `explicit_configuration_creates_then_preserves_recorded_selections`.

## Not read

- Everything outside the files named above.
- Planned parts: 2 apply steps (`analyse`, `stage`, `write`, `expand`, `readiness`, `drain`, `bindings`, `connections`, `clients`, `lock`, `status`, `show`, `rollback`, `delete`, `host_report`); 3 SQL (`sql.rs`, `sql/schema.rs`, `sql/reset.rs`, `operation.rs`, `platform_migrations.rs`); 4 `ensure.rs` and `provision_*`; 5 `platform/*`; 6 `platform_build*`; 7 the rest, and the crate split.

## Rulings

- 2026-11-12: finding 5 A (delete the converge engine now); a drift report is a later option. Finding 8 A. Finding 9 A; the two role components it builds must not overlap the one component build path (tab 40), so their build moves there. Finding 10: A preferred, decided after parts 2 to 7.
- 2026-11-12, owner accepted the recommendations: finding 1 A, extended: each component reads one typed configuration file that Helm renders from its values and mounts; \`ctl\` reads installed settings from the cluster; only credentials stay as Secrets (part 5 checks whether runtime-operator host groups accept mounts or \`envFrom\`). Finding 2 A: Helm history is the one record of applied releases; Helm owns long-lived objects (hosts, services, ConfigMaps, \`Database\` CRs with \`keep\`); SQL Jobs and credential Secrets are explicit verb steps, not Helm hooks; credentials are never Helm values. Finding 3 B: the registry holds the applied environment configuration; the ConfigMap goes. Findings 4, 6 and 7: A (recorded as the single or recommended option; the database identity check, if kept, lives only in the registry). Resulting owners: platform settings in Helm values, applied release in Helm history, environment configuration in the registry, \`environment.k\` as input.

Correction (2026-11-12): finding 5's converge engine also installs `app-schema.sql` and projects identity rows into each project database; their new owners are in 68 wamn-environment-control, part 7 finding 1.
