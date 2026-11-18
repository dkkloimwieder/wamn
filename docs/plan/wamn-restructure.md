# wamn restructure proposal

Nov 16, 2026 · @D Kloimwieder

Branch `worktree-arch-review` at bf8f13b7. Part 1 states only what is ruled now. Part 2 is the review log: research, evidence and the history of each ruling. Where they differ, Part 1 rules.

# Part 1: Current state

## Principles

- Single responsibility, single source of truth. No rewrite, restructure or rename is too large. Delete all accidental complexity.
- Deploy as close to stock wasmCloud as possible. Prefer upstream or off-the-shelf tools over our own code.
- One owner per fact. If two places state the same fact, one goes or is derived.
- No runtime checks for developer practices.
- Generated files are never edited; hand-written files are never generated.
- Names say what a thing is. Names are singular. Directory names equal crate names. No "ctl".

## Hierarchy and isolation

Org → environment → project → application. One boundary per level.

| Level | Boundary |
| --- | --- |
| Org | Kubernetes namespace with RBAC (every Role names its Secrets with `resourceNames`), a quota and a default-deny NetworkPolicy. `wamn-platform-observer` goes. The `control_org` row policy stays in the system database. |
| Environment | A NATS account with JetStream limits, and its own host group. One host serves all applications of the environment. Each CDC reader has its own credential in the account. |
| Project | One CloudNativePG `Cluster` per project database (backup and restore per project). One owner role per database. |
| Application | The host's permission check and the wasm sandbox. The project is the trust unit. |

- The system database (`registry`, `identity`) has its own cluster in namespace `platform`.
- Logins per project database: migration (owner), application, read-only (dynamic SQL), platform (all host platform work, retention included), and the CDC replication login. Until 3.6 each login is suffixed per project database and inherits the plain NOLOGIN group role; plain login names come with one cluster per project database. One system database reader per environment. Each login has a connection limit (3.6).
- One operation service account per project.
- `project_env_key` is not stored in project databases (no column, no row policy, no `project_key()` functions).

## Database, identity in SQL, grants

- The Postgres capability follows upstream `wasmcloud:postgres`, plus identity, the host operation transaction and pools. Two interfaces: `client` (query only, on the read-only login, in a host read transaction) and `statements` (operation transaction, `query`, `query-batch`, upstream result shape). No guest begin. Composed apps share one host transaction.
- Identity is "first insert wins": each transaction first inserts `platform.tx_identity` (unlogged, `xid8` key). `wamn_authority.current_identity()` reads it. Migration and platform logins resolve their identity through `platform.login_principal`.
- A guest `COMMIT` is caught at host commit, not by a SQL text check: the finish statement requires the `platform.tx_identity` row for `pg_current_xact_id()`. If it is missing, the invocation fails and the connection is discarded (W41).
- SECURITY DEFINER functions are owned by their database's owner (`datdba`), set `search_path=pg_catalog`, have no PUBLIC EXECUTE and are not grantable. Grant tests are refusal tests plus invariants derived from the role model.
- Grants: one project database read, cached in the host's local `moka` cache. One lifetime per project, written by the deploy into the workload config, positive answers only. No NATS replica, no drop notices.
- Admin inheritance (org, environment, project admin) is stored as rows in each project database. No system database read per request.
- `platform."user"` keeps `id` (UUID) as primary key and is unique on `(issuer, sub)`. One writer: `wamn_environment::application`. The four platform principals are seed rows in the schema.
- Roles and ACLs are run-time data. The permission vocabulary is derived from the schema and the operations. Applications ship with an `admin` role only. Bounds on calls are a project setting.
- Guest SQL (`wamn_app`, `wamn_query`) gets no grant on `platform.credential`, `platform.oauth_token` or `platform.oauth_authorization`.

## Request path

- One stock wasmCloud `WorkloadDeployment` per application: the route guest, the application components (stock `application/wasm` artifacts) and one workflow service component. The operator pulls and starts them.
- The route guest calls `invoke` (JSON); the host reaches the component through `dispatch_target`. The host has no loader, OCI pull or warm list of its own.
- The deploy writes into the workload config what exists nowhere else: the route table; for each operation public or private, host transaction, read or write, idempotency key field; the application's database schema; connection bindings and integration type digests. No release manifest, no `catalog.releases`, no entry image. The Helm revision is the release.
- Fixed statements of a transaction go as one batch at begin and one at commit. `begin-operation` reuses the authenticated caller.
- Features (connections, files, integrations) are built only when the application declares them.
- File routes register on the wasmCloud router per workload. No second listener.

## Connections

- A connection belongs to one project in one environment. Its tables live in the project database; the integration table folds into `connection`. Integration types and auth types stay in the system database.
- A connection's type and auth type never change.
- Per call: binding from the workload config, connection row from the local cache, secret, send. No snapshot, no hashes, no candidate bindings.
- Exclusive resources: a local mutex per resource plus a blocking `pg_advisory_lock` with a lock timeout, held for the call.
- HTTP client cache key: invocation scope, target, connection id, generation. No in-flight semaphores.
- The environment file states each connection's version once.
- A physical resource belongs to one project: `(target_id, resource_key)` is unique within one project database.
- A connection's service principal stays in the system database. `registry.connection_principal` links it to its org, project and environment for control row security.

## Workflows

- A workflow's first node is its entry node:
  - Request: a synchronous call, which needs a Response node
  - Schedule: a five-field cron, in UTC
  - Data change: CDC on one entity, with ops and an optional condition
  - Topic: a message on a subject, later work
  There is no Manual entry. "Run now" is an admin action for any workflow.
- A Request workflow is addressed by a path under `/api/<application>/flow/`. Calling it is a permission granted to roles, like an operation. It runs as the caller (invoker) by default, or as its `execution_role`. The Response sets a status and a JSON body. With no Response within 30 seconds, the call is refused with a timeout. A Request call writes a run row only when the graph continues after its Response or writes data.
- The graph owns the trigger. The author step derives the index columns that the host queries.
- Workflows are authored, enabled and disabled through host routes of the application. Enabling a Data change workflow creates one NATS pull consumer filtered to its entity, and disabling deletes it. The service pulls from these consumers through stock `wasmcloud:nats`: it admits each run through the host, acks, then runs it. Events committed before the enable are skipped by their LSN.
- A workflow service component runs in every application workload. It walks graphs and calls nodes through the host.
  - A Data change or Schedule run executes at once, in the invocation that admitted it.
  - A Request run executes inline.
  - The queue is used only to take back a run whose host died. A recovery check runs every 30 seconds.
- Runs use a small host interface. The run SQL and its login stay in the host. Each node call carries the run's identity and role.
- A run executes on the live release. `runs.manifest_digest` and `runs.node_digests_json` go.
- Not built now (future work): state after each node, backoff, the approval waiting state.

## Data change in applications

- Application developers are the main users of Data change. A component declares in `wamn.k` the models and ops (`insert`, `update`, `delete`) it handles and the role it runs as, and exports a small wamn handler interface that takes the event as JSON.
- Deploy renders one durable JetStream subscription for each declared (model, op) into the workload config, aimed at the route guest `ingress`, which exports the stock `wasmcloud:nats/jetstream-handler`. The stock plugin creates the consumers. Only declared models get consumers. `ingress` checks the owner package and dispatches to the declared component through the operation path, as the `workflow` user with the declared role.
- One handler for each (owner package, model, op) in an application: deploy refuses two. A developer who wants two reactions puts them in one component. The host accepts `dispatch-data-change` only from `ingress`.
- Delivery is at least once, with no run row. The message id lets a handler skip repeats. A handler has the same host-transaction setting as an operation, on by default: one transaction for each delivery, committed when the handler returns ok and rolled back on an error. Turn it off for handlers that make long external calls.
- One NATS account per environment. The environment host group has one stock `wasmcloud-nats` entry, and each workload narrows its grants to its project's stream and subjects.
- Each CDC reader has its own publish-only user in the environment account. The source stream keeps messages for 7 days, under a byte limit below the account's limit.

## Identity and login

- wamn keeps its own identity service for now, built so a SaaS provider can replace it.
- The host verifies standard JWTs from settings (issuer, JWKS address, audience, algorithms) with `jsonwebtoken` and a small JWKS fetcher (configured CA only, a fetch again on an unknown kid), with RFC 9068 checks, and reads only `iss`, `sub`, `aud`, `exp`. Roles come from the project database.
- One token audience for each org: one login gives one token for every environment of the org and for the control routes.
- Cookies and CSRF live in `wamn-web`; the host sees only bearer tokens.
- Session liveness is the provider's job: 15-minute tokens, no renewal after logout or disable.
- User management (create, invite, disable, list) goes through control routes and one adapter. The installer and `api_service` stop calling identity over the operator-certificate HTTP hop.
- API users are service principals with PATs. Service principals and their PATs live only in the project database (`platform."user"` type `service` with issuer `wamn`, and `platform.pat`). The host checks them with its project login.
- Fixes due: environment login works; one control-eligibility rule; the PAT route checks the certificate subject until that hop goes; `x-forwarded-for` trusted only from known proxies; a route for disabling a user; the signing key encrypted; one Argon2 check per login; two identity replicas.
- Edge: no user login while it cannot reach identity (E1). The offline window is an environment setting, default 24 hours; identity publishes the next key that far ahead. Future options: longer edge sessions (E2), local login on the device (E3).

## Deploy

- `deploy <dir>` builds once, publishes once, then runs SQL and code as one operation. `publish <build dir>` pushes a finished build without building again and prints the top package digest. `schema apply` and a deploy without `<dir>` take that digest; the top package pins its bases.
- A deploy verb goes CLI → control service → one Job. The deployer needs only a wamn token. No nested Job; the document is a mounted file; the CLI poll has a limit.
- Every Kubernetes object comes from a chart, with one upgrade per chart. Rust runs SQL and NATS calls only. Six charts: infrastructure, platform (installation, identity, edge, control service), org, environment (host group), project (cluster, database, publication, certificates), application.
- One wasmCloud version for the operator chart and the runtime crates.
- One environment file per environment (YAML with a JSON schema), with its projects and applications inside.
- Rollback is `helm rollback`. No pull-back after push; append check against the previous version only; one pull per package per operation; one KCL compile per build; one check per fact; no `helm template` pre-check and no `helm get values` read-back; one value each for the NATS, issuer and registry addresses.
- Components: stock `push_component`. Package artifact: two layers (`wamn.json`, migrations tar).
- Build: codegen's describe cache (`.wamn/describe/`, committed and checked current); a database only when SQL changes; skip unchanged builds; `wamn dev` applies new migrations. `build.json` goes (overlay builds hash the base files).

## Install and configuration

| Layer | Tool | Owns |
| --- | --- | --- |
| Infrastructure | OpenTofu, written by hand | Project, network, cluster; writes `infra.values.yaml` |
| Platform install | Helm values, one file per installation, with `values.schema.json` | Chart settings |
| Applications | KCL (`wamn.k`) | Metadata and policy, no types: routes and field labels (overrides on derived defaults), statement lists with fetch class, idempotency key fields |
| Environments | YAML environment file | Projects, applications, bindings, environment settings |
| Run-time data | SQL | Roles, ACLs, connections, app data |
| Credentials | Kubernetes Secrets, CloudNativePG managed roles | Passwords, keys, tokens |

- Install: one infrastructure release (operators and resource types first), then one platform release with identity, control and edge on; one wait at the end. One SQL Job. Identity makes its own first signing key. One owner-invitation Job. All logins in the first install of their chart. No superuser SQL from the developer machine.
- Each service reads one rendered settings file with its real inputs; constant flags become code defaults.
- Kind cluster and local registry creation is an `xtask` step. Provider differences are two values files. No key-parity check.
- Step 3.6 makes sure three install defects found by reading are gone (L1): the issuer Job's missing `--db-host`, `wamn-oauth` written to the wrong namespace, `platform status` reading the database in the wrong namespace.

## Contracts, codegen, KCL

- The developer writes SQL migrations, the SQL file for each declared statement (checked by SQLx at build), hand-written WIT for each custom operation (input, result and errors), and `wamn.k`.
- `wamn.k` holds no types. It holds: package and base pins; model overrides; routes and field labels (derived defaults, a route from the operation name, overridden here); for each custom operation, its statement list (name and fetch class; the SQL file path comes from the operation and statement name) and its idempotency key field (a field of the WIT input); connections; components.
- Generated: model WIT from the schema (with named `id` and `revision` types), Rust bindings, statement types from SQLx describe. Custom-operation WIT is not generated.
- Codegen uses `prettyplease` (Rust), `wit-encoder` (WIT) and typed OpenAPI 3.1 structs from `oas3`; the TypeScript client comes from an off-the-shelf generator fed by `openapi.json`. SQL templates stay.
- KCL holds overrides on schema-derived defaults, is never generated, and refuses names that match nothing in the schema or WIT.
- JSONata transforms declare input and output schemas, with no examples; output is validated at run time.
- Sync: only the server-side change feed on list reads.

## Crates and names

- Renames: `wamn-ctl` → `wamn-cli`; `wamn-control-provision` → `wamn-provision`; `wamn-compiler` → `wamn-codegen`; `wamn-connections` → `wamn-connection`; `wamn-web-front` → `wamn-web`; `wamn-platform-identity` → `wamn-identity`; identity service package → `wamn-identity-service` in `services/identity-service`.
- Merges: artifact crates → `wamn-oci`; `wamn-postgres-statements` + `wamn-data-access` → `wamn-guest`; build + composer → `crates/build`. `wamn-release-control` goes. Role names collect in `wamn-role` in 3.1, with the T2 login set. The `wamn-events` and `wamn-event-wire` merge waits for 3.4 (it made a dependency cycle in Epic 2).
- Splits: `wamn-execution-host` goes: file route → `wamn-blobstore`, number counter → `wamn-sql`, OAuth callback → `wamn-http`, credential cleanup → `wamn-connection`; control route, host route, application route, environment status and connection health → control service. `crates/identity` keeps passwords, sessions, PATs, principals, org membership and control eligibility; connection administration → `wamn-connection`; project database rows, configuration rows and environment fill → `wamn-environment`. The `wamn.json` model moves to `wamn-catalog`.
- Dependency fixes, in phase 3: no `wamn-postgres` → `wamn-kubernetes`; no `wamn-oci` → `wamn-kubernetes` (use `oci-client`); no `wamn-kubernetes` → `wamn-registry` (3.5).
- Docs: this doc lives at `docs/plan/wamn-restructure.md`. Everything else in `docs/plan` goes, and so do `docs/history` and `docs/audit`; no stray files at the top of `docs/`. This doc and `rulings.md` are the authority until the restructure lands; where they differ, this doc rules.

## Tests

- Three levels: unit (no services); service tests with real Postgres and NATS from testcontainers, marked `#[ignore]`, one per contract; journey tests on kind through `wamn deploy`, observed with a read-only login.
- Stock `cargo test` and `cargo test -- --ignored`; an `xtask` command maps changed files to packages. Fixture applications are built in one step before the run; a test whose input is missing fails. `#[sqlx::test]` gives each test its own database. Timing tests use `tokio`'s paused clock. No `test-util` features; helpers in `tests/common/` or one dev-only crate. One SQL crate for grant refusals.
- During phases 1 and 2 testing is minimal: a step is checked by `cargo build` and `cargo clippy` only; tests that stop compiling are deleted; no new tests until step 3.8.

## Order of work

Phase 1 deletion, phase 2 restructure (no behaviour change), phase 3 code editing. All deletion up front; the project may be non-functional across epics; every step compiles. One epic is fully scoped at a time, the next after review; work inside an epic runs in parallel.

Phase 3 steps, in dependency order: 3.1 database model; 3.2 request path (Epic 4: the host serves one application; Epic 5: connections, files, integrations, application host routes); 3.3 identity; 3.4 workflow service; 3.5 deploy; 3.6 install and isolation; 3.7 codegen; 3.7b crate boundaries; 3.8 tests.

## Epic 1: phase 1 deletion

Merged to main.

Rules: done means `cargo build` and `cargo clippy` pass; deletion plus the smallest caller edit only; a test that stops compiling is deleted; an issue edits only the files it owns; a symbol with callers in a file another wave-1 issue owns becomes an empty no-op in wave 1 and is deleted in wave 2.

Decisions: `AuthDrops`/`ConnectionDrops` become no-ops (parameters removed in phase 2); only the publication side of the release document goes (`ServingManifest`, `LoadedRelease` go in 3.2); the edge keeps the component loader; `build.json`, the component media types and the package layers wait for phase 3; `lock()` calls go in 3.8; `wamn_app_owner` is the parent role of every migration login (`workload_role.rs::acl_role`), so it goes in 3.1 with T2 and S; `cluster.k` and `ClusterDocument` stay until 3.6 (read by `services/ctl`, `services/control`, `xtask` and `test-support`), and Epic 1 deletes only the `project.k` chain.

Wave 1 (parallel):

| Issue | Owns | Deletes |
| --- | --- | --- |
| 1.1 Environment and install | `crates/environment`, `crates/platform-install`, `crates/release`, `crates/catalog/src/configuration.rs`, `services/control/src/main.rs`, `services/ctl/src/{provisioning_verbs,identity_verbs}.rs` and their `main.rs` entries, `deploy/gcp/environments`, `deploy/charts/installation/values.yaml`, `deploy/charts/release/values.yaml`, `deploy/infra/kind-config.yaml`, the named receiving and REST connector fixtures | Uncalled provisioning path; `environment/sql.rs::initialize`, `environment/stage.rs::initialize`; `environment/status.rs`, `environment/rollback.rs`, `environment/mod.rs::dry_run`, `environment/show.rs::show`, `environment/runtime.rs::resolve`; `legacy_release`; `component_source_from_env`; `ReleaseCarrier` and `wamn_release_control::delivery::ReleaseIdentity` (not `wamn_authority::identity::ReleaseIdentity`, which stays); `platform/operations.rs::provision`; `org::recovery_values`, `oauth::dev_key`; environment KCL; nested bootstrap Job; `require_chart_references`; `hostConfiguration` read-back; `write_selection`; `Provider` branches inside `platform-install`; install Jobs and port-forward SQL; bootstrap verbs; unused release chart values. Moves `kind-config.yaml` to `deploy/kind`. Keeps `cluster.k` and `ClusterDocument`. |
| 1.2 Build and test runner | `crates/compiler`, `crates/catalog/src/route_identity.rs`, `crates/publish`, `crates/package-artifact`, `xtask/build`, `xtask/src/{lib,selection,platform,platform_project,platform_project_commands,platform_tofu,platform_pools}.rs`, `xtask/kcl/project`, `deploy/gcp/project.k`, `tests/selection.toml`, `justfile`, TypeScript packages using generated clients, named overlay and WMS test files | `classify_statements` and the `transactional` field; `required-schema-contract.json`; `rustfmt.rs`; TypeScript and sync client generators; `package_artifact_digest`; pull-back after push; `validate_append` over all versions; `read_descriptors`, `load_component_facts`; the custom test runner; the `project.k` chain (`ProjectDocument`, `platform_tofu.rs`, `platform_pools.rs` with `wake`/`sleep`, `xtask/kcl/project`, `deploy/gcp/project.k`, the `Project` and `Platform` tasks in `lib.rs`). Keeps `openapi.json` and `xtask build-platform`. |
| 1.3 Host cut | `services/host`, `deploy/charts/release/templates`, `file_route.rs::serve_selected`, the `release_digest` column | Host release selection; second file listener and `-files` Service; host spawns of queue, schedule and event tasks; `replicate` calls; `lease_tests.rs` |
| 1.4 Auth caches | `crates/authority`, `crates/authentication`, `sql/session_identity.rs`, `connections/{connection_memory,connection_facts}.rs`, broker bucket code, `services/identity/src/cli.rs`, `http/src/lib.rs` (`oauth_ticket`), `execution-host/src/application_route.rs` (cache clear) | NATS KV replica and buckets; connection KV memory; host liveness read; `inherits_administration`. `AuthDrops`, `ConnectionDrops`, `Publisher`, `replicate` become no-ops. Stale identity comments. |
| 1.5 HTTP transport | `http/src/transport.rs` | The five semaphores |
| 1.6 Login roles | `crates/provision`, grant lines in `deploy/sql` | `ServiceReader`; the dead `wamn_administration` grant; `DELETE` on `runs` for `wamn_app` |
| 1.7 Tests, files, docs | Stub test files outside other issues, `test-support`, identity `mail.rs`, `crates/catalog/Cargo.toml`, `deploy/platform`, `deploy/infra` (except `kind-config.yaml`), the example RBAC template, `docs/{history,plan,audit,testing}`, `CLAUDE.md`, `AGENTS.md`, `app-schema.sql` comments, `rulings.md` | `panic!` stubs; unused `test-support` items; mail back door; `proptest`; loose YAML; docs/history, docs/audit and everything in docs/plan except wamn-restructure.md (CLAUDE.md and AGENTS.md point to it); dead links; stale comments; superseded W37 sentences |

Wave 2 (in sequence, after wave 1):

1. **2.1 Workflow library:** queue, `RouterDriver`, `contract.rs`, `read_compiled_wiring_document`, `production_lease.rs`, `release_lease`, three unused `FailType` variants, `catalog.wiring_tombstones`, five unused `runs` columns, advisory stream, `reconcile_replica_identity`, publication SQL builders and their CDC tests, the `ExecutorDatabase` impl, the `workflow.park`/`release`/`runs` operations.
2. **2.2 Invocation path:** connection snapshot and candidate types, the three hashes, `connection_generation.rs`, `resource_lease.rs`, `connection_projection.rs`, `sql/connection_binding.rs`, `ClientIdentity`/`ConnectionIdentity`, the host-path component loader and warm list, `NativeInput::Typed`; the login families the host opens pools for and the org chart templates; the no-op `replicate` and the `Provider` type.
3. **2.3 Release publication:** release OCI artifact, `catalog.releases`, `stage::write_release`, `run_queue::release_bytes`, `compose_application`, `entry_publication`, the entry image.

## Epic 2: phase 2 restructure

Merged to main.

Rules:
- No behaviour change. Allowed: moves, merges, renames, and deleting code that a move leaves with no caller. No new types, no new logic.
- Done = `cargo build --workspace` and `cargo clippy --workspace --all-targets` with no new error compared with the 45 existing ones; the same in `apps/`. A local Postgres container is allowed for the `wamn build` that check needs, and only for that. Run no tests, no kind or cluster runs, nothing on GCP.
- If a test stops compiling and an import path change does not fix it, delete the test.
- An issue may edit any file, but only to change import paths, Cargo dependency lines and workspace members for what it moved. Resolve conflicts in those lines at merge, and regenerate Cargo.lock.
- Moved modules keep their names and structure unless stated.
- Generated code is regenerated, never edited by hand.

Decisions:
- `wamn-execution-host` goes. The host no longer links it, and the control service is its only user.
- `org.rs`, `control.rs` and `paging.rs` stay in `wamn-identity`.
- Deferred: `wamn-role` to 3.1; name builders out of `wamn-kubernetes` to 3.5; test layout (`test-util` features, test file names) and the `test-support/resources` name to 3.8.

Issues:
- 2.1 Remove change notices. Runs first, alone. Delete `AuthDrops`, `ConnectionDrops`, `Publisher`, `crates/broker/src/{auth_changes,connection_changes}.rs`, and every parameter, field and call that carries them.
- 2.2 to 2.6 run in parallel after 2.1:
  - 2.2 Split `wamn-execution-host`:
    - `file_route.rs` → `wamn-blobstore`
    - `number_counter.rs` → `wamn-sql`
    - `oauth_callback.rs` → `wamn-http`
    - `credential_cleanup.rs` → `wamn-connections`
    - `control_route.rs` with `control_route/`, `host_route.rs`, `application_route.rs`, `environment_status.rs` and `connection_health.rs` → `services/control`
    - Delete `operation.rs` (`OperationHost`), `platform_http.rs`, `file_collector.rs` and `file_collection.rs`; none has a production caller. Then delete the crate.
  - 2.3 Split `crates/identity`:
    - `connections.rs` and `connections/` → `wamn-connections`
    - `application.rs`, `configuration.rs` and `environment.rs` → `wamn-environment`
    - Everything else stays.
    - Move `test-support/resources/credentials.rs` into `wamn-connections` under `test-util`.
  - 2.4 Remove `wamn-release-control`, and move build:
    - `release_chart::{uninstall, recover_pending}` → `wamn-kubernetes::helm`
    - `chart_version`, `read_platform_version`, `ImageSet`, `image_set` and `stamp` → `wamn-platform-install`
    - `values`, `roles`, `release_name`, `workload_name`, `ValuesInput`, `Role`, `RoleName`, and `TELEMETRY_VARIABLES` → `wamn-environment`; `env_policies.rs` → `wamn-control-provision`
    - Callers use `wamn_kubernetes::Target` directly. Delete the crate.
    - Move `xtask/build` (`wamn-build`) to `crates/build`, and fold `wamn-component-composer` into it.
  - 2.5 Move the `wamn.json` model: `compiler/src/{manifest,manifest_error,source_name}.rs` → `wamn-catalog`, merged into the existing `wamn_catalog::manifest` module. The existing smaller types in `wamn_catalog::manifest` move unchanged to `wamn_catalog::manifest::installation`.
  - 2.6 Merges:
    - `component-artifact` + `package-artifact` → `wamn-oci` in `crates/oci`
    - `postgres-statements` + `data-access` → `wamn-guest` in `crates/guest`, in the apps workspace. The generator's `wamn_postgres_statements::` paths become `wamn_guest::`.
- 2.7 Renames. Runs last, alone. Directory names equal crate names.
  - `wamn-ctl` → `wamn-cli` in `services/cli`; the binary stays `wamn`
  - `wamn-control-provision` → `wamn-provision`
  - `wamn-compiler` → `wamn-codegen` in `crates/codegen`
  - `wamn-connections` → `wamn-connection` in `crates/connection`
  - `wamn-web-front` → `wamn-web` in `services/web`; the binary is `wamn-web`
  - The identity service package `wamn-identity` → `wamn-identity-service` in `services/identity-service`; the binary stays `wamn-identity`. Then `wamn-platform-identity` → `wamn-identity` in `crates/identity`.
  - `guests/jsonata` → `guests/jsonata-expression`
  - Package `wamn-modbus` → `modbus`
  - Update the Dockerfile, chart, xtask and docs references to match. The `wamn-ctl` NATS certificate in the installation chart stays until 3.6.

## Epic 3: step 3.1 database model

Merged to main.

Rules:
- Only the work listed here. Nothing from 3.2 or later.
- Schema changes edit the files in `deploy/sql` directly. There is no migration chain.
- Done means all three pass:
  - `cargo build --workspace` and `cargo clippy --workspace --all-targets`, with no new error compared with the epic's base. The same in `apps/`.
  - The product's own code that applies `deploy/sql` runs against a fresh local Postgres, for the system database and one project database, with no error. Use one local Postgres container, for these checks only.
- Run no tests and write no new tests (tests come in 3.8). If a test stops compiling and changing a column, a role name or an import does not fix it, delete the test.
- Generated code is regenerated, never edited by hand.

Issues:

