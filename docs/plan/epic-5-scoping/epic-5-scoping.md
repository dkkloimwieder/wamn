# Epic 5 scoping report

Scope: Epic 5 in `docs/plan/review-restructure.md` ("Environment boundary"). It covers T1, T2, T4, T5 and T10. It also covers `connection_event` (R6), the `platform.credential` history and hint fix (L8), and the five T16 bullets marked Epic 5: the connection budget, the lock per environment, the credential hint, the connection responses and the parameter schemas. The checkout was read at commit `c2ce2a7a3`. This report changes no code.

F means read in the code or in a vendored chart at that commit. I means inference or proposal. Five read-only agents wrote the inventories in the appendices, and each appendix gives every `path:line`. Root checked the facts that the beads, corrections and questions depend on: the host environment check, the web route rule, the NATS account lock, the NATS user signing route, the project chart upgrades in project Jobs, the org quota, the scheduler credentials and the name length limits.

- `appendix-a-namespaces.md`: every object in `<org>` and where it goes, namespace readers, provisioning, deactivation, the host environment, network policies and journeys (T1, T2).
- `appendix-b-logins-identity.md`: system database logins, row security, the namespace mapping, the NATS credential exchanges, the account lock and the connection budget (T1, T16).
- `appendix-c-accounts-admission.md`: the org worker account, the rights of each Job kind, the T4 accounts, the admission policy draft and its test (T4).
- `appendix-d-quotas-nats.md`: quotas and pod resources, NATS limits, the `maxProjects` removal, revocation and Job credentials (T5, T10).
- `appendix-e-connections.md`: the hint, credential history, `connection_event`, connection responses, parameter schemas, images and test selection.

## 1. Corrections to the plan

All are F.

1. T1 says that the CA values leave the org chart. Epic 4 already did that. The org chart receives no CA value (`crates/platform-install/src/org.rs:136`). The findings table rows 1, 9, 12 and 17 and the current-state lines of T3, T9, T12 and T13 also describe the state before Epic 4.
2. Finding 8 says that the quota counts objects only. The org quota also limits `requests.storage` to 500Gi (`deploy/charts/org/values.yaml:7`).
3. T10 says that the scheduler connection reads its creds file once. The scheduler makes a key in memory and asks identity for a JWT at start (`services/host/src/host.rs::connect_scheduler_nats`). Only the data, workflow and CDC connections read a creds file (`crates/broker/src/nats.rs::credentials_options`).
4. T10 says, as I, that identity gives a project only its own recovery user. Today `POST /nats/users` checks the org only. The request body names the env, the purpose and the project (`services/identity-service/src/nats.rs::sign_user`). So any org worker token can get the recovery user of another project in the same org.
5. Section 5 goal 5 says that the database and NATS already separate dev from prod in one org. The org apply login reads every env of its org through row security (`deploy/sql/system-schema.sql:1197`), and the org worker chooses the NATS account in the request body. Only the project databases and the accounts themselves are separate.
6. T4 says that a project account holds no Role, Deployment or Job rights. Deploy, schema apply, schema reset and capture-gap recover and close upgrade the project chart, which renders Roles, RoleBindings and the CDC Deployment (`crates/env/src/env/operation.rs:409`, `crates/env/src/env/capture_gap.rs:14`). Schema reset also upgrades the shared host release (`crates/env/src/env/operation.rs:498`). Deploy and rollback read their own Job (`crates/env/src/env/operation.rs:136`). The account change alone breaks these paths. The T4 Job list also omits the `platform.schema` Job, which runs as the org worker. `schema.diff` already runs as `<coordinate>-operation`.
7. T2 says that `env apply` does not change. Today `env apply` creates the `registry.env` row when it is absent (`crates/env/src/registry/deployment.rs:194`). After T2, it must refuse an env that the operator did not provision.
8. T1 moves the route ConfigMaps into `<org>--<env>`. The web proxy drops every route whose host label is not `<prefix>--<namespace>` (`crates/routes/src/route_host.rs::valid_route_host`). So the move alone drops every app URL `<app>--<org>.<base>`. Public addresses are Epic 6. Question 4 asks how Epic 5 keeps them.
9. T10 says that deleting a project revokes its users and that deleting an env removes its account. R6 says that neither is deleted, only deactivated. Today the account is retired when the last project leaves the env (`crates/env/src/env/broker.rs::after_delete`).
10. T10 says that no signing-key layer is kept. T6 removes that layer in Epic 7. The two sections give the same work to two epics.
11. T2 says that provisioning creates the env backup bucket with admin for the env account. T7 leaves the grant scope open until the GKE check in Epic 7.
12. The T16 bullet on network calls in transactions says that `ensure_env` calls the NATS resolver inside its transaction. Only the resolver lookup and the KMS calls run inside. The push and the delete run after commit (`crates/identity/src/identity/nats_store.rs:87`, `services/identity-service/src/nats.rs:249`).
13. The T16 hint bullet says that every connection response returns the hint. Five responses do: write, copy, set_credential, get and list (`crates/routes/src/host_route.rs:748`).
14. R6 says that `connection.deactivate` deletes the connection's Kubernetes Secrets. It removes the connection's credential versions from the one project Secret `wamn-credential-<project-env-key>` after commit (`crates/kubernetes/src/kubernetes.rs:78`).
15. The T16 budget bullet counts 2 x 8 identity connections in the computed 206. Identity also opens direct connections for workload routes and keeps one dedicated connection per replica for JWKS and health (`services/identity-service/src/lib.rs:172`, `:244`, `services/identity-service/src/users.rs:141`). The near-12 org count is a measured observation, not a computed ceiling.
16. Stale names: `crates/environment` is now `crates/env`, `environment_key` is `name::env_key`, and the section 8 cases are now `app_routes_kind`, `app_stop_leaves_its_host` and `env_apply_stops_an_unlisted_app_and_keeps_its_data`. Section 7 lists Part 1 edits for T1, T2, T4, T5 and T10 that Part 1 already contains.

