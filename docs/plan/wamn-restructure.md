# wamn restructure proposal

Nov 16, 2026 · @D Kloimwieder

Part 1 holds the current rules. Each new ruling edits Part 1 in the same commit. Part 2 holds the reasons.

# Part 1: Current state

## Principles

- Single responsibility, single source of truth. No rewrite, restructure or rename is too large. Delete all accidental complexity.
- Deploy as close to stock wasmCloud as possible. Prefer upstream or off-the-shelf tools over our own code.
- One owner per fact. If two places state the same fact, one goes or is derived.
- No runtime checks for developer practices.
- Generated files are never edited; hand-written files are never generated.
- Names say what a thing is. Names are singular. Directory names equal crate names without the `wamn-` prefix. No "ctl".

## Hierarchy and isolation

Org → environment → project → application. One boundary per level.

| Level | Boundary |
| --- | --- |
| Org | Kubernetes namespace with RBAC, a quota and a default-deny NetworkPolicy. `wamn-platform-observer` goes. The system database scopes org control and apply through `registry.login_org()` and org row policies. |
| Environment | A NATS account with JetStream limits, and its own host group. The host group serves all applications of the environment. It defaults to one replica. Each CDC reader has its own credential in the account. |
| Project | One CloudNativePG `Cluster` per project database (backup and restore per project). One owner role per database. |
| Application | The host's permission check and the wasm sandbox. The project is the trust unit. |

- The system database (`registry`, `identity`) has its own cluster in namespace `platform`.
- Logins per project database: migration (owner), application, read-only (dynamic SQL), platform (all host platform work, retention included), and the CDC replication login. Each project Cluster declares `wamn_migration`, `wamn_app_login`, `wamn_query_login`, `wamn_platform_login` and `wamn_cdc` with connection limits. Hosts use project database logins. Org control and apply use separate system database logins.
- One schema-diff service account per project. Jobs that run Helm use the org worker.
- `project_env_key` is not stored in project databases (no column, no row policy, no `project_key()` functions).
- Shared platform components (infrastructure operators, NATS, identity, edge, web) are trusted, run across all namespaces and know orgs only as run-time data. Adding an org changes no shared config and restarts no shared pod. Least privilege applies to the actors inside each org: org workers, hosts, control and users (T1).
- Orgs are data in the system Postgres (`registry.orgs`). No chart values list orgs (T2).
- NATS runs in operator (JWT) mode with the full resolver. Identity pushes signed account JWTs to the running server and returns signed user JWTs to callers. One account per environment plus `PLATFORM` and the system account. JetStream limits and user permissions, including each user's inbox prefix, live in the JWTs. Clients use creds files or a JWT and local signing key over TLS (T3).
- Scope Secrets by `resourceNames` where possible. The org worker can get, create, patch and delete Secrets throughout its org. It can list release Secrets in the separate `wamn-helm-<org>` namespace, which holds only Helm records.
- Helm uses server-side apply. Org charts name the target namespace on their objects. WAMN's own charts grant no Secret watch. The org worker has no Ingress or `escalate` rights. Scope explicit `bind` rights by name. Public integration egress uses declared ports and `allowedHosts`.
- The runtime operator watches all namespaces. Its `-host-namespaces` argument stays fixed (T4). Web retries failed connections before it forwards a request, with a one-second connect timeout.
- Control uses `<org>.<base>`. Application hosts use `<label>--<org>.<base>`. Labels are unique within an org. Web checks the host against the route's namespace.
- Each host sets its runtime ID from its pod name. Identity signs a separate scheduler user in `PLATFORM` after TokenReview checks the pod-bound token and host service account. The user has rights for its host subjects, its inbox and the fixed operator replies.
- The default host data user is in the environment account and cannot publish. Environment apply creates the account. Org creation makes no NATS calls.
- Environment account storage is 256 MiB per project. Connection, subscription and payload limits scale by project count. Identity signs the account again when the count changes.
- The environment account JWT sets `data: -1` (no limit). NATS does not enforce it.

## Database, identity in SQL, grants

- The Postgres capability follows upstream `wasmcloud:postgres`, plus identity, the host operation transaction and pools. Two interfaces: `client` (query only, on the read-only login, in a host read transaction that binds identity before it becomes read-only) and `statements` (operation transaction, `query`, `query-batch`, upstream result shape). No guest begin. Composed apps share one host transaction.
- Identity is "first insert wins": each transaction first inserts `platform.tx_identity` (unlogged, `xid8` key). `wamn_authority.current_identity()` reads it. Migration and platform logins resolve their identity through `platform.login_principal`.
- A guest `COMMIT` is caught at host commit, not by a SQL text check: the finish statement compares the resolved identity's transaction ID with the starting ID. If the check fails, the invocation fails and the connection is discarded (W41).
- SECURITY DEFINER functions are owned by their database's owner (`datdba`), set `search_path=pg_catalog`, have no PUBLIC EXECUTE or grant option. Named roles can execute the functions they need. Grant tests are refusal tests plus invariants derived from the role model.
- Grants: one project database read, cached in the host's local `moka` cache. Deploy takes one `revocation_seconds` value from the environment file and writes it into each project's workload config. The cache holds positive answers only. No NATS replica, no drop notices.
- Admin inheritance (org and project admin) is stored as rows in each project database. No system database read per request.
- `platform."user"` keeps `id` (UUID) as primary key and is unique on `(issuer, sub)`. The four platform principals and the workflow service principal are seed rows in the schema.
- Roles and ACLs are run-time data. The permission vocabulary is derived from the schema and the operations. Applications ship with an `admin` role only.
- Guest SQL (`wamn_app`, `wamn_query`) gets no grant on `platform.credential`, `platform.oauth_token` or `platform.oauth_authorization`.
- Foreign keys and audit stamps on user rows use `id`. Only inherited grants can give users the `admin` role. The user list reports that role as `admin_covered`.
- The project database owner is the migration login. `wamn_app_login`, `wamn_query_login` and `wamn_platform_login` inherit their group roles. `project_env_key` stays in shared stores and resource names.
- Identity calls `platform.prune_tx_identity()` every 60 seconds in the system database. Org control handles cleanup in project databases.

## Request path

- One stock wasmCloud `WorkloadDeployment` per application: the route guest, the application components (stock `application/wasm` artifacts) and one workflow service component. The operator pulls and starts them.
- The route guest calls `invoke` (JSON); the host reaches the component through `dispatch_target`. The host has no loader, OCI pull or warm list of its own.
- The deploy writes into the workload config what exists nowhere else: the route table; for each operation public or private, host transaction, read or write, idempotency key field; the application's database schema; connection bindings and integration type digests. Cluster deploy uses workload configuration and annotations. Edge tooling keeps `ServingManifest` until the edge design replaces it. There is no `catalog.releases` or entry image. The Helm revision is the release.
- Fixed statements of a transaction go as one batch at begin and one at commit. `begin-operation` reuses the authenticated caller.
- The host registers shared plugins. Deploy adds optional workload bindings from component imports and declared connections.
- File routes register on the wasmCloud router per workload. No second listener.
- The route guest gets the route table in `wamn.http.routes` and deploy facts in `wamn.application`. The latter also carries the application coordinates, serving components, file policy and grant cache lifetime. Delete is its own operation type.
- `wamn-sql` owns `wamn:operation` and `wamn-engine` holds dispatch targets. Deploy refuses duplicate typed operation names and two versions of one host interface package. Palette exports are exempt from the operation name rule.
- The host registers `/wamn_control` and file and workflow paths per workload. It removes them on unbind. `sign-result` signs file results and leaves results without signed fields alone. Control builds its own route set directly.

## Connections

- A connection belongs to one project in one environment. Its tables live in the project database; the integration table folds into `connection`. Integration types and auth types stay in the system database.
- A connection's type and auth type never change.
- Per call: binding from the workload config, connection row and generation from the host authorization cache, then the secret and send. The host caches credential payloads separately and reads the Secret by name on a miss. No snapshot, no hashes, no candidate bindings.
- Connection creation and deploy refuse exclusive connections in cloud environments. The host holds no database guard across an external call.
- Each HTTP send builds a client and pins the selected peer address. The transport uses the invocation deadline and bounds headers and body. No in-flight semaphores.
- The environment file states each connection's version once.
- A physical resource belongs to one project: `(target_id, resource_key)` is unique within one project database.
- Connection and application service principals live in the project database. `platform.connection` records the connection's principal ID.
- The connection authority is fixed. A new authority needs a new connection. Create, edit and deploy refuse IPv6 literal authorities for Wasm connections. Cloud calls allow public addresses and refuse special addresses.
- Deploy refuses a connection bound to a different target, a name bound to two connections, or a name shared with another component.
- A Wasm integration is a sibling component. Deploy pins it by digest, sets `allowedHosts` to the connection authority and sets `wamn.integration` to its connection ID. wasmCloud links the labeled import.
- `wamn:integration/bound.begin` checks connection status and reads the cached generation and credential. A 401 or 403 invalidates the cached HTTP connection facts and triggers one fresh read.

## Workflows

- A workflow's first node is its entry node:
  - Request: a synchronous call, with a Response at the end of every terminating path
  - Schedule: a five-field cron, in UTC
  - Data change: CDC on one entity, with ops and an optional condition
  - Topic: a message on a subject, later work
  There is no Manual entry. "Run now" is an admin action for any workflow.
- A Request workflow is addressed by a path under `/api/<application>/flow/`. Calling it is a permission granted to roles, like an operation. It runs as the caller (invoker) by default, or as its `execution_role`. The Response sets a status and a JSON body. With no Response within 30 seconds, the call is refused with a timeout. A Request call admits a durable run at the start if any reachable node can write.
- The graph owns the trigger. The author step derives the index columns that the host queries.
- Workflows are authored, enabled and disabled through host routes of the application. Enabling a Data change workflow creates one NATS pull consumer filtered to its entity, and disabling deletes it. The service pulls from these consumers through stock `wasmcloud:nats`: it admits each run through the host, acks, then runs it. Events committed before the enable are skipped by their LSN.
- A workflow service component runs in every application workload. It walks graphs and calls nodes through the host.
  - A Data change or Schedule run starts after admission.
  - A Request run executes inline.
  - Every 30 seconds the service takes back runs with expired leases.
- Runs use a small host interface. The run SQL and its login stay in the host. Each node call carries the run's identity and role.
- A run executes on the live release. `runs.manifest_digest` and `runs.node_digests_json` go.
- Not built now (future work): state after each node, backoff, the approval waiting state.
- The graph format is 0.2. Only Request graphs use Response nodes. Request paths are unique among enabled workflows in an application. Publish reserves the Request workflow prefix.
- Application host routes also list workflows. Run now admits a durable run and returns its ID. The admin picks the caller or the workflow role.
- Enable checks the graph under the project database's environment lock. It reads the last source sequence before the enable LSN and starts the consumer after that sequence. It filters by entity and operation.
- The service polls schedules each minute. It refreshes consumers every 30 seconds and uses a 30-second pull wait. Schedule and Data change use the workflow principal with its configured role.
- Build the workflow service for `wasm32-wasip3`. It exports `wamn:workflow/entry` with `call` and `run`. Host dispatch selects the exact sibling and export. The default node policy allows three attempts.
- Deploy and rollback check enabled graphs under the environment lock. They refuse a release that removes a sibling or export in use. Environment apply and delete also hold the affected project locks.

## Data change in applications

- Application developers are the main users of Data change. A component declares in `wamn.k` the models and ops (`insert`, `update`, `delete`) it handles and the role it runs as, and exports a small wamn handler interface that takes the event as JSON.
- Deploy renders one durable JetStream subscription for each declared (model, op) into the workload config, aimed at the route guest `ingress`, which exports the stock `wasmcloud:nats/jetstream-handler`. The stock plugin creates the consumers. Only declared models get consumers. `ingress` checks the owner package and dispatches to the declared component through the operation path, as the `workflow` user with the declared role.
- One handler for each (owner package, model, op) in an application: deploy refuses two. A developer who wants two reactions puts them in one component. The host accepts `dispatch-data-change` only from `ingress`.
- Delivery is at least once, with no run row. The message id lets a handler skip repeats. A handler has the same host-transaction setting as an operation, on by default: one transaction for each delivery, committed when the handler returns ok and rolled back on an error. Turn it off for handlers that make long external calls.
- One NATS account per environment. The environment host group has one stock `wasmcloud-nats` entry. Each workload narrows its grants to its project's stream and subjects. Each workload has an unnamed binding on the default data connection. The workflow service and Data change handlers use named bindings.
- Each CDC reader has its own publish-only user in the environment account. The source stream keeps messages for 7 days and up to 256 MiB.
- Components export `wamn:handler@0.1.0/data-change` for Data change. The handler uses the component's own connection bindings. The subscription uses the `<application>-<entity>-<op>` queue group.
- The project chart creates the publication and CDC reader when deployed models need change capture. The recovery user can read only its project's stream and its own replies.

## Identity and login