3.1a `project_env_key` leaves project databases. Runs first, alone.
- SQL: drop the column, and its place in keys and indexes, from `catalog.packages`, `catalog.connection_requirement`, `catalog.release_connection`, `catalog.connection_binding`, `catalog.workflows`, `wamn_run.runs` and `wamn_run.run_queue`.
- SQL: drop the row policies by project on `catalog.*` and `wamn_run.*`, keeping the part that requires an identity. Drop all three `wamn_authority.project_key()` definitions (`app-schema.sql`, `catalog-schema-prefix.sql`, `run-state.sql`), `current_project_key()`, and the `project_key` expression indexes.
- Rust: remove the key from every statement and binding against these tables.
- The key stays wherever several deployments share a store: system database rows, NATS subjects and keys, resource names, login names.

3.1b to 3.1d run in parallel after 3.1a:

3.1b Connection tables move to the project database.
- In the project database (`platform` schema): `connection`, with the four integration fields (name, type, auth type, external system) as its columns; `connection_generation`; `external_system`; the credential reference (today `identity.credential`); `oauth_token`; `oauth_authorization`.
- Delete: `registry.{external_system,integration,connection,connection_generation}`, `identity.{credential,oauth_token,oauth_authorization}`, and `platform.integration`.
- `integration_type`, `integration_type_version` and `auth_type` stay in the system database.
- Move every writer and reader to the project database: connection administration (`wamn_connection::connections`), OAuth (`wamn-http`, `wamn_connection::oauth_token`), connection credentials (`wamn_connection::connection_credentials`), `wamn-blobstore`, environment bindings (`wamn_environment::environment::{bindings,connections}`) and the control routes.
- The connection reader login goes.

3.1c Login set T2 and role names.
- New crate `wamn-role` in `crates/role`. It holds every Postgres role name and nothing else. Remove the copies in `wamn-identity-protocol`, `wamn-provision`, `wamn-record-history` and `wamn-schema-introspection`.
- Project database logins:
  - migration: owner of the database and of every object in it. It replaces `wamn_db_owner` and `wamn_app_owner`, which both go.
  - application: `wamn_app`
  - read-only: `wamn_query`
  - platform: `wamn_platform`
- `AuthorityClass` keeps `GuestSql` and `DynamicSql`. `ExecutorPlatform`, `CallableHttp` and `Administration` become one `Platform` class on `wamn_platform`.
- System database: one reader login per environment for the host's system reads.
- Each role is created in exactly one SQL file.
- Every SECURITY DEFINER function is owned by its database's owner (`datdba`), sets `search_path=pg_catalog`, has no PUBLIC EXECUTE and is not grantable.
- Logins are still created by the current provisioning code under their `wamn-role` names. Creating them in charts is step 3.6.

3.1d `platform."user"` key and its writer.
- Add `issuer` and `sub` (text, NOT NULL) with `UNIQUE (issuer, sub)`. `id` stays the primary key. Foreign keys, `user_role` and the `created_by`/`updated_by` stamps keep using `id`.
- For wamn identity, `issuer` is the identity issuer address and `sub` is the principal id.
- One writer: `wamn_environment::application::{write_user, write_service}`. `wamn_environment::environment::stage` writes through it.
- The four platform principals (`wamn:provisioning`, `wamn:apply-package`, `wamn:executor`, `wamn:audit-retention`) become seed rows in `app-schema.sql`, with the ids already fixed in `user_platform_principal_check`. Delete `wamn_provision::platform_principals`.
- The host's lookup by `(iss, sub)` is step 3.3. Do not change it here.

Rulings on the scout questions (D, 2026-11-16): logins stay suffixed per project database until 3.6; `registry.connection_principal` keeps control row security for connection principals; `configuration.rs` loses its external system, integration and connection writes; the workflow user is a fifth seed row; stage and the control service take the issuer from the identity issuer address setting; the `Database` owner in `database.yaml` is the project's migration login, and `clusters.yaml` waits for 3.6.

## Epic 4: step 3.2, part 1, the host serves one application

Rules:
- Only the work listed here. Connections, file routes, integrations and features built only when declared are Epic 5. The workflow service is 3.4.
- Done means all of these pass:
  - `cargo build --workspace` and `cargo clippy --workspace --all-targets`, with no new error compared with the epic's base. The same in `apps/`.
  - The schema-apply check from Epic 3.
  - `helm template` of the release chart, with values for the largest fixture application in `apps/`. The rendered values must stay under Helm's 1 MiB release limit. Report the size.
  - A local Postgres container is allowed for these checks only.
- Run no tests and write no new tests (3.8). If a test stops compiling and an import or name change does not fix it, delete the test.
- No kind or cluster runs, nothing on GCP.
- Generated code is regenerated, never edited by hand.

Issues:

4.1 Application workload. Runs first, alone.
- The release chart (`deploy/charts/release/templates`) renders one `WorkloadDeployment` per application. The last version of that template is `workloads.yaml` at commit 6b6d9c22; use it as the starting point. The workload holds:
  - the route guest
  - every application component, by stock OCI reference and digest
  - host interfaces: `wasi:http` handler with `config.host`, `wamn:ingress`, `wasi:config`, `wamn:operation`, and the `wamn:postgres` interfaces the components import
- Components are published with stock `push_component` (`application/wasm`). The wamn component media types (`COMPONENT_LAYER_MEDIA_TYPE`, `COMPONENT_CONFIG_MEDIA_TYPE`) and `pull_verified` leave the publish and host paths.
- The route guest is the platform's one pinned image. If `xtask build-platform` does not publish it, stop and ask.
- The deploy (`wamn_environment::release_chart` and `environment::write`) writes two config keys on the route guest:
  - `wamn.http.routes`: the route table, as today
  - `wamn.application`, one JSON document holding:
    - for each operation: public or private, whether the host begins the transaction, read or write, and the idempotency key field
    - the application's database schema
    - the project's grant cache lifetime
    - for each operation, the component that serves it
    - the org, project, environment and application names
    - delete as its own operation type
- The grant cache lifetime moves into the workload config, and these go:
  - `registry.environment_projects.revocation_seconds`
  - `registry.environment_applications.revocation_seconds`
  - the `revocation-seconds` rows written by `environment/document.rs::write_revocation_configuration`, and that function
  - the application-level field in the environment input
  The project-level value in the environment input stays, as the deploy's input.

4.2 to 4.4 run in parallel after 4.1:

4.2 Host operation plugin.
- A `wamn:operation` plugin in `wamn-engine`, registered in `services/host`. For each application component, it resolves `ResolvedWorkload::dispatch_target` in `on_workload_resolved` and holds it for the binding. `invoke` calls the component through it.
- JSON conversion stays (`engine/operation/json.rs`).
- Delete the host's own component handling: `engine/operation/native_workload.rs`, its pull of application components, the warm list, and the repeated `NativePolicy::admit` (one check per invocation).
- Code the edge still uses moves into `services/edge` (4.4 rule).
- `wamn-sql` keeps the `wamn:operation` interface; `wamn-engine` holds the dispatch targets and `invoke` calls through them. No connection state on the invoke path until Epic 5.

4.3 SQL and auth.
- The fixed statements of a transaction go as one batch at begin and one at commit (`wamn-sql`: operation transaction, write log).
- `begin-operation` reuses the caller that `authenticate` produced. The second grants read goes.
- Grants, environment status, PAT admission and session answers live in one local `moka` cache, with the project lifetime from `wamn.application`. It caches positive answers only.
- Environment status comes from that cache.
- `services/host/src/host/projects.rs` reads the projects file once, not on each request.

4.4 The release document leaves the request path.
- Delete `LoadedRelease` (`wamn_engine::release_manifest`). Remove the release document from host routing, authentication, authority, SQL and the control routes; they read the workload config written in 4.1.
- The control service builds its routes from `HostRouteSet::Control` directly. `LoadedRelease::control_root()` goes.
- `ServingManifest` stays in `wamn-catalog` for its remaining readers: workflow (3.4), deploy identity (3.5) and connection and file readers (Epic 5).
- The parts of `LoadedRelease` and the wamn component pull (`pull_verified`) that `services/edge` uses move into `services/edge`, unchanged, until the edge design. `crates/dev/src/dev/edge_bundle.rs` stays.

## Epic 5: step 3.2, part 2, connections, files and application host routes

Rules:
- Only the work listed here. The workflow service is 3.4. The environment file and the registry binding tables are 3.5.
- Done means all of these pass:
  - `cargo build --workspace` and `cargo clippy --workspace --all-targets`, with no new error compared with the epic's base. The same in `apps/`.
  - The schema-apply check from Epic 3.
  - `helm template` of the release chart with values for the largest application in `apps/` that builds. The Helm release record must stay under 1 MiB. Report the size.
  - A local Postgres container is allowed for these checks only.
- Run no tests and write no new tests (3.8). If a test stops compiling and an import or name change does not fix it, delete the test.
- No kind or cluster runs, nothing on GCP.
- Generated code is regenerated, never edited by hand.
- Each issue deletes the `ServingContext` and `ServingManifest` readers it replaces. The readers for workflow (3.4), deploy identity (3.5) and the edge stay.

Issues:

5.1 Deploy facts for connections and files. Runs first, alone.
- `wamn.application` gains, for each component:
  - `connections`: for each declared connection name, the connection id, read or write mode, the interface, and the integration type digest when the type is wasm or native
  - the file facts that the file routes and the blobstore plugin need and that today come from the release document
- The deploy takes the bindings from `environment/bindings.rs::analyse` and writes them into `wamn.application`. `bindings::write` stops writing the catalog. `catalog.release_connection` and `catalog.connection_binding` go, with their grants, writers and readers. `catalog.connection_requirement` stays.
- The chart's host interfaces are the route guest's set plus each platform package the components import (`wamn:postgres`, `wamn:connection`, `wasmcloud:blobstore`, `wamn:integration`). A package that no component imports is not rendered.
- Each declared connection of a wasm integration type becomes a sibling component in the workload. It is named after the declared connection name, pinned by digest, has `allowedHosts` set to the connection's authority, and has `wamn.integration` set to the connection id. wasmCloud links the application's labelled import to it.
- The deploy refuses one declared name bound to two connections in one workload, and a declared name equal to another component's name.
- The authority is fixed on `platform.connection`. A new host is a new connection.
- Connection create, connection edit and the deploy refuse an IPv6 literal authority for wasm types, because the operator CRD's `allowedHosts` pattern has no bracketed IPv6 form. A hostname that resolves to the device works.
- The deploy refuses a binding whose connection's target is not the environment's target, for every connection type. A connection meant only for a local target never reaches a cloud workload.
- The deploy refuses two versions of one host interface package, because the operator allows only one unnamed entry for each namespace and package.
- The deploy refuses an operation name that two components declare, among operations that have a type. Palette exports are exempt.

5.2 Connections on the call path. After 5.1; runs in parallel with 5.3.
- Host plugins for `wamn:connection/http` and `wamn:integration`, registered in `services/host`. They link only for components that import them. The host builds connection state for a workload only when `wamn.application` declares connections.
- Per call: the binding from `wamn.application` for the component and the declared name; the connection row, its generation and its secret from the host's local `moka` cache, with the project lifetime and positive answers only; then send. A 401 or 403 removes the cached row from the local cache and reads it once again.
- Connection answers use the same `moka` cache as grants (`services/host/src/host/authentication.rs`).
- These go: `ConnectionFacts::release`, `ConnectionFacts::version`, `ConnectionFacts::release_component_bindings_ready`, and what is left in `ConnectionMemory` from the deleted replica. The host makes no system database read for connections.
- The host's integration loader goes. wamn does not pull or compile integration code.
- `wamn:integration/bound` gains `begin`, which returns a `call` resource for each call. `begin` checks the connection status and reads the generation and credential from the cache. It sends no guard SQL and holds no connection during the external call. A cache miss fills the 5.2 cache with short reads.
- Control `connection_test` refuses wasm types until the open item on health probes resolves.
- The host socket floor stays (`allow_private: false`, `deny_special: true`). Integration siblings in the cloud reach public addresses only.
- No exclusive lock in the host. Exclusive connections are refused in cloud environments (5.5).
- HTTP client cache key: no change. No client is kept from one send to the next.
- The invoke path carries the connection invocation again (the `Scope` record in `sql/invocation.rs`).
- `session_operation_permissions` caches a grant set only under the same condition as `principal_grants`.

5.3 File routes. After 5.1; runs in parallel with 5.2.
- The blobstore host plugin is registered in `services/host` and links only for components that import `wasmcloud:blobstore`.
- When an application workload resolves, the host registers a native path on the wasmCloud router for that workload. On unbind, it removes the path. The prior art is `crates/execution-host/src/platform_http.rs` at 6b6d9c22 (`on_service_http_resolved`, `ServiceHttpJob`).
- File routes are served on that path. There is no second listener.
- `sign-result` signs file results again.

5.4 Application host routes. After 5.3.
- The host serves `HostRouteSet::Application` (permission.mine, roles, grants, users, counters) on the path from 5.3, built from `wamn.application`.
- The Application half of `services/control/src/host_route.rs` and `application_route.rs` moves into `services/host`. The control service keeps the Control half.
- `workflow.start` refuses with the existing unavailable refusal until 3.4, and the `workflow` field leaves the handler.
- `user.list` lists users and roles from the project database, without the `admin_covered` field until 3.3. The host has no system database login.

5.5 Exclusive connections are refused in the cloud.
- The deploy refuses a binding to an exclusive connection, and connection create refuses an exclusive connection, when the environment's target type is `cloud`. This covers every connection type.
- The call limit, `platform.call_slot` and the host's exclusive lock are removed. The platform pool keeps its default size.

Rulings on the scout questions (D, 2026-11-17):
- Plugin code that Epic 1 deleted is restored from 0943187e1^, without snapshots, candidate bindings, hashes, semaphores or the KV replica.
- 5.1:
  - gives the host its project database logins through a `wamn-postgres` entry in the host group's `plugins` (ruled 2026-11-17, Part 2)
  - adds runtime type, exclusivity, target type, signed-result facts and the full integration reference to `wamn.application`
  - moves every binding-table reader to `wamn.application`
  - defines the one binding lookup that 5.2 and 5.3 use
- `release_values` takes the bindings as input, and its caller is 3.5.
- The check on the connection principal's status goes; only `platform.connection.status` counts.
- `sign-result` returns a result with no signed fields unchanged.
- The shared cache is a trait in `wamn-connection`, implemented by the host.
- `wamn-sql` owns a signing trait, and the host supplies its implementation.
- The host registers `/wamn_control` and the file stream prefix as `<workload>/platform-http` through a `HostHandler` wrapper. The `files` field leaves `route.json`.
- `workflow.start` answers `workflow_not_served`.

## Epic 6: step 3.3, identity

Rules:
- Only the work listed here. The edge device design, and the edge publishing keys ahead, are later work.
- Done means all of these pass:
  - `cargo build --workspace` and `cargo clippy --workspace --all-targets`, with no new error compared with the epic's base. The same in `apps/`.
  - The schema-apply check.
  - `helm template` of the release chart with values for the largest application in `apps/` that builds. The Helm release record must stay under 1 MiB. Report the size.
  - A local Postgres container is allowed for these checks only.
- Run no tests and write no new tests (3.8). If a test stops compiling and an import or name change does not fix it, delete the test.
- No kind or cluster runs, nothing on GCP.
- Generated code is regenerated, never edited by hand.

Issues:

6.1 Token profile. Runs first.
- Identity signs RFC 9068 access tokens:
  - header `typ: at+jwt`, `alg: EdDSA`, `kid`
  - claims `iss`, `sub` (the principal id), `aud`, `exp`, `iat`, `jti`, `client_id` (`wamn-web`)
  - no `roles`, `org`, `authority` or `csrf` claims
- `aud` is one audience for each org: the value that `control_audience(org)` produces today, renamed the org audience.
- One login is one password check (one Argon2 check) and gives one org token. Session targets, `with_session_targets` and `/password/environments` go.
- The lifetime stays 900 seconds. Renewal is refused after logout or disable.
- The `wamn-session+jwt` profile and its types in `wamn-identity-protocol` go.
- The login request names the org audience in `aud`. Any active org member may log in. Identity activates only keys published at least the JWKS cache lifetime plus a margin earlier.