## 2. Proposed beads

| Bead | Content | Depends on |
| --- | --- | --- |
| E5.1 | Typed connection responses and no hint (T16). The handlers build `ConnectionSummary` from selected columns, not from `to_jsonb(c)` (`crates/connection/src/project_connections/administration.rs`). The hint goes from `platform.credential`, from `store_credential` and from `wamn_routes::host_wire::ConnectionSummary`. Regenerate the org-control client. | Question 1 |
| E5.2 | Closed auth schemas (T16). `"additionalProperties": false` on the `api_key`, `oauth2_client_credentials`, `oauth2_authorization_code` and `aws_sigv4` parameter schemas in `deploy/sql/system-schema.sql`. | none |
| E5.3 | Credential history (L8). Add `platform.credential` to the history loop in `deploy/sql/catalog-schema.sql`. Its grants follow `pat_history`, not the general pattern, so the query role cannot read it (`crates/env/src/project_setup.rs`). | E5.1, Question 2 |
| E5.4 | `connection_event` (R6), as ruled. | E5.1, Question 3 |
| E5.5 | Pod requests (T5). Requests on control, the CDC reader, the env Job and the install Jobs (`deploy/charts/org/templates/control.yaml`, `deploy/charts/project/templates/cdc-reader.yaml`, `services/control/src/platform.rs`, `crates/platform-install/src/org_install.rs::org_job`). | none |
| E5.6 | Connection budget (T16). Pooler pools, the `max_connections` arithmetic and identity's real connection count (`deploy/charts/platform/templates/_system-database.tpl`, `system-database-poolers.yaml`, `services/identity-service/src/lib.rs`). | Question 9 |
| E5.7 | One lock per environment (T16). A session advisory lock per issuer, org and env on one pinned connection replaces the issuer-wide transaction lock. The resolver lookup and KMS move out of the transaction. A short transaction compares and stores the revision. The push runs after commit under the same session lock (`crates/identity/src/identity/nats_store.rs`, `services/identity-service/src/nats.rs`). | none |
| E5.8 | NATS limits as data (T10). Per-env limits in the system database. Store size in bytes. `global.maxProjects`, `registry.meta.max_projects`, `registry.project_capacity_available` and their 20 files go (appendix d). | E5.6, E5.7, Question 10 |
| E5.9 | Mapping and env worker login (T1). A unique namespace column on `registry.env`. A worker login per env and `registry.login_env()`. The env check on the eight env tables and in `identity.project_access_grant`. The worker loses `INSERT` on `registry.env`. | E5.6, Question 5 |
| E5.10 | Env provisioning (T2, T5). The operator command makes `<org>--<env>` and `wamn-helm-<org>--<env>` with their labels, the env account and Role, default-deny, the quota and LimitRange from data, and the worker login Secret. trust-manager and the network policies select env namespaces. `env apply` refuses an env that is not provisioned. | E5.5, E5.9, Questions 5, 6 and 14 |
| E5.11 | Namespace cutover (T1). Control, the worker, Helm storage, the project database endpoint, the CDC reader, the host group and the workloads target the env namespace. The host reports its pod namespace (`services/host/src/host.rs`). The web proxy keeps app URLs as ruled. | E5.10, Questions 4 and 15 |
| E5.12 | Job accounts and admission (T4). The env account runs env work. `<project>-operation` runs project work with named rights. One ValidatingAdmissionPolicy and binding in the platform chart. | E5.11, Questions 7, 8 and 12 |
| E5.13 | Env-scoped identity (T1, T10). Identity reads the mapping after TokenReview and checks env, account and project. Stream-admin and recovery users become JWTs in Job memory with an expiry, and their Secrets go. Revocation on deactivation. | E5.8, E5.9, E5.12, Questions 11 and 13 |
| E5.14 | Epic close. One host image build, then the kind journeys on a fresh cluster, after your go. Plan and Part 1 corrections. | all |