- wamn keeps its own identity service for now, built so a SaaS provider can replace it.
- Host and control verify RFC 9068 JWTs with `jsonwebtoken` and a small JWKS fetcher (configured CA only, a fetch again on an unknown kid). They check `typ: at+jwt`, EdDSA, `kid`, and `iss`, `sub`, `aud`, `exp`, `iat`, `jti` and `client_id`. The host derives the JWKS URL and the audience from its issuer and org. Roles come from the project database.
- One token audience for each org (`urn:wamn:org:<org>`): one login gives one token for every environment of the org and for the control routes.
- Cookies and CSRF live in `wamn-web`; the host sees only bearer tokens.
- Session liveness is the provider's job: 15-minute tokens, no renewal after logout or disable.
- User management (create, invite, disable, list) goes through control routes and one adapter. Identity checks control and worker service account tokens through TokenReview and requires a registered org. Control checks the owner invitation Job's token through TokenReview (T5).
- API users are service principals with PATs. Service principals and their PATs live only in the project database (`platform."user"` type `service` with issuer `wamn`, and `platform.pat`). The host checks them with its project login.
- Identity serves environment logins, trusts `x-forwarded-for` only from configured proxy hops, and uses one Argon2 check per login. It encrypts its signing keys and runs two replicas. Control checks eligibility in the system database.
- Edge: no user login while it cannot reach identity (E1). Identity publishes the next key ahead of activation by the JWKS cache lifetime plus a margin. The edge design owns the offline window. Future options: longer edge sessions (E2), local login on the device (E3).
- The JWKS fetcher uses the response lifetime up to 300 seconds. The verifier allows 30 seconds of clock tolerance for `exp` and `iat` and requires a nonempty `sub`, `jti` and `client_id`. The host looks up the active project user by issuer and subject.
- Web proxies login, renewal and logout. The session and renewal cookies are `__Host-`, `Secure` and `HttpOnly`. The readable CSRF cookie is signed against the session `jti` and a random value. Unsafe methods require the signed cookie and matching header. Login, enrollment, recovery and reset require a same-origin `Origin`. The platform chart creates or reuses `wamn-web-csrf` and mounts it in web.
- The host checks the PAT prefix, hash, expiry, revocation and the principal's type and status. An org control login changes only principals whose memberships all belong to that org.

## Deploy

- `wamn deploy --application <application> --env <file>` takes a positional source directory or `--release <digest>`. The source form builds once, publishes once, then runs SQL and code as one operation. `publish <build dir>` pushes a finished build without building again and prints the top package digest. `schema apply` takes that digest through `--release`. The top package pins its bases.
- A deploy verb goes CLI → control service → one Job. The deployer needs only a wamn token. No nested Job; the document is a mounted file; the CLI poll has a limit.
- Charts define persistent resources, with one upgrade per chart. The six charts are infrastructure, platform (shared services and system database), org (control and the org boundary), environment (host group), project (cluster, database, publication, certificates and CDC reader) and application.
- One stock wasmCloud version for both runtime-operator charts and the runtime crates.
- Accepted risk: `environment/templates/host.yaml` includes the stock chart's private templates by path (`runtime/host-plugin-config.yaml`, `deployment.yaml`, `service.yaml`). A file move in a wasmCloud chart update breaks the render, and the chart-update review catches it.
- One environment file per environment (YAML with a JSON schema), with its projects and applications inside.
- Rollback is `helm rollback`. No pull-back after push. Check migration appends against the previous digest under the same tag. Pull each package once per operation and compile KCL once per build. The CLI validates input, control authorizes the target, and the worker checks the bound target and package. No `helm template` pre-check. Rollback alone reads prior Helm manifests and values. Project updates can use `--reuse-values` within the same release. One value each for the NATS, issuer and registry addresses.
- Components: stock `push_component`. Package artifact: two layers (`wamn.json`, migrations tar).
- Build: codegen's describe cache (`.wamn/describe/`, committed and checked current); a database only when SQL changes; skip unchanged builds; `wamn dev` applies new migrations. `build.json` goes (overlay builds hash the base files).
- Helm writes its release records and can create the initial platform namespace. The org installer creates the org Helm storage namespace, an optional pull Secret, the org SQL Job and the invitation Job. Control creates operation Jobs and their Job-owned input ConfigMaps. Environment apply creates NATS user credential Secrets.
- Org provisioning fills chart-created login and local OAuth Secrets. The org installer patches the control Deployment to reload credentials. Identity fills the NATS operator and runtime-operator Secrets. Control patches the chart-created project credential Secret. Environment deletion removes its entry from the control administration ConfigMap.
- Deploy applies SQL before code. Migrations must work with the previous code. Deploy never resets the database. WorkloadDeployment annotations carry the package digest and content revision. Rollback without a revision selects the newest earlier deploy revision.
- The environment file lists projects in apply order. Apply reserves the selected projects, configures NATS and starts the host chart upgrade. Then it applies each project chart and its SQL, and waits for the host. A rerun completes projects left reserved after a failure.
- The application and environment charts use the kstatus watcher. The project and org charts use bounded named waits in Rust. Wait for the project database before SQL and for the workload's `Ready=True` condition.
- A plain environment delete keeps the Cluster and ObjectStore. Reapply starts a new generation. Delete with `--data` removes kept data. Restore copies back into the live database at the same endpoint.
- Capture-gap recovery and close use operation Jobs. The project chart keeps the CDC reader paused while a gap is open.

## Install and configuration

| Layer | Tool | Owns |
| --- | --- | --- |
| Infrastructure (future) | OpenTofu, written by hand | Project, network and cluster. Writes `infra.values.yaml`. |
| Platform install | Helm values, one file per installation, with `values.schema.json` | Chart settings |
| Applications | KCL (`wamn.k`) | Metadata and policy, no types: routes and field labels (overrides on derived defaults), statement names, fetch classes and bind names, idempotency key fields |
| Environments | YAML environment file | Projects, applications, bindings, environment settings |
| Run-time data | SQL | Roles, ACLs, connections, app data |
| Credentials | Kubernetes Secrets, CloudNativePG managed roles | Passwords, keys, tokens |

- Install: one infrastructure release (operators and resource types first), then one platform release with identity and edge. Wait for CloudNativePG before installing the platform. Wait for rollouts and the system SQL Job. Identity makes its own first signing key. No superuser SQL from the developer machine.
- Each service reads one rendered settings file with its real inputs; constant flags become code defaults.
- `wamn platform deploy <values> --chart <bundle>` is the install entry point (Q1 a). `WAMN_RELEASE_CHART` can supply the bundle. `--only infrastructure|platform` selects one shared chart. Hosts update through `env apply`.
- `wamn org create <org> --owner <email>` creates an org (T2). It creates Helm storage and installs the org chart. The platform Job creates the org row and both system database logins and fills their Secrets. The installer restarts that org's control and runs the owner invitation Job.
- The host's settings file arrives through the stock runtime-operator `volumes`/`volumeMounts` and an extra `--settings` argument; the stock host arguments stay (Q3 a).
- Kind cluster and local registry creation is an `xtask` step. Provider differences are two values files. No key-parity check.
- The platform values schema checks the GCP project ID, zone and bucket names. The install sizes JetStream storage from `maxProjects`. The org SQL Job fills the org's `wamn-oauth` Secret when OAuth uses local encryption. The platform chart supplies identity's envelope settings.
- System migrations run only in the platform SQL Job. The platform seeds integration types in system SQL. Org control cannot publish them.
- Migration appliers stay, though `deploy/sql/migrations/system` and `project` are empty. System migrations need them.
- The environment file supplies project database sizes. The project chart uses `initdb` and creates the ObjectStore and ScheduledBackup when backup is enabled.
- Direct platform and project database logins use client certificates. Each project Cluster has its own CA, and the host mounts `tls.crt` and `tls.key`. Use namespaced issuers for trusted platform identities.
- The org control and apply logins use passwords outside Helm records. They use separate single-replica CloudNativePG Poolers in session mode. Control has a per-login limit of 8, apply has 16, and apply can wait 15 minutes. The chart derives the system connection limit from its budgets. The apply Job reuses its lock-holding connection.
- The host chart sets the HTTP listener and uses the configured NATS address. Environment apply writes the project's `registry.event_readers` row.

## Contracts, codegen, KCL

- The developer writes SQL migrations, the SQL file for each declared statement (checked by SQLx at build), hand-written WIT for each custom operation (input, result and errors), and `wamn.k`.
- `wamn.k` holds no types. It holds package and base pins, model overrides, routes and field labels, statement lists, idempotency key fields, connections and components. Routes default from operation names. Modbus retains its integer range check in `wamn.k`.
- Each statement entry holds its name, fetch class and bind names (K1). Types and nullability come from the WIT input. The operation and statement names determine the SQL path. An idempotency key names a field in the WIT input.
- A computed bind (a bind with no input field of the same name) takes its type from SQLx describe, as non-null. A renamed bind takes the name of its input field in the bind list. Binds are positional, so the SQL does not change.
- Result column nullability that SQLx cannot infer uses SQLx's own aliases in the SQL file: `"col!"` (not null) and `"col?"` (nullable). A result column type comes from a real SQL cast, not from a type-override alias. Describe types a JSON aggregate column as JSONB, and the hand-written handler converts it to its WIT result type.
- Generated: model WIT from the schema (with named `id` and `revision` types), Rust bindings, and statement result types from SQLx describe. Value lists use WIT enums. Use named WIT aliases for `uuid`, `numeric` and `timestamp`. Custom-operation WIT is not generated.
- The host writes WIT names to JSON in snake_case (record fields, error codes, enum cases, flags) and reads them back the same way.
- Authored WIT files carry no package line. Only the generated `package.wit` declares the package and its version, and the development-build version rewrite touches only that file.
- Generate Rust with `quote` and format it with `prettyplease`. Start the formatted Rust with a fixed `// @generated` line. Spell keywords and dotted names as wit-bindgen does.
- Generate WIT with `wit-encoder =0.254.2`. A labeled world import is one identifier. It is the only WIT string fragment and carries a comment.
- Generate each application's `openapi.json` with typed OpenAPI 3.1 structs from `oas3`. Generate control's OpenAPI from the contract JSON files. Keep those files and `contract_files.rs`.
- Pin `@hey-api/openapi-ts` and use only its type, SDK and fetch-client plugins. Use the `@wamn/web-runtime` transport as fetch, and call `toWire` and `reviveOutcome` in interceptors. The client serializer JSON-encodes query values as `readQuery.ts` does. Reads use `readQuery` and `readCache`. The server keeps its query format.
- A pnpm script in `apps/platform_ui` runs the generator from the existing build. It writes `src/generated/{wamn_control,wamn_control_org}`. Keep the examples under `web/wfe` as they are.
- A `cargo xtask` command writes control's OpenAPI files to `apps/target/wamn/platform_ui/`. `cargo xtask build-platform` calls the same xtask function directly before its pnpm build. The pnpm script calls the xtask command only for local UI work. `crates/codegen/build.rs` writes no files for other tools.
- SQL templates stay.
- KCL holds overrides on schema-derived defaults, is never generated, and refuses names that match nothing in the schema or WIT.
- Keep matching Rust or SQL checks where the runtime accepts the same input.
- JSONata transforms declare input and output schemas, with no examples. The workflow must validate the output against its schema. It currently checks only that the output is JSON.
- Sync: only the server-side change feed on list reads.

## Crates and names

- Renames: `wamn-ctl` → `wamn-cli`; `wamn-control-provision` → `wamn-provision`; `wamn-compiler` → `wamn-codegen`; `wamn-connections` → `wamn-connection`; `wamn-web-front` → `wamn-web`; `wamn-platform-identity` → `wamn-identity`; identity service package → `wamn-identity-service` in `services/identity-service`.
- Merges: artifact crates → `wamn-oci`; `wamn-postgres-statements` + `wamn-data-access` → `wamn-guest`; build + composer → `crates/build`. `wamn-release-control` goes. `wamn-role` owns Postgres role names. `wamn-event` owns shared event types.
- Splits: `wamn-execution-host` goes: file route → `wamn-blobstore`, number counter → `wamn-sql`, OAuth callback → `wamn-http`, credential cleanup → `wamn-connection`; control routes, environment status and connection health → control service. Application routes → host. `crates/identity` keeps passwords, sessions, principals, org membership and control eligibility, plus the project service-principal and PAT SQL; connection administration → `wamn-connection`; project database rows, configuration rows and environment fill → `wamn-environment`. The `wamn.json` model moves to `wamn-catalog`.
- `wamn-connection-type` owns the connection types (portable connection definitions and run-time tags), below `wamn-engine` and `wamn-authority`. `wamn-oci-reference` (`crates/oci/reference`) owns artifact references and their validation, with no network code, for `wamn-oci` and `wamn-edge`.
- `wamn-postgres` uses mounted credentials and has no `wamn-kubernetes` dependency. `wamn-kubernetes` has no `wamn-registry` or `wamn-catalog` dependency. In 3.7b, remove the `wamn-oci` dependency on `wamn-kubernetes` (use `oci-client`).
- Docs: this doc lives at `docs/plan/wamn-restructure.md`. Everything else in `docs/plan` goes, and so do `docs/history` and `docs/audit`; no stray files at the top of `docs/`.
- The CLI binary is `wamn`, the identity service binary is `wamn-identity`, and the web binary is `wamn-web`.