6.2 Verification in the host and control.
- Host and control verify tokens with `jsonwebtoken` and a small JWKS fetcher (jsonwebtoken 11 `JwkSet`, the host's moka cache, a reqwest 0.13 client with only the configured CA, a fetch again on an unknown kid), from settings:
  - issuer, JWKS address, audience and allowed algorithms (`EdDSA`)
  - the CA bundle that the JWKS fetch trusts
- RFC 9068 checks: `typ`, an exact `iss`, `aud`, `exp` with 30 seconds of tolerance, and `iat` not in the future.
- The host resolves the caller to `platform."user"` by `(iss, sub)` in the project database, cached in the host's moka cache (positive answers only). A missing row or an inactive status is refused.
- Sessions and PATs both read grants with `user_operation_permissions`. These go:
  - `session_operation_permissions`, `session_grants` and roles read from tokens
  - `IssuerKeys` (`authentication/session_keys.rs`), `SessionVerifier` and `VerifiedSession`
  - `session_is_active` and `control_session_is_active`
- Cookie and CSRF handling leave the host (`engine/ingress.rs`). The host accepts only `Authorization: Bearer`.
- Control keeps its control-eligibility read on the system database.
- The environment host group defaults carry the verifier settings. The edge refuses protected routes until the edge design is done.

6.3 Cookies and CSRF in `wamn-web`.
- `wamn-web` proxies login, renewal and logout. It takes the tokens from identity's JSON response and sets these `__Host-` cookies:
  - the session token: HttpOnly, Secure
  - the renewal token: HttpOnly, Secure
  - a readable CSRF token, which is a signed double submit: an HMAC with the web service's key over the session token's `jti` and a random value
- For unsafe methods, `wamn-web` requires the CSRF header to match the cookie and the signature.
- When it forwards a request, `wamn-web` replaces the cookies with `Authorization: Bearer`.
- The HMAC key is a Secret that the web chart mounts.
- Identity's cookie carrier (`password.rs`, `CSRF_COOKIE`, `cookie_response`) goes. Identity returns tokens as JSON only.
- Login, enrollment, recovery and reset require a same-origin `Origin` header in place of the signed CSRF check. The cookies live until the login deadline. platform-install writes the Secret `wamn-web-csrf`.

6.4 Admin rows for users.
- `project_authority` writes the `admin` user role row for a user who holds org admin or project admin, and removes it when the user stops holding it. Every path that changes org or project admin calls it.
- Admin grants and revokes through `mutate_grant` stay refused, so every `admin` row is inherited.
- `user.list` puts `admin_covered` back, as "holds `admin`".
- Blocked by 6.7.

6.5 Service principals and PATs in the project database.
- `app-schema.sql` gets `platform.pat`:
  - `id`, `user_id` (references `platform."user"`), a unique `prefix`, `hash`, `label`, `expires_at`, `revoked_at`, plus the history stamps
  - the platform login gets SELECT, INSERT and UPDATE
  - no grant to `wamn_app` or `wamn_query`
- The PAT token format and hash from `wamn-identity-protocol` are reused.
- Service principals are `platform."user"` rows with `type = 'service'`, `issuer = 'wamn'`, and `sub` set to today's subject strings (for example `connection-<id>`).
- Control routes create and disable service principals, and issue, list and revoke their PATs, for one project environment.
- The host checks a PAT on its project login:
  - the prefix lookup, the hash, the expiry, the revocation, and the row's type and status
  - the existing connection-principal check
  - cached like grants
- These go:
  - the service principal rows and `create_service` in identity
  - `issue_pat`, `authenticate_pat`, `authenticate_route_pat`, `list_pats` and `revoke_pat`
  - the `identity.pats` table and the `/pats` route
  - `identity-client`'s PAT issuer
  - the host's `identity_reader`
- `api_service.rs` and `provision_project_env/pat_secrets.rs` use the new path.
- Connection principals and application principals also live only in the project database. `registry.connection_principal` goes.
- The operator PAT and the management-author PAT are retired. Nothing in production issued them.

6.6 User management through control.
- Control routes for users: create, invite, disable and list.
- One adapter in the control service calls identity over the operator-certificate HTTPS hop. Identity checks that the certificate subject is the control service's. Control is the only caller.
- The installer and `api_service` stop calling identity directly and use control routes.
- Identity gets a route for `disable_principal`. A disabled user cannot renew.
- Control uses its own certificate, `CN=control-<org>`. `user.disable` refuses while the user has an active membership in another org.

6.7 Identity fixes.
- Identity trusts `x-forwarded-for` only from a configured number of trusted proxy hops, the way `wamn-web` does.
- The signing private key is encrypted with the envelope that OAuth tokens use, moved into the new crate `wamn-envelope`, and stored in `identity.session_keys.private_key_envelope`.
- Two identity replicas (`deploy/charts/identity`). Password requests use a database pool, not a new connection for each request. Key rotation stays correct with two replicas.
- One control-eligibility rule: `environment-admin` goes, with its table, the SQL branch (`system-schema.sql`, near line 1222), `grant_environment_admin` and `revoke_environment_admin`.

6.8 Leftovers in `crates/identity` (N6).
- `set_environment_status`, `status_environments`, `org_environments` and `create_project` move from `wamn_identity::org` to `wamn-environment`.

## Epic 7: step 3.4, the workflow service

Rules:
- Only the work listed here. The Topic entry node is later work.
- Done means all of these pass:
  - `cargo build --workspace` and `cargo clippy --workspace --all-targets`, with no new error compared with the epic's base. The same in `apps/`.
  - `guests/workflow-service` builds for `wasm32-wasip3`.
  - The schema-apply check.
  - `helm template` of the release chart with values for the largest application in `apps/` that builds. The Helm release record must stay under 1 MiB. Report the size.
  - A local Postgres container is allowed for these checks only.
- Run no tests and write no new tests (3.8). If a test stops compiling and an import or name change does not fix it, delete the test.
- No kind or cluster runs, nothing on GCP.
- Generated code is regenerated, never edited by hand.

Issues:

7.1 Run interface in the host. Runs first.
- A new WIT package, `wamn:workflow@0.1.0`, served by a host plugin in `services/host`:
  - `run`: admit due schedule runs for a given time, admit runs for one data-change message, take a run whose lease expired, renew, and finish
  - `node`: call one sibling's export with JSON input for a run or a Request call. The host dispatches through the engine's dispatch targets and resolves components from `wamn.application`.
- The service exports `entry`:
  - `call`: a Request, synchronous
  - `run`: a durable run that the host admitted, for "run now"
- Identity for node calls:
  - Request: the caller, or the workflow's `execution_role`
  - Schedule and Data change: the `workflow` user with its `execution_role`
  - Run now: the admin's choice, under the same `execution_type` rule
- The run SQL stays in `wamn-run-state` on the host's platform login.
- `runs.manifest_digest` and `runs.node_digests_json` go, and leases carry no digest.
- `catalog.workflows`:
  - the trigger becomes `request`, `schedule` or `data_change`
  - add `path` for Request workflows, unique among the enabled workflows of one environment and application
  - the trigger columns are written only by the author step (7.5)
  - add the enable LSN; `enable_sequence` goes

7.2 Workflow service guest.
- `guests/workflow-service`, for `wasm32-wasip3`. It runs as the WorkloadDeployment `service`.
- A loop:
  - each minute, admit the due schedules and run them at once
  - every 30 seconds, take runs whose lease expired and run them
- It pulls from the consumer of each enabled Data change workflow through stock `wasmcloud:nats/jetstream`: admit the run through the host, ack, then run it. It refreshes its consumer list every 30 seconds and pulls with a 30-second long wait.
- It exports `wamn:workflow` `entry`.
- The graph walk is ported from the router deleted in 07c1f3ee3 (`07c1f3ee3^`), with the same node retries (3 by default).
- Response node: status and a JSON body. Nodes after a Response continue as a durable run.
- A Request call with no Response within 30 seconds is refused with a timeout.

7.3 Chart, deploy and the environment NATS account.
- `workloads.yaml` adds the workflow service as the `service` of every application WorkloadDeployment. The image is a platform image, built and pinned like the route guest.
- F7's NATS part: one NATS account and user per environment, the project source streams in that account, the environment Certificate in the org namespace; the per-project NATS users and Certificates go.
- The host group gets one stock `wasmcloud-nats` entry on the environment credential. Each application workload narrows `stream-allow` and `subject-allow` to its project.
- A `PLATFORM` account for the platform users, and one `ENV_<org>_<env>` account per environment, with limits fixed in code. The configure Job creates streams with the environment credential. Each CDC reader gets a publish-only user. The source stream gets a 7-day age limit and a byte limit.
- The CDC reader Deployment comes back through the environment chart. The fresh-install bootstrap writes the accounts document. Both runtime-operator charts move to 2.12.1.
- The workflow consumer created at enable has one filter subject per op.

7.4 Workflow routes in the host.
- The host registers the native prefix `/api/<application>/flow/` on the path from 5.3. For each call it:
  - authenticates the bearer token
  - resolves the enabled Request workflow by path
  - checks the caller holds the workflow's permission
  - dispatches to the service's `entry.call`
- Publish refuses authored routes under that prefix.
- An application host route `workflow.run` (admin only) admits a durable run, dispatches it to `entry.run`, and returns the run id.
- `workflow.author`, `workflow.enable`, `workflow.disable` and `workflow.list` are application host routes (admin only). Enable takes the environment key on the project database, checks the graph against the host's `wamn.application`, reads the source stream's last sequence and then the LSN, stores the LSN, and creates the workflow's consumer starting one after that sequence. Environment apply and delete also take that key on each project database they change. Disable deletes it.
- `workflow.start` goes: its route, its contract, its CLI verb and the `workflow_not_served` refusal.

7.5 Authoring check and CLI.
- `workflow.author` checks the graph:
  - exactly one entry node
  - a Request workflow has a Response on every path that ends
  - a Response appears only in a Request workflow
  - a Request path is unique
  Then it derives the index columns.
- Control's workflow routes go.
- The CLI verbs follow the routes.

7.6 Workflow leaves the release document.
- Node resolution comes from `wamn.application`. Palette nodes are dispatched by component.
- Delete the workflow readers of `ServingManifest` and `ServingContext` (`workflow/admission.rs`, `workflow/resolution.rs`, `events/transport/admission.rs` and their callers). Afterwards only deploy identity (3.5) and the edge read them.

7.8 Data change handlers in application components. Runs after 7.3.
- The route guest is renamed `ingress` (component name, `guests/ingress`, WIT `wamn:ingress`), because from 7.8 it receives HTTP and NATS and dispatches both.
- `wamn.k` declares model, ops (`insert`, `update`, `delete`) and role for a component, which exports a wamn handler interface taking the event as JSON.
- Release planning renders one durable `jetstream-subscriptions` entry for each (model, op) into the workload config, targeted at `ingress`, which exports the stock `jetstream-handler`. `ingress` checks the owner package and dispatches through the operation path.
- One entry for each (application, model, op), with queue group `<application>-<model>-<op>`. Release planning refuses two components that declare the same (owner package, model, op).
- The export is `wamn:handler@0.1.0/data-change` (`handle(event: string) -> result<_, string>`). `wamn:operation` gets `dispatch-data-change(component, event)`, which the host accepts only from `ingress`.
- At least once, no run row. Handler calls to wamn host functions run as the `workflow` user with the declared role.
- The invocation authority gets a handler case: connections come from the component's own bindings, and no per-operation grant check applies. Each handler has the operation host-transaction setting, on by default; when on, the transaction commits on ok and rolls back on an error.

7.7 Event crate merge. Runs last.
- `wamn-events` and `wamn-event-wire` merge into `wamn-event`, and the event transport code that nothing calls is deleted.

## Epic 8: step 3.5, deploy

Rules:
- Only the work listed here. The `platform` chart (merging `installation`, `identity` and `edge`), install (J) and settings (K) are 3.6.
- Done means all of these pass:
  - `cargo build --workspace` and `cargo clippy --workspace --all-targets`, with no new error compared with the epic's base. The same in `apps/`.
  - The schema-apply check.
  - `helm template` of every changed chart, with values for the largest application in `apps/` that builds. Each Helm release record stays under 1 MiB. Report the sizes.
  - A local Postgres container is allowed for these checks only.
- Run no tests and write no new tests (3.8). If a test stops compiling and an import or name change does not fix it, delete the test.
- No kind or cluster runs, nothing on GCP. The first kind run comes after 3.6, with D's approval.
- Generated code is regenerated, never edited by hand.

Issues:

8.1 Environment file.
- The environment file is YAML with a JSON schema (D2). Today `document.rs::compile` reads JSON.
- The registry binding tables (`registry.environment_application_connections`, `registry.environment_integrations`) follow the file: one version for each connection name, stated once in the environment file. This is the version authority for integrations.
- Each KCL `check:` rule in `wamn.k` also exists in Rust or SQL.
- `catalog/configuration.rs::read` stops selecting `release_digest`. The Helm revision is the release; its callers (`control_route/operations.rs`, `environment/document.rs`) read it from Helm.

8.2 Deploy flow.
- `deploy <dir>` builds once, publishes once, then runs SQL and code as one operation (B1). `publish <build dir>` pushes a finished build and prints the top package digest. `schema apply` and a deploy without `<dir>` take that digest.
- A1: CLI → control service → one Job. No Job starts a second Job. The environment document reaches the Job as a mounted file, not a command argument. The CLI poll (`operation_verbs.rs::run_value`) gets a limit.
- One check for each fact (application declared, environment exists, org matches). One pull of each package for each operation.
- `release_values` gets its caller here, with the bindings as input.
- A deploy checks the enabled workflows against the new `wamn.application`, under the environment key on the project database, and refuses when an enabled workflow names a sibling or export the release removes.
- Deploy creates the operation service accounts (`<env>-sql`, `<env>-code`) and the application principals, through charts and SQL.
- `/_wamn/residency` goes unless the deploy flow needs it.
- `wamn rollback [--revision N]` runs `helm rollback` in the worker Job, under the lock, after the enabled-workflow check. The application chart puts the top package digest and `.Release.Revision` on the WorkloadDeployment, and control reads them there.

8.3 Publish and artifacts.
- Publish does not pull back what it pushed. Tags are mutable: a publish under an existing tag with new content runs the append check against the digest the tag points to now, then moves the tag. A new tag has no append check.
- Components are pushed with stock `push_component` (`application/wasm`). The package artifact has two layers: `wamn.json` and the migrations tar.
- `wamn_catalog::manifest::installation` goes; its readers use the full package model.
- The deploy-identity readers of `ServingManifest` go. Afterwards only the edge reads it.

8.4 Charts.
- C1: every Kubernetes object comes from a chart, including the Secrets and ConfigMaps that Rust makes with `kubectl` or the Kubernetes API today. Rust runs SQL and NATS calls only. One upgrade for each chart.
- `release` becomes `application`. `environment` splits into `environment` (host group) and `project` (database, publication, accounts, certificates).
- No `helm template` pre-check and no `helm get values` read-back.
- One value each for the NATS address, the identity issuer address and the registry address.
- Delete the `wamn-platform-observer` Role, RoleBinding and ClusterRole with the read-back; today they let every service account in an org namespace read the installation namespace's Secrets.

8.5 Build.
- Codegen's describe cache (`.wamn/describe/`, committed, with a refresh flag and a check that it is current). The build uses a database only when SQL changes. The build is skipped when its inputs are unchanged.
- `wamn dev` applies new migrations.
- `build.json` goes. An overlay build reads the base's built wasm and `wamn.json` directly and hashes them.
- One KCL compile for each build.

8.6 `wamn-kubernetes` dependencies.
- The name builders leave `wamn-kubernetes`, so it drops `wamn-registry` and `wamn-catalog` (today one type, `ApplicationName`).

## Open items

- Checks in phase 3: Helm values size for the largest application (3.2).
- wasip3: needed by the workflow service. wash-runtime 2.12.1 has `p3` on by default with p3 service tests; Rust 1.100 has `wasm32-wasip3`.
- Deferred: the edge device after the restructure; the four workflow behaviours.
- 3.8: these tests compile but use SQL on tables Epic 1 dropped (`catalog.releases`, `catalog.wiring_tombstones`): `crates/provision/tests/{deploy_sql_authority,family_denial_matrix,record_history}.rs`, `crates/provision/tests/fixtures/catalog_record_history.sql`, `crates/identity/tests/fixtures/connections.sql`.
- 3.8: `crates/dev/tests/support/platform_schema.rs` and `platform_upgrade.rs` run the binary `wamn-ctl`, which Epic 1 deleted.
- 3.8: `docs/operations/running-tests.md` describes test targets that no longer exist.
- Clippy has 45 errors in test code that predate Epic 1. Epic 1 counted green as no new error.
- 3.6: a connection limit for each login, set in the CloudNativePG managed roles.
- After 3.5 only the edge reads `ServingManifest`. It goes with the edge design.
- 3.6: the control service takes the issuer from `session_issuer` and stage takes it from `platform.identity_issuer`. They become the one identity issuer address setting (E6, K).
- 3.8: `deploy/sql/postgres-init.sql` is an old test fixture that still defines `project_key()`, `current_project_key()`, the old row policies and its own roles. Only `crates/provision/tests/{family_denial_matrix,deploy_sql_authority}.rs` load it; it goes with them or is rewritten.
- Health probes for wasm integration types: `connection_test` cannot run integration code outside an application workload. To be designed.
- Edge device design: plant-network devices such as Modbus run at the edge, and the edge runtime gets its own address rule. A cloud integration that needs a private address (a customer network linked by VPN) is decided when a real case appears.
- Native integration types (for example the NetSuite ODBC driver) are native Rust host plugins compiled into the wamn host, as wasmCloud advises for drivers with no WASI binding. Decide this when the first one is built.
- Host component plugins (`host-component-plugins`) suit platform-wide wasm capabilities with fixed bindings. They are not used for connection-bound integrations.
- Edge device design: how exclusive connections are enforced at the edge (inference: one edge runtime for each site, so a local mutex may be enough).
- A call limit for each application, if a real case appears: either a per-host semaphore named as per-host, or a Postgres design decided with real numbers.
- 3.7: nothing writes the host route TypeScript clients since `cargo xtask host-route-clients` was removed.
- 3.6: nothing in the install fills the `wamn-control-administration-<org>` map that control reads its project logins from.
- 3.6: the first signing key, the installer source for the identity envelope settings, and the `CN=control-<org>` certificate.
- 3.7: the TypeScript clients cannot be generated again until codegen is reworked (the `admin_covered` contract changed in Epic 6).
- Topic entry node: after 3.4. Topics use stock `wasmcloud:nats`: application components publish within their workload's grants, and a Topic workflow pulls from its own consumer like Data change.
- The capture-gap observer in control is unwired (it defaults to `null`).
- 3.7b: split `wamn-sql`. `WamnPostgres` has 77 inherent methods mixing pools, auth lookups, per-component scope state, plugin glue and workflow node dispatch, and eight modules form one cycle (crate root, `executor`, `invocation`, `operation`, `operation_transaction`, `resources`, `session_identity`, `write_log`).
  1. `pool`, `host_project` and `types` move into `wamn-postgres`, after it loses `wamn-kubernetes` (P5).
  2. The auth lookups (`read_project_pat`, `route_pat`, `principal_grants`, `read_principal_grants`, `user_operation_permissions`, `resolve_subject`, `require_provisioned_principal`, `oauth_connection`) move to `wamn-authentication`, taking a pool from `wamn-postgres`. The host's authorization cache (`services/host/src/host/authentication.rs`) moves there too and becomes the one `AuthorizationStore`. `WamnPostgres` stops implementing that trait. Today a lookup goes `WamnPostgres` → the host cache (`self.authorization.get()`) → back into `WamnPostgres` through a `Weak` pointer.
  3. The rest is the operation host, renamed (for example `wamn-operation`). `session_identity` splits into the scope registry and the plugin glue. Workflow node and Data change handler dispatch stay in it, because a node runs under the operation transaction and write-log rules.
- 3.7b: capability crates hold control-plane code. `wamn-connection` holds connection administration and Kubernetes credential handling (`connections/administration.rs`, `credentials.rs`, `credential_cleanup.rs`), and the event crate holds Kubernetes lifecycle and capture-gap code. Split by plane: administration to control or environment; runtime use stays. Also split:
  - `wamn-broker`: the runtime NATS helpers (`nats::connect`, `tls_options`, `InitialConnect`, `events::event_consumer_config`) stay; `authorization.rs` (the NATS authorization document written as a Kubernetes Secret through `wamn_kubernetes::secret::update`) and stream provisioning move to the control side.
  - `wamn-http`: the OAuth flow and callback (`oauth_authorization`, `oauth_callback`) leave the capability crate and go with connection administration. This settles the conflict between the "Crates and names" line (OAuth callback → `wamn-http`) and the Part 2 P2 ruling (→ `wamn-connections`).

  Goal: the host and the CDC reader link neither `wamn-kubernetes` nor `wamn-provision`.
- 3.8: 19 crates still define a `test-util` feature, against AA. Phase 2.4 did not do this. `wamn-http` and `wamn-publish` enable test crates through it.
- With P5: generic HTTP clients live in infrastructure crates. `wamn-kubernetes::http_transport` is used by `wamn-web` and `wamn-edge`, and `wamn-oci::package_artifact::http_client` by `wamn-http` and `wamn-identity-service`. Use one small client module, or `reqwest` directly, so these crates stop linking Kubernetes or OCI code.
- 3.6, needs research and a ruling: the host reads connection secrets from the Kubernetes API on each call (`host/authentication.rs` builds `ConnectionCredentials::for_org`; `connection_credentials::read_secret` calls `KubernetesStore::in_cluster`). Epic 5 chose mounted `secretFrom` for project logins, and upstream has plugin-backed secrets (`plugin/secret_source.rs`, `wasmcloud_secrets`).
- 3.7b: `platform.tx_identity` is written in eight crates (`connection`, `environment`, `http`, `identity`, `provision`, `sql`, `services/control`, `services/identity-service`), through two helper modules (`provision/transaction_identity.rs`, `identity/transaction.rs`) and inline SQL. One function owns it.
- 3.7b: the identity service writes users and passwords as `PlatformComponent::Provisioning` (`users.rs`, `password.rs`) and depends on `wamn-provision` for it. It uses its own principal, and principal ids move to a small crate.
- 3.7b: `Refusal` moves out of `services/host/src/host/application_route.rs` into a module that both route modules use; today `host/workflow/lifecycle.rs` imports it upward from `application_route`. Optional, for a clear boundary only: `host::workflow` becomes its own crate. The host binary links it either way.
- 3.7b runs before 3.8, so service tests are written once, against the final crates.
- With P5: `wamn-kubernetes` drops its `wamn-catalog` dependency. Its only use is the type `wamn_catalog::ApplicationName`, in 3 places.
- Edge device: the edge runs the cluster `ingress.wasm` and turns every import it does not provide into a trap (`define_unknown_imports_as_traps` in `services/edge/src/serve.rs`). Risk: a capability missing on the edge fails on the first call, not at load. The stock `wasmcloud_nats` plugin cannot fill the NATS imports on the edge, because it needs a `servers` value and connects at bind. The edge design decides between a real NATS server on the edge (see the next item) and an HTTP-only build of `ingress`.
- Edge device, if the edge needs NATS: use the stock NATS way. A leaf node on the device with its own JetStream domain, one per edge environment. Mirror cloud streams to the edge, and source edge streams into the cloud. The edge then binds the stock `wasmcloud_nats` plugin to the local server, which also fills `ingress`'s NATS imports, and the guest code does not change. Not ruled; the edge design decides.
- After 3.6, with D's approval: the first kind run deploys through the new install and deploy paths, and covers a Data change handler (the subscription and the dispatch through `ingress`).
- 3.6: the NATS volume (10 Gi today) must hold the sum of all environment accounts, which is 256 MiB times the number of projects across all environments. Size it at install.
- 3.6: `cluster.k` checks the GCP project ID, the zone and the bucket names, but no Rust or SQL code does. Add the three checks in Rust.
- The xtask dependency check fails on `wamn-edge`, which links `oci-client` through wash-runtime. This predates Epic 7. Fix the check's allowance or the dependency.
- 3.6: RBAC that reaches past its need: `wamn-platform-operations` gives `wamn-operation-<org>` full Secret writes and RBAC `bind` and `escalate` in the platform namespace, and control's org Role has `get` on every Secret in the org namespace. Shrink both to named resources.
- 3.7: `apps/platform_ui` reads residency and live fields from the old status contract. Regenerate its clients against the new `environment.status` and `application.rollback` contracts.
- Recorded exceptions to C1 until 3.6: operation Jobs and their per-request input ConfigMaps, `wamn-credential-*` (written by connection administration at run time), and `wamn-nats-authorization`.

# Part 2: Review log

## Summary and principles

This proposal lists every change agreed in the review so far. Buckets are out of scope for now. Open items are in the last section.

D's direction (verbatim): "single responsibility, single source of truth, no rewrite, restructure or rename is too large. preference for deletion of all accidental complexity".

How we apply it:

- One owner per fact. If two files state the same thing, one is deleted or derived from the other.
- Delete before adding. Prefer the upstream or off-the-shelf tool over our own code.
- Names say what a thing is. Directory names equal crate names.
- No runtime checks for developer practices. They are documented, not enforced.
- Generated files are never edited. Hand-written files are never generated.

The main changes, in short:

1. Postgres capability shrinks to upstream's shape plus identity, the host transaction and pools.
2. Identity uses "first insert wins" instead of a signed token.
3. Crates are renamed and split so each has one job.
4. Developers write SQL, WIT for custom operations, and KCL for metadata only. We generate the rest.
5. Roles and ACLs become data. Apps ship with an admin role only.
6. Each configuration layer has one tool and one owner: OpenTofu, Helm values, KCL, SQL, Kubernetes Secrets.

## Postgres capability and identity

The capability follows upstream `wasmcloud:postgres` 0.2.0. We add only what upstream lacks: identity, the host operation transaction with its claim, and pools.

### Decided

- Two interfaces:
  - `client`: query only, on `wamn_query`, run inside a host read transaction.
  - `statements`: `operation-transaction`, then `query` and `query-batch`, with upstream's result shape.
- No guest `begin`. Composition keeps one host transaction across apps.
- No row limit. No `after_connect` privilege check. One pool map.
- Connections are declared per component.
- SQLx stays. Dynamic SQL is only for the analytics query explorer.
- No enforcement of "no external call in a transaction" (W16). It is a developer practice.
- Guest `COMMIT`: no SQL text check. The check runs at host commit, in the finish statement (W41).
- Retire the warm instance when a credential is refused.
- Local host runs without credentials until the kind step.

### Identity: first insert wins (W38)

- `platform.tx_identity`: UNLOGGED, `xid8` primary key, `principal_id NOT NULL`, INSERT-only grants.
- Trigger `check_identity_xid` enforces `xid = pg_current_xact_id()`.
- Every transaction inserts the row first, read transactions too. Every call carries a principal, service calls included.
- `wamn_authority.current_identity()` (SECURITY DEFINER) reads it. Migrations and platform logins resolve through `platform.login_principal`.
- Replaces the signed token. No `synchronous_commit` change: an unlogged insert writes no WAL.

### Follow-ups on lane/int2

- [ ] Delete the `app.project_env_key` `set_config` calls and the probe field.
- [ ] Add a test: no app login is a member of a login in `login_principal`. Needed because the fallback uses `COALESCE(NULLIF(current_setting('role'),'none'), session_user)`.
- [ ] `write_log::commit_sql(xid)` takes a bind parameter, not `format!`.
- [ ] Record W41 in `rulings.md`.
- [ ] Report the W43 dependency edge.

### Grants and tests

- Grant tests: refusal tests plus invariants derived from the role model (W42). Each SECURITY DEFINER function is owned by the owner of its database (pg\_database.datdba): wamn\_system in the system database, wamn\_db\_owner in project databases.
- `wamn_app` grants: provisioning applies default privileges for the migration role (W44).
- Delete the roster, presence and `pg_locks` checks.
- Keep the delayed-grant contract test, without its mock.

## Crate structure and names

Each crate gets one job and a name that says it. Directory names equal crate names. Names are singular. No "capability" prefix or suffix.

| Today | Change | Why |
| --- | --- | --- |
| `wamn-postgres-capability` | Rename to `wamn-sql`. Do not merge into `wamn-postgres` (W43). | Merge makes a dependency cycle. |
| `wamn-web-front` | Rename to `wamn-web`. | Says what it is. |
| `wamn-ctl` (`services/ctl`: the `wamn` CLI plus the hidden `wamn-ctl` bootstrap binary) | Rename to `wamn-cli` at `services/cli`, holding only the `wamn` binary. The three bootstrap verbs (`provision-system`, `provision-org`, `provision-identity-issuer`) become subcommands of `wamn-control`, which bootstrap Jobs run. The `ctl` image and chart key, the `wamn-ctl` NATS user and certificate, and every "ctl" name and comment go. | It is the CLI; one image fewer; "ctl" names nothing. |
| `wamn-control-provision` (`crates/provision`) | Rename to `wamn-provision`. | Directory equals name. |
| `wamn-compiler` | Rename to `wamn-codegen`. Split by output (Rust, WIT, SQL, feed, OpenAPI). | One output per module. |
| TypeScript client codegen (`client_ts.rs`, `client_plan.rs`, `client_ir.rs`: 4,047 lines) | Delete. Use an off-the-shelf generator. | Not our code to own. |
| Component and package artifact crates | Merge into `wamn-oci`. | One OCI owner. |
| `wamn-connections` + `wamn-integrations` | Rename to `wamn-connection`; `wamn-integrations` merges in (W37). | Singular; one owner of connections. |
| `wamn-events` + `wamn-events-capability` | Rename to `wamn-event`; the capability merges in (W37). | Singular; one owner of events. |
| `wamn-event-wire` | Rename to `wamn-event-type`. Subjects and message ids move to `wamn-broker`. | Types only. |
| Role modules spread across crates | Collect in `wamn-role`. Move stray modules out. | One owner of roles. |
| `wamn-release-control` | Delete. | No unique job. |
| `wamn-platform-identity` (`crates/identity`, 6.4k lines) | Split. `connections` → `wamn-connection`. `org`, `environment` and `application` rows → `wamn-registry` or `wamn-environment` (owner to check). The rest (passwords, sessions, session keys) moves into `services/identity` if that is its only user; otherwise it becomes `wamn-principal`. | `wamn-identity` is taken by the service; one job per crate. |
| `wamn-execution-host` (12.2k lines) | Split by owner. File routes, file collection and file collector → `wamn-blobstore`. OAuth callback, connection health and credential cleanup → `wamn-connection`. Number counter set route → `wamn-sql`. Control route → `services/control` (to check). The rest (host HTTP entry, operation dispatch, readiness) folds into `services/host` if only services use it; otherwise it becomes `wamn-dispatch`. Drop the `wamn-workflow` edge (P5). | One job per crate. |
| `wamn-postgres-statements` + `wamn-data-access` | Merge into `wamn-guest`. | One guest library; says where it runs. |
| Control execution adapters in `wamn-environment` | Move to `services/control`. | Service code, not library. |
| `wamn-environment-control` | Split into `wamn-environment` and `wamn-platform-install` (landed). | One owner each (see deploy). |
| `wamn-build` + `wamn-component-composer` | Move to `crates/build`. Fold the composer in. | One build owner. |

Test crates keep the idiomatic `-tests` suffix (`wamn-wms-tests`, `wamn-receiving-tests`, `wamn-journey-tests`).

### Invocation types (option B)

- `ReleaseContext` holds `ConnectionFacts`. That causes the cycle.
- Move `ConnectionFacts` to the authority crate.
- The integrations merge waits for this move (see open questions).

### Types and tests (T1–T3)

- Delete serde library-behaviour tests. They test serde, not us.
- Add newtypes: `Digest`, `EnvName` / `ProjectEnvKey`, `PrincipalId` / `LoginName`, `PackageName` / `ApplicationName`.
- Audit seven crates for tests the type system already guarantees.
- Keep TypeScript spelling tests only if their set is derived.

## Contracts, codegen and KCL

The developer writes SQL, WIT for custom operations, and KCL metadata. Everything else is generated, one way, and never edited.

### What the developer writes

| Input | Owner of | Notes |
| --- | --- | --- |
| SQL migrations | Tables, columns, types, nullability | The schema is the source of model types. |
| Declared SQL statements | Queries the app runs | Checked at build by SQLx `describe`. They stay. |
| WIT for custom operations | Inputs and results of custom operations | Hand-written, because their inputs are not columns. |
| KCL (`wamn.k`) | Metadata and policy only | UI configuration, user-facing labels and similar. |

### What we generate

- Model WIT from the schema, with named `id` and `revision` types.
- Rust bindings from the WIT.
- Statement parameter and row types from SQLx `describe`. Stop restating them in `wamn.k`.
- TypeScript client from an off-the-shelf tool, not our code.

### KCL rules

- KCL holds overrides on top of defaults derived from the schema.
- KCL is never generated. No generate-then-edit loop.
- A KCL name that matches nothing in the schema or WIT is refused at build.
- Remove type fields from `schema Field` and `schema Value` where SQLx already derives them.

### Why SQL stays in the guest

- Standard wasmCloud: the guest holds the SQL text and the host runs it.
- SQLx `query!` cannot be used in guests. Its expansion calls the driver, and guests have no DB socket.
- `describe` still gives types and nullability at build. That is what we use.

### Sync feed (option C)

- Option C: keep only the server-side change feed on list reads. The client sync generator and outbox go until screens adopt them.

### Codegen method

Use off-the-shelf builders wherever one exists. No hand-built source text where a library can build it.

| Output | Today | Change |
| --- | --- | --- |
| Rust | `format!` / `writeln!` (57 calls), then a `rustfmt` subprocess (`rustfmt.rs`) | `quote` + `syn` (already workspace dependencies) and `prettyplease`. Delete `rustfmt.rs`. |
| WIT | 185 format calls in `generate/wit.rs` | `wit-encoder`, pinned to the same wasm-tools release as the `wit-parser` 0.254.2 already used. |
| OpenAPI | `serde_json::json!` in `generate/publication.rs::openapi` | Typed OpenAPI 3.1 structs from `oas3` (`openapiv3` covers only 3.0; we emit 3.1.0). |
| TypeScript client | Our generator (`client_ts.rs`, \~4,950 lines) | Off-the-shelf generator from the OpenAPI file. |
| SQL (history triggers, policies) | Text templates (64 calls) | No good off-the-shelf builder for triggers and policies. Keep the templates. SQLx checks them at build. |

### JSONata transforms

- Each transform declares an input schema and an output schema.
- The output is validated at run time against the declared output schema. Schemas carry no examples.

## Permissions and data

Roles and ACLs are run-time data, edited by an app administrator. They are not KCL and not code.

- The permission vocabulary (the actions a role can be granted) is derived from the schema and the operations. Nobody writes it by hand.
- Apps ship with one role: admin.
- Other roles and ACLs are rows in SQL, edited in a UI. Nothing writes them back to KCL.
- A tool that suggests starter roles is a separate concern, not part of this work.
- API callers are service principals (Q11 = A). Q13 = A.
- Bounds on calls (Q29) are stated in project or application configuration.
- No edge connection cap (Q8).

### Refs and revisions

- A model field that points at another model has a named `id` type. Its `revision` type carries the row version used for ETags and conflict checks.
- Both come from the schema. A custom operation only refers to them when its WIT input uses a model's `id` or `revision` type.
- Keep `route.reads`: it feeds list ETags in `catalog/http_routes.rs`.

## Deploy path and web routes

Deploy ownership follows option A. `wamn-environment` owns everything about an environment, including its Helm release and credentials. `wamn-platform-install` owns only the platform and orgs.

- `wamn-environment-control` splits into these two crates.
- Move `dev::source_deploy` into publish. Dev and cloud use one publish path.
- The control route is the one execution path for deploy verbs.
- `wamn-web` keeps reading route and release ConfigMaps. `apply` writes them; nothing else does.
- The environment-configuration ConfigMap (`hosts/<environment>` with `environment.json`) stays removed. Environment configuration lives in registry tables, with `environment.k` as input.
- The release profile route follows W40.

## Configuration layers

Five layers, each with one tool and one owner. No Rust code builds configuration documents.

| Layer | Tool | Owns | Written by |
| --- | --- | --- | --- |
| Infrastructure | OpenTofu | Project, network, cluster, databases | Platform operator, `.tf` files |
| Platform install | Helm values | Chart settings per installation | Operator; infra part generated by OpenTofu |
| Apps | KCL | `wamn.k` (app) and `environment.k` (environment) only | App developer |
| Run-time data | SQL | Roles, ACLs, connections, app data | Admin UI and `wamn` CLI |
| Credentials | Kubernetes Secrets | Passwords, keys, tokens | Provisioning |

### Infrastructure (A1)

- Write OpenTofu directly. Delete `deploy/gcp/project.k` and `xtask/src/platform_tofu.rs` (808 lines).
- OpenTofu writes its outputs to `infra.values.yaml` with `local_file` and `yamlencode`.
- That file is the only place infra facts (`project_id`, `zone`, `cluster_name` and similar) reach Helm. Delete the cross-check in `platform_pools.rs`.

### Platform install (B)

- One Helm values file per installation, plus `infra.values.yaml`.
- Delete `deploy/gcp/cluster.k`, `deploy/kind/cluster.k` and the Rust document code (`platform-install/platform/document.rs`, `artifact_plan.rs`, `bootstrap.rs`, `org.rs`).
- Add `values.schema.json` to the chart, so Helm refuses bad values.
- Convert `deploy/platform/*.yaml` into chart values or templates. Remove the duplicate `registry-helper`.

### Local (option A)

- kind config plus a static `infra.values.yaml` checked into the repo.
- A key-parity check: the local file has the same keys as the OpenTofu output. This is how dev config reaches cloud without surprises.
- Same chart, same schema, same deploy path as cloud.

### Other cleanup

- `deploy/gcp/connections/*.json` are request bodies for `wamn connection write/test --file`. Move them to docs as examples.
- Cut the 108 environment variables and 202 CLI flags down to what the layers above do not already own.
- Ignore the one-off guard tool. Buckets are out of scope for now.

## Status of the sections above (pass of 2026-11-16)

Every line above not listed here stays as written. Lines listed here are changed or gone by a later ruling in this doc.

| Section and line | Status | By |
| --- | --- | --- |
| Summary: "Each configuration layer has one tool and one owner: OpenTofu, Helm values, KCL, SQL, Kubernetes Secrets" | Changed: KCL is used for `wamn.k` only; the environment file is YAML with a JSON schema | D2, K8a |
| Postgres: "Retire the warm instance when a credential is refused" | To re-check: the host's own loader and warm trust list go; the operator runs components with `poolSize` | Core request path (A) |
| Follow-ups: "Report the W43 dependency edge" | Done: `wamn-integrations` merged into `wamn-connections` (commit 463da0ca) | — |
| Grants: "…`wamn_db_owner` in project databases" | Changed: one owner role for each database, no shared `wamn_db_owner` membership across projects | S |
| Crates: `wamn-compiler` → `wamn-codegen`, "split by output (Rust, WIT, SQL, feed, OpenAPI)" | Changed: the `wamn.json` model moves to `wamn-catalog`. OpenAPI: see the conflict below. | P4, L2 |
| Crates: component and package artifact crates → `wamn-oci` | Changed in content: components use the stock artifact (`push_component`); the package artifact keeps two layers | H |
| Crates: `wamn-events` + capability → `wamn-event`; `wamn-event-wire` → `wamn-event-type` | Changed: `wamn-event-wire` and `wamn-events` merge into one `wamn-event` | P1 |
| Crates: `wamn-platform-identity` split; rest into `services/identity` or `wamn-principal` | Changed: the library becomes `wamn-identity`; the service package becomes `wamn-identity-service` (binary `wamn-identity`); the non-identity half moves to its owners | P1, N6 |
| Crates: `wamn-execution-host` split; rest becomes `wamn-dispatch` if needed | Changed: the rest folds into `services/host`; no `wamn-dispatch`, no `wamn-route` | P2 |
| Invocation types: move `ConnectionFacts` to the authority crate | To re-check: `ConnectionFacts::version` and the connection snapshot go; connection facts sit in the host's local cache | F1 connection rulings |
| Codegen table: OpenAPI row; TypeScript client "\~4,950 lines" | OpenAPI: see the conflict below. The client generator is 4,047 lines in three files. | L2 |
| Permissions: "Bounds on calls (Q29) are stated in project or application configuration" | Changed: project only; the application level goes | F1 grants ruling |
| Refs: "Keep `route.reads`: it feeds list ETags in `catalog/http_routes.rs`" | Kept as input; it now reaches the guest through the workload configuration written at deploy, not through the release manifest | G2 |
| Deploy: "`wamn-web` keeps reading route and release ConfigMaps" | Kept for routes. `release.json` no longer carries a release document; its unset `org` and `project` values go | G2 |
| Deploy: "Environment configuration lives in registry tables, with `environment.k` as input" | Changed: input is one YAML environment file per environment | D2, K8a |
| Layers table, Infrastructure row: OpenTofu owns "databases" | Changed: OpenTofu owns project, network and cluster. Databases are CloudNativePG clusters rendered by the project chart | W2 |
| Layers table, Apps row: "`wamn.k` and `environment.k`" | Changed: `wamn.k` only; the environment file is a Helm-style values file | D2 |
| Platform install: "Convert `deploy/platform/*.yaml` into chart values or templates" | Changed: delete them; the charts already render the same objects | E7 |
| Local: "A key-parity check" | Gone | K5 |
| Other cleanup: "Cut the 108 environment variables and 202 CLI flags…" | Made concrete: constants become code defaults; each service reads one rendered file with its real inputs | K |

Open items, checked at commit bf8f13b7:

| Item | Status |
| --- | --- |
| Open in F1: type version | Ruled: digest in the workload config, written at deploy (OI-2a) |
| Open in F1: bindings stored twice | Ruled: binding tables are the stored environment file; one version statement per name |
| Open in F1: release digest | Ruled: the Helm revision is the record (release delivery ruling, G2) |
| Open in F1: five insert sites for users | Ruled: one writer (N4) |
| Open in F1: `project_env_key` | Ruled: leaves the project database |
| Open questions 1: integrations merge | Done: `crates/integrations` no longer exists (commit 463da0ca) |
| Open questions 2: `classify_statements` | Ruled, not landed: still in `compiler/src/materialize.rs` |
| Open questions 3: W37 wording | Ruled, not landed: the old sentence is still in `rulings.md` |
| Open questions 4: stale identity comments | Ruled, not landed: 7 lines still name `app.user_id` / `app.role` in `app-schema.sql` |
| Open questions 5: buckets | Out of scope |

### Conflict to rule: OpenAPI

- L2 deleted `openapi.json` because nothing reads it today.
- The codegen ruling (2026-11-15) makes the TypeScript client come from an off-the-shelf generator fed by the OpenAPI file.
- Both cannot hold once that generator is built.

* Ruled (D, 2026-11-16): OA2. `openapi.json` stays as the input of the off-the-shelf TypeScript client generator. L2 now deletes only `required-schema-contract.json` and `build.json`.

## One fact, one store (F1)

A request touches two databases, and rows are copied between them. Each copy has writer code, a change notice on NATS and tests. This section records the rulings of 2026-11-15 that remove copies. Facts are from `worktree-arch-review` at 389bab3.

### Facts

- The system database holds `registry` and `identity`. Each project database holds `platform`, `catalog` and the application.
- Applications reference users and connections by foreign key. A foreign key cannot cross databases, so the rows are copied.
- Grants: one query on `platform."user"`, `user_role` and `permission` (`authentication/authorization.rs`). Each host also keeps the answer in a NATS KV replica (`authority/auth_cache.rs`, `authority/kv_memory.rs`). The replica holds four answers: grants, environment inactive, PAT admitted, session active.
- Connections: `registry.external_system`, `integration` and `connection` are copied to `platform.*` by `identity/connection_projection.rs`. The host reads connection, generation and credential reference from the system database (`connections/connection_credentials.rs`).
- An integration row holds a name, a type, an auth type and an optional external system. It holds no parameters.

### Ruled: grants and the auth replica

- The NATS KV replica goes. Each host keeps one local `moka` cache for the four answers.
- No drop notices. `broker/auth_changes.rs`, `AuthDrops` and the `wamn-auth` bucket go. `kv_memory.rs` and the watch half of `auth_cache.rs` go.
- The cache lifetime is a setting per project: `registry.environment_projects.revocation_seconds`.
- The application level goes: `registry.environment_applications.revocation_seconds` and its `environment.k` field.
- All four answers use the same lifetime.
- Only positive answers are cached, so a new grant or login works on its first request.
- Accepted cost: a revoke, a disabled user and an inactive environment take effect up to one lifetime late on every host.

### Ruled: connections

- A connection belongs to one project in one environment. The integration table goes; its four fields become columns of `connection`.
- The project database owns `connection`, `connection_generation`, `external_system`, the credential reference and the two OAuth tables. The registry copies and `connection_projection.rs` go.
- This replaces the 2026-10-28 ruling (integration per project) and the 2026-11-10 ruling (integration per environment).
- `integration_type`, `integration_type_version` and `auth_type` stay in the system database. They are the platform's list of integration code.
- Connection facts in the host use the same local `moka` cache and project lifetime as grants. The connection KV replica and `ConnectionDrops` go.
- Accepted cost: a changed parameter or credential is late by up to one lifetime. The "same type in every environment" rule becomes a convention; apply still refuses a binding whose connection lacks the declared interface.

### Ruled: connection call path

- The candidate branch goes, with its tests: `ConnectionExecutionClosure::Candidate`, `CandidateBindingWorld`, `CandidateConnectionBinding`, `matches_snapshot`, `authorize_candidate_closure`, the Candidate branches in `sql/connection_binding.rs::resolve` and `sql/invocation.rs::validate_invocation`, and the snapshot fields `pinned_generation` and `instance_revision`. No production code builds a candidate.
- A binding is validated once, at deploy, and written into the workload config (OI-2a). `catalog.connection_binding` goes.
- The host reads the bindings from the workload config when it loads a workload. It holds, per component and declared name: connection id, read or write mode, version.
- Per call the host looks up the binding, reads the cached connection row, gets the secret and sends. `ConnectionEffectSnapshot`, `authorize_snapshot`, `authorize_release_closure` and the three hashes go.
- Rule: a connection's type and auth type never change. A different type is a new connection.
- Accepted cost: no per-call recheck of the binding rules.

### Ruled: exclusive resource lock and transport limits

Ruled 2026-11-15, confirmed against `connections/resource_lease.rs`, `integrations/lib.rs` and all of `http/transport.rs`.

- Exclusive resource lock: per call. A local mutex per resource, then a blocking `pg_advisory_lock` with a lock timeout on a pooled connection held for the call. After the call the host tests that connection; a dead one discards the result. The state machine, the session task, the 100 ms poll and the revisions in `resource_lease.rs` go.
- Inference from the code, not run: today a lease is released only when the connection changes, the watch clears or the host stops. A second connection with the same resource key, or a second host, waits 30 seconds and fails. The per-call lock removes this.
- Transport limits: the five semaphores in `http/transport.rs` go (clients, sockets and requests for the host; sockets and requests per connection). The invocation deadline, the body and header size limits and the pinned peer address stay.
- Accepted cost: one pool connection per exclusive call in flight; no cap on sockets from one application to one remote system.

* HTTP client cache key: invocation scope, target (host, address, TLS name), connection id and generation. `ClientIdentity`, `ConnectionIdentity` and the two hashes in the key go.
* Reason: a client lives only as long as its invocation (`ClientExecutor` in `http/transport.rs`), so the project, package and component fields never vary. The connection id and generation stay so that a socket never serves two connections. Today every supported auth type sends its credential in each request (`http/authentication.rs`), so this guards a later client certificate or socket-bound login.

### Ruled: release delivery and storage

Principle, ruled 2026-11-15: deploy stays as close to stock wasmCloud as possible.

- The operator is the only carrier of "which release" to the host. The host treats a release as live while a workload for it is bound, as wasmCloud does. Old and new are both live during a rollout.
- The chart renders a workload for every application. It has the HTTP interface only when the release has routes.
- File routes register on the wasmCloud router for each workload, the way `/wamn_control/` does today (`platform_http.rs::bind`). The second listener (`file_route.rs::serve_selected`), port 8082 and the `<release>-files` Service go.
- In the host these go: the ConfigMap list once a second (`releases.rs::selected`), the selection for each application, the pause on two ConfigMaps, and the comparison of `release.json` with annotations.
- `registry.environment_applications.release_digest` and `configuration::write_selection` go. The Helm revision is the one record of the deployed release. The four readers (`environment/mod.rs`, `bindings::analyse`, `retention.rs`, `lifecycle.rs`) read it from Helm, as `status.rs` and rollback already do.
- Superseded by G2: `catalog.releases` goes (Epic 1, wave 2.3). The release is the Helm revision.
- Accepted cost: a release loads when its workload binds, not ahead of it. During a rollout two releases admit schedules and events; their unique keys make one run. `/_wamn/residency` needs a new place.
- This corrects the line under "Deploy path and web routes": only the web service reads the route and release ConfigMaps. The host does not.

### Found by reading the connection call path

Read in full: `registry/connections.rs`, `connections/connection_credentials.rs`, `connection_facts.rs`, `connection_memory.rs`, `authority/connection.rs::authorize_snapshot`, the snapshot builder in `sql/session_identity.rs`, `integrations/lib.rs::resolve`, `blobstore/plugin.rs::container_for` and `file_store`. Not read yet: `http/lib.rs`, OAuth, resource leases, `authority/effect_span.rs`.

- A call needs five facts: the bound connection, is it active, its parameters, its secret, read or write.
- On every call the host compares nine identity fields, reads bindings, connection and generation through separate caches, builds a 25-field `ConnectionEffectSnapshot` with three SHA-256 hashes, and then `authorize_snapshot` tests fields the builder set itself (`node_permitted` is always true).
- `blobstore/plugin.rs::container_for` then reads the connection and generation a second time.
- Inference: the snapshot keeps the shape of the SQL row that W20 removed.
- Direction, not ruled: resolve the binding once when the host loads a release. Per call, one cached read of the connection row remains.

Second read, also in full: `http/lib.rs` (non-test part), `sql/connection_binding.rs`, `authority/identity.rs` (snapshot and candidate types), `authority/effect_span.rs`, `environment/bindings.rs`. Still not read: `http/transport.rs` body, `connections/oauth_token.rs`, `resource_lease.rs`, `production_lease.rs`.

- Fact: no production code builds `ConnectionExecutionClosure::Candidate`. Only tests do (`http/lib.rs` tests, `environment/tests/connection_binding_live.rs`). So `CandidateBindingWorld`, `CandidateConnectionBinding`, `matches_snapshot`, `authorize_candidate_closure`, the Candidate branches in `sql/connection_binding.rs::resolve` and `sql/invocation.rs::validate_invocation`, and the snapshot fields `pinned_generation` and `instance_revision` serve tests only.
- Fact: the same binding rules run three times. Apply runs them in `bindings::analyse` and again in `bindings::write` (interface exported, read or write mode, parameters valid, type served). Every call runs them again (snapshot builder, `authorize_snapshot`, `authorize_release_closure`).
- Fact: `http/lib.rs::attempt` reads the connection row and the generation after `resolve` already read both.
- Fact: `validation_hash` and `definition_hash` have one production reader: they are parts of the HTTP client cache key (`ClientIdentity` in `http/transport.rs`). `effect_span.rs` does not read them. `requirement_hash` is read only by the candidate matcher and as input to `validation_hash`.
- Inference: the key already holds connection, component digest, name and generation, and a generation is immutable. So the two hashes add nothing to the key.
- Fact: on a 401 or 403 the plugin calls `read_again`, which sends a drop notice to every host and reads the row again. This retry is needed (a rotated credential). Under the `moka` ruling it becomes a local drop and one read.

Third read, in full: `connections/resource_lease.rs`, `production_lease.rs`, `oauth_token.rs`, and `http/transport.rs` up to `client`. Not read: the `PinnedConnector` half of `transport.rs`, `http/oauth.rs`, `http/authentication.rs`.

- `resource_lease.rs` (489 lines). Must do: one caller at a time for one physical resource, across hosts. Does: its own state machine (four states in atomics, epochs, `Notify`), a task with a command channel on its own database session, a `pg_try_advisory_lock` poll every 100 ms, pings, a lost state, a map of local mutexes, and revisions tied to the KV watch.
- `production_lease.rs` (293 lines) is the run queue lease API of workflows. It is in the connections crate. Five of its methods repeat the same "resolve runner" block. It belongs to the workflow path.
- `oauth_token.rs`. Must do: keep refresh tokens durable and let one host refresh at a time. Does that with an encrypted row and a row lock. Its local cache uses its own revision tickets, one of them tied to the KV watch.
- `transport.rs`. `execute` and `send_once` repeat about 45 lines. It holds five semaphores (clients, sockets and requests for the host; sockets and requests per connection).
- Inference: the HTTP client cache key includes the invocation scope, and a client is dropped when its invocation ends. So a client is reused only inside one invocation, and the seven-field key rarely hits.

Fourth read, in full: `services/host/src/host/releases.rs` (non-test), `host/projects.rs::prepare` and `for_workload`, `workflow/run_queue.rs::release_bytes`. Not read: how `host.rs` binds a workload and uses a selection, `catalog/configuration.rs`, the apply code that writes the release ConfigMap and the registry column.

- Release bytes: the host loads them from `catalog.releases` in the project database (`run_queue::release_bytes`). The OCI artifact carries them from publish to apply. A table constraint ties the digest to the bytes. This is an installed copy, not a second owner.
- `release_bytes` and `unfinished_releases` live in the workflow crate, and `releases.rs::load` builds a workflow `QueueService` for every release. So loading any release depends on the workflow crate.
- `load` parses the release bytes three times (`from_canonical_bytes` twice, then `load_canonical_bytes`).
- Release digest: the host gets it two ways. The workload annotation `wamn.release-digest` arrives through the operator (`for_workload`). The host also lists every ConfigMap in its namespace through the Kubernetes API once a second for each project (`releases.rs::selected`, called from `refresh`), reads `release.json`, and compares it with the annotations on the same ConfigMap.
- The same one-second loop reads unfinished runs from the project database, to keep their releases loaded.
- `projects.rs::prepare` opens, for each project, five logins to the project database and two to the system database (identity reader, connection reader), plus one NATS connection. The connection reader goes under the connections ruling.

Fifth read, in full: the rest of `host/releases.rs::serve`, `catalog/configuration.rs` (`read`, `write`, `write_selection`), and the deploy step in `environment/mod.rs` that records the selection and runs Helm. Not read: how the host binds and unbinds a workload (`execution-host/platform_http.rs`, `engine/flow_http_routing.rs`, `operation/native_policy.rs`).

- What the host uses the polled ConfigMap value for: file routes by hostname, workflow schedule admission, workflow event admission, loading the current release ahead of a workload, and the residency report. Operation requests do not use it; they use the workload annotation.
- The poll is a list of all ConfigMaps in the namespace once a second for each project. Nothing in Kubernetes requires a poll: the workload already arrives through the operator, and the Kubernetes API has a watch.
- `registry.environment_applications.release_digest` has one writer, `configuration::write_selection`, called once in the deploy step of `environment/mod.rs`.
- That step commits the column first and runs the Helm upgrade after, in a separate step. Inference: if the upgrade fails, the column names a release that Helm did not deploy.
- Readers of the column: the next plan (`environment/mod.rs`), `bindings::analyse`, `retention.rs` and `lifecycle.rs`. `status.rs` does not read it; it reads the digest from the Helm revision. A rollback with no digest reads Helm history (`analyse::rollback_revision`) and then writes the column.

### How wasmCloud handles a rollout (research, not ruled)

Read in the wasmCloud source at v2.12.1 (commit 8b0e24cb, the revision wamn pins): `runtime-operator/internal/controller/runtime/workload_deployment_controller.go` in full, and the router in `crates/wash-runtime/src/host/http.rs`. Also the wasmCloud operator documentation.

- A changed `WorkloadDeployment` template makes the operator create a new `WorkloadReplicaSet`. The deployment status records `currentReplicaSet` and `previousReplicaSet` (`reconcileDeploy`).
- The operator deletes the previous set only after the current one is available on a host (`reconcileScale`). With `deployPolicy: Recreate` the old one stops first.
- The host router maps a hostname and path prefix to a set of workloads. A request picks one of them at random (`DynamicRouter::select_workload`). So during a rollout the old and the new workload both serve.
- The deployment's annotations are copied to the replica set and the workload, and reach the host with the workload.
- wasmCloud has no single "current release" value for the host to read. Desired state is the resource in Kubernetes; the host is told by the operator.

What wamn adds on top (from the fourth and fifth reads): a release ConfigMap, a list of ConfigMaps once a second in the host, one "selection" for each application, and a pause when two ConfigMaps name one application.

Read in wamn for this: `workflow/schedule.rs` in full and `workflow/admission.rs::admit_schedule_run`.

- Schedule admission is safe with two releases live. Its key is `schedule:<application>/<workflow>:<due>` and `wamn_run.runs` has a unique index on that key (`runs_idempotency`). Two admitters make one run.
- Not read yet: `workflow/event_admission.rs`, `execution-host/file_route.rs`, `engine/expected_router.rs`, `execution-host/platform_http.rs`. They decide whether event admission and file routes also work with two releases live.

Research completed. Read in full (non-test): `workflow/event_admission.rs`, `execution-host/platform_http.rs`, `engine/expected_router.rs`, `release/release_chart/plan.rs::roles`, the file listener in `services/host/src/host.rs`, and in `execution-host/file_route.rs` the connection lookup, `native_route` and `serve_selected`. In wasmCloud v2.12.1: `ServiceHttpJob` in `host/http.rs`.

- Event admission is safe with two releases live. Its key is `["event", environment, application, workflow, "event:<source event id>"]` under the same unique index as schedules.
- File routes do not go through the wasmCloud router. The host opens a second listener (port 8082, `file_route.rs::serve_selected`) behind a `<release>-files` Service. It maps the Host header to a release through the selection. This is the one use that needs a hostname to release map.
- wamn already serves a native path through the wasmCloud router for each workload: `platform_http.rs::bind` registers `/wamn_control/` as a path route for the workload id. Upstream's `ServiceHttpJob` carries a streaming request body.
- `expected_router.rs` wraps the upstream router to answer "unavailable" for an expected hostname with no workload, and while stopping. It adds no selection.
- The chart renders a workload only when the release has an HTTP route (`release_chart/plan.rs::roles`). An application with only workflows gets no workload, so the operator tells the host nothing about it. Inference: this is why the ConfigMap path exists.
- The file listener also serves `/_wamn/residency`, which the deploy step reads while it drains.

### Ruled: type version and binding configuration

- The deploy writes the integration type digest into the workload config with the other operation facts (OI-2a); the host reads it there. `catalog.release_connection` goes. `ConnectionFacts::version`, its cache and the two reads that only test that the version row exists go (`blobstore/plugin.rs::file_store`, the `file_route.rs` lookup). The host makes no system database read for connections.
- The environment file (YAML, D2) states a connection's version once, in a per-name section `connections: {name: {version}}`. The `name@version` form on a reference and `registry.environment_application_connections.connection_version` go. The word "integration" leaves the environment file.

### Ruled: no deployment key inside a project database

- `project_env_key` leaves the nine project database tables that carry it: `catalog.packages`, `releases`, `connection_requirement`, `release_connection`, `connection_binding`, `wiring_tombstones`, `workflows`, and `wamn_run.runs`, `wamn_run.run_queue`. Their keys and indexes lose the column.
- The row policy by project on those tables goes. The part that requires an identity to be set stays.
- `wamn_authority.project_key()` and `wamn_authority.current_project_key()` go.
- The key parameter leaves about 100 SQL statements in non-test Rust.
- The key stays where more than one deployment shares a store: system database rows, NATS subjects and keys, resource names.
- Reason: one database holds one project in one environment. A copied database then needs no row rewritten.
- Accepted cost: no second barrier if an application login were granted on another project's database. `host/projects.rs::prepare` still refuses a credential for the wrong database.

### Remaining F1 points: research (not ruled)

Read for this: every caller of `ConnectionFacts::version`, `bindings::analyse` and `bindings::write`, `catalog/configuration.rs::read` and `write`, and the database name built in `host/projects.rs::prepare`.

- Type version. The host calls `facts.version` in three places. Two discard the answer and only test that the row exists (`blobstore/plugin.rs::file_store`, `file_route.rs` lookup); apply already tested that in `bindings::analyse`. One uses the digest, to pull the bytes of a Wasm integration (`integrations/lib.rs::resolve`). Apply already holds that digest (`Binding.digest`) but `bindings::write` does not store it.
- Binding tables. `registry.environment_application_connections` and `registry.environment_integrations` are the stored form of `environment.k`. They are not copies of the catalog rows, which are per release and resolved. But the version is stated twice: once per name in `integrations`, once on the reference as `name@version`. `bindings::analyse` refuses when the two differ.
- `project_env_key`. A database is one project in one environment (its name is built from org, project, environment and a suffix). So the column is constant in `catalog.*` and `wamn_run.*`. It appears 935 times in non-test Rust and 51 times in the project database SQL files. It is the only thing that marks rows from a database copied from another environment.

More on `project_env_key`, read in `deploy/sql/app-schema.sql`, `catalog-schema.sql`, `run-state.sql` and `run-queue.sql`:

- Nine project database tables carry the column: seven in `catalog`, plus `wamn_run.runs` and `wamn_run.run_queue`. Application tables do not.
- About 100 SQL statements in non-test Rust name it. The earlier count of 935 included uses that stay (the type, NATS keys, system database rows).
- Each of those tables has a row policy: `wamn_authority.project_key(project_env_key) = wamn_authority.current_project_key()`. `project_key` is a SHA-256 over the key and the database name, with the database name written into the function when it is installed. `current_project_key` reads 40 hex characters out of the login name (`wamn_app_<hash>`).
- So a database copied to another environment would need the key rewritten on every row of nine tables, the function recreated and new logins. The column does not protect a copy; it breaks it.
- `host/projects.rs::prepare` already refuses a credential whose database name is not the expected one.
- Inference: with one project in one database, the row policy by project repeats what the database boundary already gives.

### Open in F1

1. Type version: the host reads exports and digest from the system database on a call. Option A: record them on `catalog.release_connection` at apply. Option B: the host keeps a system login for this read.
2. Bindings are stored in two registry tables and again in `catalog.connection_binding`. Option A: keep only the catalog rows. Not read yet why the registry copy exists.
3. Release digest: it is in a registry column, Helm values, a ConfigMap and a workload annotation. Option L-a: Helm values own it (2026-11-12 ruling) and the registry column goes. Release bytes (OCI or `catalog.releases`) not decided; `host/releases.rs` not read yet.
4. Users: `platform."user"` has five insert sites in four crates. The one-writer ruling of 2026-11-12 has not landed.
5. `project_env_key` on every `catalog.*` row looks constant inside one project database (inference). Option: drop the column.
6. Closed: the snapshot hashes are read only as HTTP client cache key parts and by the test-only candidate matcher (see the second read above).

## Core request path (F2): research

Research only; nothing in this section is ruled. Facts are from `worktree-arch-review` at 389bab3 and wasmCloud v2.12.1 (commit 8b0e24cb).

Read in full (non-test): the route guest (`guests/http-route/src/guest.rs`, `entry.rs`, `lib.rs`); `engine/flow_http_routing.rs`; `authentication/*`; `authority/auth_cache.rs`, `kv_memory.rs`; `sql/operation.rs`, `operation_transaction.rs`, `write_log.rs`, `pool.rs`, `query.rs`, `executor.rs`, `resources.rs`, `invocation.rs`, `session_identity/*`; `execution-host/operation.rs`, `operation/native_policy.rs`, `signed_result.rs`; `engine/operation*`, `json.rs`, `invocation_scope.rs`, `component_admission.rs`; `services/host/src/*`; the environment chart. Upstream: `engine/workload.rs` and `linked_call.rs` (linking parts), the plugin list, `wasmcloud_postgres`, `crates/wash/src/cli/host.rs`. Not read: tests (about 12,000 lines on this path), `services/edge`.

### How a request runs today

- The operator starts one workload for an application: the route guest. It is the same pinned bytes for every application. `publish_release.rs::compose_application` returns those bytes unchanged as the "composed entry".
- The application components are not in that workload. The wamn host loads them itself: `engine/operation/native_workload.rs::load_component_workload` builds one wasmCloud workload for each component, with bytes pulled by wamn's own OCI client (`ComponentArtifactSource::pull_verified`). The operator does not know these workloads.
- The route guest reaches the application through a host function, `wamn:operation` `invoke`, with JSON text. The host turns JSON into typed values from the export's WIT type (`engine/operation/json.rs`, 635 lines).
- The route guest reads and parses the whole route table from `wasi:config` on each request (`guest.rs::GuestBackend::routes`).

### Fixed database statements for one request

- Authentication with a session: 8 statements, 2 on the project database and 6 on the system database. A PAT: 8. The auth cache holds nothing in production: `AuthCache::default()` has a lifetime of zero and only tests call `WamnPostgres::configure_revocation`.
- `projects.rs` `Environment::inactive` reads and parses a JSON file from disk on each request, then runs a query. This runs for policy `none` too.
- A read: 6 statements besides the application's own (`BEGIN`, insert into `platform.tx_identity`, `SET TRANSACTION READ ONLY`, `set_config`, the identity test, `COMMIT`), each its own round trip.
- A write: 8 (a grants read, `BEGIN`, the identity insert, `set_config`, the claim, the finish, a causation message, `COMMIT`). The grants read repeats the one authentication made.
- So an authenticated read sends 14 statements before the application's first query.

### What the host builds whether or not it is used

- For each project (`projects.rs::prepare`): a data NATS connection, two KV watch tasks, two system database pools, JetStream, the OAuth envelope, connection facts, resource leases, the blob store, a registry client, a Kubernetes client.
- For each release (`releases.rs::load`): `OperationHost`, integrations, file routes, a workflow `QueueService` with a 250 ms poll, a file collector.
- For each application: a schedule task and an event admission task.
- No branch skips any of these.
- Configuration: 51 flags, about 20 more environment variables, and 1 + 10 mounted files for each project. Eight `WAMN_*` variables have no writer in the charts or crates.

### Only tests reach these

- `OperationClosure::Candidate`; `NativeInput::Typed` (never built, so the typed half of `signed_result.rs` cannot run); `invoke_native`, `invoke_owned`; `ArtifactSource::pull_entry`.
- `WamnPostgres::configure_revocation`; the `run` argument of `prepare_identity` (always `None`); `Authority.wal_position`; `set_role`, `set_operation`, `set_application`, `set_release_identity`, `set_current_run`, `probe_checkout`, `WamnPostgres::from_env`.
- `AuthenticatedCaller::admin()`, `AuthenticatedPrincipal::pat_id()`, the `--nats-connect-timeout` flag.
- `NativePolicy::admit` runs three times for one invocation.

### What wasmCloud v2.12.1 provides

- A workload may hold several components. An import that a sibling exports is linked to it directly (`workload.rs::link_components`, `resolve_component_imports`).
- An async sibling call with plain values runs the callee in its own store. A call that carries a resource handle runs the callee in the caller's store (`linked_call.rs::func_is_ephemeral_safe`, `LinkedTarget`).
- A plugin can call an export of a component in the operator's workload: `ResolvedWorkload::dispatch_target`.
- Instance pools are a field of the component (`pool_size`, `max_concurrency`, `max_invocations`).
- Stock plugins: config, logging, otel, Postgres (queries only, no transaction), secrets, blob store, key-value, messaging.
- Nothing upstream authenticates a request or carries a user identity to a component.
- The charts pin `runtime-operator` 2.11.0. The crates pin wash-runtime v2.12.1.

### Ruled: core request path

Ruled 2026-11-15.

- One stock workload for each application. The release chart lists the route guest and the application components in one `WorkloadDeployment`. The operator pulls and starts all of them.
- The host function `wamn:operation` `invoke` stays, with JSON text. The host calls the application component through wasmCloud's `ResolvedWorkload::dispatch_target`. The host converts JSON as today (`engine/operation/json.rs`).
- These go: the host's own workload loader (`native_workload.rs::load_component_workload`, `load_native_application`), its OCI pull of application components (`ComponentArtifactSource::pull_verified` on this path), the load step in `OperationHost`, and the warm list by digest (`--trusted-warm-component-digest`, `warm_reuse.rs`). Instance pools are set with `poolSize` on the component.
- The fixed statements of a transaction are sent as one batch at begin and one at commit.
- `begin-operation` uses the caller that `authenticate` produced. The second grants read goes.
- The environment status comes from the auth cache. The projects file is read once, not on each request.
- The test-only code listed in the research section is deleted, with its tests.
- One wasmCloud version for the operator chart and the runtime crates.
- Not chosen now: a typed link from a generated route component to the application (it would remove `invoke` and `json.rs`). It needs this change first.

* Declared features are built only when the release declares them. A release whose manifest declares no connection, no file store and no integration gets no connection facts, file routes or integrations (`releases.rs::load`, `projects.rs::prepare`).
* Dynamic workflows are not covered by that rule, because a workflow is created at run time and no release declares it. Superseded: ruling C (workflow service component) decides where the workflow loops run.

## Workflows (point 4b): research

Read in full (non-test code): `crates/workflow`, `crates/run-state`, `run-state.sql`, `run-queue.sql`, `catalog.workflows`, `host/releases.rs`, `host.rs::serve_application_events`, the CLI verbs and control routes for workflows, `services/cdc-reader`, `crates/events`, `crates/event-wire`, `crates/broker` (events, streams, advisories). Upstream v2.12.1: `trigger_service`, `wasmcloud_nats`, `wasmcloud_messaging`, operator workload types, the `cron-service` fixture. Not read: test code, `wiring_jsonata.rs`, the `pg_walstream` fork.

### What exists and runs

- A workflow is one row for each version in `catalog.workflows`. The row holds the graph, the trigger (manual, schedule, event) and the execution role. One version is enabled at a time.
- The CLI can author, enable, disable, list, park and release a workflow, and start one (`workflow.start` host route). There is no UI.
- All three triggers end in one function, `run-state/admission.rs::admit_run`. It inserts a row in `wamn_run.runs` and a row in `wamn_run.run_queue` in one transaction. A repeated trigger is dropped by the unique idempotency key.
- The host takes a run from the queue (`queue.rs::drain_one`), holds a lease of 30 s that it renews, and walks the graph in memory (`router_driver.rs::execute_workflow`). Each node is one operation call on a component of the release. Grants are read before each node.
- A node retries 3 times by default. A run with a lost host is taken again when its lease ends, up to 20 attempts.

### The timers in one host

| Loop | Code | How many | Interval | Work when nothing happens |
| --- | --- | --- | --- | --- |
| Queue | `workflow/queue.rs::serve_queue` | one for each loaded release of each application | 250 ms | 2 transactions (`select_exhausted_production_sql`, `select_production_lease_sql`) |
| Events | `host.rs::serve_application_events` | one for each application | 1 s | 1 select of enabled event workflows; for each entity with a trigger, a consumer lookup and a 100 ms fetch from NATS |
| Schedule | `workflow/schedule.rs::serve` | one for each application | 1 min | 1 select of enabled schedules |
| Unfinished runs | `host/releases.rs::refresh` | one for each project | 1 s | 1 select of unfinished runs, plus the ConfigMap list that an earlier ruling removes |

- All of these run with no workflow authored.
- The schedule and event tasks are started once for an application and never stopped (`admission_tasks` only inserts).
- Example: a host with 5 applications and no workflow makes about 40 queue transactions and 5 event selects each second.

### Why the queue loop is for each release

- A run stores the release digest it was admitted under (`runs.manifest_digest`).
- The lease statement takes one digest (`selected_run.manifest_digest = $4`). `resolution.rs::resolve_workflow` refuses a run of another digest.
- So the host keeps an old release loaded while it has unfinished runs (`releases.rs::refresh`).
- Conflict with a ruling already made: with one stock workload for each application, the operator removes the old workload when the new one is available. The host can then not keep an old release loaded.

### Not finished, or reached only by tests

- `respond` cannot work. `router_driver.rs` always starts the walk with `caller_attached: false`. A Respond node fails the run (`walk.rs`, `RespondWithoutCaller`). The 2026-10-23 ruling (the response returns the workflow result) is not built.
- No state is stored after a node. A run taken again starts at the first node and repeats its side effects. The 2026-10-23 ruling (log after each side-effect node) is not built. The tables `effect_attempts` have no writer.
- `persistence.rs::release_lease` (return to the queue with backoff) has test callers only. The 2026-10-28 agreement is not connected. A run with a credential or pool failure waits for the lease to end.
- No waiting state, approval step, signal or sub-workflow exists in code or schema.
- A run has no time limit. Only a node has one (30 s maximum).
- A missed schedule minute is logged and dropped.
- A failed event admission is not acknowledged. NATS sends it again each 30 s with no limit (`application_consumer_config`, `max_deliver: -1`). Inference: 64 such events stop that consumer.
- `author_workflow.rs::read_compiled_wiring_document` has a test caller only. The control route does not run its operation version check.
- Never constructed: `FailType::DepthBudget`, `DispatchBudget`, `UnboundRequirement`.
- No reader or writer in product code: `catalog.wiring_tombstones`; `runs` columns `run_deadline_at`, `platform_revision`, `registration_id`, `invocation_context`.
- Events: the advisory stream is written and never read (`read_retained_advisories` has a test caller only). `reconcile_replica_identity` has a test caller only. The publication SQL builders have test callers only.
- Tests: no journey test uses a workflow. The nearest test (`workflow_manual_run_live.rs`) is `#[ignore]` and calls Rust functions directly. `queue_recovery.rs` in `wamn_receiving` is five `panic!` lines.

### Sizes (non-test lines)

| Part | Lines |
| --- | --- |
| `crates/workflow` | 4,500 |
| `crates/run-state` | 2,080 |
| `crates/events` | 1,646 |
| `crates/broker` | 1,773 |
| `services/cdc-reader` | 1,479 |
| `crates/event-wire` | 353 |

### Prior art

- wasmCloud has no schedule trigger. Its own test fixture is a service component that sleeps and calls a sibling (`tests/fixtures/cron-service`).
- wasmCloud `wasmcloud:nats@0.1.3` gives a JetStream handler: the host owns the consumer and calls the component for each message, with ack, nak with backoff, and a delivery limit. The wamn host already registers this plugin when `--user-config` is set.
- A JetStream pull fetch can wait on the server for a message (`expires`). A 1 s tick with a 100 ms fetch is not needed to learn of a new event.
- Postgres: `FOR UPDATE SKIP LOCKED` is the documented way for many consumers of one queue table (wamn uses it). `LISTEN/NOTIFY` wakes sessions at commit; it is a signal and not a queue, so a slow timer stays as a second path.
- Kubernetes `CronJob`: about one Job for each schedule time, two or none can happen, one minute is the finest step. `pg_cron` is not in the CloudNativePG images.

### Ruled: a run executes on the live release (P-a)

- D, 2026-11-15: P-a.
- A run keeps its workflow version. The release digest leaves the lease statement (`select_production_lease_sql`) and `resolve_workflow`.
- A queued run can meet a new release. It fails at resolve if a node is gone.
- These go: the unfinished-runs read in `releases.rs::refresh`, the backlog drain for each release (`pinned_backlog`), the queue loop for each release.
- Open: how the loops are scoped and how the queue wakes (4b); what to do with the parts that are ruled and not built.

### Workflow service component (option C): facts

- A wasmCloud workload has at most one service (`workload_types.go::WorkloadService`: image, `maxRestarts`, local resources). The operator starts and restarts it.
- A service calls sibling components of its workload by a plain import (upstream test `integration_cron_service.rs`, fixture `cron-service`: a sleep loop that calls `cron.invoke`).
- So a workflow service is one for each application workload, not one for each project. Each host replica runs one.
- A service that a plugin calls into must target wasip3 (`engine/workload.rs`, line 2462). The JetStream handler is such a call.
- Not stock, needed from the wamn host: a way for the service to run a node as the `workflow` user with the run's role (today only `authenticate` on an HTTP request gives a caller), and access to the run tables.
- `wamn:postgres` has `client` (queries) and `statements` (with a `transaction` resource).
- The timers do not go away. The schedule loop and the queue loop move from the host into the service.
- `crates/workflow/src/graph` (the walk) is plain Rust with no host types; it compiles to a guest as it is (inference, not built).

### Ruled: workflow service component (C, C1)

- D, 2026-11-15: C and C1. Reason: the feature is not complete, so the move is cheaper now than later.
- Each application workload holds one workflow service next to the route guest and the application components. The bytes are the same for every application. The operator starts and restarts it.
- The service holds the schedule loop and the queue loop, and walks the graph. It calls nodes on sibling components.
- Events: the service exports the stock `wasmcloud:nats` JetStream handler. The host owns the consumer, retry and delivery limit. `host.rs::serve_application_events` and the wamn pull code for events go.
- C1: one small wamn host interface for runs: admit, take next, renew, finish. The run SQL and its database role stay in the host (`crates/run-state`). "Take next" returns the run; the host ties the `workflow` user and the run's role to it for node calls.
- These go from the host: `QueueService` and its bind in `releases.rs::load`, `workflow/queue.rs::serve_queue`, `RouterDriver`, the schedule and event tasks in `releases.rs`, `admission_tasks`.
- This replaces the direction "one set of workflow loops for each project" noted under the core request path.
- Not decided: whether the parts that are ruled and not built (`respond`, state after each node, return to the queue with backoff, waiting state) are built in the service as part of this move.
- Not decided: deletion of the dead code and schema listed in the research above.

## Deploy path and formats (F3, F6): research

Read in full (non-test code), by three readers, main claims checked in the code: `services/ctl` deploy verbs, `services/control`, `crates/release`, `crates/environment` (all but the delete helpers and prune files), `crates/publish` (package and release push), `crates/kubernetes` (`job.rs`, `helm.rs`), `execution-host/control_route/operations.rs`, `crates/platform-install`, `xtask` platform files, all seven charts. Partly read: `crates/provision`, `platform-install/status.rs`, `compiler/generate`. Upstream v2.12.1: operator resource types, `wash` CLI, the `runtime-operator` chart, OCI push and pull.

### How an application reaches a host today

Documented order: `wamn env apply`, then `wamn schema apply`, then `wamn deploy`.

| Verb | Processes | What it does |
| --- | --- | --- |
| `env apply` | CLI, control service, Job, and a second Job started by the first | KCL to JSON. The whole document is a command argument of the Job. Registry rows, database, roles, grants, replication, NATS user and streams, host group. 4 `helm upgrade --wait` calls on one chart under two release names. |
| build and publish (inside `schema apply <dir>` and `deploy <dir>`) | developer machine | KCL to `wamn.json`, a temporary database, SQLx describe, generated code, `cargo build`, composition, push of each component, the package and the release. |
| `schema apply` | CLI, control service, Job | Pulls the release and each package, runs SQLx migrations for each package, writes `catalog.packages`, then replication, streams and the CDC reader Deployment. |
| `deploy` | CLI, control service, Job | Reads registry rows and Helm values, pulls the release and each package again, writes `catalog.releases` and binding rows, writes a values file, `helm upgrade --install --wait`, waits for the workload. |

- The CLI polls the Job state each second with no limit (`environment_requests::run_value`). The Job limit is 900 s.
- `schema apply <dir>` and `deploy <dir>` each build and publish the source (`operation_verbs::publish`). The documented flow builds twice.

### Work done more than once (fact)

- The publisher pushes each package, then pulls it, unpacks it and packs it again to compare the digest (`publish_release::open_application`).
- `push_package` lists every tag in the repository and pulls every earlier version of the package on each push (`package_artifact.rs`, line 666).
- One `schema apply` pulls and unpacks each package 3 times. The release manifest is pulled 3 times.
- `wamn.k` is compiled by KCL 2 or 3 times in one build.
- The release digest is checked in 4 places. "Application is declared", "environment exists" and "org matches" are each checked 3 times.
- `bindings::analyse` runs twice in one deploy.
- `helm template` and cluster reads run before each Helm write to check references that Helm then checks.
- The entry image `entries:<digest>` is a byte copy of the route guest, pushed again for each release.

### One fact in many formats (fact)

- Environment: `environment.k` to JSON to HTTP body to Job argument to struct to registry rows to struct.
- Routes: `wamn.k` to `wamn.json` to release manifest to a JSON string to a Helm value to the workload resource to guest configuration.
- Host settings: Rust and a values file to Helm values of release `wamn`, read back with `helm get values`, to values of a second release, to flags and environment variables. The host has 51 flags, 26 of them bound to environment variables, plus about 25 variables it reads directly.
- Platform image digests: values file to chart to `platform.chart` in three releases to control `config.json` to `helm pull` in each Job.

### Three ways to create a Kubernetes object (fact)

- Helm charts (7).
- Rust JSON sent to the Kubernetes API (`job::submit`).
- Rust JSON with `kubectl apply`, `create` or `patch`: ConfigMaps for each project, the CDC reader Deployment, Secrets, the bootstrap Job.
- ConfigMaps of one environment come from all three.

### Platform install (fact)

- `wamn platform deploy` runs 17 steps from the developer machine: 1 infrastructure release, 3 upgrades of release `wamn`, 4 upgrades of the org chart (3 of them from inside Jobs), 8 Jobs, port-forwards, and Secrets made by `kubectl`.
- The environment chart is installed as two releases: one for each project in an environment (database, accounts, certificates) and one for each environment (host group).
- The NATS address is a literal in 19 places. The identity issuer address is built in 3 places.
- Files in `deploy/platform`, `deploy/infra` and `deploy/kind` repeat objects that the installation chart renders. No code names most of them.
- To deploy one application to a local cluster a developer meets 7 formats and 8 tools; to the cloud, 8 formats and 10 tools.

### Reached only by tests, or by nothing

- A second environment provisioning path: `environment/mod.rs::apply`, `ensure::provision_resources`, `ensure::provision`, four `*_from_env` functions that read about 20 `WAMN_*` variables. About 1,100 lines.
- `sql::initialize`, `stage::initialize`, `environment/status.rs::status` (261 lines), `rollback::rollback`, `dry_run`, `show::show`, `ReleaseCarrier`.
- Routes with no CLI verb: `application.stop`, `release.retain`.
- Error texts name verbs that do not exist: `env provision`, `env initialize`, `env connections`.

### Possible defects found by reading (not run)

- The service accounts `<env>-sql` and `<env>-code` that the `schema apply` and `deploy` Jobs use (`control_route/operations.rs`, line 394) come from the chart value `operations`. Only `platform/operations.rs::provision` writes it, and its two callers are in the path that nothing calls. `lifecycle::configure` does not. Inference: on the live path these accounts are not created.
- `stage::write_runtime_grants` refuses with "application principal is missing; run env initialize". The function that creates the principals (`materialize_application_principals`) is called only from `stage::initialize`, which only tests call.
- `release.json` gets `org` and `project` from chart values that `release_chart::values` never sets. The host requires both to match. This goes with the ConfigMap read, which is already ruled out.
- The OpenTofu data source reads the network endpoint group `wamn-edge`, which a later Helm step creates.

### What stock wasmCloud and Kubernetes give

| Need | Stock answer |
| --- | --- |
| Build a component | `wash build` with `.wash/config.yaml` |
| Publish | `wash oci push` |
| Declare an application | one `WorkloadDeployment`: `components[]` and one optional `service` |
| Route HTTP | host interface `wasi:http` with `config.host`, a Service, your own Ingress. By hostname only. |
| Configuration and secrets for a component | `localResources.config`, `environment`, `configFrom` (ConfigMap), `secretFrom` (Secret) |
| Roll out | change `image`; the operator makes a new replica set |
| Roll back, history | none in wasmCloud; `helm rollback` and `helm history` |
| SQL migrations | none; a Job |
| Database and roles | none; CloudNativePG `Database` and managed roles |
| NATS streams | none; the NATS controller (NACK) |
| Deploy command | none; `wash` has no `deploy`. Apply Kubernetes resources. |
| Host settings | chart values to flags, `envFrom`, and one `--user-config` file |

### Ruled: deploy path and formats

D, 2026-11-15: confirmed A1, B1, C1, D2 and all of E. Stated with it: deploy is still the single biggest mess, so any simplification of configuration formats and process helps; clients will be able to deploy their own applications, and that limits the choices.

- **A1, trimmed.** A deploy verb goes CLI to control service to one Job. The deployer needs only a wamn token. No Job starts a second Job (`platform-install/environment.rs::initialize`, `bootstrap-project`). The document reaches the Job as a mounted file, not as a command argument. The CLI poll (`environment_requests::run_value`) gets a limit.
- **B1.** `deploy <dir>` builds once, publishes once, then runs SQL and code as one operation. `publish` and `schema apply` stay as verbs for use alone. Each takes a release digest; neither builds again.
- **C1.** Every Kubernetes object comes from a chart. This includes the CDC reader Deployment, the ConfigMaps for each project and the Secrets that Rust makes with `kubectl` today. Rust runs SQL and NATS calls only. One upgrade for each chart. The environment chart splits in two, named for what they hold: environment (host group) and project (database, publication, accounts, certificates).
- **D2.** An environment is declared in a YAML values file with a JSON schema. `environment.k`, its KCL schema and `environment/kcl.rs` go. KCL remains only in `wamn.k`. This replaces the earlier line "`environment.k` as input". Open: confirm that each KCL `check:` rule also exists in Rust or SQL.
- **E, all nine.**
  1. Delete the uncalled provisioning path (`environment/mod.rs::apply`, `ensure::provision_resources`, `ensure::provision`, the `*_from_env` functions) and the test-only functions (`sql::initialize`, `stage::initialize`, `status::status`, `rollback::rollback`, `dry_run`, `show::show`, `ReleaseCarrier`), with their tests.
  2. Publish does not pull back what it just pushed. The append check compares with the previous version only.
  3. One pull of each package for each operation. One KCL compile for each build.
  4. One check for each fact (application declared, environment exists, org matches, release digest).
  5. No `helm get values` read-back to feed another release (`application-host.yaml` goes). No `helm template` pre-check.
  6. One value each for the NATS address, the identity issuer address and the registry address.
  7. Delete the loose YAML in `deploy/platform`, `deploy/infra` and `deploy/kind` that the charts repeat.
  8. The entry image copy `entries:<digest>` for each release goes.
  9. Rollback is `helm rollback`.

Open after this ruling:

- Confirm on a cluster the two possible defects (Job accounts `<env>-sql` and `<env>-code`; application principals).
- Platform install order (17 steps, three passes for identity and control): not read yet.
- Host settings: 51 flags and about 50 variables to one rendered file (ruled 2026-11-12, not designed).

## Platform install order: research

Read in full (non-test code): `crates/platform-install` (all), `services/ctl` platform and provisioning verbs, `services/control` start, `services/identity` start, `services/host/src/host.rs` lines 40-1040, the installation, infrastructure, org, identity, edge and release charts, `kubernetes/helm.rs`, `kubernetes/job.rs`, `environment/identity_issuer.rs`. Not read: identity mail and session code, cdc-reader source, the environment chart templates.

### What forces the order

Orders forced by a server or by data (fact):

1. cert-manager and CloudNativePG resource types and controllers before any `Certificate`, `Issuer` or `Cluster` object. This forces one earlier Helm release.
2. PostgreSQL ready before any SQL. The system schema before org rows, grants, identity and control.
3. The NATS authorization content and server certificate before NATS starts.
4. A password Secret for each login before CloudNativePG gives the login a password.
5. Identity serving before the owner invitation.

Everything else is ordered by two choices (fact, inference on "choice"):

- Every Helm call uses `--wait` (`kubernetes/helm.rs`, line 232). A pod that needs a Job result can never be ready inside the same upgrade.
- Jobs run from the developer machine between Helm calls (`kubernetes/job.rs::run`).

Identity and control exit when the database or NATS is missing, and Kubernetes restarts them (`identity/cli.rs::run`, `control/lib.rs::serve`). Without `--wait` they would restart until their inputs exist.

### Why release `wamn` is upgraded three times

- Pass 2 turns identity on, after Jobs made its login and connection file.
- Pass 3 turns control and edge on. They need ConfigMap `identity-ca`, which the chart builds with Helm `lookup` of the Secret `identity-tls`. `lookup` sees only objects that existed before the upgrade began.
- Pass 1 already issues `identity-tls`. Inference: control and edge could join pass 2.
- `identity`, the control NATS certificate and the host certificates all use the issuer `wasmcloud-ca`. Control already mounts that CA.

### The 8 Jobs and the org chart upgrades

| Job | Does | Could be |
| --- | --- | --- |
| `platform-system`, `platform-org`, `platform-identity-issuer` | SQL as superuser; each also upgrades the org chart and writes a ConfigMap | one Job (same image, same login) |
| `platform-session-key-publish`, `-activate` | insert and activate the first signing key | identity at start; `rotate_session_keys` already has the code and a lock |
| `platform-identity-probe` | `curl` of `/healthz` and JWKS | the readiness probe |
| `platform-owner-users`, `-invitations` | `curl` POST with the operator certificate | one Job |

- Of the three org chart upgrades made from inside Jobs, two add no role (`roles.rs::platform` already has `wamn_apply` and the issuer login). Only the control login is new. All four logins are known before install: each comes from the org name and constants.
- The developer machine also runs superuser SQL through port-forwards: the signing key state, and the owner invitation, which holds a transaction open across a Job.

### Secrets made by Rust

| Secret | Material | Other owner possible |
| --- | --- | --- |
| `wamn-nats-authorization` | user list, no passwords | a chart template; environment provisioning edits it later |
| `wamn-registry-pull` (kind) | developer file | external secret source |
| `identity-resend` (GCP) | Secret Manager | external secret source |
| `wamn-oauth` (kind) | 32 random bytes | any generator that keeps the value |
| one basic-auth Secret for each login | 32 random bytes, kept if present | a chart with `lookup` |

### Host and service settings

- Host: of 51 flags, 20 are never set, 20 have one fixed value, 11 vary. The 11 come from six facts: org, environment, pod address, registry, provider, drain bound.
- About 15 environment variables read by libraries are constants in the repo.
- The host settings reach the host through 10 steps and two Helm releases, including a `helm get values` read-back (`runtime.rs::resolve_platform`, `host_group`; `ensure.rs::environment_host_values`, `provision_environment_host`). Values key `hostConfiguration` is rendered by no template; it lives only in Helm values to be read back.
- control `config.json`: 22 keys, 5 real inputs (org, route host, registry, provider, chart). identity: 14 flags, 6 real inputs. cdc-reader `reader.json`: 15 keys, 3 real inputs. web `web.json`: 5 keys, 3 real inputs.

### Possible defects found by reading (not run)

- The issuer Job (`bootstrap.rs`) passes `--issuer` and `--namespace` but not `--db-host`. The flag has no default and no environment variable (`identity_verbs.rs`, line 61). `identity_issuer.rs` line 50 fails with "issuer database host is absent". Checked by me.
- `oauth::ensure_key` writes `wamn-oauth` to namespace `hosts`. Control and the hosts mount it from the org namespace (`control.rs` line 232, `installation.rs` line 193; hosts live in the org namespace, `installation.rs` line 39). Checked by me.
- `platform status` reads the database Cluster in namespace `platform` (`platform_verbs.rs` line 230, `status.rs` line 1032). The Cluster is in the org namespace. Status then fails the deploy.

With the two possible defects in the deploy path, inference: the install and deploy on this branch have not run end to end.

## Build and release documents: research

Read in full (non-test code): `crates/publish` (all), `crates/package-artifact` (main files), `component-artifact`, `component-composer`, the catalog manifest types (`serving_manifest.rs`, `manifest.rs`, `package.rs`, `http_routes.rs`), `compiler/build.rs`, `build_record.rs`, `output.rs`, `migrations.rs`, `publication.rs`, `xtask/build`, `crates/release`, the deploy stage files, `host/releases.rs`, `engine/component_artifact.rs`, `guests/http-route/src/guest.rs`, the release chart. By grep: every reader of the release manifest in `execution-host`. No build output exists in the repo, so document sizes are not known.

### The chain for one application

`wamn.k` → `wamn.json` + `build.json` + `publication/*.json` + `openapi.json` + `required-schema-contract.json` → component artifacts (wamn format) → package artifact (5+ layers) → entry artifact → release manifest (OCI) → `catalog.releases` row → Helm values → WorkloadDeployment config `wamn.http.routes`, `release.json`, `route.json`.

### Who reads what (fact)

- The host reads the release manifest from `catalog.releases`, never from OCI (`releases.rs::load`). Only the deploy Job pulls the OCI release.
- The route guest reads one config key, `wamn.http.routes` (`guest.rs`, line 49). The manifest fields `attachments[].definition`, `routes[].reads` and `routes[].revision` exist only to build that string at deploy.
- The entry image is the route guest, byte for byte (`publish_release.rs`, line 320). It is the same for every application.
- Facts the host needs that exist nowhere else: for each operation, public or private, whether the host begins the transaction, read or write, the idempotency key field; and the application's database schema. The operation list is the component's exports. Imports and effects come from the bytes. Connection requirements are also in `catalog.connection_requirement`.
- No reader outside tests: `openapi.json`, `required-schema-contract.json`, `build.json` fields `outputs.files`, `dependencies`, `generator`, manifest fields `composed-entry.route-digest`, `attachments[].definition-hash`; `descriptor.effects` is only checked against itself.
- The schema and deploy Jobs need layer 0 (`wamn.json`) and the `migrations` tar. The `publication` and `sources` tars and the descriptor layers are read by no Job. `compiled_operations` in `wamn.json` is parsed by the Jobs and not used.
- Unused Helm values: `release.artifactBase`, `audit`, `platform.*` in the release chart.

### Component artifacts against stock

|  | stock (`wash-runtime/src/oci.rs`) | wamn |
| --- | --- | --- |
| Layer type | `application/wasm` | `application/vnd.wamn.component.v1+wasm` |
| Config | `oci_wasm::WasmConfig` | wamn config: digest, imports, fingerprint |

- The stock host cannot pull a wamn component artifact, and wamn's `pull_verified` refuses a stock one. The wamn config holds facts that come from the bytes.
- Today the WorkloadDeployment lists one component (the route guest). The application components are pulled and started by wamn code (`execution-host/operation.rs::load_released_application`). The ruling "one stock workload for each application" removes this.

### Digests

Most digests repeat an OCI digest: the component digest is the OCI layer digest; `build.json outputs.manifest` is the layer 0 digest; the release digest is the release layer digest; `route-digest` is the entry layer digest; entry member digests repeat `components[]`.

### Build and dev loop

- Each build creates a database, runs the migrations, describes each statement with SQLx, and drops the database.
- `wamn dev --watch` runs the whole chain on each change: KCL, build with database, `cargo build`, composition, push with a pull of every earlier package version, release publish, deploy Job with Helm `--wait`. There is no "inputs unchanged, skip" step (`source_deploy::build`).
- `apply_package` returns early when the package is installed, so `wamn dev` does not apply a new migration.

### Reached only by tests, or by nothing

- `release_composition.rs` (about 180 lines, "for the POC loop"), `ReleaseCarrier`, `ReleaseIdentity`, the old ConfigMap release mount (`LoadedRelease::load`, `load_from`, `RELEASE_MANIFEST_*`), `pull_package`, `read_descriptors`, `load_component_facts`, `pull_entry`, `read_release`, `package_route`, `PackageSource::Directory`, `entry_members`, `composing_member`, `composing_application`.

### Ruled: release documents, artifacts, build, install and settings

D, 2026-11-15: G2, H accepted, I1 with both fixes, J, K and L accepted.

- **G2. No release artifact.** The deploy Job writes the facts that exist nowhere else into the workload config, as the route table already travels. For each operation: public or private, host transaction, read or write, idempotency key field; and the application's database schema. The Helm revision is the release. These go: the release publish step, the OCI release artifact, most of `ServingManifest`, `catalog.releases`, the composition step (`compose_application`, `entry_publication`, `entries:<digest>`). Workflow authoring checks a graph against the package rows and the live workload instead of `catalog.releases`. Open: confirm that the Helm values stay well under the size limit for the largest application.
- **H. Artifacts.** Components are pushed with stock `push_component` (`application/wasm`) and listed in the workload's `components[]`. The wamn component format and its config blob go (`COMPONENT_LAYER_MEDIA_TYPE`, `COMPONENT_CONFIG_MEDIA_TYPE`, `pull_verified`). The package artifact keeps two layers: `wamn.json` and the migrations tar. The `publication` and `sources` tars and the descriptor layers go.
- **I1 and fixes.** SQLx offline data (`cargo sqlx prepare`, a committed `.sqlx` folder, a check that it is current). The build uses a database only when SQL changes. The build is skipped when its inputs are unchanged. `wamn dev` applies new migrations (today `apply_package` returns early when the package is installed).
- **J. Platform install.** One infrastructure release (forced: resource types first). One platform release with identity, control and edge on; pods restart until their inputs exist, with one wait at the end. One SQL Job instead of three. Identity creates its own first signing key (the publish and activate Jobs go). One owner invitation Job. The probe Job becomes a readiness probe. All logins go in the first org chart install. Superuser SQL through port-forwards from the developer machine goes.
- **K. Settings.** The 40 constant host flags become code defaults. The six real inputs (org, environment, pod address, registry, provider, drain bound) go into one file rendered by the environment chart from its own values. The `hostConfiguration` value and its `helm get values` read-back go. The same for control, identity, cdc-reader and web.
- **L. Fixes and deletions.**
  1. Fix the three install defects: issuer Job `--db-host`; `wamn-oauth` namespace; `platform status` database namespace.
  2. Delete `openapi.json`, `required-schema-contract.json` and `build.json`.
  3. Delete the test-only and unused code listed in the two research sections above.
  4. Delete the unused release chart values (`release.artifactBase`, `audit`, `platform.*`).

* Correction to L2 (Claude, same day): `build.json` does have readers. `outputs.components` gives the base component file and digest to an overlay build (`xtask/build components.rs::component_file`, `build.rs::generate_package`); `outputs.manifest` and `package` are read by `build.rs::dependencies`, `web_artifact/plan.rs` and `source_deploy.rs`. Only `outputs.files`, `dependencies` and `generator` have no reader. Deleting the file means an overlay build reads the base's built wasm and `wamn.json` directly and hashes them. Not ruled until D confirms.

- Ruled (D, 2026-11-15): L2a. `build.json` goes. An overlay build reads the base's built wasm and `wamn.json` directly and hashes them.

## Identity (F5): research

Read in full (non-test code): `services/identity`, `crates/identity` (auth and non-auth files), `crates/identity-protocol`, `crates/authentication`, `authority/identity.rs` and `auth_cache.rs`, `broker/auth_changes.rs`, the identity tables and functions in `system-schema.sql`, the platform user tables in `app-schema.sql`, `tx_identity` and `login_principal`, the identity chart and certificates, ctl identity and role verbs, `services/edge/authenticator.rs`. Partly read: `engine/flow_http_routing.rs` (auth parts), `sql/session_identity.rs` (grant parts), `identity-client`, `web-front` proxy, `control_route.rs` (invite, member). Web research on wasmCloud and self-hosted identity providers (sources below).

### What wamn runs today (fact)

- Its own identity provider: Argon2id passwords, invitations and resets by mail (Resend), a login throttle table, renewal credentials with replay detection, Ed25519 session JWTs, signing keys stored in the system database, a rotation loop, a JWKS endpoint. About 5,000 lines. No OIDC, no SSO, no MFA.
- Principals in `identity.principals` (system database): users, service principals (API users, connection principals, application principals), one provisioning row. A copy of each lives as `platform."user"` in every project database.
- Service principals use PATs, issued by an HTTPS route that accepts any certificate from the org operator CA (no subject check; `services/identity/src/lib.rs`, lines 413 and 450).
- The host verifies the JWT offline against a cached JWKS (`authentication/session_keys.rs`, `identity-protocol/verifier.rs`).
- The edge device verifies against a static JWKS file. No code writes that file. Inference: after the 30-day key rotation the edge refuses new tokens.

### Per request (fact)

1. Verify the JWT (no database).
2. Grants from the project database (cached).
3. Admin inheritance from the system database on every request, not cached (`sql/session_identity.rs::inherits_administration`, `identity::control::is_project_admin`). This contradicts the 2026-10-01 ruling that the admin hierarchy is stored as rows and not walked at request time. Users get no stored admin row.
4. Session liveness from the system database (cached until `exp`).
5. Every role signed into the JWT must still be held.

Roles are read three times: at login by identity (it holds a reader login for each environment), and twice by the host.

### Defects and gaps found by reading

- **Users cannot log in to an environment on an installed platform.** The installer passes no session targets to identity (`platform-install/platform/identity.rs`, line 366: `targets: Vec::new()`). Only control audiences work. Checked by me.
- The signing private key is plaintext `bytea` in `identity.session_keys` (`system-schema.sql`, line 665).
- Identity trusts the last `x-forwarded-for` value with no trusted-hop check (`lib.rs`, line 259). Inference: a direct caller can avoid the per-source throttle.
- Control eligibility rules disagree: the SQL accepts environment-admin, `session.rs::holds_control` does not. Nothing writes environment-admin rows (`grant_environment_admin` has no caller).
- `disable_principal` has a test caller only (`password.rs`, line 1606, inside the test module), so a user cannot be disabled.
- Identity runs one replica. Each password request opens a new database connection.
- One login costs two Argon2 checks (`/password/environments` then `/password/session`).
- The "one writer of project user rows" ruling is broken: `environment/stage.rs` and `provision/platform_principals.rs` insert directly.
- About 3,100 of the 6,000 lines in `crates/identity` are not identity: grants and role administration, configuration rows, connection administration, org and project lists.

### Prior art

- wasmCloud v2 has no end-user authentication or caller identity. Auth stays in a gateway or in the host. ([v2 announcement](https://wasmcloud.website.cncfstack.com/blog/wasmcloud-v2-is-here/))
- Open-source identity providers that cover organizations, an org bringing its own Entra or Okta, invitations, SCIM, machine tokens and Ed25519:

|  | Keycloak | Zitadel |
| --- | --- | --- |
| Language, license | Java, Apache-2.0 | Go, AGPL-3.0 |
| Orgs | Organizations in one realm | instances and orgs |
| Storage | Postgres and others | Postgres only |
| Memory | 1,250 MB baseline per pod | not published |
| Access token | JWT | opaque by default; JWT per application |
| Releases | operator-managed, frequent patches | new major every 3 months |

- Ory and Authentik put multi-tenancy behind a paid licence. Logto open source has no SCIM. Dex, Kanidm, Rauthy, Pocket ID and Authelia have no organizations.
- Common practice: the identity provider owns login, MFA and federation. The product keeps roles and grants in its own database. Rust crates for verification: `jsonwebtoken` 11.1 (EdDSA, JWK sets), `jwks-cache`.
- Any provider that signs JWTs with a JWKS lets an edge device verify offline with a cached JWKS, if the offline window is shorter than the provider's old-key retention.

Sources: [Keycloak 26.8.2](https://www.keycloak.org/2026/11/keycloak-2682-released), [Keycloak sizing](https://www.keycloak.org/high-availability/single-cluster/concepts-memory-and-cpu-sizing), [Zitadel on Kubernetes](https://zitadel.com/docs/self-hosting/deploy/kubernetes), [Zitadel opaque tokens](https://zitadel.com/docs/concepts/knowledge/opaque-tokens), [Zitadel release cycle](https://zitadel.com/docs/product/release-cycle), [Ory multi-tenancy](https://www.ory.com/docs/kratos/guides/multi-tenancy-multitenant), [Authentik tenancy](https://docs.goauthentik.io/sys-mgmt/tenancy/), [RFC 9068](https://datatracker.ietf.org/doc/html/rfc9068#section-4), [jsonwebtoken](https://docs.rs/jsonwebtoken/latest/jsonwebtoken/).

### Ruled: identity provider (M1)

- D, 2026-11-15: M1 for now. Identity stays in the platform. D would like to be able to swap it for a third-party (SaaS) provider later.
- Not yet ruled: what wamn keeps (N), fixes now (O), and the swap boundary (P).

### Ruled: identity boundary, what wamn keeps, fixes

D, 2026-11-15: accepted P, N and O in full. Edge offline window: configurable, default 24 hours; an environment setting (E1, below).

**P. Swap boundary** (so a SaaS provider can replace wamn identity later)

1. The host verifies standard tokens from settings: issuer, JWKS address, audience, allowed algorithms. RFC 9068 checks. `jsonwebtoken` with `jwks-cache` replaces the hand-written JWKS cache.
2. The host reads only `iss`, `sub`, `aud`, `exp`. The `wamn-session+jwt` type header goes.
3. Cookies and CSRF move to `wamn-web` (signed double submit). The host sees only a bearer token. The `csrf` claim goes. With a SaaS provider, `wamn-web` does the login redirect.
4. Session liveness is the identity provider's job. wamn identity keeps 15-minute tokens and refuses renewal after logout or disable. The host's liveness read and the `authority.login` claim go. Accepted cost: a token works until it expires after logout or disable.
5. `platform."user"` is keyed by `(issuer, sub)`.
6. User management (create, invite, disable, list) goes through control routes and one adapter. Today the adapter calls wamn identity. Nothing else calls identity directly; the installer and `api_service` stop using the operator-certificate HTTP hop.

**N. What wamn keeps**

1. Roles and grants stay in the project database.
2. The JWT carries no roles.
3. Admin inheritance is stored as rows in the project database (as ruled 2026-10-01). The uncached system-database read (`inherits_administration`) goes.
4. One writer creates `platform."user"`.
5. API users keep wamn PATs, service principals only.
6. The non-identity half of `crates/identity` (grants and roles, configuration rows, connection administration, org lists) moves to its owners.
7. Edge: the device fetches the JWKS from identity while online. The offline window is a project setting, default 24 hours. Identity keeps an old public key in the JWKS for the longest offline window of any project it serves, plus the token lifetime (today: 930 s, `session_keys.rs::KEY_RETENTION_SECONDS`). Inference: keys are per issuer, not per project, so the retention is the maximum over projects.

**O. Fixes now**

- Environment login works (session targets, or removed through N2 and P4).
- One control-eligibility rule.
- The PAT route checks the certificate subject (until P6 removes the hop).
- `x-forwarded-for` is trusted only from known proxies.
- A route for `disable_principal`.
- The signing key is encrypted: a Kubernetes Secret or the KMS envelope that OAuth tokens use.
- One Argon2 check per login.
- Two identity replicas.

### Ruled: edge offline (E1)

D, 2026-11-15: E1 for now. This replaces the offline window part of N7 above.

- Correction (Claude): key retention does not need to cover the offline window. It covers the longest token lifetime only (today 930 s). A device that syncs the JWKS at least once per offline window already holds the next key, because identity publishes a key before it activates it.
- E1: no user login on the edge while it cannot reach identity. The device keeps collecting, storing and forwarding data. The local UI locks when the session ends (at most 15 minutes). Revocation stays at 15 minutes.
- The offline window is an environment setting (an edge install is its own environment, ruled 2026-10-22), default 24 hours. Under E1 it sets how far ahead identity publishes the next signing key. Inference: with keys per issuer, identity publishes ahead by the longest window of the environments it serves.

Future directions, not ruled:

- **E2. Longer edge sessions.** Identity issues tokens for the edge audience that live up to the offline window. Users logged in before an outage keep working. Cost: a revoked or disabled user keeps edge access for up to the window; no new logins during an outage.
- **E3. Local login on the device.** The device checks users itself against credentials synced from the cloud. Full use offline, including new logins. Cost: password hashes or PINs on every device, and much more code.

## Isolation (F7): research

Read in full: the org, installation, environment, release, identity and edge chart templates and values; all of `deploy/sql`; `crates/provision` role and grant files; `platform-install` roles, credentials, org, environment, installation, operations, database; `broker/authorization.rs`; `sql/pool.rs`; `host/projects.rs`. Partly read: `ensure.rs`, `component-policy`, `component-composer`. Web research on Kubernetes, CloudNativePG, Postgres, NATS, wasmCloud and other platforms (sources below).

### What exists, by level (fact)

| Level | Mechanisms today |
| --- | --- |
| Org | Namespace per org. Roles and service accounts. No NetworkPolicy object exists. `control_org` row policy on 20 system tables. |
| Environment | One host group per environment, all in the org namespace (`allowSharedHosts: false` therefore separates nothing between environments). The host refuses a workload whose annotation names another environment (`projects.rs::for_workload`). One NATS user per project in an environment; no NATS accounts. |
| Project | One database per project in an environment, `CONNECT` revoked from PUBLIC. 13 login families (`workload_role.rs::ALL`), 12 of them per project. Row policies on catalog and run tables keyed by `project_env_key` (already ruled out under F1). Service accounts per project. |
| Application | Same logins, database and NATS user for all applications of a project. Separation is the host's permission check and the wasm sandbox. |
| Function | Wasm import allowlist, egress allowlist (one value for the whole host), per-family logins. |

- Default install: the system database and every project database are on one CloudNativePG cluster `wamn-pg` (`bootstrap.rs::organization_job`, template `trials`).
- One org, one environment, two projects: about 58 Secrets, 32 Postgres logins, 10 NATS users, 16 certificates, 11 service accounts, about 17 Roles. One host opens 7 pools for each project, up to 65 connections for each project (130 for two). Nothing sets `max_connections` (Postgres default 100). Inference: two projects can exhaust the server.

### Holes that cancel a boundary (fact, checked by me where marked)

- **Secrets.** The project operation Roles grant `get, list` on all Secrets in the org namespace, with no `resourceNames` (`environment/templates/operations.yaml`, checked). So do `wamn-database-provisioning`, `wamn-operation-<org>` and `wamn-control-status`. Any of them can read every database credential, including the superuser. This cancels the per-family Secret split.
- **Database owner.** `roles.rs::values` adds every `wamn_migration_*` login of the cluster to every login that is in `wamn_db_owner` (checked). `wamn_db_owner` owns every project database. Inference: one project's platform migration login reaches every project database on the cluster.
- **Workloads.** Each project's operation Role may create a WorkloadDeployment anywhere in the org namespace. The host trusts the `wamn.project_env_key` annotation. Inference: a project worker can write another project's key.
- **Platform secrets.** `wamn-platform-observer` gives every service account of the org `get, list` on all Secrets in `platform` (NATS server key, operator certificate, NATS authorization).
- **CDC.** One certificate `wamn-cdc-reader-nats` for every project's reader, allowed to publish to `evt.>` (`events/lifecycle.rs`, checked).
- **Network.** No NetworkPolicy object exists, although three Roles may write them (checked).

### Unused or inconsistent

- `ServiceReader` family: login, Secret, managed role and host access, read by tests only (checked: no non-test reader).
- `wamn_app_owner` holds no grants. `wamn_administration` has `EXECUTE` on system functions its logins cannot connect to. `wamn_app` keeps `DELETE` on `runs` beside the retention family.
- `IdentityReader`: one login per project with identical grants.
- Each family's provisioning re-applies the org chart: about 12 Helm upgrades per project (`workload.rs`, line 101).
- `runtime-operator-events-rbac.example.yaml` sits in a chart's `templates/`, so Helm renders it (inference).
- Stale comments: `platform."user"` unique key, the old RLS builder, "database is the org boundary" (three places in `app-schema.sql`), namespace per project-env.

### Prior art

| Boundary | Common single mechanism | Source |
| --- | --- | --- |
| Org | Namespace with RBAC, quota and default-deny NetworkPolicy | [Kubernetes multi-tenancy](https://kubernetes.io/docs/concepts/security/multi-tenancy/) |
| Environment | NATS account (a user inside one account isolates nothing); a host group | [NATS accounts](https://docs.nats.io/running-a-nats-service/configuration/securing_nats/accounts), [wasmCloud workload security](https://wasmcloud.com/docs/kubernetes-operator/workload-security/) |
| Project | Postgres database | [Postgres](https://www.postgresql.org/docs/current/manage-ag-overview.html), [AWS SaaS matrix](https://docs.aws.amazon.com/prescriptive-guidance/latest/saas-multitenant-managed-postgresql/matrix.html) |
| Application and function | Wasm component sandbox | [WASI](https://wasi.dev/security) |

- Row policies matter when tenants share tables, or when untrusted clients reach tables directly (the Supabase case). With a database per project they add no isolation.
- The usual login set is three: owner or migration, application, read-only ([AWS](https://aws.amazon.com/blogs/database/managing-postgresql-users-and-roles/), [Redgate](https://www.red-gate.com/simple-talk/databases/postgresql/postgresql-basics-a-template-for-managing-database-privileges/)). Pools multiply as logins times databases.
- CloudNativePG recommends one cluster per database. Backup and point-in-time restore are per cluster; a restore makes a new cluster ([CNPG FAQ](https://cloudnative-pg.io/docs/1.27/faq/), [recovery](https://cloudnative-pg.io/docs/1.27/recovery/)).
- wasmCloud: a host is "a host-wide guardrail, not a tenancy boundary"; workloads that must not share belong on different host groups.

### Ruled: isolation, one boundary per level

D, 2026-11-15: accepted Q, R, S, T2, V and the application line; W2 instead of W1, "for simplicity and alignment with cnpg".

- **Org (Q).** Namespace with RBAC, a quota and a default-deny NetworkPolicy. Every Role names its Secrets with `resourceNames`. `wamn-platform-observer` (all Secrets in `platform` for every org service account) goes. The `control_org` row policy in the system database stays.
- **Environment (R).** A NATS account per environment with JetStream limits, plus its host group. NATS users per project go. Each CDC reader gets its own credential in its environment's account; the shared `wamn-cdc-reader-nats` certificate goes.
- **Project (S).** The database is the boundary. One owner role per database (the cross-project `wamn_db_owner` membership in `roles.rs::values` goes). One operation service account per environment, not per project.
- **Function (T2).** Per project database: migration (owner), application, read-only (dynamic SQL), platform (all host platform work). Per environment: one system database reader. A connection limit for each login. The other families go, with their Secrets, managed roles and per-family Helm upgrades.
- **Application.** Separation is the host's permission check and the wasm sandbox. The project is the trust unit.
- **Postgres clusters (W2).** One CloudNativePG `Cluster` for each project database, as CloudNativePG recommends. Backup and point-in-time restore are per project. The system database gets its own cluster. Consequences: the project chart (C1) renders the `Cluster`; the org chart's cluster list and the `trials`, `standard`, `dedicated` templates go; sizing (instances, storage, CPU, memory) is an environment value. Open: where the system database cluster lives (namespace `platform` or the org namespace).
- **V.** Delete `ServiceReader`, `wamn_app_owner`, the dead `wamn_administration` grant and `DELETE` on `runs` for `wamn_app`. One identity reader login. Delete `runtime-operator-events-rbac.example.yaml` from `templates/`. Fix the stale comments. All logins in the first install of their chart.

* Ruled (D, 2026-11-16): the system database cluster lives in namespace `platform`.

## Tests (F4): research

At commit 5760c6c1 (after the T2, T3 and `sel3` work). Read in full: `docs/testing`, `xtask/src/selection.rs`, `tests/selection.toml`, `tests/journeys`, all of `test-support`, the justfile and xtask entry points, the `test-util` feature lines of every crate, 15 live test files across 11 crates, the T1–T3 commits. Test lines mapped against every deletion already ruled in this review, by script. Upstream wasmCloud v2.12.1 tests and CI read; web research (sources below).

### Size (fact)

- About 129,000 test lines against about 168,000 source lines: tests are about 40% of the Rust. 1,638 tests, 253 `#[sqlx::test]`, 340 `#[ignore]`, 66 `*_live.rs` files (33,000 lines).
- At least 15,900 test lines (12%) test only code already ruled to go; with probable cases 18,500 (14%). Another 15,500 lines call ruled code and need edits. Largest: deploy path 5,300, NATS replica 2,900, workflow host parts 2,500, connection call path 2,200, login families 1,200.
- About 5,100 lines of workflow logic tests move with the workflow service component; the graph tests (1,300 lines) stay.

### How tests run (fact)

- Levels: unit, contract, live-postgres, live-nats, kind. A test's level comes from the reason text in `#[ignore = "requires: …"]`, parsed by `test-support/postgres/lib.rs::parse_prerequisites`.
- One runner: `cargo xtask test-changes` (`selection.rs`, 1,673 lines, no tests of its own; `selection.toml`, 405 lines). It maps the Git diff to packages, walks reverse dependencies, parses test sources with `syn` to find levels, builds fixture applications with `wamn build`, then runs each test binary with `--test-threads=1`.
- Postgres: a private PostgreSQL 18 per test binary, started by the `wamn-test-postgres` runner. NATS: a local `nats-server` binary.
- No CI (no `.github`, no nextest configuration). No wall times recorded except kind cases (about 30 minutes each).
- 21 crates define a `test-util` feature: test-only constructors (`for_test`, test clocks, endpoint overrides), fake modules (`http/src/test_util.rs` 869 lines, registry SQL seeds), test databases. `wamn-http`, `wamn-connections` and `wamn-publish` have optional production dependencies on test crates. `crates/provision` compiles a file from its own `tests/` directory into the library (`#[path]`). `services/identity/src/mail.rs` has a `WAMN_TEST_RESEND_ENDPOINT` back door that nothing enables.

### Brittle or failing (fact)

- 13 files end tests in `panic!("wamn-…")` product-gap stubs. Three suites in `selection.toml` select them, so those suites fail every run (for example `crates/dev/tests/platform_apply_kind.rs`, line 77, checked).
- 80 sleeps, 18 of them 200 ms or more followed by "nothing happened". Worst: two 65 s sleeps in `workflow_schedule_live.rs`; a 30–35 s elapsed window in `environment/tests/support/exclusive_resources.rs`, lines 371–372 (checked).
- 218 calls to `wamn_test_postgres::lock()` that the runner's `--test-threads=1` already makes redundant.
- 90 process spawns, 60 loopback binds. Literal error-message asserts and snapshot files are essentially gone after T1–T3.
- Journey tests (8, 514 lines) drive product commands, but through `wamn dev`, not `wamn deploy`; one ends in a `panic!` stub; the observer connects as system admin.
- Unused: `ctl_process` (whole module), `run_result::require_success`, five other `test-support` items, `proptest` in `wamn-catalog`. Nine dead links in `docs/testing`.

### Prior art

- Upstream wasmCloud: inline unit tests; integration tests that start a real host in process, with Postgres and NATS from testcontainers; a Go suite on kind for the operator. Tests that need Docker carry `#[ignore]`. CI runs `cargo test`, then `cargo test -- --ignored`. No custom selector, no `test-util` features (helpers in `tests/common/`), no snapshots.
- [cargo-nextest](https://nexte.st/docs/filtersets/): profiles, default filters, test groups and timeouts cover most of a selector. It does not map changed files to packages. It runs each test in its own process.
- [`#[sqlx::test]`](https://docs.rs/sqlx/latest/sqlx/attr.test.html): a fresh database per test with migrations. [testcontainers](https://rust.testcontainers.org/) starts Postgres or NATS from code (needs Docker).
- [Google SWE book, ch. 11](https://abseil.io/resources/swe-book/html/ch11.html): small (no I/O), medium (one machine, a real database), large (many machines). Large tests are sanity checks, not the main bug net.
- [Cargo features](https://doc.rust-lang.org/cargo/reference/features.html): `cargo test --workspace` unifies features, so a `test-util` feature turned on anywhere reaches every binary built in that run.

### Ruled: tests

D, 2026-11-16: accepted X2, Y2, Z, AA and AB.

- **X2. Three levels, as upstream.** Unit tests with no services. Service tests that start a real host in process with real Postgres and NATS, marked `#[ignore]`, one test per contract. Journey tests on kind through product commands.
- **Y2. Stock runner.** `cargo test`, then `cargo test -- --ignored`, as upstream. An `xtask` command maps changed files to packages with `cargo metadata`. Fixture applications are built in one explicit step before the run. A test whose input is missing fails; it is not skipped. `xtask/src/selection.rs`, `tests/selection.toml` and the level syntax in ignore reasons go. This reopens and replaces the earlier choice to keep a trimmed selector.
- **Z. Services.** Postgres and NATS from testcontainers (Docker on Linux is the supported developer setup). `#[sqlx::test]` gives each test its own database. `wamn-test-postgres` and the 218 `lock()` calls go.
- **AA. No `test-util` features.** Shared helpers live in each crate's `tests/common/` or in one dev-only crate. Test-only constructors, test clocks and back doors leave production code (`for_test`, `with_test_clock`, `with_test_endpoint`, `send_for_test`, `WAMN_TEST_RESEND_ENDPOINT`, the `#[path]` include in `crates/provision`, the optional production dependencies on test crates in `wamn-http`, `wamn-connections`, `wamn-publish`). Timing tests use `tokio`'s paused clock.
- **AB. Fixes.**
  1. Delete the tests that belong to code already ruled out (at least 15,900 lines); review the probable cases (2,600 lines).
  2. Delete the 13 `panic!("wamn-…")` stubs; file each missing product step as an issue. No test that always fails is selected.
  3. Sleep-then-assert-nothing becomes a paused clock or an awaited event; the 30–35 s window in `exclusive_resources.rs` goes.
  4. Journey tests deploy with `wamn deploy`; the observer uses a read-only login.
  5. Delete unused `test-support` items (`ctl_process`, `run_result::require_success`, `connection_refused`, `operations::sealed_in`, `credentials::set_reference`, `event_broker::load`, `collector::render_collector`, the `event_broker_files` example), the unused `proptest` dependency, and the nine dead links in `docs/testing`.
  6. Test files are named by what they need; `_live` goes from files that need no service.
  7. One SQL crate holds the grant refusal tests for the four remaining logins.
- Not proposed: CI (ruled 2026-10-21: no reliance on GitHub infrastructure).

## First-round findings K and P: status

Checked at commit bf8f13b7.

| # | Finding | Status |
| --- | --- | --- |
| K1 | Service flags and environment variables had no rule | Settled by K (constants become defaults, six inputs in one rendered file) |
| K2 | One NATS stated as two (`schedulerUrl`, `dataUrl`, two TLS flag sets) | Settled by E6 (one NATS address) and K (TLS paths are constants) |
| K3 | Registry bases stated twice | Settled by E6 (one registry address) |
| K4 | `provider.rs` (723 lines), `oauth.rs` (176), `installation.rs` (201) branch on provider in Rust | Open |
| K5 | Key-parity check between the values file and OpenTofu outputs | Open |
| K6 | Loose YAML in `deploy/platform` and `deploy/infra` | Settled by E7 |
| K7 | Seven charts | Open. After W2 the org chart holds only `clusters.yaml`, which moves to the project chart. |
| K8 | `environment.k` is one file per project in an environment | Open: shape of the YAML environment file (D2) |
| K9 | No owner for database sizing | Settled by W2 (an environment value) |
| K10 | Hand-written OpenTofu (A1) reverses the 2026-10-19 "no hand-written HCL" ruling | Doc fix: A1 (2026-11-15) replaces the 2026-10-19 ruling |
| P1 | Proposed crate names break the singular rule or collide | Open |
| P2 | `wamn-execution-host` holds several owners (10,400 lines, 21 internal dependencies) | Open, tied to P1 |
| P3 | Deploy crates had no target module table | Settled by the F3/F6 rulings (A1–L) |
| P4 | Manifest types live in `wamn-compiler`; 10 crates link the generator for them (`wamn_compiler::manifest` used 106 times) | Open |
| P5 | Wrong-direction edges: `wamn-postgres` → `wamn-kubernetes` (one in-cluster API call), `wamn-package-artifact` → `wamn-kubernetes` (HTTP transport), `wamn-kubernetes` → `wamn-registry` (names, ids), `wamn-workflow` → `wamn-execution-host` (`HostRouteDelivery`) | Open; all four still present |
| P6 | `test-util` features | Settled by AA |
| P7 | `docs/` is about 43,600 lines: history 11,400, review export 11,100, audit 9,000, plan 4,100 | Open |

### Ruled: first-round findings

D, 2026-11-16: accepted all recommendations.

- **K4.** Creating the kind cluster and its local registry moves to xtask (developer only). On GCP the user runs `gcloud get-credentials`; the CLI uses the current kube context. Provider differences are two values files. The `Provider` branches in `provider.rs`, `oauth.rs` and `installation.rs` go.
- **K5.** No key-parity check. `values.schema.json` (required keys, no extra keys) refuses a missing or extra key in either file.
- **K7b.** Six charts: infrastructure, platform (installation, identity, edge, control service), org (namespace, RBAC, quota, NetworkPolicy), environment (host group), project (cluster, database, publication, certificates), application. The control service leaves the application chart (`controlService`) for the platform chart.
- **K8a.** One environment file per environment, with its projects and their applications inside. Environment settings (offline window, sizing, NATS limits) are written once.
- **K10.** Hand-written OpenTofu (A1, 2026-11-15) replaces the 2026-10-19 "no hand-written HCL" ruling.
- **P2.** `wamn-execution-host` splits by owner: file route to `wamn-blobstore`, control route to the control service, number counter to `wamn-sql`, OAuth callback to `wamn-connections`, the rest into `services/host`. No `wamn-route` crate.
- **P1.** `wamn-ctl` becomes `wamn-cli`. `wamn-event-wire` and `wamn-events` merge into `wamn-event`. Role names live in `wamn-role`. `wamn-platform-identity` becomes `wamn-identity`; the identity service package becomes `wamn-identity-service`, binary still `wamn-identity`. `wamn-web-front` becomes `wamn-web`. These replace the earlier names `wamn-roles`, `wamn-event-types`, `wamn-identity-core`, `wamn-route`.
- **P4.** The `wamn.json` model moves from `wamn-compiler` to `wamn-catalog`.
- **P5.** `wamn-postgres` → `wamn-kubernetes` goes with mounted credentials. `wamn-package-artifact` uses `oci-client`, not the Kubernetes HTTP transport. Name builders leave `wamn-kubernetes` (no `wamn-registry` dependency). `wamn-workflow` → `wamn-execution-host` goes with the workflow service component.
- **P7a.** Delete `docs/history`, `docs/plan` and `docs/audit` now (Git keeps them). This doc and `rulings.md` are the authority until the restructure lands; then `docs/architecture` and `docs/operations` are rewritten.

## Order of work (proposal, not ruled)

D's direction (2026-11-16): mass deletion first, then restructure, then code editing; one item may be split across phases.

Rules proposed for every step:

- Each step ends with the workspace building and unit tests passing. Service and journey tests may be off until phase 3 restores them; each step names what it turns off.
- A ruled item with a replacement is split: the old code goes in phase 1 when nothing in production depends on it, otherwise in the phase-3 step that adds the replacement.
- Phase 2 changes no behaviour: renames, moves, splits and merges only.

### Phase 1: deletion

| Step | Deletes | Production effect |
| --- | --- | --- |
| 1.1 Dead and test-only code | Uncalled provisioning path (`environment/mod.rs::apply`, `ensure::provision_resources`, `ensure::provision`, `*_from_env`); `sql::initialize`, `stage::initialize`, `environment/status.rs::status`, `rollback::rollback`, `dry_run`, `show::show`; `ReleaseCarrier`, `ReleaseIdentity`, `release_composition.rs`, the old ConfigMap release mount, `pull_package`, `read_descriptors`, `load_component_facts`, `pull_entry`, `read_release`, `package_route`, `PackageSource::Directory`, `entry_members`, `composing_*`; the candidate connection branch; `NativeInput::Typed`; `classify_statements` and the `transactional` field; `ServiceReader`, `wamn_app_owner`, the dead `wamn_administration` grant; `required-schema-contract.json` | None (fact: no production caller) |
| 1.2 Tests | Tests of code ruled out (at least 15,900 lines, with their code in later steps where the code stays until then); the 13 `panic!` stubs; unused `test-support` items; the identity mail back door; unused `proptest`; `wamn-test-postgres` `lock()` calls | None |
| 1.3 Files and docs | `deploy/platform`, `deploy/infra` (keep the kind config under `deploy/kind`); `runtime-operator-events-rbac.example.yaml`; unused release chart values; `docs/history`, `docs/plan`, `docs/audit`; dead links in `docs/testing`; W37 sentence and stale `app.*` comments fixed | None |
| 1.4 Machinery with no replacement | NATS KV replica, drop notices and `wamn-auth` bucket (`kv_memory.rs`, `auth_changes.rs`, `AuthDrops`) and the connection KV memory; the five semaphores in `http/transport.rs`; the custom test runner (`selection.rs`, `selection.toml`; use `cargo test` and `cargo test -- --ignored` until the script in 3.8); the sync client generator and outbox | None for the replica: the auth cache lifetime is zero in production (fact). Connection facts are read from the database on each call until 3.2. |
| 1.5 Host release selection | Host ConfigMap poll and selection (`releases.rs::selected`, `refresh`), `registry.environment_applications.release_digest`, `write_selection`, the second file listener and `-files` Service | Temporary loss until 3.2 and 3.4: file routes, workflow schedule and event admission. Move this step to 3.2 if that loss is not acceptable. |

Where this phase-1 table and Epic 1 (below) differ, Epic 1 rules. Narrowed there: the `lock()` calls move to step 3.8; the edge keeps the component loader; `ServingManifest` and `LoadedRelease` wait for 3.2; `build.json`, the component media types and the package layers wait for phase 3.

### Phase 2: restructure (no behaviour change)

| Step | Scope |
| --- | --- |
| 2.1 Renames | `wamn-ctl` → `wamn-cli` (the bootstrap verbs are already deleted in Epic 1 issue 1.1); `wamn-control-provision` → `wamn-provision`; `wamn-compiler` → `wamn-codegen`; `wamn-connections` → `wamn-connection`; `wamn-web-front` → `wamn-web`; `wamn-platform-identity` → `wamn-identity`, service package → `wamn-identity-service` in folder `services/identity-service`; directory names equal crate names |
| 2.2 Merges | Component and package artifact crates → `wamn-oci`; `wamn-events` + `wamn-event-wire` → `wamn-event`; `wamn-postgres-statements` + `wamn-data-access` → `wamn-guest`; `wamn-build` + `wamn-component-composer` → `crates/build`; role modules → `wamn-role`; delete `wamn-release-control` |
| 2.3 Splits and moves | `wamn-execution-host` by owner (P2); the non-identity half of `crates/identity` to its owners (N6); control execution adapters to `services/control`; `wamn.json` model to `wamn-catalog` (P4); name builders out of `wamn-kubernetes` (P5). (`production_lease.rs` and the queue code are deleted in Epic 1, wave 2.1, so they do not move.) |
| 2.4 Test layout | `test-util` features become `tests/common/` or one dev-only crate (AA); test files named by what they need (AB6) |

### Phase 3: code editing

In dependency order. Each step deletes the old code its replacement makes dead.

| Step | Scope | Needs |
| --- | --- | --- |
| 3.1 Database model | `project_env_key` leaves project databases (columns, row policies, `project_key()`); connection tables move to the project database, integration table folds into `connection`; type version digest on `catalog.release_connection`; one owner role per database (S); login set T2 with connection limits; admin inheritance as rows (N3); one writer of `platform."user"`, keyed `(issuer, sub)` (N4, P5) | — |
| 3.2 Request path | One stock workload per application with components listed (A, H: stock `push_component`); `invoke` through `dispatch_target`; own loader, OCI pull and warm list go; batched fixed statements; `begin-operation` reuses the caller; local moka cache for grants and connection facts; per-call connection path (binding, cached row, secret), exclusive lock per call, client key B2; deploy writes operation facts into workload config (G2), so `ServingManifest`, `catalog.releases` and the entry image go; file routes on the wasmCloud router | 3.1, 2.3 |
| 3.3 Identity | Standard token checks from settings (`jsonwebtoken`, `jwks-cache`); claims `iss`, `sub`, `aud`, `exp`; cookies and CSRF in `wamn-web`; liveness read and `inherits_administration` go; user management adapter; fixes O | 3.1 |
| 3.4 Workflow service | Workflow service component per application workload (C), run interface C1, P-a, events through the `wasmcloud:nats` handler; `QueueService`, `RouterDriver` and host workflow tasks go | 3.2 |
| 3.5 Deploy | One `deploy` flow (B1); trimmed Job path (A1); charts per K7b and C1; YAML environment file (D2, K8a); E2–E9; package artifact with two layers (H); `build.json` goes (L2a); SQLx offline data, build skip, `wamn dev` applies migrations (I1) | 3.2 |
| 3.6 Install and isolation | Install as J; settings as K; K4, K5; W2 clusters per project, system cluster in `platform`; Q (NetworkPolicy, `resourceNames`, no platform-observer); R (NATS account per environment, CDC credentials); install defects (L1) | 3.5 charts |
| 3.7 Codegen | `prettyplease`, `wit-encoder`, off-the-shelf TypeScript client from `openapi.json`; KCL metadata only | 2.1 |
| 3.8 Tests | testcontainers and `#[sqlx::test]` (Z); changed-packages script (Y2); journey tests through `wamn deploy`; paused clocks; one SQL grant crate (AB7) | after the steps above |

Not in the order (not ruled): the workflow parts that are ruled and not built (`respond`, state after each node, backoff, waiting state), and deletion of the workflow dead code and schema.

**Ruled (D, 2026-11-16): all deletion up front.** "Project is greenfield and can be non-functional across issues/epics." Step 1.5 stays in phase 1. This replaces the first proposed rule: a step may leave features and service tests non-functional; the remaining proposed minimum is that the workspace compiles. Consequence (Claude, to confirm): ruled code that has a replacement also goes in phase 1, and phase 3 builds the replacements. Candidates for a step 1.6: `ServingManifest`, `catalog.releases`, the entry image and `compose_application`; the host's own component loader, OCI pull and warm list; the connection snapshot, the three hashes, `resource_lease.rs`; the host session liveness read and `inherits_administration`; `QueueService`, `RouterDriver` and host workflow tasks; the nine login families; the wamn component artifact format; the `publication` and `sources` package layers; `environment.k` and its KCL schema; the org chart cluster templates; the `Provider` branches; the TypeScript client generator and `rustfmt.rs`; the install Jobs and port-forward SQL.

**Ruled (D, 2026-11-16): W-b.** Step 3.4 moves only the workflow behaviour that works today into the workflow service. Four ruled behaviours stay as future work, not built now: `respond` (attached workflow returns its result as the response), state after each side-effect node (resume, restart or manual rerun), return to the queue with backoff, and the waiting state for approvals. Their scaffolding is deleted in phase 1: `effect_attempts` and `effect_attempt_outcomes`, `persistence.rs::release_lease`, `runs.run_deadline_at` and its index, `runs.attachment_id`. With it, under "all deletion up front", phase 1 also deletes the workflow dead code: `FailType::DepthBudget`, `DispatchBudget`, `UnboundRequirement`; `catalog.wiring_tombstones`; `runs.registration_id`, `invocation_context`, `platform_revision`; the advisory stream and `read_retained_advisories`; `reconcile_replica_identity`; the publication SQL builders; `contract.rs`. `read_compiled_wiring_document` is test-only, so the operation version check it holds does not run today; it goes too, and the check is rebuilt in the workflow service if wanted.

**Ruled (D, 2026-11-16): step 1.6 accepted.** Ruled code that has a replacement goes in phase 1 (the list above); phase 3 builds the replacements. The one rule kept for every step: the workspace compiles.

**Ruled (D, 2026-11-16): testing is extremely limited until the later steps.** During phases 1 and 2 (deletion and restructure), testing is churn. Proposed meaning, to confirm: a step is checked by `cargo build` (and `cargo clippy`) only; tests that no longer compile are deleted with their code, not fixed; no new tests and no test rewrites until phase 3; service and journey tests are rebuilt in step 3.8 against the finished code.

- Confirmed (D, 2026-11-16): the testing rule as written above.
- Ruled (D, 2026-11-16): only one epic is fully scoped at a time; the next is scoped after the previous one is reviewed. Work inside an epic is as parallel as possible.

## Phase 1 mapping: facts (commit bf8f13b7)

Two readers mapped every phase-1 item to the files it deletes, the callers it must edit to compile, and the files two items would both edit. Main claims checked by me where marked.

- `services/host` is a choke point: six of the eight runtime items edit `host/releases.rs`; most also edit `projects.rs` and `host.rs`.
- `ServingManifest` and `LoadedRelease` are read in about 31 source files, including the control service (`services/control/src/lib.rs:347`, checked) and the edge device (`services/edge/src/release.rs`).
- The edge device loads applications with the host's loader (`services/edge/src/application.rs:44`, `policy.rs:212`, checked).
- Drop notices (`AuthDrops`) pass through about 25 source files; 51 files call `AuthDrops::default()` (checked), about 37 of them tests of features that stay.
- Without the NATS bucket the auth and connection caches keep nothing (`kv_memory.rs::keep_entry`, `auth_cache.rs::keep_token`), so callers only drop the cache lookups.
- The `effect_attempts` tables no longer exist in code or SQL.
- The publication SQL builders exist only to set up tests; deleting them deletes four CDC test files.
- Items whose deletion needs new code to compile (not only removal): `build.json` (read by publish, web artifact, dev deploy, overlay build), the wamn component artifact media types (the host pulls through `pull_verified`), the package `publication`/`sources` layers.
- Items whose deletion only removes more features (no new code): the login families the host opens pools for, the org chart templates, the `Provider` branches, the install Jobs, `cluster.k`/`project.k`.
- `wamn_test_postgres::lock()`: 229 calls in 88 files, touching every group.

## Epic 1: phase 1 deletion (scoped)

Ruled by D, 2026-11-16 ("accept"), with these decisions:

1. `AuthDrops` and `ConnectionDrops` become no-op types in phase 1; their parameters are removed in phase 2.
2. Phase 1 deletes only the publication side of the release document (OCI release artifact, `catalog.releases`, entry image). `ServingManifest` and `LoadedRelease` and their readers go in step 3.2 with their replacement.
3. The edge device keeps the host's component loader. The edge after the restructure is a design item for a later epic.
4. Deletions that need new code to compile wait for phase 3: `build.json`, the wamn component artifact media types (`pull_verified`), the package `publication` and `sources` layers. Deletions that only remove more features go now: the login families the host opens pools for, the org chart templates, the `Provider` branches, the install Jobs, `cluster.k` and `project.k`.
5. The `wamn_test_postgres::lock()` calls go in step 3.8 with `wamn-test-postgres`.

Rules for every issue:

- Done means `cargo build` and `cargo clippy` pass for the workspace (and the `apps` workspace where touched). No other checks.
- Only deletion, plus the smallest caller edit that keeps the code compiling (remove a call, an argument, a branch, a field). No new behaviour.
- A test that stops compiling is deleted, not fixed. No test edits otherwise.
- An issue edits only the files it owns. A caller in a file another issue owns is left for that issue or for wave 2.
- Features may stop working. Expected: `env apply`, `platform deploy`, host serving, workflows, file routes, the TypeScript clients.

### Wave 1: seven issues in parallel

| Issue | Owns | Deletes |
| --- | --- | --- |
| 1.1 Environment and install | `crates/environment`, `crates/platform-install`, `crates/release`, `crates/catalog/src/configuration.rs`, `services/control/src/main.rs`, `services/ctl/src/{provisioning_verbs,identity_verbs}.rs` and their entries in `services/ctl/src/main.rs` (the bootstrap verbs), `deploy/gcp/environments`, `deploy/gcp/{cluster,project}.k`, `deploy/kind/cluster.k`, `deploy/charts/installation/values.yaml`, `xtask` platform project files, receiving and REST connector test fixtures named in the mapping | `environment/mod.rs::apply`, `dry_run`, `*_from_env`; `ensure::provision_resources`, `provision`, `resources_in_place`, `restore_project_database_access`, `prepare_cdc`, `ensure_cdc`; `sql::initialize`; `stage::initialize`, `create_number_sequences`; `status.rs`, `sql::schema::status`; `rollback.rs`; `show::show`; `runtime::resolve`; `legacy_release`; `component_source_from_env`; `mint_management_author_pat`; `ProvisionedRoute`; `ReleaseCarrier`, `ReleaseIdentity`; `operations::provision`; `org::recovery_values`; `oauth::dev_key`; environment KCL (`kcl.rs`, `crates/environment/kcl`, the `.k` environment files; `compile` keeps JSON only); nested bootstrap Job (`environment.rs::initialize`, `initialize_database`, `bootstrap-project`); `require_chart_references`; `hostConfiguration` and the `helm get values` read-back (`resolve_application`, `application-host.yaml`); `write_selection` and its callers; `Provider` branches; install Jobs and port-forward SQL; `cluster.k`/`project.k` documents and their KCL, `platform_tofu.rs`; unused release chart values (`release.artifactBase`, `audit`). Moves `deploy/infra/kind-config.yaml` to `deploy/kind`. Keeps `materialize_application_principals` and `to_kcl` for now. |
| 1.2 Build and test runner | `crates/compiler`, `crates/catalog/src/route_identity.rs`, `crates/publish`, `crates/package-artifact`, `xtask/build`, `xtask/src/{lib,selection}.rs`, `tests/selection.toml`, `justfile`, TypeScript packages that consume generated clients, `apps/client_acme_receiving/tests/overlay_compatibility*.rs`, `apps/wamn_wms/tests/{release_bindings,wms_browser_kind}.rs` | `classify_statements`, `plan_needs_transaction`, `called_routines`, the `transactional` field; `required-schema-contract.json`; `rustfmt.rs`; TypeScript client generator (`client_ts.rs`, `client_component.rs`, `ClientPlan`; keep `client_ir.rs` and the parts of `client_plan.rs` that publish uses); sync generator (`client_ts_sync.ts`) and the TypeScript sync and outbox code; `package_artifact_digest`; pull-back after push in `publish_and_verify`; `validate_append` over all versions; `read_descriptors`, `load_component_facts`; the custom test runner. Keeps `openapi.json`. |
| 1.3 Host cut | `services/host`, `deploy/charts/release/templates`, `crates/execution-host/src/file_route.rs::serve_selected`, the `release_digest` column in `deploy/sql/system-schema.sql` | Host release selection (`Selection`, `selected`, `refresh`, pause, `files()`); second file listener (`files_addr`) and the `-files` Service; host spawns of the queue, schedule and event tasks (`releases.rs::serve` admission and queue spawns, `serve_application_events`); `replicate` calls; `lease_tests.rs`. The libraries they called stay until wave 2. |
| 1.4 Auth caches | `crates/authority`, `crates/authentication`, `crates/sql/src/session_identity.rs`, `crates/connections/src/{connection_memory,connection_facts}.rs`, `crates/broker/src/{kv_memory,auth_changes,connection_changes}.rs` and the bucket parts of `authorization.rs`, `services/identity/src/cli.rs`, `crates/http/src/lib.rs` (`oauth_ticket` compare), `crates/execution-host/src/application_route.rs` (cache clear) | NATS KV replica and buckets (`kv_memory.rs`, `AuthCache` replica, `wamn-auth` and `wamn-connection` buckets, `keep_buckets`); connection KV memory; host liveness read; `inherits_administration` and the hierarchy reader. `AuthDrops`, `ConnectionDrops` and `Publisher` become no-op types. Fixes the stale `app.*` comments in `authority/src/identity.rs`. |
| 1.5 HTTP transport | `crates/http/src/transport.rs` and its tests | The five semaphores and their constants, quotas and permits |
| 1.6 Login roles | `crates/provision`, `deploy/sql/{run-state,system-schema}.sql` grant lines | `ServiceReader`; the families the host opens pools for (`ExecutorPlatform`, `HttpAdmitter`, `Administration`, `IdentityReader`, `ConnectionReader`) and `Retention`, `AuditRetention`, `SessionRoleReader`, `Control`, as far as their code is in this crate; org chart templates (`template.rs`); the dead `wamn_administration` grant; `DELETE` on `runs` for `wamn_app`. Callers in other crates are edited in wave 2. |
| 1.7 Tests, files and docs | Test files holding `panic!("wamn-` stubs (outside other issues), `test-support/resources`, `test-support/postgres/run_result.rs`, `services/identity/src/mail.rs`, `crates/catalog/Cargo.toml`, `deploy/platform`, `deploy/infra`, `deploy/charts/installation/templates/runtime-operator-events-rbac.example.yaml`, `docs/history`, `docs/plan`, `docs/audit`, `docs/testing`, `CLAUDE.md`, `AGENTS.md`, `deploy/sql/app-schema.sql` comments, `docs/wamn-architecture-review-full/rulings.md` | The 32 stubs and their helpers; unused `test-support` items (`ctl_process`, `connection_refused`, `sealed_in`, `set_reference`, `event_broker::load`, `render_collector`, `event_broker_files`, `run_result`); the `WAMN_TEST_RESEND_ENDPOINT` back door; `proptest` in `wamn-catalog`; loose YAML; the three doc folders; dead links; stale `app.*` comments; the superseded W37 sentences |

Overlaps resolved in the ownership above: 1.1 moves the kind config before 1.7 deletes `deploy/infra` (1.7 leaves `kind-config.yaml` if it lands first); `release_chart/plan.rs` values belong to 1.1; identity comment fixes in `crates/authority` belong to 1.4.

Compile rule across wave-1 issues (added by Claude after drafting the tables, to keep each issue compiling on its own):

- Every wave-1 issue must compile when merged alone. So a symbol that has callers in a file another wave-1 issue owns is not deleted in wave 1. It becomes an empty no-op (as `AuthDrops`), and wave 2 deletes it.
- This applies to: `auth_cache` / `connection_memory` `replicate` (called from `services/host`, owned by 1.3): no-op in 1.4, deleted in 2.2; the `Provider` type (used by tests in other crates): 1.1 removes the branching inside `crates/platform-install`, the type goes in wave 2.
- Issue 1.6 is narrowed: it deletes `ServiceReader`, the dead `wamn_administration` grant and `DELETE` on `runs` for `wamn_app` only. The other login families and the org chart templates have callers in `services/host`, `crates/platform-install` and `services/ctl`, so they move to 2.2, which already edits the host's pool setup.

### Wave 2: three issues in sequence

After wave 1 merges. Each pair shares files, so they run one after another.

| Issue | Deletes |
| --- | --- |
| 2.1 Workflow library | `workflow/queue.rs` and its tests, `router_driver.rs`, `contract.rs`, `read_compiled_wiring_document`; `connections/production_lease.rs`; `run-state` `release_lease`; `FailType::DepthBudget`, `DispatchBudget`, `UnboundRequirement`; `catalog.wiring_tombstones`; `runs` columns `attachment_id`, `registration_id`, `run_deadline_at` (and index), `invocation_context`, `platform_revision`; the advisory stream and `read_retained_advisories`; `reconcile_replica_identity`; the publication SQL builders and their CDC tests; the `ExecutorDatabase` impl; the `workflow.park`, `release`, `runs` control operations. Edits the callers left by 1.6 in these crates. |
| 2.2 Invocation path | `ConnectionEffectSnapshot`, `CandidateConnectionBinding`, `CandidateBindingWorld`, `matches_snapshot`, `pinned_generation`, `instance_revision`; `authorize_snapshot`, `authorize_release_closure`, `authorize_candidate_closure`, `ConnectionExecutionClosure`; the three hashes; `connection_generation.rs`; `resource_lease.rs`; `connection_projection.rs` and the registry connection projection; `sql/connection_binding.rs`; `ClientIdentity`, `ConnectionIdentity`; the host component loader on the host path (`load_component_workload` callers in `execution-host`, `warm_reuse.rs`, the warm list, `NativeInput::Typed`). The edge keeps `load_native_application`. Edits the remaining login-family callers in the host's pool setup. |
| 2.3 Release publication | Release OCI artifact (`release_manifest_artifact.rs`, `release_manifest_source.rs`, `push_manifest_bytes`), `catalog.releases`, `stage::write_release`, `run_queue::release_bytes`, `compose_application`, `entry_publication`, `entries:<digest>`, and the `catalog.releases` foreign keys. `ServingManifest` and `LoadedRelease` stay until 3.2. |

## Open items

One list of everything not yet ruled or not yet checked. Replaces the earlier "Open questions" section (all five of its items were ruled; see the Status table).

**Ruled (D, 2026-11-16): OI-1a, OI-2a and all four proposed fixes.**

- OI-1a: the workflow service polls "take next" every 1 s. `LISTEN/NOTIFY` only if the delay is measured as a problem.
- OI-2a: the deploy writes connection bindings and integration type digests into the workload config with the other operation facts. `catalog.release_connection` and `catalog.connection_binding` go (in step 3.2, with `catalog.releases`' readers).
- The changed-files helper of Y2 is an `xtask` command.
- The identity service folder becomes `services/identity-service` (phase 2.1).
- Epic 1 issue 1.1 also deletes the bootstrap verbs `provision-system`, `provision-org`, `provision-identity-issuer` and their `services/ctl` code (1.1 owns those files). Phase 2.1 no longer moves them. Step 3.6 builds the one SQL Job (J) in `wamn-control`.
- After Epic 1 is reviewed, the doc is rewritten as current state only, research moves to a second tab, and the Status table goes.
- Ruled later (D, 2026-11-16): one doc with one tab. Part 1 holds the current state; Part 2 is this log.

**Ruled (D, 2026-11-16), on the external review of Part 1:**

- Routes and field labels are defined in `wamn.k` and must be overridable. Both have derived defaults. Not ruled: where the statement list and fetch class, the error list and the idempotency key field are written (K1 or K2).
- 2a: `publish <build dir>` pushes a finished build and prints the top package digest; `schema apply` and a deploy without `<dir>` take that digest. Fact: an overlay's `wamn.k` pins each base by package, version and digest (`apps/client_acme_receiving/wamn.k:14–20`).
- 3a: `wamn_app_owner` goes in 3.1, not Epic 1. Fact: it is the parent role of every migration login (`provision/workload_role.rs::acl_role`, `environment/provision_project_env/workload.rs:107`).
- 4a: Epic 1 deletes only the `project.k` chain, owned by 1.2. `cluster.k` and `ClusterDocument` stay until 3.6. Fact: `ClusterDocument` is read in `services/ctl/src/platform_verbs.rs`, `services/ctl/src/bin/wamn.rs`, `services/control/src/platform.rs`, `xtask/src/{tasks,platform}.rs` and `test-support/application/src/lib.rs`.
- Added to Part 1 (rulings it had left out): `login_principal`, W41, SECURITY DEFINER rules and grant tests, one wasmCloud version, `oas3`, no JSONata examples.

**Ruled (D, 2026-11-16): K1 and W2.**

- K1: the statement list with its fetch class and the idempotency key field stay in `wamn.k`. A statement entry holds its name and fetch class; the SQL file path comes from the operation and statement name.
- W2: input, result and errors of a custom operation are written by hand in WIT; `wamn.k` holds no types. Fact: today all custom-operation WIT is generated from `wamn.k` (`compiler/generate/operations.rs::emit_custom_operation` → `generate/wit.rs::emit_custom_operation_wit`), and each `authored.wit` only includes the generated world. W2 confirms the earlier "written by hand" line; the custom-operation WIT generator goes.

**Ruled (D, 2026-11-16): doc location.** This doc is committed at `docs/plan/wamn-restructure.md`. Epic 1 issue 1.7 deletes everything else in `docs/plan`, with `docs/history` and `docs/audit`. No stray files at the top of `docs/`.

### Ruled 2026-11-16 (were open before phase 3)

**OI-1. Queue wake (workflow service, step 3.4).** Ruling C moved the queue loop into the workflow service: one loop per host replica per application. How it learns of a new run was never ruled.

| Option | Gained | Lost |
| --- | --- | --- |
| a. The service polls "take next" every 1 s | Nothing new to build. `FOR UPDATE SKIP LOCKED` already handles many takers. | Up to 1 s delay. Idle cost: one query per second per application per replica (5 applications, 2 replicas: 10 queries a second). |
| b. The host's "take next" (run interface C1) waits on Postgres `LISTEN/NOTIFY`, with a slow timer as a fallback | Runs start at once; almost no idle queries | One listening connection per project database in the host; `NOTIFY` is a signal, not a queue, so the timer stays |

Recommend a; b if the delay is ever measured as a problem.

**OI-2. Connection bindings after G2.** `catalog.release_connection` (the integration type digest) and `catalog.connection_binding` are keyed by the release digest with a foreign key to `catalog.releases`, which wave 2.3 deletes.

| Option | Gained | Lost |
| --- | --- | --- |
| a. The deploy Job writes bindings and type digests into the workload config, like the other operation facts (G2); both tables go | One carrier for deploy-time facts; one database read less at load | A binding change needs a deploy (it already does: bindings come from the environment file at deploy) |
| b. Keep both tables, keyed by the release digest text, with no foreign key | Bindings stay queryable in SQL | A second store for a deploy-time fact |

Recommend a.

### Checks during phase 3

- **G2 size:** Helm values stay under the release size limit for the largest application. No build output exists today to measure; check in 3.2.
- **D2 checks:** every KCL `check:` rule of `environment.k` also exists in Rust or SQL. Check in 3.5.
- **Residency endpoint:** `/_wamn/residency` lived on the file listener that Epic 1 deletes; its readers (deploy analyse and drain) are rewritten in 3.5. Proposed: it goes, unless 3.5 needs it.
- **Deploy defects** (Job accounts `<env>-sql`/`<env>-code`, missing application principals): the code paths are deleted in Epic 1 and rewritten in 3.5, so no cluster check is needed; 3.5 must create both.
- **wasip3:** ruling C needs a wasip3 service, because a plugin calls into it (`engine/workload.rs:2462`). Fact: wash-runtime 2.12.1 builds Wasmtime with `p3` on by default and has integration tests for p3 services and streams (`integration_p3_loopback_service`, `integration_wasip3`). D is moving to Rust 1.100, which has the `wasm32-wasip3` target.

### Deferred by ruling

- The edge device after the restructure (design; Epic 1 decision 3).
- The four workflow behaviours (W-b): `respond`, state after each node, backoff, waiting state.

**Ruled (D, 2026-11-16): Epic 2 scope.** `wamn-execution-host` goes into the control service, by owner. Deferred: `wamn-role` to 3.1, name builders out of `wamn-kubernetes` to 3.5, test layout and `test-support/resources` to 3.8. Fact-forced targets: `org.rs` stays in `wamn-identity` (`control.rs` and the moved code use it); OAuth callback → `wamn-http` and connection health → control service, because `wamn-connections` would form a cycle with `wamn-http` and `wamn-blobstore`.

**Ruled (D, 2026-11-16): Epic 2 targets.** `credentials.rs` → `wamn-connections` under `test-util`; `env_policies.rs` → `wamn-control-provision`; the existing catalog manifest types → `wamn_catalog::manifest::installation`; the event crates merge in 3.4.

**Ruled (D, 2026-11-16): Epic 3 scope.** `platform."user"` keeps `id` and adds `UNIQUE (issuer, sub)` (1a); the platform principals are seed rows (2a); connection limits wait for 3.6 (3a); the epic is checked by applying the schema to a fresh local Postgres (4a); `catalog.release_connection` and `catalog.connection_binding` lose `project_env_key` now and are deleted in 3.2 (5a). Fact: admin inheritance is already stored as rows (`application::project_authority`, run on org and project admin grants), so N3 needs no work.

**Ruled (D, 2026-11-16): Epic 3 scout questions.** 21 questions answered (see the Epic 3 section of Part 1). Facts behind the changed answers: project databases share the org cluster, so logins span all of them (11); `control_principal` is the row policy of `identity.principals` (Q1, Q3); `configuration.rs` connection writes have no caller with rows (10); `workflow_user_id()` is a fixed constant (17).

**Ruled (D, 2026-11-16): Epic 4 scope.** Step 3.2 splits into Epic 4 (workload, invoke, SQL and auth, release document) and Epic 5 (connections, file routes, integrations, features built only when declared). Deploy facts reach the host in config key `wamn.application`, and the guest keeps `wamn.http.routes`. The grant cache lifetime travels in the workload config, and its registry columns and `platform.setting` rows go. The edge keeps what it uses of the release document and the wamn component pull, moved into `services/edge`. The control service uses `HostRouteSet::Control` directly. The epic checks the rendered Helm values size. No new tests until 3.8.

**Ruled (D, 2026-11-16): Epic 4 scout questions.** `wamn.application` gains the serving component per operation, the coordinate names and delete as an operation type. `wamn-sql` keeps `wamn:operation` (`wamn-sql` depends on `wamn-engine`). No connections on the invoke path until Epic 5. 4.4 narrows to `LoadedRelease` and the request path, and `ServingManifest` stays for 3.4, 3.5 and Epic 5. The edge receives only the `LoadedRelease` parts and `pull_verified`.

**Ruled (D, 2026-11-16): Epic 4 review.** Epic 4 is accepted. Three fixes move to Epic 5: the deploy refuses an operation name that two components declare; the session cache keeps a grant set only under the `principal_grants` condition; the deploy refuses two versions of one host interface package. The application host routes and the file routes have had no server since Epic 1 deleted `services/host/src/host/releases.rs` (b84b5739). Serving them moves to Epic 5.

**Ruled (D, 2026-11-17): Epic 5 scout questions.** All recommendations are accepted. The connection principal's status check goes (D-a): disabling a connection stops its calls. Facts: the host has had no reader for the projects file and no plugins for connections, blobstore or integrations since Epic 1 (0943187e1). Every read route answers 503 since 4.2, because the route guest calls `sign-result` for each read.

**Ruled (D, 2026-11-17): host project logins (5.1 stop).** Option b, the upstream plugin entry.
- The environment values write one `wamn-postgres` entry in the host group's `plugins`. It has `workloadConfig: deny` and lists the CNPG login Secrets in `secretFrom`.
- Its config holds one JSON value for each project: host, port, database, the login Secret for each authority class, and pool settings.
- The host reads `username` and `password` from each mounted Secret and does not merge them into the config, because every login Secret has the same keys.
- Pools are built at first bind, and a changed password rebuilds them at the next bind.
- `projects.json`, `projects_from_json` and the startup credential map go.
- Fact: `projects.json` held only project names, and no chart mounted it on the host.

**Ruled (D, 2026-11-17): integrations are sibling components (option c).** Loading components is wasmCloud's job, so the host's integration loader goes.
- Each wasm-type connection a component declares is a sibling component in the application workload, named after the import label and pinned by digest, with `allowedHosts` set to the connection's authority. wasmCloud links the labelled import to it.
- The disabled-connection check, fresh credentials and the exclusive lock move into the `wamn:integration/bound` guard (`begin` returns a `call` resource for each call).
- The authority is fixed for each connection. Before this ruling, an authority edit applied on the next call.
- Rejected:
  - a: `world()` updated from start requests
  - b: a wasmCloud patch for matching
  - each integration as its own workload: no linking between workloads
  - build-time composition: a rebuild for each version
  - component host plugins: not in release images, and one version for each host group
- This matches the 2026-10-21 ruling (a request-time service is composed into the consuming application) and the 2026-10-31 ruling (each connection name is a named import routed by label).

**Ruled (D, 2026-11-17): integration authority and lock wait.**
- An IPv6 literal authority is refused for wasm types at connection create, connection edit and deploy. The operator CRD's `allowedHosts` pattern accepts hostnames and IPv4 addresses but no bracketed IPv6 form. A hostname also permits the DNS lookup and raw socket connections to the addresses it returns.
- `begin` bounds the lock wait with a `lock_timeout` of `MAX_HOST_CALL_DURATION` (30 seconds). The sibling cannot see the caller's deadline, and the calling attempt has the same bound.
- Rejected: a patch to the operator CRD, and a refusal only at deploy.

**Ruled (D, 2026-11-17): socket floor and connection targets.**
- The wamn-d0w4 floor stays: `allow_private: false` for every guest on a cloud host. Modbus belongs at the edge, not in the cloud cluster.
- The deploy refuses a binding whose connection's target is not the environment's target, for every connection type, so connections meant only for a local target never go to the cloud.
- Fact: the deleted integration loader allowed a private address for each integration through its own engine. A shared host engine cannot do that.
- Rejected: `allow_private: true` on cloud hosts, and an upstream range grant for each component.

**Ruled (D, 2026-11-17): integration placement, call limit and lock cost.**
- Wasm integrations stay sibling components (c).
- Host component plugins were weighed and not chosen for connection-bound integrations. They run one shared store for each host, with one `allowedHosts` for each plugin; bindings in host config roll the hosts; and a plugin that exhausts its restarts stops every workload's calls until the host restarts.
- Native drivers become native host plugins.
- A call limit for each application on a connection is enforced with Postgres advisory slots, because a limit kept in host memory counts only one host pod.
- The exclusive lock moves to a pooled connection and one round trip (5.5). Fact: the 5.2 draft opened a new Postgres connection for each exclusive call.

**Decided (2026-11-17): 5.4 `user.list`.** `user.list` works in the host without `admin_covered` until 3.3, and the host gets no system database login. Fact: admin role grants and revokes are always refused, so `user.list` was the only reader of the system identity reader.

**Ruled (D, 2026-11-17): no Postgres guards in the cloud.** This replaces today's call-limit ruling and, for the cloud, the 2026-11-15 per-call advisory lock ruling.
- Exclusive devices belong to local targets, so the deploy and connection create refuse exclusive connections in cloud environments.
- The call limit is removed: external systems enforce their own limits.
- `begin` sends no guard SQL. A cache miss still fills the 5.2 cache, and the platform pool keeps its default.
- Fact: a guard held for each call kept one Postgres connection for the whole external call, so the pool would have had to grow with hosts × limits. Lease rows would have added two writes for each guarded call.
- Rejected: pool sizing by the deploy, a separate lock pool, a bigger fixed pool, and lease rows.

**Ruled (D, 2026-11-17): Epic 5 review.** Epic 5 is accepted. The cleanup removes the unused `wamn-broker` dependency, the uncalled `ReleaseIdentity::from_context` and its stale comment, and the stale `cargo xtask host-route-clients` references. The test-only `test_release` fixture that builds a `ServingContext` stays until 3.8.

**Ruled (D, 2026-11-17): Epic 6 scope.**
- One token audience for each org.
- PATs move into the project database, and service principals live only there (issuer `wamn`).
- The signing key uses the OAuth envelope.
- `environment-admin` is deleted.
- Control is the only caller of identity, over the operator-certificate hop with the subject checked.
- All eight issues.
- Facts:
  - The host authenticated no user and no API user after Epic 5: it had no session verifiers and no system login for `route_pat`.
  - `project_authority` wrote admin rows only for services.

**Ruled (D, 2026-11-17): Epic 6 scout questions.** The agent's recommendations are accepted, with three changes:
- The envelope code goes into a new crate, `wamn-envelope`, not `wamn-oci`.
- The operator and management-author PATs are retired (no production caller issued them).
- The key column is renamed `private_key_envelope`.
Service row email stays `<subject>@<platform domain>`. `jwks-cache` is replaced by a small fetcher, because its reqwest 0.12 and jsonwebtoken 10 do not match the workspace.
Facts:
- Login was limited to control holders (`holds_control`), and one login cost two password checks.
- Nothing in the install fills the control administration map.
- The environment host group has no verifier settings.

**Ruled (D, 2026-11-17): Epic 6 review.** Epic 6 is accepted. The fix commit:
- needs no mount change: `ensure.rs::host_project` already mounts `identity-ca` at `/etc/identity-ca` on the environment host group. The review had checked only the chart defaults.
- takes the JWKS lifetime from the response's `max-age`, capped at 300 seconds, not from a wamn identity constant
- renames the org audience to `urn:wamn:org:<org>`.

**Ruled (D, 2026-11-17): workflow entry nodes and Epic 7 scope.**
- A workflow starts at an entry node: Request with Response, Schedule, Data change, or Topic (later). There is no Manual entry; "run now" is an admin action. This brings the synchronous Request/Response forward from W-b; state after each node, backoff and approval waiting stay future work.
- Request workflows:
  - a path under `/api/<application>/flow/`
  - a permission granted to roles
  - invoker by default
  - a status and JSON body
  - refused after 30 seconds with no Response
  - a run row only when the graph continues after the Response or writes
- The graph owns the trigger, and the author step derives index columns.
- Runs execute where admitted, with a 30-second recovery check. This replaces OI-1a (polling each second).
- The service runs in every application workload. Nodes are called through the host. `node_digests_json` goes with `manifest_digest`. The authoring check reads the live WorkloadDeployment. Data-change consumers are bound in the workload config. The event crate merge is part of this epic.

**Ruled (D, 2026-11-17): Epic 7 scout answers.** wasmCloud refuses NATS handlers on a service, so the service pulls. One durable pull consumer for each enabled Data change workflow, filtered to its entity, created at enable and deleted at disable by the host. Workflow author, enable, disable and list are application host routes; control's workflow routes go. The enable LSN replaces `enable_sequence`. Graph format 0.2 with typed entry nodes and a response node. Exact dispatch by sibling and export. One checkpoint at the Response. A Request call admits a run row at its start when any reachable node can write. Request permission `wamn-workflow:<application>/<workflow_id>`. No role grant to the `workflow` user; the host reads the `execution_role` grants directly. emit is refused until Topic.

**Ruled (D, 2026-11-17): Epic 7 lock and deliver policy.** The host takes the environment key on the project database, and environment apply and delete take it there too. A new workflow consumer starts one after the source stream's last sequence, read before the enable LSN. Open items added: the 3.5 deploy check of enabled workflows, and the source stream retention limit.

**Ruled (D, 2026-11-17): Epic 7 Data change delivery.** This replaces the guest pull in the scout answers. The stock `wasmcloud:nats` plugin cannot give a shared host group's guest a per-project credential: a base `inbox-prefix` is refused, named bindings come only from WIT labels, and the project user may subscribe only to its own inbox. So the host fetches with the project's client, admits, acks, and returns leased runs through `take-data-changes`. The guest drops `wasmcloud:nats`. The `wamn-nats-<key>` Certificate moves to the environment's namespace. Topic publish and subscribe later go through host functions too.

**Ruled (D, 2026-11-17): stock `wasmcloud:nats` for Data change.** This replaces the host fetch ruling. The blocker came from per-project NATS users, which F7 already removed. Epic 7 builds F7's NATS account per environment, with one stock `wasmcloud-nats` host-group entry, and workloads narrow their grants to their project. Application developers are the main users of Data change: components declare entity, ops and role in `wamn.k` and export the stock `jetstream-handler`, and deploy renders durable subscriptions. This narrows the 2026-10-23 ruling: packages ship no workflows, but may declare Data change handlers. The workflow service pulls through stock imports.

**Ruled (D, 2026-11-17): Epic 7 NATS accounts and Data change handlers.**
- 7.3 covers install, the Jobs, the host, release planning and the image pipeline.
- A `PLATFORM` account and one account per environment, with limits in code. The source stream has a 7-day age and a byte limit.
- The configure Job uses the environment credential. Each CDC reader has a publish-only user, as F7 states. The reader Deployment comes back, the bootstrap is fixed, and the operator charts move to 2.12.1.
- App handlers: `flow-http` is the workload's one stock `jetstream-handler`. It checks the owner package and dispatches to the declared component through the operation path, as the `workflow` user with the declared role. Components export a wamn handler interface with JSON.
- Ops are `insert`, `update` and `delete`, and `wamn.k` names a model of the component's package.

**Ruled (D, 2026-11-17): one Data change handler for each model and op.** The stock `message-handle` does not say which consumer delivered a message, so an application has one subscription for each (model, op), and release planning refuses two components that declare the same (owner package, model, op). A failure re-runs only that handler. The host accepts `dispatch-data-change` only from the route guest, so one component cannot run another's handler under its role.

**Ruled (D, 2026-11-17): handler transactions and workflow polling.** A Data change handler has no operation. The invocation authority gets a handler case, and the handler takes the operation host-transaction setting, on by default, with one transaction per delivery tied to the ack. The workflow service refreshes its consumer list every 30 seconds instead of every second, and pulls with a 30-second long wait. One run at a time per consumer stays until something is measured.

**Ruled (D, 2026-11-17): the route guest is `ingress`.** `flow-http`, `guests/http-route` and `wamn:flow-http-routing` become `ingress`, `guests/ingress` and `wamn:ingress`. "Flow" was a leftover name with no meaning in the current design.

**Ruled (D, 2026-11-17): external crate-graph review.** Checked against the code. Not true as stated: `http-route` is a dev-dependency of `wamn-sql`, and the test crates in `wamn-http` and `wamn-publish` are optional behind `test-util` (still against AA). Already ruled: P5 and the event crate merge. New open items after 3.5: the host's narrow use of `wamn-environment` and `wamn-provision`, `wamn-sql` named for what it is, and control-plane code split out of capability crates. Rejected: a broker protocol carrying resolved grants to the host, and a new types crate before P4 and P5 are done. The host's unused `wamn-postgres` dependency goes in 7.7.

**Ruled (D, 2026-11-17): crate-graph follow-up.** The host's two edges into `wamn-environment` and `wamn-provision` brought codegen, build, publish and install into the host binary, so the move of the grant administration happens in 7.7, not after 3.5. 7.7 also removes thirteen unused dependencies. New open items: generic HTTP clients out of infrastructure crates (with P5), how the host reads connection secrets (3.6, research), one owner for `platform.tx_identity`, and the identity service's own principal.

**Ruled (D, 2026-11-17): `wamn-sql` split and step 3.7b.** A module analysis, checked against the code, shows `WamnPostgres` as a hub with 77 inherent methods and an eight-module cycle. Not true as stated: `wamn-http` and `wamn-events` use `WamnPostgres` (only `wamn-blobstore` and the host do), and `http-route` is only a dev-dependency. Split: the pool layer into `wamn-postgres`, the auth lookups into `wamn-authentication`, and the rest renamed as the operation host, with node and handler dispatch kept in it. A new step 3.7b, crate boundaries, holds this split and the other crate-graph items, before 3.8. The 14 methods with no production caller go in 7.7.

**Ruled (D, 2026-11-17): `wamn-host` module review.** Checked against the code. `host::workflow` is the Epic 7 host plugin, as ruled, and its dependencies (`wamn-run-state`, `wamn-event-wire`, `wamn-broker`) are its own. Not true as stated: host startup does not use `wamn-environment`, `wamn-provision` or `wamn-postgres`; `UserHost`, `UserPlugin` and `UserSecret` parse the upstream plugin config, and 7.7 already removes those dependencies. Real: `AuthorizationStore` has two implementations that call each other (`WamnPostgres` and the host cache), and `workflow::lifecycle` imports `Refusal` upward. Both are 3.7b items.

**Ruled (D, 2026-11-17): crate diagram review.** Checked against the code. Not true as stated: host startup using provisioning (only `application_route.rs` did, and 7.7 removes it), and control using `wamn-events`. Overstated: `wamn-kubernetes` → `wamn-catalog` is one type in 3 places. `wamn-http`'s test crates sit behind `test-util` (AA). New: `wamn-broker` mixes runtime NATS helpers with control-plane authorization that writes Kubernetes Secrets, so the host links Kubernetes through it. The 3.7b plane split adds `wamn-broker` and `wamn-http`'s OAuth flow, with the goal that the host and the CDC reader link neither `wamn-kubernetes` nor `wamn-provision`.

**Ruled (D, 2026-11-17): Epic 7 review.** Accepted at `435deffb4`. Fixes before main: the edge keeps trapping every import it does not provide, recorded as a risk until the edge design, and each environment account's file limit is 256 MiB times its project count, rewritten by the configure and delete Jobs. Open items: kind coverage of Data change handlers in 3.5, NATS volume sizing in 3.6, and the edge dependency check.

**Ruled (D, 2026-11-17): Epic 8 scope.** Step 3.5 does the deploy charts (`release` → `application`, `environment` split into environment and project, C1); the `platform` chart merge goes to 3.6 with install and settings. The version authority for integrations is the environment file, one version per connection per environment, as Part 1 states. Epic 8 ends with build, clippy and size checks; the first kind run comes after 3.6, with D's approval.

**Ruled (D, 2026-11-18): Epic 8 scout answers.** `serde-saphyr` and a schema generated from the Rust types; the project is found from the application name; one pin per (org, env, connection). Per-request input ConfigMaps owned by the Job. The release is read from WorkloadDeployment annotations (top package digest, Helm revision), and rollback runs `helm rollback` in the worker Job; control gets no Helm or Secret-list access. Codegen's describe cache lives in `.wamn/describe/`, not `.sqlx/`. Residency goes, with approved status and rollback contract changes. One operation account per project. Inside one deploy, SQL runs before code, migrations must work with the previous code, and deploy never resets. `wamn.json` gets a compiled deploy section that pins component and base digests. The append check compares with the latest digest under the same tag; versions are free tags. 8.4 also deletes the platform observer RBAC, which let every org service account read the installation namespace's Secrets.

**Ruled (D, 2026-11-18): package tags are mutable.** Following the version rulings of 2026-10-26 and 2026-11-10, a tag names the latest digest pushed under it. `push_package` no longer refuses a new digest under an existing tag; it runs the append check against the tag's current digest, then moves the tag.