E5.2, E5.5 and E5.7 start at once. E5.1 and E5.6 start after their rulings. They own separate files. Root resolves the overlap in `deploy/sql/system-schema.sql` (E5.2, E5.8, E5.9) and in the platform chart (E5.6, E5.8, E5.10) at merge. E5.9 to E5.13 run in order, because each one changes the namespace and authority that the next one reads.

## 3. Changes

### Schema

All are I, on the bootstrap SQL only, with no migration.

| Change | File and symbol |
| --- | --- |
| Drop `platform.credential.hint` (Question 1). | `deploy/sql/catalog-schema.sql:82` |
| New `platform.credential_history`, private grants. | `deploy/sql/catalog-schema.sql`, `crates/env/src/project_setup.rs` |
| New `platform.connection_event` with lookup tables, if ruled (Question 3). | `deploy/sql/catalog-schema.sql` |
| Four auth parameter schemas close. | `deploy/sql/system-schema.sql:1105` to `:1116` |
| `registry.env` gets a unique `namespace` column (Question 5). | `deploy/sql/system-schema.sql:595` |
| One worker login per env, named from org, env and database. `registry.login_env()` beside `registry.login_org()`. | `deploy/sql/system-schema.sql:164`, `deploy/sql/system-pooler-auth.sql`, `crates/platform-install/src/provision_org.rs` |
| The env check on `registry.env`, `project_env`, `env_app`, `env_integration`, `env_app_connection`, `event_reader`, `capture_gap`, `identity.project_env_membership`, `provisioning.platform_event` and `identity.project_access_grant`. The worker loses `INSERT` on `registry.env`. | `deploy/sql/system-schema.sql:1197`, `deploy/sql/apply-grants.sql` |
| New env quota data (Question 14). | `deploy/sql/system-schema.sql` |
| New env NATS limits and storage reservation (Question 10). | `deploy/sql/system-schema.sql`, `identity.nats_key` |
| Drop `registry.meta.max_projects` and `registry.project_capacity_available`. | `deploy/sql/system-schema.sql:127`, `:909` |
| New `identity.nats_user` with public keys only, for revocation (Question 11). | `deploy/sql/system-schema.sql` |
| New outcome code `env_mismatch` in the `authentication_event` outcome table. Decided under rule 1: a new lookup row, no new column. | `deploy/sql/system-schema.sql:694` |

### Wire

| Change | File and symbol | Mark |
| --- | --- | --- |
| The connection summary loses `hint` in five responses, the control OpenAPI, the org-control client and the CLI output. | `crates/routes/src/host_wire.rs:126`, `apps/platform_ui/src/generated/wamn_control_org/types.gen.ts` | F today, I change |
| Write, copy, set_credential and activate refuse an undeclared auth key. | `crates/connection/src/administration.rs::validate_connection_parameters` | I |
| A new operator CLI command creates an env. `env apply` refuses an env with no mapping row. | `services/cli/src/env_requests.rs`, `crates/env/src/registry/deployment.rs:194` | I |
| The host reports `<org>--<env>` as its environment, and workloads are placed in that environment. | `services/host/src/host.rs:469`, `:651`, `crates/env/src/app_chart/plan.rs:482` | F today, I change |
| Kubernetes objects move from `<org>` and `wamn-helm-<org>` to `<org>--<env>` and `wamn-helm-<org>--<env>`, with new env labels. | appendix a, "Object placement" | I |
| The chart values `global.maxProjects` and `identity.natsLimitsPerProject` go. A store size in bytes replaces them. Control loses `--max-projects`. | `deploy/charts/platform/values.yaml:10`, `:76`, `services/control/src/main.rs:28` | I |
| The account JWT carries per-env limits, two streams and 512 MiB per project, and `revocations`. Job users carry `exp`. | `crates/identity/src/nats.rs::env_limits`, `crates/identity/src/nats/claims.rs` | I |
| `POST /nats/users` and `POST /nats/accounts` check the mapped env, the account and the project. The body shape stays. | `services/identity-service/src/nats.rs:219`, `:289` | I |
| The stream-admin and recovery Secrets go. | `crates/env/src/env/broker.rs:12`, `:44` | I |
| One ValidatingAdmissionPolicy and one binding. | `deploy/charts/platform/templates` | I |
| The web proxy reads Namespace labels, if Question 4 is ruled (a). | `deploy/charts/platform/templates/edge-rbac.yaml`, `crates/kubernetes/src/web_release.rs:53` | I |