## Tests

- Three levels: unit (no services); service tests, marked `#[ignore]`, one per contract; journey tests on kind through `wamn deploy`, observed with a read-only login. Service tests call product library code with real Postgres and NATS containers. wamn-host runs only on Kubernetes, so its contracts are journey tests on kind. The edge (`services/edge`) has no Kubernetes code and keeps its service tests.
- Stock `cargo test` and `cargo test -- --ignored`; Step 3.8 restores the `xtask` command that maps changed files to packages. Fixture applications are built in one step before the run; a test whose input is missing fails. Each test that needs Postgres or NATS starts its own container from the production digest through one helper in `wamn-test-infrastructure`. No shared server, no locks, no `#[sqlx::test]`. Timing tests use `tokio`'s paused clock. Step 3.8 removes `test-util` features.
- One dev-only crate, `wamn-test-infrastructure`. A helper used by one crate only lives in that crate's `tests/common/`. Grant refusal tests live in `wamn-test-sql`.
- During phases 1 and 2 testing is minimal: a step is checked by `cargo build` and `cargo clippy` only; tests that stop compiling are deleted; no new tests until step 3.8.
- Standard epic checks include build, clippy, `cargo xtask build-platform`, applying the schema to a fresh local Postgres, and checking the largest application fixture's Helm release records against the 1 MiB limit.
- Before a kind install, build platform images and check that every rendered image resolves. Use `kubectl apply --dry-run=server` with the CRDs installed. Run each rendered WAMN container command with `--help` appended and check the exit code.
- Before an epic merges, the short smoke run installs the platform, creates an org, applies an environment and deploys an application. Call the application and control through web. At each step, collect all pod failures before stopping. Local kind has standing approval. Other clusters need owner approval.
- For a wasmCloud chart update, render the environment and platform charts with both versions and the same values. Review the object diff before merge.

## Order of work

Phase 1 deletion, phase 2 restructure (no behaviour change), phase 3 code editing. All deletion up front; the project may be non-functional across epics; every step compiles. One epic is fully scoped at a time, the next after review; work inside an epic runs in parallel.

Phase 3 steps, in dependency order: 3.1 database model; 3.2 request path (workloads, connections, files, integrations and application host routes); 3.3 identity; 3.4 workflow service; 3.5 deploy; 3.6 isolation, then install; 3.7 codegen; 3.7b crate boundaries; 3.8 tests.

## Epic 14: step 3.8, service tests

Rule: one service test per contract. Service tests call product library code with real Postgres and NATS containers. The host is not started in tests. No new product behaviour.

Issues:

14.0 Plan, doc only, done first. Record the rulings and the contract list below. After 14.0, the other issues run in parallel.

14.1 Grant tests in `wamn-test-sql`. Cross-org RLS: a second org (globex) from the public `create_org_logins`, the tables from `pg_policies` and the statements from `has_*_privilege`, with 0 rows, 42501 or 23514 for each acme login and a positive control for each probe. REFERENCES: `has_table_privilege` and `has_any_column_privilege` return no REFERENCES rows for each restricted login. Functions: the full catalog query, with aggregates, trigger functions and pseudo-typed arguments. Extension functions are listed apart, and the test checks that the installed extensions are exactly the production list. PUBLIC EXECUTE is revoked on `registry.retire_project_env`, `wamn_run.guard_event_lineage_immutable` and `wamn_run.guard_run_admission_pins_immutable`. The four tests that build an app login per generation by hand are fixed here if they disagree with the five fixed logins.

14.2 NATS isolation service tests. A NATS container from the production digest, configured from the rendered platform chart. Accounts and users are signed by `wamn_identity::nats::bootstrap` and the other public signing code, with fresh keys per test. No host, no identity service and no copy of product code. Tests: one account per environment and no cross-account traffic, the scheduler user rights (subscribe and publish lists), and the account limits.

14.3 Tests with Postgres only, and unit tests. Session identity and the write log, only through production functions that take an open connection. `login_principal` resolution, the credential tables with no app or query grant, and the refusal of a removed sibling. Unit tests: the `plan.rs::roles` refusals, `valid_route_host`, the grant cache, `access_token.rs`, `browser_session.rs`, `result_signing.rs` and the 256 MiB account limit. The stays-alive half of a NATS outage: a paused-clock unit test on `lifecycle.rs::watch_liveness`. The database installers move from `test-support/sql/tests/common/` into `wamn-test-infrastructure`.

14.4 Tooling. `cargo xtask helm-size` checks every package under `apps/*/wamn.k` and fails when a release record is over 1 MiB. The manifest step moves into one public `wamn-publish` function that xtask calls. The guests in the apps workspace use the `cfg(target_arch = "wasm32")` pattern of `ingress`, so the host build has no link failures.

Contract list (14.0), by tier:
- Service, Postgres: cross-org RLS, REFERENCES and function EXECUTE (14.1). Session identity and the write log through functions that take an open connection, `login_principal` resolution, the credential tables, and the refusal of a removed sibling (14.3).
- Service, NATS: one account per environment, no cross-account traffic, the scheduler user rights and the account limits (14.2).
- Unit: the `plan.rs::roles` refusals, `valid_route_host`, the grant cache, `access_token.rs`, `browser_session.rs`, `result_signing.rs`, the 256 MiB account limit and `watch_liveness` (14.3).
- Tests that exist and stay: first insert wins (`test-support/sql/tests/project.rs::app_login_refusals`), SECURITY DEFINER (`crates/provision/tests/platform_logins_grants.rs`), no `project_env_key` (`crates/postgres/tests/project_schema.rs`), address pinning (`crates/authority/tests/connection_authority.rs`), web retry (`services/web/src/rollout_tests.rs`), cron (`services/host/src/host/workflow/schedule.rs`) and the drain order (`services/host/src/host/commands.rs`).
- Journey: every contract of wamn-host, and the rest of Part 1. They come in Epic 15.

End of the epic: the standard checks (with `cargo xtask helm-size`), an apps host build with no link failures, `cargo xtask prepare-tests`, `cargo test --workspace`, the ignored run on a machine with Docker, the preflight and the short smoke run.

Next: Epic 15 (step 3.8, third part): the journey harness (`cargo xtask journey-cluster`, a read-only observer login) and the journey tests on kind. The host contracts move there: the host's probes, signals, drain and NATS outage recovery; another project environment's credential is refused and never served; the request path, connection calls, workflows and Data change; the grant cache; result signing; the cold parse, the operation in the transaction and the write log claims; and any other session identity or write log contract that has no production function taking an open connection.

## Open items