### Behaviour

| Change | File and symbol | Mark |
| --- | --- | --- |
| Env applies serialize per env, not per installation. | `crates/identity/src/identity/nats_store.rs:27` | F today, I change |
| Storage admission counts bytes, not projects. | `crates/env/src/env_sql_effects.rs:629` | I |
| A compute quota and a LimitRange in each env namespace. Requests on every wamn pod. | appendix d, "Proposed quota contract" | I |
| An env worker reads and writes only its own env rows. | appendix b, section 1 | I |
| Identity signs a NATS user only for the caller's own env and project. | appendix b, section 3 | I |
| Project deactivation in an env revokes that project's NATS users. | appendix d, "Deactivation" | I |
| The admission policy refuses `secretFrom`, `configFrom`, a Service that is not a selectorless ClusterIP, another Service name, and an egress policy of another shape. | appendix c, "Admission policy draft" | I |

## 4. Network calls and open transactions

The new work adds no network call inside an open transaction (I). Each new path follows a pattern that the code already uses (F):

- Env provisioning commits its SQL first, then creates the Kubernetes objects and writes the login Secret, as org creation does (`crates/platform-install/src/provision_org.rs:154`).
- Control reads the mapping row and commits before it submits or observes a Job (`services/control/src/control_route/operations.rs:47`).
- Identity runs TokenReview before its SQL, then reads the mapping (`services/identity-service/src/users.rs:121`).
- Quota changes commit the data, then apply the Kubernetes objects.
- NATS limits and revocations: a session advisory lock is held on one pinned connection, not inside a transaction. The KMS calls and the resolver lookup run between short SQL transactions. The push runs after commit. A failed push leaves the stored revision, and the next request publishes it again. The global storage reservation is a short SQL-only step.
- Job JWTs: TokenReview and the Pod and Job reads run first, then the signing, then the standalone event insert.
- `connection_event` success rows commit with their SQL change. Rows after a remote call commit in their own short transaction.
- Credential history is a trigger. It makes no network call.

Four pre-existing places are in code that Epic 5 rewrites. Question 13 asks whether to fix them in the bead that rewrites them:

- `crates/identity/src/identity/nats_store.rs::ensure_env` runs the resolver lookup and KMS inside its transaction. E5.7 rewrites it, and the lock item cannot keep the revision check correct without moving them.
- `crates/env/src/env/lifecycle.rs` holds the system transaction across Helm recovery and the host upgrade. E5.11 changes its namespace target.
- `services/control/src/control_route/integration.rs` patches credential Secrets inside the project transaction. E5.1 changes the credential writer.
- `services/control/src/control_route.rs::set_status` reads login Secrets inside the system transaction. E5.11 changes the namespace that it reads, and E5.13 adds revocation after it.

The other pre-existing places in the transaction audit stay T16 bullets.

## 5. The host image rebuild

`services/host/src/host.rs` forces the rebuild (F). It refuses a pod namespace that is not the org (`:469`), and it reports `settings.org` as its environment (`:651`). Other Epic 5 changes reach crates in the host image (F, `services/host/Cargo.toml:8`): `wamn-connection` and `wamn-routes` (E5.1), `wamn-postgres`, `wamn-registry` and `wamn-kubernetes` (E5.11), and `wamn-identity-protocol` (E5.13). The host image also holds the CDC reader.

To keep to one build, land every host-linked bead before E5.14, and run no kind run before E5.14. Each bead runs its unit tests, its live tests and offline chart renders. The build at E5.14 builds all five targets once, and the journeys use that one bundle, as in E4.11.

## 6. Where the plan does not say enough