- The local mutex and Postgres advisory lock for exclusive connection calls are not built. The existing advisory lock in `sql/session_identity.rs::lock_file` serves file operations.
- The HTTP client cache key by invocation scope, target, connection ID and generation is not built.
- `registry.connection_principal` is not built. Project connection rows carry the principal ID.
- Deferred: the edge device after the restructure; the four workflow behaviours.
- Journey tests on kind, deferred from the first kind run: rollback, `env delete`, apply again, and `env delete --data` after a plain delete; the Data change check for `client_acme_receiving` (K24); the 10.13 per-pod scheduler users beyond host start; the Pooler per-login limits and waits under load; that kind's CNI enforces NetworkPolicy, and that an environment's host cannot reach another environment's host, Postgres or Services.
- Health probes for wasm integration types: `connection_test` cannot run integration code outside an application workload. To be designed.
- Edge device design: plant-network devices such as Modbus run at the edge, and the edge runtime gets its own address rule. A cloud integration that needs a private address (a customer network linked by VPN) is decided when a real case appears.
- Native integration types (for example the NetSuite ODBC driver) are native Rust host plugins compiled into the wamn host, as wasmCloud advises for drivers with no WASI binding. Decide this when the first one is built.
- Host component plugins (`host-component-plugins`) suit platform-wide wasm capabilities with fixed bindings. They are not used for connection-bound integrations.
- Edge device design: how exclusive connections are enforced at the edge (inference: one edge runtime for each site, so a local mutex may be enough).
- A call limit for each application, if a real case appears: either a per-host semaphore named as per-host, or a Postgres design decided with real numbers.
- 3.6: nothing in the install fills the `wamn-control-administration-<org>` map that control reads its project logins from.
- Topic entry node: after 3.4. Topics use stock `wasmcloud:nats`: application components publish within their workload's grants, and a Topic workflow pulls from its own consumer like Data change.
- Edge device: the edge runs the cluster `ingress.wasm` and turns every import it does not provide into a trap (`define_unknown_imports_as_traps` in `services/edge/src/serve.rs`). Risk: a capability missing on the edge fails on the first call, not at load. The stock `wasmcloud_nats` plugin cannot fill the NATS imports on the edge, because it needs a `servers` value and connects at bind. The edge design decides between a real NATS server on the edge (see the next item) and an HTTP-only build of `ingress`.
- Edge device, if the edge needs NATS: use the stock NATS way. A leaf node on the device with its own JetStream domain, one per edge environment. Mirror cloud streams to the edge, and source edge streams into the cloud. The edge then binds the stock `wasmcloud_nats` plugin to the local server, which also fills `ingress`'s NATS imports, and the guest code does not change. Not ruled; the edge design decides.
- After 3.6, first kind run: verify the kstatus waits on the `application` and `environment` charts, and the explicit Rust waits on the `project` and `org` charts (Certificate, Database connection, the CDC reader's wait for its replication slot, CDC reader rollout, Cluster).
- After 3.6, first kind run: The two Poolers: session mode with the org logins, the auth query, waiting at the per-login limit, and the 15-minute apply wait.
- After 3.6, first kind run: Pooler certificate renewal: renew one Pooler certificate, restart the CloudNativePG operator to force a reconcile, and confirm that PgBouncer serves the new certificate.
- When Helm ships readiness annotations (HIP 25, a draft today; Helm v4.2.2 has none) and no waited chart renders a Secret, revisit Helm waits for the `project` and `org` charts.
- After the first kind run: hand-written OpenTofu for GCP (A1, K10) and `infra.values.yaml`. Epic 10 targets kind, with a GCP values file checked by `values.schema.json`.
- After the first kind run: three NATS pods with three-replica streams.
- Move the NATS server from 2.12.15 to the vendored chart's 2.15.2. Decide with its own kind run.
- Later option: custom domains for applications through a verified-domain list (E).
- Later option: integration types owned by one org (Q4).
- Later option: users in more than one org, through an invitation the user accepts (grant ruling, change 1).
- Next CloudNativePG upgrade: move the Pooler's certificate lookup to `.spec.pgbouncer` `clientTLSSecret`, `clientCASecret`, `serverTLSSecret` and `serverCASecret` (added in 1.28), away from the deprecated field.
- Later option: registry access for each org. Today every org uses the same registry credential (a copied file on kind, the node's token on GCP), so an org's hosts could pull another org's packages. For example, one repository per org, with read access only to its own.
- Before any install that must survive an upgrade: system schema migrations. The system schema loads only when it is absent (`provision_system.rs:242`), and no system migration exists, so a schema change today needs a new cluster.
- Before running more than one real customer: a fleet rollout command, run after `wamn platform deploy`. It applies `wamn org create` again for every org in `registry.orgs`, then `wamn env apply` for every environment. It records each environment's application bundle digest, so version skew is visible. It is resumable, and stops an org's remaining environments at their first failure. Reason: org and environment releases pick up a platform, `host.yaml` or runtime-operator change only when they are applied again, and nothing does that for all of them today. This replaces the earlier open item about `wamn platform deploy` applying `wamn org create` again for every org. The rollout detects and recovers a release left in a pending state by a run that died partway.
- The org owner's access token lasts about 15 minutes, which can be shorter than a long `env apply` run by hand.
- Before the first production customer: split scheduler NATS from data NATS with the stock `schedulerUrl` and `dataUrl`, so no tenant can starve the control plane. NATS has no CPU or rate limits per account.
- Before the first production customer: a PriorityClass for production environments, so production outranks dev. The environment values already accept `resources`, `nodeSelector`, `tolerations` and `affinity`.
- `pg_walstream` runs from the fork `dkkloimwieder/pg-walstream` with two patches (typed keepalive, client certificate). The fork is wamn's own.
- Before any real customer (second review, 2026-11-20, of 02c1a085):
  - Pod security: `pod-security.kubernetes.io/enforce: restricted` on org namespaces, a `securityContext` on every wamn workload, and a non-root `USER` in the Dockerfile. Today a privileged pod in an org namespace would reach the node.
  - The system database: a second instance, an ObjectStore and a ScheduledBackup. It holds the NATS operator and account keys and all users.
  - Backups: one credential per org, scoped to that org's prefix or bucket. Today one credential reaches every org's backups.
  - The org worker's rights: narrow its Role. Review NetworkPolicy, Role and all-Secret rights.
  - Ingress policies in `platform` for NATS, the system database and the Poolers.
  - Web reads ConfigMaps through a Role per org namespace, not a ClusterRole.
  - NATS users for Jobs expire, and a revocation list exists.
  - CAs come from cert-manager everywhere; no Helm `genCA` keys in release records.
  - `platform.tx_identity` stamps: either state them as tamper-evident only within one project database, or bind the identity through a definer function.
  - Org quotas count CPU and memory, with a LimitRange, and requests on control and the CDC reader.
- Possible future exploration, not scheduled: a wamn hey-api plugin.
- `wamn eject component` returns only if a component generator is built.
- Rebuild the Operations and Roles views of platform_ui when a route serves the release profile.
- Postgres array columns have no WIT type. Decide when a second real case appears.
- A WIT package whose name is a Rust keyword, such as `type`: pinned wit-bindgen writes the module name bare, which does not compile. Decide whether validation refuses such names or WIT generation derives a safe package name.
- Variant case names go on the wire in kebab-case (462 cases today). Record fields, error codes, enum cases and flags use snake_case. Decide whether variants follow the same rule.
- `apps/platform_ui/src/host-client.ts` reads the OpenAPI document at run time to build routes and camelCase field maps, and makes a new client on every write. This conflicts with the rule that nothing in the web UI is driven by descriptors at run time. Cause: camelCase TypeScript members against the snake_case wire. Options for the next scope: (a) the UI uses the wire names, which removes the field maps and `toWire` and reverses the camelCase ruling; (b) generate the field maps at build time (the hey-api plugin exploration).
- Typed control inputs, then OpenAPI derived with `schemars`.
- The JSONata output check: P146 says output is validated at run time, but `walk.rs::invoke` only checks that the output is JSON.
- Where `revocation_seconds` lives: P43 and P336 set the grant cache lifetime per project, but the environment file has one top-level `revocation_seconds`.
- `HostArgs` (`services/host/src/host.rs`) is a hand-kept copy of the stock host flags. Check the wash-runtime 2.12.2 interface before changing it.
- The environment document has two owners: the ConfigMap from `project/templates/operations.yaml` and the registry rows. Design one owner.
- Org, project and environment are written five times in workload config (`release_chart/plan.rs::roles`, the `wamn.project_env_key` annotation in `workloads.yaml`) and never compared. Design one source.
- "wiring" still names workflows (`WiringDocument`, `runs.wiring_id`, `catalog.workflows.wiring_hash`; about 213 hits). Rename with a schema change.
- After the first kind run:
  - Charts: `enablePDB: false` for single-instance project clusters; delete the dropped `affinity` and `topologySpreadConstraints` host values, or render them.
- Crate graph review (2026-11-21). For one crate-graph epic after Epic 15.
  - The grant SQL leaves `wamn-identity` for `wamn-authentication`. The host uses `wamn_identity::application::OPERATION_GRANT_LOCK_SQL` and `GrantError` (`services/host/src/host/application_route.rs:21-80`). Authorization belongs with authorization, so identity stays replaceable by a SaaS provider.
  - The cloud metadata client (`wamn_oci::package_artifact::metadata::Metadata`) leaves `wamn-oci`. `wamn-envelope` uses it in `pod_identity.rs:9`, and that pulls the registry client into connection, authentication, operation, http, blobstore and the host.
  - Rename `test-support/resources` to `test-support/infrastructure`, to match the crate name `wamn-test-infrastructure`.
  - `wamn-postgres` splits into two crates instead of its `host` feature (engine, registry and pool code). Cargo unifies features within one build, so `wamn-operation` and `wamn-authentication` turn it on for every user.
  - The plain shared types (for example `application_config.rs` and `native_routes.rs`) leave `wamn-engine`, so control-plane crates (`wamn-environment`, `wamn-connection`, `wamn-control`) stop linking wasmtime and wash-runtime.
  - `wamn-environment` (24 wamn dependencies, 9,184 lines) splits by job: release chart planning, connection administration, environment lifecycle, package apply, history pruning.
  - `wiring.rs` (the workflow graph) leaves `wamn-routes`, with the "wiring" rename that is already an Open item.
  - Record the five dev-dependency cycles (for example `wamn-identity` with `wamn-environment`), and remove them where the move above allows.
  - Extend the dependency lint (`repo-lint-dependencies` in `xtask/src/tasks.rs`) past its three crates.
  - `wamn-registry` and `wamn-package` (30 dependents each) link sqlx with postgres, so `wamn-web`, `wamn-edge`, `wamn-event` and `wamn-broker` compile a database driver. The `sqlx::Type` impls must stay with their types (orphan rule), so a fix needs a feature or wrapper types. Decide with the edge device design, and add sqlx to the edge dependency lint then.
  - "ProjectDatabase lives in wamn-connection": no type with that name exists at 07a737a88. Record it only when the exact type and file are known.

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
- Routes with no CLI verb: `application.stop`.
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

**Ruled (D, 2026-11-16): Epic 3 scout questions.** 21 questions answered (see the Epic 3 section in revision `375315958`). Facts behind the changed answers: project databases share the org cluster, so logins span all of them (11); `control_principal` is the row policy of `identity.principals` (Q1, Q3); `configuration.rs` connection writes have no caller with rows (10); `workflow_user_id()` is a fixed constant (17).

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

**Ruled (D, 2026-11-18): Epic 8 read-back, CSRF key and project selection.** `platform update` keeps its `helm get values` read-back until the 3.6 `platform` chart; every other read-back and the `helm template` pre-check go, and the environment Jobs take platform facts from control's settings file. The edge chart renders `wamn-web-csrf` with `lookup`, and the Rust writer goes. `status` covers every project unless an application narrows it; `platform schema --target project` covers every project in file order; `schema reset` requires `--project`, because it clears the whole project database.

**Ruled (D, 2026-11-18): Epic 8 settings facts.** The control settings generator derives the three artifact bases and `insecure_registry` from the one registry value, with the shared loopback rule; the environment Jobs read them from control's settings, and their read-back goes. This also fixes control settings that lacked required fields. Org apply builds the `wamn_system` cluster entry from the org ID, and its read-back of the previous org release goes.

**Ruled (D, 2026-11-18): capture-gap recovery and control's login.** Recover and close become operation Jobs that set the project chart's `pause` value, and the values builder keeps the reader paused while a gap is open, so a deploy cannot restart it early; the routes change from synchronous calls to operations. Control gets its own least-privilege system-database login, a member of `wamn_control`, under org row security; `wamn_apply` is not used for control's request path.

**Ruled (D, 2026-11-18): env apply order.** Every selected project's registry row is reserved first, so the org chart is upgraded once with every project's logins. The host chart upgrade is submitted before the project loop and waited on after it; the first project waits for the broker certificate with a bounded wait. A failure leaves later projects reserved and unconfigured, which a rerun completes.

**Ruled (D, 2026-11-18): wasmCloud v2.12.2.** wamn moves to v2.12.2 in one commit after Epic 8 merges. Nothing in it changes a ruling; #5932 (structural type imports) does not help the edge, whose NATS imports are functions and resources.

**Ruled (D, 2026-11-18): rollback reads and numbers.** The worker reads the target revision's package digest from Helm for the workflow check, as a recorded exception for rollback only. The revision annotation names the revision whose content is live, so it stays N after a rollback to N. A rollback with no revision goes to the newest earlier revision that was a deploy, read from each candidate's rendered annotation, so a second rollback goes further back instead of forward.

**Ruled (D, 2026-11-18): capture-gap recovery credential.** A per-project recovery user in the environment account may only get stream info and messages from its own project's stream, and receives replies only on its own inbox prefix. The CDC reader stays publish-only, and control's unwired `event_observer` setting goes.

**Ruled (D, 2026-11-18): scoped inboxes for every NATS user.** The CDC reader user, and any other user that subscribed to `_INBOX.>`, gets its own inbox prefix, so no user can read replies meant for another user in its account. This corrects the earlier note that only the recovery user lacked one.

**Ruled (D, 2026-11-18): the environment user and inbox scoping.** The environment user, which holds full rights in its account, is exempt from the scoped-inbox rule, because a scope would protect nothing. Every restricted user is scoped. Splitting the environment user into a restricted workload user and an admin user is a 3.6 item.

**Ruled (D, 2026-11-18): Epic 8 review.** Accepted at `29bb71afe`. `release.retain` goes: with no release artifact there is nothing to retain. wamn does not use Helm's legacy waiter, which lane 8.2 chose because the operation Role grants only name-scoped rights and the watcher needs list and watch across the namespace. Waits are chosen per chart, so no wait adds a right on Secrets (Helm's own release storage still needs Secret list in the release namespace, a recorded exception): the `application` and `environment` charts, which render no Secret, use the kstatus watcher; the `project` and `org` charts, which do, use no Helm wait and bounded name-scoped waits in Rust. Deploy and env apply wait for the project database to accept connections before SQL. Helm v4.2.2 has no readiness annotations; the earlier instruction to add them was wrong.

**Ruled (D, 2026-11-18): CDC reader start order.** The reader waits up to 120 seconds for its own replication slot, which the worker creates only after the Publication is applied. So the reader starts after its Publication without reading the Publication object and without a second chart upgrade.

**Ruled (D, 2026-11-18): Secret watch scope.** No Role or ClusterRole in wamn's own charts grants `watch` on Secrets. `platform update` uses no Helm wait on the `installation` chart, which renders a Secret, and waits in Rust for the rollout of the Deployment it changed. Vendored operator charts keep their upstream RBAC; the rule does not cover them.

**Ruled (D, 2026-11-18): CloudNativePG 1.30.2.** wamn moves from operator 1.29.1 (series past end of life) to 1.30.2 (chart 0.29.2), the Barman Cloud plugin chart to 0.9.0, and the PostgreSQL image pin to 18.7, for the 1.30 security fixes. `DatabaseRole` (new in 1.30) is a 3.6 item.

**Ruled (D, 2026-11-18): step 3.6 split.** Epic 9 does isolation (W2, Q, the org chart of K7b, Helm release storage, R, connection secrets, P5's `wamn-postgres` edge); Epic 10 does install (the `platform` chart, J, K, K4, K5, L1, the bootstrap items, NATS volume sizing); the first kind run follows Epic 10. With one Cluster per project database, each project Cluster declares its own logins in its managed-role list, so `DatabaseRole` is not needed. Hand-written OpenTofu waits until after the first kind run.

**Ruled (D, 2026-11-18): Epic 9 scout answers.** Fixed project logins with derived connection limits; environment-level database sizing; one ObjectStore and ScheduledBackup per project; `initdb` creates the database; Cluster and ObjectStore kept unless `--data`; system login Secrets rendered in `platform` and each org. Default-deny NetworkPolicy with named allow rules, public egress only on declared integration ports; no `escalate`; `wamn-credential-<project>` created empty by the chart and written by control under server-side apply. Helm releases stored in `wamn-helm-<org>` with server-side apply; watched charts keep list and watch on non-Secret kinds. Named NATS bindings with inbox prefixes for the workflow service and `ingress`; one NATS accounts Secret per org, included by the server config, and wamn's writer with name-scoped RBAC keeps each org to its own accounts; NATS itself does not enforce this. Connection secrets stay an API read by name, with a stronger cache. `wamn-postgres` mounts only `username`, `password` and `ca.crt`. D6 and D7 go to Epic 10.

**Ruled (D, 2026-11-18): Epic 9 install entry, platform update, restore, NATS names and Helm accounts.** The org chart's install entry point is Epic 10. The control Job path for `platform update` goes now, with its read-back and the org worker's wide rights in `platform`; `--only` returns with the `platform` chart. Restore is copy-back only into the live database, whose endpoint never changes. NATS does not enforce per-org account names; wamn's writer and name-scoped RBAC do. Jobs that run Helm use the org worker account, because Helm release records hold login passwords and only the org's worker may list them; client-certificate logins, which would remove passwords from the records, are an Epic 10 item.

**Ruled (D, 2026-11-18): recovery after total loss.** A lost project Cluster is recovered as a new generation: restore to `<cluster>-restore`, dump, `env delete --data`, `env apply`, load the dump. Under `--data` only, env delete accepts an unreachable project database and reports it. This corrects the earlier full-loss text, which could not work: env apply reads the live database first, and a same-generation Cluster would archive into the old prefix.

**Ruled (D, 2026-11-18): tenancy and Epic 10 install.**
- T1. Shared platform components (infrastructure operators, NATS, identity, edge, web) are trusted, run across all namespaces and know orgs only as run-time data. Adding an org changes no shared config and restarts no shared pod. Least privilege applies to the actors inside each org: org workers, hosts, control and users.
- T2. Orgs are data in the system Postgres (`registry.orgs`). No chart values list orgs. `wamn org create <org> --owner <email>` creates an org.
- T3. NATS runs in operator (JWT) mode with the full resolver. Accounts and users are signed JWTs, pushed to the running server. One account per environment plus `PLATFORM`. JetStream limits and user permissions, including each user's inbox prefix, live in the JWTs. Clients connect with creds files; TLS stays, for encryption only.
- T4. wasmCloud operator: `watchNamespaces` is empty (all namespaces). `-host-namespaces` stays fixed and never lists org namespaces, so the operator never restarts for a new org. Cost: no host-pod finalizer, so a gone host leaves the application endpoints only after heartbeats time out (about 1 to 2 minutes). The web proxy retries when a connection fails, with a connect timeout of about 1 s; nothing reached the host, so this is safe for every request. No upstream change, ever.
- T5. Identity accepts `CN=control-<org>` for any org in `registry.orgs`. The `controlSubject` value goes. Replaced on 2026-11-19 by the service account token ruling.
- Q1 a: `wamn platform deploy <values>` is the install entry point. It runs the infrastructure chart, then the platform chart, with one wait at the end.
- Q3 a: each service reads one settings file. The host's file arrives through the stock runtime-operator `volumes`/`volumeMounts` and an extra `--settings` argument; the stock host arguments stay.
- Q4 a: database logins by client certificate are in Epic 10.
- Q5 a: `--only infrastructure|platform` picks which charts to upgrade. Host images move through `env apply`. No read-back.
- Q6: the GCP project ID, zone and bucket checks become `pattern` rules in `values.schema.json`. The NATS volume (JetStream plus resolver) is sized from a `maxProjects` value, and `env apply` refuses a project once the total would go over it. Three NATS pods come after the first kind run.
- Overturned from Epic 9: the per-org NATS accounts Secret, its include mounts and `natsAccountOrgs`; the org worker's access to that Secret; NATS users mapped from certificates (`verify_and_map`); the Secret writer in `wamn_broker::authorization`.

**Ruled (D, 2026-11-19): Epic 10 scout answers.**
- A. Identity signs NATS accounts and user JWTs. The caller creates each user's key pair and sends only the public key; user private keys never leave the org. Identity signs only for the org whose CA the caller's certificate chains to (G). Identity becomes a NATS client, so that it can push accounts. The caller is identified by its service account token (see the service account token ruling), not by a certificate.
- B. The NATS root key and the operator signing key live in identity's encrypted store in the system Postgres; daily signing uses the signing key. The public operator JWT and system account JWT go into one Secret in `platform`, which the chart creates empty and identity fills. NATS mounts that Secret once, through `$include`, and needs no private key.
- C. `nkeys` plus wamn's own claim types, written with serde, in the format of the NATS Go reference. The first kind run checks that the server accepts them.
- D. `org create` does nothing in NATS. `env apply` creates each environment's account.
- E. The first owner comes from a control operation that needs a one-time token and works only while the org has no owner. `org create` stores the token's hash in `registry.orgs`; the invitation Job reads the token from a Secret in the org namespace. Changed by the service account token ruling.
- F. Dropped: no upstream work in Epic 10.
- G. No ClusterIssuer signs an identity that NATS, identity or Postgres trusts; platform issuers are namespaced in `platform`. `org create` creates a CA for each org, with a namespaced Issuer in the org namespace, and stores its public certificate in `registry.orgs`. Identity accepts `CN=control-<org>` only when the certificate chains to that org's CA. Platform logins on the system database and all project database logins use certificates. The org's control login on the system database keeps a password, which `org create` writes and which is never in a Helm record; this narrows Q4 a. A ValidatingAdmissionPolicy refuses Certificates in org namespaces that point at a ClusterIssuer. The org worker loses `ingresses`. The edge's ACME issuer becomes namespaced. Reason: today the org worker can create a Certificate from `ClusterIssuer/wasmcloud-ca`, which NATS maps to users by name, so it can connect to NATS as `wasmcloud-operator`. Changed by the service account token ruling.

**Ruled (D, 2026-11-19): workload identity by service account token.** This replaces part of A, E, G and T5.
- Identity, for NATS signing, and control, for the first owner, verify the caller's projected service account token through the Kubernetes TokenReview API. The token's audience is the receiving service. The token's namespace gives the org, which must exist in `registry.orgs`. The service account name gives the caller's role.
- Prior art: Vault's Kubernetes auth method binds roles to service account names and namespaces, and checks tokens with TokenReview.
- Removed: the CA for each org, its certificate in `registry.orgs`, the ValidatingAdmissionPolicy, the first-owner token and its hash, and the `CN=control-<org>` checks.
- Kept from G: no ClusterIssuer signs an identity that NATS, identity or Postgres trusts; platform issuers are namespaced in `platform`; the org worker loses `ingresses`; the edge's ACME issuer is namespaced; the org's control login on the system database keeps a password, written by `org create` and never in a Helm record.
- E becomes: the invitation Job runs as its own service account in the org namespace. Control accepts the first-owner operation only from that service account, and only while the org has no owner.
- T5 becomes: identity accepts control by its service account token, not by certificate subject.
- Cost: identity and control need the right to create `tokenreviews`, a standard cluster permission. An org's worker can run pods as any service account in its own namespace, which stays inside the org.

**Ruled (D, 2026-11-19): org control login and host scheduler users.**
- The org's control login: `org create` starts one Job in `platform`. The Job uses the same image and binary as the 10.3 install SQL Job, with a different subcommand. The Job generates the password in the cluster and runs `CREATE ROLE ... LOGIN PASSWORD ... CONNECTION LIMIT 4 IN ROLE wamn_control`. It writes the `registry.orgs` row and patches the password into the org's basic-auth Secret. The org chart creates that Secret empty, because Kubernetes RBAC cannot limit `create` to one resource name. The org chart grants the Job's service account `get`, `update` and `patch` on that one Secret name only. The password never leaves the cluster and is never in a Helm record. The system Cluster spec never changes when an org is added. The role leaves the `installation` chart's `systemDatabase.roles` and `platform-install/src/roles.rs::control`.
- Host scheduler users: each host group gets its own PLATFORM user with its own inbox prefix and the same subject rights as `wasmcloud-runtime` today. This is a stated exception to T1. Reason: host ids are random, single-token subject parts, so no NATS permission can separate one org's hosts from another's. wasmCloud's threat model treats the host process, and anyone who can publish on `runtime.host.>`, as trusted. Later option: one NATS user per host pod, with the host id set from the pod name and a JWT from identity.

**Ruled (D, 2026-11-19): Epic 10 logins, CAs and host names.**
- A. System migrations run only in `platform`, in the 10.3 SQL Job. No org namespace holds the system migration login. Each org gets its own apply login, made by the org SQL Job in `platform` in the same way as the org's control login: the password is created in the cluster and patched into a Secret that the org chart creates empty. The shared `wamn_apply` login goes. The org-scoped grants tie each org's apply login and control login to that org, so a login can write only its own org's rows. Reason: each org's control mounted the system migration login and the shared apply login (`org/templates/control.yaml`, `platform-install/src/org.rs`), so any org could change the whole system database.
- B. One CA per project Cluster signs that project's login certificates. Projects are their own isolation level (F7).
- C. Each org's control host name is `<org>.<base>`, covered by the platform wildcard certificate.
- D. The org worker keeps Secret rights across its own namespace, as a stated exception inside the org.
- E. Application host names are `<label>--<org>.<base>`. The user picks only `<label>`; the registry keeps it unique within the org. Web drops any route whose host does not end in `--<namespace>.<base>`, where the namespace is the route ConfigMap's namespace. One level deep, so the platform wildcard certificate covers it. This is the pattern Netlify uses (`branch--site.netlify.app`). Later option: custom domains through a verified-domain list. Reason: host names were free text with no uniqueness across orgs, and web served the lowest ConfigMap name on a clash (`web/src/lib.rs::Releases::routes`), so any org could take over another org's host name.

**Ruled (D, 2026-11-19): system cleanup, control's host, project Helm Jobs, integration types, principals.**
- Q1. Identity runs the system database cleanup of `platform.tx_identity` in its existing 60 s loop. It calls one function, `platform.prune_tx_identity()`, which is SECURITY DEFINER, owned by the table owner, and runs only `DELETE FROM platform.tx_identity WHERE xid < pg_snapshot_xmin(pg_current_snapshot())`. Identity's login gets EXECUTE on that function, not DELETE on the table. The owner login stays in the one-time install SQL Job. The project database cleanup stays in each org's control, using its own project login.
- Q2. Web accepts a route host that is exactly `<namespace>.<base>` (control) or ends in `--<namespace>.<base>` (applications), where the namespace is the route ConfigMap's namespace. Org ids allow only single hyphens, so the two forms never meet. Any future platform host under `<base>` is `wamn-<name>.<base>`; org ids cannot start with `wamn-`.
- Q3. Project Helm Jobs stay on the org worker. Helm stores every project's records in `wamn-helm-<org>` and needs list there, so an account for each project gains no isolation. Client certificates (10.8) keep passwords out of the records.
- Q4. Only the platform publishes integration types. They ship as seed data in the platform's system SQL, like `http`, `gcs`, `s3` and `modbus` today, and change only through `wamn platform deploy`. EXECUTE on `registry.publish_integration(...)` is revoked from `wamn_control`. Later option: integration types owned by one org, which needs an org column.
- Principals. An org's control login can create a principal, and can change only principals whose memberships are all in its own org. A principal with memberships in more than one org changes only through identity. Reason: `wamn_control` has INSERT and UPDATE on `identity.principals`, which has no org column, so any org could change another org's user's email or status and take over the account through a password reset.

**Ruled (D, 2026-11-19): org-scoped grants on the system database.**
- Accepted as the agent's plan (third note on wamn-0vj2i.4, checked at 51b6cf1a9):
  - Each org has two logins, `wamn_control_<hash>` in group role `wamn_control` and `wamn_apply_<hash>` in group role `wamn_apply`; neither group role logs in.
  - `registry.login_org()` replaces `registry.control_org()`, and `registry.login_principal(uuid)` replaces `registry.control_principal(uuid)`.
  - The org policy covers members of both group roles, and is added to `registry.event_readers`, `wamn_authority.author_login_projects`, `registry.capture_gap` and `registry.retired_project_envs`.
  - Principals: INSERT is allowed; UPDATE only when every membership is in the login's org (`identity.principal_in_login_org_only`).
  - The seven new apply grants.
  - Platform roles stay unlimited.
  - The unreachable `SET ROLE wamn_system` helper in `environment/connections.rs` is deleted.
- Change 1: an org login may insert or activate a membership only for a principal that has no memberships, or memberships only in that org. Adding a user who already belongs to another org is refused. Reason: control creates memberships as `active` at once, and the foreign key check ignores the row policy, so an org could attach itself to another org's user without consent and lock that user's own org out of changing them.
- Change 2: the org policy's `USING` clause is the org check only. `WITH CHECK` keeps the bound actor and the org check. Reason: with no bound actor, reads returned no rows, so `env delete` and other apply reads did nothing.
- Change 3: `registry.project_capacity_available(requested int) RETURNS boolean` replaces `registry.project_stream_count()`, under the same capacity lock, so no org sees the total project count.
- Later option: users in more than one org, through an invitation that the user accepts and identity activates.

**Ruled (D, 2026-11-19): Epic 10 review.**
- Fix 1. `wamn platform deploy` always waits for `cnpg-controller-manager` after the infrastructure release and installs the platform release once. No retry based on error text.
- Fix 2 (10.11). Org logins reach the system database through one CloudNativePG Pooler in session mode, in `platform`. PgBouncer limits are fixed at install: two Poolers (see the pooler ruling below), and `max_db_connections` as the total. A client over its limit waits rather than fails. Postgres `max_connections` is set once at install, so a new org changes no Postgres setting (T1). Role connection limits match the pooler limits. Platform logins connect directly with certificates. wamn provides PgBouncer authentication, because the system Cluster uses its own certificates. Reason: each org's apply login had `CONNECTION LIMIT 101` against the default `max_connections` of 100, so one org could take every connection, and raising `max_connections` later needs a Postgres restart.

**Ruled (D, 2026-11-19): the system database pooler and platform certificate logins.**
- Two Poolers in `platform`, both CloudNativePG `Pooler` type `rw` in session mode: one for org control logins with `max_user_connections` 8, and one for org apply logins with `max_user_connections` 16 and `query_wait_timeout` of about 15 minutes, below the CLI's 16-minute poll limit, so a queued deploy waits rather than fails. Reason: a stock Pooler has one set of PgBouncer parameters, with no per-user section.
- Totals set at install: the two Poolers' `max_db_connections` add up to about 150, and the system Cluster's `max_connections` is 200, which leaves room for the direct platform logins and admin work. Role `CONNECTION LIMIT`: 8 for each org control login, 16 for each org apply login. A larger platform raises the totals with the database size at install.
- What these connections carry: the control login serves org administration (users, grants, projects, integration settings, environment status, permission checks, starting operation Jobs). The apply login serves the operation Jobs (`deploy`, `rollback`, `stop`, `diff`, `apply`, `reset`, `recover`, `close`). Application traffic uses project databases and is not limited by this.
- One replica per Pooler. Each PgBouncer instance applies its own limits, so more replicas multiply every limit and total.
- 10.12: platform logins use certificates, as ruling G says. That covers the SQL Job, the system migration login, each provision-org Job, and the `postgres` superuser that the SQL Job uses (`CN=postgres` from the platform issuer and a `hostssl ... postgres ... cert` rule). Their password Secrets go.
- One session per apply Job: the work runs on the connection that holds the environment lock (a session advisory lock stays held across transactions on that session), so a Job never waits for a second session.
- Accepted from the agent: the auth query function lives in the `postgres` database, is owned by `postgres` and returns password hashes only for org logins; each Pooler has a certificate from the platform issuer and clients check its name; the database accepts org logins only from the Pooler pods.

**Ruled (D, 2026-11-19): pooler review.**
- `max_connections` on the system Cluster is computed by the platform chart: Pooler totals, password lookups, identity's pool, the fixed Job connections, reserved slots and a margin of 10. The provision-org Job and the system SQL Job each have one fixed name, so Kubernetes runs at most one of each at a time, and the count is exact. Reason: the hand-set 200 could be exceeded at peak, and identity would be refused first.
- Control reconnects its writer after a failed connection. Reason: a Pooler restart left control broken until it restarted.
- Pooler certificate renewal needs no code. The CloudNativePG resync (about 10 hours) reconciles each Pooler and writes the current Secret versions into its status. The PgBouncer pod sees the change and reloads. A renewal at day 60 is in use well before the expiry at day 90.

**Ruled (D, 2026-11-19): kubectl error text.**
- Recorded exception until 3.7b: `crates/kubernetes/src/kubectl.rs` reads kubectl's error text to tell Conflict and AlreadyExists from NotFound. kubectl gives no structured error, only exit code 1. The Conflict check is older; the NotFound check came with the fixed Job names in Epic 10.
- In 3.7b, all Kubernetes calls move to `kube-rs`, which reads kubeconfig (exec plugins included) and returns typed API errors. The kubectl error-text checks and the hand-written in-cluster client (`kubernetes_api.rs`) both go.

**Ruled (D, 2026-11-19): first kind run, B1 and checks.**
- B1: palette sealing uses `ProjectEnvKey::platform_palette()`, a constructor in `wamn-registry` with the reserved org `wamn`, which `parse` refuses. The parser stays strict. The key never reaches the stored palette entry. `read_palette` had no caller and is deleted. Cause: `platform` became a reserved org in Epic 9 (4d3ea4ea4), so the parsed key panicked in `cargo xtask build-platform`.
- Standard checks from now on: build, clippy, `cargo xtask build-platform`, the schema-apply check and the Helm size check. `build-platform` is a build, not a test.
- The first kind run deploys `apps/wamn_receiving` and `client_acme_receiving`, which has the only Data change handler.
- C1: `cargo xtask build-platform` always uses its own buildx builder, `wamn-platform` (docker-container driver, host network). It creates the builder when `docker buildx inspect` exits non-zero, and never changes Docker's current builder. Lanes never change Docker's current builder either; they use `--builder` or `BUILDX_BUILDER`. Cause: a lane left Docker's current builder set to a deleted builder, so buildx fell back to the default driver, which cannot export the registry cache.
- N1: overlays commit their `.wamn/describe/` files, the same as bases, so they build without a database.
- C2: the Dockerfile copies `mise.toml`, because `crates/codegen/src/kcl.rs::binary` builds the KCL version pin into the program at compile time. `mise.toml` stays the one source of tool pins. Cause: the compile-time read came in on 2026-11-15 (494238ae4), and no check ran `build-platform` until the first kind run.
- K1: the NATS probes drop the chart's default `httpGet` through `container.patch`; a `null` in parent values did not remove it, so each probe had two handlers and the API server refused the StatefulSet.
- K2: PostgreSQL is pinned to `18.6` by tag and digest. The `18.7` pin from the 2026-11-18 CloudNativePG ruling named an image that CloudNativePG never published.
- K3: `--settings` is an argument of each service subcommand. A required global argument is checked before the subcommand is read, so `serve --settings X` failed.
- K4: the first owner's membership is written before identity sends the invitation; a retry re-sends the invitation until the owner enrolls. Identity still refuses users with no membership.
- Org `wamn-oauth`: the provision-org Job fills one random key per org into a Secret that the org chart creates empty. Skipped when OAuth encryption uses KMS.
- `registry.env_policies` is deleted. `registry.environments` holds `readiness_budget_seconds` (default 600) and `drain_bound_seconds` (default 300), written by `env apply` from the environment file and read by `env delete` from the environment's own row. Reason: the table held only these two numbers and nothing in the product wrote it.
- N2: cert-manager deletes a certificate's Secret with its `Certificate` (`enableCertificateOwnerRef`). Certificates used by kept Clusters are kept. Cause: an uninstall left TLS Secrets signed by deleted CAs, which a reinstall reused.
- K5: `identity.password_logins` had row-level security with only a SELECT policy, so identity's insert was refused. Each table with row-level security now has a policy for every command a role is granted on it, with the table's org check; grants still limit org logins. Cause: the org-scoped grant work (2bde29040) added a SELECT-only policy.
- Org control mounts the org's `wamn-oauth` key and the OAuth encryption setting, like the org's hosts: control encrypts OAuth connection tokens and hosts decrypt them, inside one org.
- Vendored charts stay stock: no repacked chart for the Barman plugin certificates.
- K6: `wamn org create` writes `wamn-registry-pull` into the org namespace from the operator's registry auth file, as listed in "Secrets made by Rust". On kind the file may be `{"auths":{"localhost:5001":{}}}`, because the local registry has no authentication. Skipped with registry token metadata. Cause: the old install path that wrote it was deleted (550cdc5f1).
- Pulls read an explicit empty entry for the exact registry as anonymous, the same as pushes; a missing or partial entry is refused before any request.
- K7: `wamn platform deploy` records the application bundle's digest reference in `wamn-org-inputs`, and `wamn org create` sets control's `chart` setting from it, as it already does for `controlImage`. Cause: `org create` pointed control at the plain chart folder in the image, while the worker needs the stamped bundle.
- K8: `bundle::select` takes the one pulled entry that holds `Chart.yaml`, and refuses zero or several. Cause: Helm 4.2.2 adds an empty `<chart>@sha256:<digest>` folder when it pulls by digest.
- K9 (fixed by the agent under the standing rule): identity installs the aws-lc-rs rustls crypto provider at start (546ed2004).
- K10: a project with no deployed schemas has no Publication and no CDC reader. `env apply` waits for the publication only when there are schemas, and the first deploy with models creates both through the deploy path. Cause: an empty `objects: []` was dropped by CloudNativePG, which then refused the Publication.
- K11 (fixed by the agent): `org_values` writes the event stream policy again (e4320450d).
- K12: control installs the aws-lc-rs rustls provider in `main`, like identity, cdc-reader and the CLI. Cause: the operation worker panicked on its first TLS connection to NATS.
- K15 (fixed by the agent): a chart test gives the project chart its environment document (e2846d11f).
- K16 and K22 (fixed by the agent; K16 touched two files because the key constructor lives in `wamn-registry`, per the palette ruling).
- K17: the registry's egress address goes through `wamn-org-inputs` into each org's `networkPolicy.registry`.
- K18: loopback registry references map to `POD_REGISTRY` in one shared function, digest unchanged, only when the registry is loopback.
- K19: `env apply` after `env delete` makes a new generation; kept Clusters and ObjectStores stay for a manual dump (copy-back ruling), and `env delete --data` after a plain delete removes them and lists what it removed.
- K20: Rust waits for `Ready=True` on each WorkloadDeployment. Helm's watcher does not check `Ready` on custom resources.
- K21: the `pg_walstream` fork sends the client certificate and key. It carries two patches, typed keepalive and client certificate.
- O11: the org invitation Job's first pod can fail while control rolls out; the Job's retry covers it. Kept as is.
- The first kind run passed on the short path at 822261402 (2026-11-20): `env apply`, host start with its own scheduler user, the `wamn_receiving` deploy, and calls to the application and control through web (401 with no token, 200 with the owner token).

**Ruled (D, 2026-11-19): preflight and smoke runs.**
- Preflight, run before any kind install:
  (1) `cargo xtask build-platform`;
  (2) every image reference in the rendered charts resolves, checked with `docker buildx imagetools inspect` by exit code;
  (3) every rendered chart is accepted by `kubectl apply --dry-run=server` against the kind cluster with the CRDs installed;
  (4) every wamn container in the rendered charts accepts its rendered command and arguments: run the image with them and `--help` appended, by exit code.
- Every epic ends with a kind smoke run before it merges to main: the preflight, then `wamn platform deploy`, `wamn org create` and one application deploy. Local kind has standing approval for these runs; GCP and other clusters still need D's approval.
- During a run, collect every failing pod and its cause at each step before stopping, rather than stopping at the first.
- Reason: the standard checks never built images, pulled images, sent objects to an API server or started a binary, so ten epics of runtime defects surfaced one per round in the first kind run.
- During a run, the agent fixes a defect without asking when the fix stays inside one function or file, matches the rulings and patterns, and changes no schema, chart values contract, permission or security boundary, dependency or CLI. Each such fix is committed on its own and listed in the report for review afterwards. The agent stops only for design choices, the excluded areas, or a conflict with a ruling. Reason: most defects of the first kind run were small and local, and each one waited for D.
- A dependency already in the workspace, added with `workspace = true` and no new crate, version or feature set beyond another member's, counts as no new dependency under the run rule.
- Any wasmCloud chart upgrade (the vendored runtime-operator chart) also renders the environment and platform charts with the old and the new vendored chart, using the same values, and the diff of the rendered objects is reviewed before merging. Reason: `deploy/charts/environment/templates/host.yaml` includes the vendored chart's internal template files by path (`charts/runtime-operator/templates/runtime/deployment.yaml`, `host-plugin-config.yaml`, `service.yaml`). A moved file fails the preflight render, but a changed `.Values` key or helper can render wrong defaults without failing.
- Smoke runs, from 2026-11-20, are the short path: the preflight, `wamn platform deploy`, `wamn org create`, `wamn env apply`, one application deploy, and one call to the application and to control through web. The fuller journey runs as automated journey tests on kind in 3.8.

**Ruled (D, 2026-11-19): NATS and tenancy review.**
- Scheduler users: one per host pod, replacing the shared scheduler rights (this overturns the 2026-11-19 stated exception to T1). Reason: every host group's scheduler user could publish on `runtime.host.>`, and wasmCloud's threat model says that anyone who can publish there can start any workload with any grant, so a sandbox escape in one org's host would control every host. Design:
  - The host sets its id to its pod name (`HostBuilder::with_id`).
  - At start it makes a key pair and asks identity for a user limited to what upstream documents (`wash-runtime/src/washlet/mod.rs`, `ClusterHost`): subscribe `runtime.host.<id>.>`; publish `runtime.host.<id>.__control.ready` and `runtime.operator.heartbeat.<id>`; its own inbox and reply subjects.
  - Identity checks the request with TokenReview, takes the pod name from the pod-bound token and the environment from the service account. No expiry, because pod names are never reused.
  - The operator's user keeps its broad rights.
- The host's default data user moves from `PLATFORM` to the environment's own account, with publish denied. Nothing tenant-related stays in `PLATFORM`.
- Environment accounts get finite limits for connections, subscriptions, payload size and data, set at install like the other platform caps. Reason: these were `UNLIMITED` (`identity/src/nats/claims.rs`); only JetStream was capped.
- One NATS stays (E6), with these limits and the three NATS pods already planned. The shared runtime operator stays, the same as CloudNativePG and cert-manager.

**Ruled (D, 2026-11-20): no upstream; K24; 10.13 details.**
- No upstream changes or upstream pull requests, ever. wasmCloud stays stock with no patches and no fork. The `pg_walstream` fork is wamn's own.
- K24: every application workload has an unnamed `wasmcloud:nats` binding on the host's default data connection. The data user lives in the environment's own account with publish denied, so the plain import routes through nothing in `PLATFORM`. Cause: the Data change handler's message resource comes from the plain `jetstream` interface, which wash-runtime links only for an unnamed binding.
- 10.13:
  - TokenReview and the host service account only.
  - Key-based inbox. Operator replies use a fixed publish right on `_INBOX_wasmcloud-operator.>`, not NATS `resp`. Reason: NATS grants a `resp` reply right for any message the host receives, with a reply subject the sender chooses. Because wasmCloud requires the host to receive its own messages, a host could set another host's command subject as the reply on its own `__control.ready` message and then publish once to that host. The operator's inboxes carry random tokens a host cannot guess, so the fixed right reaches no other host.
  - No revocation; reissue for the same pod.
  - No migration code.
  - Connection, subscription and payload limits per project, multiplied by the project count, and signed again when changed.
  - No `data` claim: NATS does not enforce it; the payload, subscription, connection and JetStream caps are the limits.
- Remaining host risk, accepted under wasmCloud's threat model, which trusts hosts: a host's heartbeat carries labels it reports itself (`HostHeartbeat.Labels`), and the operator places workloads by them. So a host taken over by a sandbox escape could announce another org's host group and attract its workloads and traffic. Per-pod users stop it from taking over other hosts' ids and commands, and org NetworkPolicies keep it from other orgs' databases.

Epic 9 closed at 01bb4a1e2: review accepted, main fast-forwarded. The application record is 69,220 B on the 772-route wamn_wms fixture, a different fixture from the Epic 8 one, not growth. The slot cleanup connects to the postgres database, and the deadline for an unreachable database under --data is 10 s.

**Ruled (D, 2026-11-20): Epic 11 scope (step 3.7, codegen), issues 11.0 to 11.6.**
- Q1 (corrected after the scout): the TypeScript client generator is `@hey-api/openapi-ts`, version pinned, with only its type, SDK and fetch-client plugins. No `@tanstack/solid-query`. Reason: the plugin broke two earlier rulings, Epic 16 (a small store, not TanStack Query) and sync (Solid 2 owns optimistic state). The client uses `@wamn/web-runtime`: its transport as the client's fetch, interceptors that run `toWire` and `reviveOutcome`, and a query serializer that JSON-encodes values, as `readQuery.ts` does. Reads keep going through `readQuery` and `readCache`.
- A wamn hey-api plugin (calls into `@wamn/web-runtime` for each operation, sync code for each model) is a possible future exploration only. It is not an epic and not scheduled.
- Q2: 11.6 (no types in `wamn.k`, hand-written custom-operation WIT, apps updated) is done in this epic, because 11.3 to 11.5 rewrite the same generators.
- The 3.7b items stay in 3.7b.

**Ruled (D, 2026-11-20): Epic 11 scout choices.**
- 1: control's OpenAPI comes from the existing contract JSON files. `contract_files.rs` and the JSON files stay. Reason: control's schemas exist only there.
- 2: a pnpm script in `apps/platform_ui` runs the generator, called by its existing build.
- 3: `wit-encoder =0.254.2`, which matches the workspace `wit-parser` and wasmtime 48.0.8.
- 4: a labeled world import is one identifier. That is the only string fragment left in WIT output, and it carries a comment.
- 5: a fixed `// @generated` line goes before the formatted output.
- 6: keyword and dotted names are spelled as wit-bindgen spells them.
- 7: there is no Solid Query (Q1 corrected).
- 8: a client-side query serializer JSON-encodes values. The server does not change.
- 9: each statement entry in `wamn.k` lists its bind names only. Types and nullability come from the WIT input. This extends K1: a statement entry holds its name, fetch class and bind names.
- 10: value lists become WIT enums. `uuid`, `numeric` and `timestamp` become named WIT aliases.
- 11: the modbus integer range stays in `wamn.k` as a check.
- 12: the web/wfe examples stay as they are. They belong to the wfe agent.
- 13: all of 11.0 is doc fixes. The JWT `data` claim is doc text only: "data: -1 (no limit); NATS does not enforce it." The JWT does not change.
- P5: Part 1 holds the current rules. Each new ruling edits Part 1 in the same commit. Part 2 holds the reasons.
- The second review's 8 contradicted rules:
  - "`wamn.k` holds no types" and "custom-operation WIT is not generated": 11.6.
  - The exclusive lock (only `sql/session_identity.rs::lock_file` exists), the HTTP client cache key (`http/transport.rs::HttpTransport` holds TLS settings only) and `registry.connection_principal` (absent): removed from Part 1 and listed in Open items as not built.
  - Control is in the platform chart: doc fix, it is in the org chart.
  - "Every Kubernetes object comes from a chart": doc fix that states the exceptions, `platform-install/org.rs::create` and `environment/broker.rs::ensure_user`.
  - One writer for `platform."user"`: a 3.7b open item (`identity/application.rs::write_user`).
- Contradiction 17, route ownership: the crate splits said "control route, host route, application route, environment status and connection health → control service", but step 5.4 said "The host serves `HostRouteSet::Application`" and moved the Application half of control's route code into `services/host`. The code follows 5.4. 11.0 fixed the doc: the splits now say "control routes, environment status and connection health → control service. Application routes → host."

**Ruled (D, 2026-11-20): Epic 11 questions 1 to 5.**
- 1: a bind with no input field of the same name. A computed bind takes its type from SQLx describe, as non-null. A renamed bind takes the name of its input field in the `wamn.k` bind list, and the SQL does not change, because binds are positional. If a computed bind can be null, the work stops for a ruling. Reason: SQLx describe gives positional bind types, and the handler computes these values, so no WIT input owns them.
- 2: result columns that SQLx cannot type for null use SQLx's own aliases, `"col!"` and `"col?"`, with no new convention. `inventory.merge` `transaction_ids` gets a real SQL cast to `uuid[]` so that describe returns the right type, with no type-override alias. Reason: the SQL file owns statement types.
- 3: `not_required`: the generated Rust enum carries `#[serde(rename_all = "snake_case")]`, so the WIT case `not-required` goes on the wire as `not_required`. The wire does not change, and no other mapping code is added. Reason: WIT requires kebab-case cases.
- 4: version rewrite: authored WIT files carry no package line. Only the generated `package.wit` declares the package and its version, and the development-build rewrite touches only that file. Reason: the WIT spec requires at least one file in a package directory to declare the package, and the others can leave the line out, so authored files are never rewritten.
- 5: `wamn eject component` is retired. Its only producer, the TypeScript emitter, was deleted in 11.2, so it fails today. `wamn eject action` stays.

**Ruled (D, 2026-11-20): Epic 11 questions 6 to 8.**
- 6: control's `openapi.json`: a `cargo xtask` command writes control's OpenAPI files to `apps/target/wamn/platform_ui/`, and `crates/codegen/build.rs` writes no files. `cargo xtask build-platform` calls the same xtask function directly before its pnpm build, so the chain is never xtask, then pnpm, then xtask. The pnpm script in `apps/platform_ui` calls the xtask command only for local UI work. Reason: a build script does not write files for other tools to read.
- 7: SDK writes: `@wamn/web-runtime` exports one fetch-shaped function on the same authenticated transport, with the same auth and CSRF, and no existing caller changes. Interceptors run `toWire` on requests and `reviveOutcome` on responses. A caller of the SDK must get the same classified `Outcome` as a caller of `Transport.invoke`. If hey-api interceptors cannot give that, the work stops and reports. Reason: one added function changes no existing caller.
- 8: `release.profile`: no route serves it, so the Operations and Roles views of `platform_ui` are already broken. Remove the two routes in `projects.tsx` that use `RELEASE_PROFILE_ROUTE`, the profile read in `environment.tsx`, the `release/profile` contract files and anything only they use. Role administration (`application-roles.tsx`, `role-users.tsx`) stays. Reason: the freeze adds no new route, and an Open item records the rebuild.

**Ruled (D, 2026-11-20): Epic 11 questions 9 and 10.** Both correct earlier rulings, which were made without checking the host code.
- 9: `inventory.merge` `transaction_ids`: `merge.sql` keeps its JSON aggregate, which describe types as JSONB, and the hand-written handler converts it to the `list<uuid>` of its authored WIT result. This replaces the `uuid[]` cast of ruling 2. Reason: `crates/sql/src/types.rs::decode_row` has no array arm, and the host gets no change.
- 10: `not_required`: in `crates/engine/src/operation/json.rs`, enum cases and flags follow the rule for record field names and error codes: `-` becomes `_` on output, and `_` becomes `-` on input. This replaces the guest serde attribute of ruling 3. Reason: the host, not the guest, writes the JSON. Variant case names are not converted, and this epic does not change them.

Epic 11 closed on 2026-11-20 after review. The smoke run on kind passed.

**Ruled (D, 2026-11-20): Epic 12 scope (step 3.7b) and the second review's unrecorded items.**
- Issues 12.0 to 12.8, as in the Epic 12 section of Part 1. After 12.0, the issues run in parallel.
- Three review items go into issues. 12.6: `wamn platform deploy` uses Helm's own kstatus wait, so `kubectl::wait_deployment_ready` and `job::clear_finished` go. 12.7: `wamn.application` is parsed once per bind, not seven times. 12.8: the dead SQL `platform.api_key` and `wamn_authority.author_login_projects` go.
- Four review items become Open items: `HostArgs` is a hand-kept copy of the stock host flags; the environment document has two owners; org, project and environment are written five times in workload config and never compared; "wiring" still names workflows. Each needs a design or a schema change, so the freeze keeps them out of Epic 12.
- Two review items are kept. The migration appliers stay, because system migrations need them. The private stock chart templates in `environment/templates/host.yaml` are an accepted risk, because the chart-update review catches a file move.

**Ruled (D, 2026-11-20): Epic 12 scout questions 1 to 6.**
- 1: the host links `kube-rs` directly for the run-time read of connection Secrets, with only a typed get on Secrets in its own namespace. It links neither `wamn-kubernetes` nor `wamn-provision`. Reason: the host reads credentials at run time, so it needs a Kubernetes client, but not the control-plane crates.
- 2: the old fixed-name system SQL Job gets a typed clear. Rust deletes the finished Job with `kube-rs` before `job.rs::submit` creates the next one. The CDC reader wait and the other Rust waits whose chart renders a Secret stay as typed `kube-rs` waits. `kubectl::wait_deployment_ready` and `job::clear_finished` go once their callers use these. Reason: Helm does not delete the old Job, a Job spec is immutable, and Helm cannot wait on a chart that renders a Secret.
- 3: `wait_administration` (`crates/platform-install/src/platform/control.rs`) and its call in `crates/environment/src/environment/delete.rs` go, and `pods/exec` leaves the org worker's Role in the same commit. Reason: `kube-rs` exec would need a wider permission, and the Open item already planned to remove it.
- 4: `kube` 4.2.0 and `k8s-openapi` 0.28.0, pinned, with the version feature for the lowest supported Kubernetes, not "latest". Facts: kind nodes run 1.36.1 (the default node image of kind v0.32.0), and the last recorded GKE server is v1.35.8 (6d6b9ce48), so the feature is `v1_35`. Nothing pins kube 1.x: wasmCloud's workspace declares `kube = "1"` and `k8s-openapi = "0.25"`, but no wasmCloud crate uses them and `Cargo.lock` has neither.
- 5: the identity service writes as a new seeded actor of its own, not `PlatformComponent::Provisioning`. Reason: each writer has one principal.
- 6: fix all 28 clippy errors, so that clippy exits clean. These are fixes in existing code, not new tests.

**Ruled (D, 2026-11-20): Epic 12 questions Q1 to Q3.** Q1 corrects ruling 4 above, because kube 5.1.0 is the current release.
- Q1: pin `kube` =5.1.0 and `k8s-openapi` =0.29.0, with the feature `v1_35`. Reason: kube 4.2.0's optional `serde-saphyr` ^0.0.29 needs `smallvec` below 1.16, and the `pg_walstream` fork (rev 9957974) needs 1.16.4 or later, so the workspace does not resolve. kube 5.1.0 (released 2026-11-12) uses `serde-saphyr` 2, needs Rust 1.95, and resolves with the current fork. The fork does not change, and no `[patch.crates-io]` is added.
- Q2: the connection types leave `wamn-catalog` for a new small crate, `wamn-connection-type`, below `wamn-engine` and `wamn-authority`. Reason: `wamn-connection` would make the cycles authority → connection → authority and engine → connection → engine, and `wamn-registry` keeps only its database (one owner per fact).
- Q3: the artifact reference module and `validate_composed_entry_path` go to a new crate under `crates/oci/` with no network code, `wamn-oci-reference`. `wamn-oci` and `wamn-edge` both depend on it. Reason: `wamn-edge` validates composed-entry references, and `repo-lint-dependencies` forbids network crates in `wamn-edge`. No feature flag is added, and the dependency check rules do not change.
- With Q2 and Q3 done, `wamn-catalog` is deleted.

Epic 12 closed on 2026-11-20 after review. The smoke run on kind passed.

**Ruled (D, 2026-11-20): Epic 13 scope (step 3.8, first part).**
- Q1: 3.8 splits into two epics. Epic 13 is the machinery and the existing tests. Epic 14 is service tests per contract and the journey tests on kind. Reason: Epic 14's contract list depends on what Epic 13 leaves standing, and the machinery work stays apart from the test cluster runs.
- Q2: existing `#[ignore]` service tests that check a current contract are ported; the rest are deleted. Epic 14 adds only what is missing.

**Ruled (D, 2026-11-20): Epic 13 scout choices and questions.**
- Choice 1: `testcontainers` =0.28.0 alone. No `testcontainers-modules`: Postgres and NATS use `GenericImage`. Reason: the modules crate needs 0.27, and the 0.27.3 of `wash-runtime` is a wasmCloud dev dependency that does not enter the wamn lock.
- Choice 2: the CNPG digest `ghcr.io/cloudnative-pg/postgresql:18.6@sha256:899d3ed526b659d77935dde0e6bf2d69dbbf17d3d8c6486ca8cfd04bd3c18533` with a test-owned initdb. Reason: tests use the production digest, and the CNPG image has no entrypoint that creates a database.
- Choice 3: `nats:2.15.2-alpine@sha256:afae2fdbe2e326c65e1246cb83b9d05d319188734f3bd577ac8072daae78a10f` is pinned in the platform chart values. This chart values change is allowed in this epic, because digests are identity in wamn. Tests use the same digest.
- Choices 4 and 5: neither scout option. Each test that needs Postgres starts its own CNPG container through one helper in the dev crate. The helper runs the test-owned initdb and returns the URL. Each test gets its own server, so tests run in parallel. No `#[sqlx::test]`, cargo runner, custom test main, locks or `RUST_TEST_THREADS`. NATS tests use the same pattern.
- These two rulings change the F4 text "`#[sqlx::test]` per-test databases" (Z in "Tests (F4): research"). Reasons: `#[sqlx::test]` reads `DATABASE_URL` once, before the test binary starts, so a container that a test starts cannot supply it. Roles are server-wide, and the product installs fixed role names, so per-test databases on one server still collide. A cargo runner also wraps `cargo run` and `cargo bench`.
- Choice 6: one dev-only crate, `wamn-test-infrastructure`. `test-support/application` and `test-support/resources` fold into it. A helper used by one crate only goes into that crate's `tests/common/`. Reason: this avoids the dev cycles the scout found, for example the provision setup.
- Choice 7: grant tests go into the new `wamn-test-sql` at `test-support/sql/`, the one SQL crate of AB7.
- Choice 8: a changed file that no package owns selects the whole workspace.
- Choice 9: `cargo xtask prepare-tests` builds the fixtures.
- Choice 10: `measure-tx-identity` becomes an `xtask` command. The Python script goes, and `docs/operations/deployment.md` calls the command.
- Choice 11: an ignored test that needs only Wasmtime, loopback or a prepared input is ported at the lowest tier.
- Choice 12: history assertions of the deleted provision tests move to `crates/postgres/tests/project_schema.rs`, and grant assertions move to `wamn-test-sql`.
- Choice 13: an absence wait with no product event is tested at a lower tier. No product events are added.
- Choice 14: back doors are deleted. Tests use the production constructors with real configuration. A test that cannot be written that way is reported, not worked around.
- Question 1: 13.6 covers every restricted deployed login, each through the real login: `wamn_migration`, `wamn_app_login`, `wamn_query_login`, `wamn_platform_login`, `wamn_cdc`, `wamn_platform_sql_<digest>`, `wamn_identity_issuer_<digest>`, `wamn_pooler_auth`, `wamn_control_<digest>` and `wamn_apply_<digest>`. `wamn_system_reader` is tested through `SET ROLE` and recorded apart.
- Question 2: `cargo test -- --ignored` leaves out the journey and cluster targets by package scope. They come in Epic 14.
- Question 3: the NATS digest pin is allowed, as choice 3.
- Question 4: no product-gap stubs remain, so no gap beads are filed.
- Question 5: `platform-policy/data-access.json` goes with the two tests in `development_assertions.rs` that read it.
- Question 6: a private Postgres 18 server is enough. The command compares commit cost with and without the identity insert on the same server. Absolute numbers under synchronous standbys belong with the Open item "The system database: a second instance, an ObjectStore and a ScheduledBackup".
- The Q2 table stands: 108 tests are ported and 14 are deleted.

**Ruled (D, 2026-11-21): Epic 13 questions 1 to 10.** Ruling 1 corrects choice 3 and question 3 of the 2026-11-20 entry.
- Question 1: NATS stays at 2.12.15. The server, the init container and the NATS test containers use `nats:2.12.15-alpine@sha256:b270f5e2428354c0335612694d7dd2fb588148e567a5757fdff325ef9c9332e6`. Reason: since `a84e44f14` the platform values set 2.12.15, the version stock wasmCloud uses, and every kind run passed on it. Under the freeze, a digest pin must not also change the server version. The move to 2.15.2 is an Open item.
- Question 2: `services/host/tests/native_lifecycle_live.rs` and `scheduler_nats_move_live.rs` are deleted. Reason: they fail because scheduler NATS signing needs an identity issuer. Their contracts (probes, signals, drain, NATS outage) go to Epic 14, which builds the real host in process with the real identity code.
- Question 3: `crates/envelope/src/pod_identity.rs::pod_signing_credentials_never_contain_a_private_key` and the `with_test_endpoint` back door are deleted. Reason: its only check is the literal `private_key: None`, and the Q2 table deletes tests that read back a literal.
- Question 4: `Mailer::fixture` and the `ResendMailer` endpoint override are deleted. The test uses `MailConfig::Local`, and the token cleanup on delivery failure gets a unit test in the invitation code. Reason: ruling 14 deletes back doors.
- Question 5: `native_call.rs::prepare_native`, `ReadinessCall` and the 3 readiness tests are deleted. Reason: only tests call them.
- Question 6: `release_manifest::load_from`, `ManifestUnreadable` and their tests are deleted. Reason: only tests call them.
- Question 7: if nothing fills `ReleaseTarget.applications`, the membership refusal in `ServingContext::new`, its test and the field are deleted. Reason: the refusal cannot fire.
- Question 8: `crates/codegen/src/client_ir.rs` and all of `client_projection.rs` are deleted, with the 7 tests ported in this epic. Reason: the code is dead, and Part 1 deletes the file.
- Question 9: the `pg_authid_rolname_index` retry in `crates/codegen/src/build.rs::create_history_tables` is deleted. Reason: the retry is dead.
- Question 10: the deleted `42501` check in `crates/identity/tests/password_live.rs::enrollment_preserves_identity_and_pat_and_consumes_all_invitations` stays deleted. Reason: it was written for `wamn_db_owner`, and 13.6 asserts the refusals of every login.
- Limits on root's decisions: the `provision_org` and `provision_system` split stays only if production calls the same public functions. The system roles from `helm template` are parsed into typed objects, with no text matching. Project logins log in by password in 13.6, because grants depend on the role, not the login method. Certificate login is tested in Epic 14 on kind.
- Count: 96 ignored tests are ported.

**Ruled (D, 2026-11-21): Epic 13 questions 11 and 12.**
- Question 11: each of the eight operation live tests that `StaticCredentialProvider::resolve` refuses is sorted by its cause. A test that supplies a credential that production never issues gets a fixture from the production constructors, where that fits in the test. Otherwise the test is deleted, and its contract goes to Epic 14 by name. If production refuses a credential that production issues, that is a product defect, and the check and the test stay. Reason: under the freeze, a test does not get a credential back door, and Epic 14 has the real host and identity code.
- Result of question 11: all eight tests made `wamn_app_<hash>` logins on a loopback port, which production never issues. `bind_host_project` issues only `wamn_app_login` or `wamn_query_login` on `<cluster>-rw.<org>.svc.cluster.local` port 5432, so no test container can serve it. Seven tests are deleted, and their contracts are on the Epic 14 line. The eighth is the test of question 12.
- Question 12: `require_provisioned_principal`, `UnprovisionedPrincipal` and its test in `crates/operation/src/session_identity/tests.rs` are deleted. Reason: production never calls it, and production fails closed. `authorization.rs::held_grants` returns no grants when `!grants.active || !grants.present`, and `authorization/store.rs:182` needs `grants.present && grants.active`. `crates/operation/src/host.rs:185` checks only `grants.active`, but `principal_grants_from_rows` leaves `active` false when no `platform."user"` row exists, so it needs no fix.
- Count: 88 ignored tests are ported, the 96 of the earlier entry less these 8.

**Ruled (D, 2026-11-21): Epic 13 fix 1.**
- The apps workspace host build has a baseline of 18 link failures. `test-support/fixtures/guests/connection-http-standard` stays as it is, because its panic handler is already correct for both the wasm target and the host target.
- The review was wrong to call the fixture failure a regression from 13.7. Reason: in Epic 12 the fixture stopped at compile time with the duplicate `panic_impl` error. Now it compiles and fails at link with mold `[async-lift]`, like the other 17 guests.

Epic 13 closed on 2026-11-21 after review. cargo test --workspace: 603 passed. --ignored: 93 passed. The smoke run on kind passed. The apps host build baseline is 18 link failures.

**Ruled (D, 2026-11-21): Epic 14 scope (step 3.8, second part).**
- Q1: 3.8 splits again. Epic 14 is the grant tests, the service harness and the service tests. Epic 15 is the journey harness and the journey tests on kind. Reason: the two are separate work with separate failure points, and the kind runs are where churn grew before.
- Q2: the service harness uses the real identity code with a fresh key per test, not a stub issuer.
- Q3: `cargo xtask helm-size` and the wasm-only guest build are in this epic.

**Ruled (D, 2026-11-21): Epic 14 blockers B1 to B6 and choices 1 to 13.**
- Main ruling: the host is not started in tests. wamn-host runs only on Kubernetes: TokenReview (B4), mounted Secrets and fixed paths (B2), the cluster DNS name of the database (B1), and workloads that the runtime-operator starts from an OCI digest (B6). The identity service starts NATS only with the Kubernetes API (B5). Reason: wamn-host runs only on Kubernetes (T1 to T5, TokenReview), so a host in process needs back doors or a Kubernetes API in containers, and kind gives the real thing.
- This changes the F4 text. Old: service tests start a real host in process. New: service tests call product library code with real Postgres and NATS containers, and the contracts of wamn-host are journey tests on kind. The edge (`services/edge`) has no Kubernetes code and keeps its service tests.
- Not allowed: back doors, Kubernetes API containers, a Docker network alias, a new lib target for wamn-host, and product changes for tests.
- Moved to Epic 15: the host's probes, signals, drain and NATS outage recovery; another project environment's credential is refused and never served; the request path, connection calls, workflows and Data change; any session identity or write log contract that has no production function taking an open connection.
- Issues rewritten: 14.1 grant tests as scoped, 14.2 NATS isolation service tests in place of the host harness, 14.3 tests with Postgres only and unit tests, 14.4 tooling.
- Choice 4: the NATS configuration comes from the rendered platform chart.
- Choice 6: session identity and the write log are tested only through production functions that take an open connection.
- Choice 7: the database installers move from `test-support/sql/tests/common/` into `wamn-test-infrastructure`.
- Choice 9: REFERENCES is a catalog check.
- Choice 10: extension functions are listed apart, and the test checks that the installed extensions are exactly the production list.
- Choice 11: PUBLIC EXECUTE is revoked on the three trigger functions. This narrows rights, and a trigger function cannot be called directly.
- Choice 12: the guests use the `cfg(target_arch = "wasm32")` pattern of `ingress`.
- Choice 13: the manifest step moves into one public `wamn-publish` function. `flate2` becomes a direct xtask dependency; it is already in `Cargo.lock`.
- Choices 1, 2, 3, 5 and 8 are not needed.
- Fixes under the standing fix rule: delete `replica_refusals` and `test-support/resources/{event_broker,nats_tls}.rs`, which have no caller, and fix the stale comment at `crates/provision/src/sql.rs:181`.

**Ruled (D, 2026-11-21): crate graph review.**
- The items of the review are accepted as Open items for one crate-graph epic after Epic 15, not into Epic 14, because of the freeze.
- Reason: Epic 14's tests sit in the test crates, and Epic 15's tests go through product commands, so the crate moves barely touch them.
- Reason: sqlx in the hub crates costs compile time and binary size, but nothing connects through it. The fix waits for the edge device design.
- The size of `manifest.rs` (3,136 lines) is not a defect by itself, and it is not listed.

**Ruled (D, 2026-11-21): Epic 14 questions 7 to 11.**
- Question 7: the grant cache moves to Epic 15. Reason: it reads through the `WamnPostgres` pools with a mounted platform credential, which is the fixed-path problem (B2), so a container test would need a back door.
- Question 8: result signing moves to Epic 15. Reason: `result_signing.rs` is a trait only. The signing decision is in host-only code (`operation.rs::sign_result`, and `Files::sign` in `services/host`), and the admission half already has a unit test in `component-policy`.
- Question 9: the access token refetch test stays. Reason: its real 1 s wait comes from the product's std `Instant` rate limit, and the test then asserts that a fetch happens. That is a wait for an event, not a sleep followed by an assert that nothing happened.
- Question 10: the `watch_liveness` test stays as it is. Reason: the stop when the bound passes is already tested, and the new test covers the other half as far as std `Instant` allows.
- Question 11: the cold parse, the operation in the transaction and every write log claim move to Epic 15. Reason: `commit_transaction` is `pub(super)`, and no public production function takes an open connection. It is not made public for tests, because that would be a test-only door.
- `start_nats` comes back with its 14.2 caller. This matches the Epic 13 ruling, which deleted it only because it had no caller.