- R6 names `connection_event` only as a possible foreign key. No section gives its rows, writers, readers or retention (Question 3).
- T2 does not name the command, its retry contract or its result after a partial failure (Question 5).
- T5 gives no quota or LimitRange values (Question 14).
- T10 gives no store size, no reservation rule and no rule for a limit that would shrink below use (Question 10).
- T1 and R6 do not say what env deactivation does to the namespace, the hosts, the streams or the NATS account (Question 11).
- T4 does not say who runs the project chart and host upgrades, who may write Helm history, or how kind runs Deny for the refusal test (Questions 7, 8 and 12).
- The T16 budget bullet gives no target org count, env count or load level (Question 9).

## 7. Questions for rulings

1. The hint. (a) Drop the column and the field. (b) Keep a value that shows no secret characters, for example the auth type and the generation. Recommendation: (a). Fresh install, so no old rows need a purge.
2. Credential history readers. (a) Private history, readable only by the platform login, like `pat_history`. (b) No history: the immutable rows are the record. Recommendation: (a).
3. `connection_event`. (a) A project table with type and outcome lookup tables, immutable rows, a required foreign key to the connection, and rows for activate, deactivate, credential change, OAuth authorize, callback and refresh outcomes. Operators read it. (b) Lifecycle rows only, no OAuth. (c) Leave it out of Epic 5. Recommendation: (a), with the Epic 4 rules for success and refusal writes.
4. App URLs after the move. (a) The web proxy reads an org label that provisioning sets on each namespace, with a Namespace list and watch as T11 already plans. URLs do not change. (b) App URLs become `<app>--<org>--<env>.<base>` now. (c) Move T11 into Epic 5. Recommendation: (a).
5. Mapping and provisioning. (a) A unique `namespace` column on `registry.env`, and one idempotent operator command `wamn env create <org>/<env>` that a retry can finish. (b) A separate mapping table. Recommendation: (a). Provisioning also checks that org and env fit the namespace and database name limits.
6. Cloud scope of T2. (a) Kind only in Epic 5: namespaces, RBAC, SQL and the local registry pull. Bucket and repository grants stay in Epic 7. (b) Write the GCP branch now, untested. Recommendation: (a).
7. Project chart and host upgrades under T4. (a) Control runs them as an env-account phase of the same operation. This changes Part 1's one-Job rule. (b) The project account gets named rights on its own CDC Deployment and Role, as an exception to T4. (c) Env apply owns them, and project Jobs wait for it. Recommendation: (a).
8. Helm history writes. (a) Project accounts write release records in the env's Helm namespace, recorded as an accepted risk, because projects of one env already trust each other. (b) A Helm namespace per project for app releases. (c) All Helm writes go through the env account. Recommendation: (a).
9. Connection budget. (a) A fixed allowance per org and a cap on envs per org, session Poolers kept, and identity counted at its real use of about 36 connections. (b) One shared ceiling with queueing. Recommendation: (a). It needs your target for orgs per system database and envs per org.
10. NATS storage. (a) Limits per env in the system database, a 16 GiB store on a 20 Gi volume, and a short SQL-only reservation. Identity publishes again an unpublished revision. A shrink below use is refused. (b) Keep 8 GiB, which holds 16 projects at the new default. Recommendation: (a).
11. Deactivation and revocation. (a) Env deactivation keeps the namespace, the account and the streams, and identity refuses new credentials for it. Project deactivation in an env revokes that project's users through `revocations`, with a public-key ledger. (b) Env deactivation removes the account and stops the hosts. Recommendation: (a).
12. Admission on kind. (a) A platform value selects Deny. The kind refusal case is a server-side dry run as the project account, as the privileged-pod case is. (b) A fixture bundle with a bad chart, deployed through the product path. Recommendation: (a).
13. Pre-existing transaction places in rewritten code (section 4). (a) Fix each one in the bead that rewrites its function. (b) Fix only `ensure_env`, and keep the other three as T16 bullets. Recommendation: (a).
14. Quota values. (a) Keep the nine object and storage limits per env, and add 8 CPU, 16Gi requested memory and 32Gi memory limits, with LimitRange defaults of 100m, 256Mi and a 2Gi limit. All are provisional, not measured. (b) You set the values. Recommendation: (a).
15. The local OAuth key. The host and control both mount `wamn-oauth` from `<org>` today. (a) One dev key per env namespace. (b) Copy the org key into each env. Recommendation: (a), so dev and prod share no key.
16. The signing-key layer. (a) Keep it until T6 in Epic 7. (b) Remove it in Epic 5. Recommendation: (a).
