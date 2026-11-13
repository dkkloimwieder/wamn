# Audit report E: Security

Three read-only Codex gpt-6.1-sol runs at reasoning effort high wrote this file, one per part, on 2026-10-31 against commit 8c831349d. Root copied each run's final message and checked that every finding cites a file and line. Root then removed each part's proposed-epics section, because README.md holds the only plan, and marked owner review edits inline with the date. Beads: wamn-hgdta.1, wamn-hgdta.2, wamn-hgdta.3. Shared tool output: [measurements.md](measurements.md).

<!-- report E1, bead wamn-hgdta.1 -->

## Part 1: identity

### Threat model

This static review found risks in org isolation, grant revocation, and cached authority. No files changed, and no builds or tests ran. The final Git status was clean. Identity spans five libraries and one service, as recorded in measurements.md section 2.

Actors are anonymous clients, users, service principals, organization administrators, project administrators, provisioning operators, and compromised workloads. A principal is global. Organization membership and environment access are separate grants. Deactivation must remove access within one organization while preserving access elsewhere (`docs/architecture/overview.md:175`).

Assets include application data, operation grants, credential secrets, password hashes, renewal credentials, signing keys, and service availability. Trust boundaries separate clients from HTTPS endpoints, identity from PostgreSQL, hosts from environment databases, hosts from Kubernetes Secrets, and hosts from the shared NATS cache.

PATs, or personal access tokens, contain 32 random secret bytes. PostgreSQL stores their digest, and authentication compares digests in constant time (`crates/identity/platform/src/lib.rs:163`, `crates/identity/platform/src/lib.rs:783`, `crates/identity/platform/src/lib.rs:1082`). Passwords use bounded Argon2id work, a password hashing method, with two concurrent jobs. Enrollment requires 15 characters (`crates/identity/platform/src/password.rs:24`, `crates/identity/platform/src/password.rs:209`). Renewal credentials rotate once, and replay revokes their login family (`crates/identity/platform/src/password_login.rs:193`).

Session tokens bind issuer, organization, audience, authority, roles, and time. Their maximum lifetime is 900 seconds, plus 30 seconds of tolerance (`crates/identity/session/src/token.rs:16`). Cloud verification uses an explicitly configured JWKS, a public signing key set. Fetches require HTTPS, reject redirects, and replace the complete cached set (`crates/platform/runtime/src/session_keys.rs:115`, `crates/platform/runtime/src/session_keys.rs:211`).

Mutual TLS authenticates provisioning operators through a dedicated certificate authority. Any accepted operator certificate grants the same issuance authority, and writes identify `wamn:provisioning` as their actor (`services/identity/src/lib.rs:389`, `services/identity/src/lib.rs:430`, `services/identity/src/pat.rs:75`). This credential is therefore a platform-wide trust anchor.

The inspected identity router protects operator writes and requires credentials for session issuance. Public keys, health, and password entry endpoints are intentionally public (`services/identity/src/lib.rs:250`). Application anonymity requires an explicit `none` policy. Unsupported policies refuse admission (`crates/platform/engine/src/flow_http_routing.rs:769`). No additional unintended anonymous route was established. Administration requires an explicit application `admin` row, but that row grants every served operation (`crates/platform/engine/src/flow_http_routing.rs:493`). The findings below describe paths that undermine these boundaries.

## Finding E1-1: Credential resource names collide across environments

Severity: high  
Effort: S  
Area: crates/identity/credential-store/src/kubernetes.rs  
Evidence: `crates/identity/credential-store/src/kubernetes.rs:21`; `crates/control/lib/src/environment/ensure.rs:826`; `crates/control/lib/src/environment/ensure.rs:866`  
Problem: Secret and ServiceAccount names join organization, project, and environment with single hyphens. Valid coordinates such as `acme/orders-eu/prod` and `acme/orders/eu-prod` produce identical names. Provisioning reuses existing resources, so these environments share a Secret and its read authority within the same namespace.  
Proposal: Use one collision-free coordinate encoding for Secrets, ServiceAccounts, Roles, and RoleBindings: the existing double-hyphen convention. No fallback for old names.  
Deletes: The two independent single-hyphen naming formulas.  
Risk: Existing resource references change. The only deployed coordinates are `dkk--receiving--dev` and `dkk--wms--dev`, and they do not collide. A test must provision both colliding coordinates and prove that neither account can read the other Secret.

Owner review 2026-10-31: keep, Group 3 Do, small. One encoding, no fallback.

## Finding E1-2: Shared NATS credentials can forge another environment’s grants

Severity: high  
Effort: L  
Area: crates/platform/runtime/src/auth_cache.rs  
Evidence: `deploy/gcp/auth-nats-clients.yaml:1`; `deploy/platform/host-environment-certs.example.yaml:28`; `crates/platform/runtime/src/auth_cache.rs:292`; `crates/platform/runtime/src/plugins/wamn_postgres/session_identity.rs:2359`
Problem: Hosts and identity writers share the `wasmcloud-runtime` NATS identity. The inspected bundled NATS configuration grants that identity no subject restrictions. Hosts accept grant values from the shared bucket, so a stolen runtime certificate can publish an `admin` grant for a principal in another environment.  
Closed by [alignment](../plan/alignment.md) R4, R5 and R7 (owner addendum 2026-10-31): every client authenticates by a client certificate mapped to a NATS user, each environment has its own certificate and the key prefix `$KV.wamn-auth.<project_env_key>.>`, and R5's two-environment test proves that environment A's certificate is refused on environment B's subjects and keys. The audit proposal (drop the bucket, read grants from PostgreSQL) is rejected. The bucket stays per R7. The One NATS epic owns the change.

## Finding E1-3: A delayed grant can restore revoked administration

Severity: high  
Effort: L  
Area: crates/execution/host/src/control_route.rs  
Evidence: `crates/execution/host/src/control_route.rs:323`; `crates/execution/host/src/control_route.rs:337`; `crates/execution/host/src/control_route.rs:685`; `crates/execution/host/src/control_route.rs:1072`  
Problem: Administrative grants commit system rows before writing application rows, while revocation removes application rows before committing system rows. The mutex coordinates only one host process. Across two hosts, a delayed grant can write application `admin` after a revoke completes, leaving ordinary environment membership sufficient to use the restored privilege.  
Proposal: Fix the order and hold a database lock. Application rows are a projection of committed system authority. Write them while holding the system row lock for (principal, environment), so a grant and a revoke cannot interleave. Recovery re-projects the application rows from committed system state. No new owner process.  
Deletes: The grant-before-revoke ordering difference and the process-local mutex as the only coordination.  
Risk: Administration completion and recovery behavior change. A test must pause a grant after its system commit, complete a revoke elsewhere, and prove that resumed work cannot restore `admin`.

Owner review 2026-10-31: keep, high, Group 3 Do. Not one serialized owner across hosts. Ordering plus the row lock.

## Finding E1-4: Inactive organization membership is not an admission boundary

Severity: high  
Effort: S  
Area: crates/identity/platform/src/org.rs  
Evidence: `crates/identity/platform/src/org.rs:27`; `crates/identity/platform/src/org.rs:75`; `crates/identity/platform/src/org.rs:470`; `crates/identity/platform/src/control.rs:52`; `crates/identity/platform/src/session_token.rs:152`  
Problem: Grant admission reads active membership without locking its row. A concurrent deactivation can remove existing grants and mark membership inactive before the admitted grant writes new rows. Control and application session admission inspect the resulting roles or environment membership without requiring active organization membership.  
Proposal: Grant admission locks the membership row (`SELECT ... FOR UPDATE`). Session admission requires active org membership in addition to roles.  
Deletes: Role-only and environment-membership-only admission predicates for user principals.  
Risk: Previously inconsistent accounts lose access. A concurrent grant/deactivation test must prove that inactive membership cannot retain or regain authority, including control sessions.

Owner review 2026-10-31: keep, high, Group 3 Do, small.

## Finding E1-5: Identity database traffic carries signing material without TLS

Severity: medium  
Effort: M  
Area: services/identity/src/lib.rs  
Evidence: `services/identity/src/lib.rs:156`; `services/identity/src/lib.rs:165`; `crates/identity/platform/src/session_keys.rs:182`; `crates/identity/platform/src/password.rs:555`  
Problem: Identity opens PostgreSQL connections with `NoTls`. Those connections return private signing keys and password hashes, so a deployment without independent transport encryption exposes these assets to network interception. HTTPS on the public listener does not protect this boundary.  
Proposal: Require authenticated TLS for identity database connections, including certificate and hostname validation. Apply the same transport contract to environment role readers.  
Deletes: Production `NoTls` connections in the identity service.  
Risk: Database certificates and connection inputs must change. Transport tests must reject plaintext, an untrusted certificate, and a certificate for another hostname.

Owner review 2026-10-31: medium, Group 3 Do. CNPG serves TLS by default, so the change is the client side. Question 2 is answered: no independent transport encryption exists, and this finding is the fix.

## Finding E1-6: Cached PAT approval survives credential expiry

Severity: medium  
Effort: S  
Area: crates/platform/runtime/src/auth_cache.rs  
Evidence: `crates/platform/runtime/src/auth_cache.rs:72`; `crates/platform/runtime/src/auth_cache.rs:135`; `crates/platform/runtime/src/plugins/route_authentication.rs:504`; `crates/identity/platform/src/kv_memory.rs:11`  
Problem: Cached token approvals contain a receipt time but no credential deadline. A PAT accepted just before expiry remains accepted for up to 60 seconds without another identity read. A session’s cached authority approval can likewise outlive its source PAT, even while the signed session remains valid.  
Proposal: Cache entries carry the credential deadline. Approval expires at the earlier of the credential deadline and the cache deadline.  
Deletes: Deadline-free `Token` approval entries.  
Risk: Requests near expiry begin to refuse earlier. A test with an active cache must cross natural PAT expiry without publishing an invalidation and expect refusal immediately.

Owner review 2026-10-31: keep, small. Question 5 is answered: a cache delay is acceptable only after a lost invalidation. Natural expiry refuses immediately.

## Finding E1-7: Cache replication restarts the age of old authorization facts

Severity: medium  
Effort: S  
Area: crates/platform/runtime/src/kv_memory.rs  
Evidence: `crates/platform/runtime/src/kv_memory.rs:130`; `crates/platform/runtime/src/kv_memory.rs:178`; `crates/platform/runtime/src/kv_memory.rs:244`; `crates/identity/platform/src/auth_changes.rs:115`  
Problem: Watch delivery assigns `Instant::now()` to every received value and ignores its original age. A host that receives a nearly expired value gives it another full cache lifetime. This exceeds the documented 60-second backstop when an invalidation is lost.  
Proposal: Carry one absolute deadline through reads, publication, and replication.  
Deletes: Receipt-time renewal of replicated authorization values.  
Risk: Old watch entries become immediate misses. A delayed-delivery test must prove that replication never extends the original deadline.

Owner review 2026-10-31: keep, small. This adds a field. The alignment R7 rule that the `auth_cache` tests pass unchanged still holds.

## Finding E1-8: File-backed signing keys remain trusted indefinitely

Severity: low  
Effort: M  
Area: crates/identity/session/src/file_keys.rs  
Evidence: `crates/identity/session/src/file_keys.rs:30`; `crates/identity/session/src/file_keys.rs:80`; `services/edge/src/authenticator.rs:52`  
Problem: File-backed keys load once and always report fresh evidence. After a signing key compromise, removing the key from cloud publication cannot stop an edge process from accepting newly forged tokens indefinitely. The short token lifetime does not bound this risk because the attacker can sign new issuance times.  
Proposal: No trust deadline on the local key file. A revoked-list file ships with the bundle when wanted.  
Deletes: Nothing now. The unconditional `true` freshness result stays.  
Risk: A disconnected box trusts a verified token until its expiry. A test proves that a key listed in the revoked-list file is refused once that file ships.

Owner addendum 2026-10-31: unchanged. Neither [alignment](../plan/alignment.md) nor [integration-schema](../plan/integration-schema.md) covers file-backed signing keys.

Owner review 2026-10-31: low, deferred until the owner picks. The architecture ruling for the local box is: trust a verified token until its expiry, keep no session state, and a revoked-list file can ship with the bundle later. A trust deadline on the key file means a disconnected box refuses every session after the deadline.

Owner review 2026-10-31 against origin/main 64dcf04: question 3 is answered: no trust deadline. A revoked-list file ships with the bundle when wanted. Low, Group 3 In passing when the bundle format is next touched.

## Finding E1-9: CLI and HTTP routes enforce different grant rules

Severity: medium  
Effort: M  
Area: crates/control/lib/src/user_role.rs; crates/control/lib/src/role_permission.rs  
Evidence: `crates/control/lib/src/user_role.rs:152`; `crates/identity/platform/src/application.rs:436`; `crates/execution/host/src/application_route.rs:407`; `crates/execution/host/src/application_route.rs:365`; `crates/control/lib/src/role_permission.rs:49`  
Problem: The HTTP route refuses removal of application `admin` while `project-admin` covers it, but the CLI calls an unconditional delete. The HTTP permission route also rejects fixed host operations before calling an owner function that does not enforce that rule. The shared owner therefore permits states that another entry point declares invalid.  
Proposal: One authorized mutation owner for grants and revokes. CLI and HTTP call it. The covering rule (project-admin covers application admin) and the no-escalation-beyond-yourself rule live there and nowhere else.  
Deletes: Transport-specific semantic guards and direct bypass paths.  
Risk: Some existing CLI commands begin to refuse. A parity test must submit the same covered revoke and fixed-operation grant through both entry points.

Owner review 2026-10-31: keep, medium, Group 3 Do.

## Finding E1-10: An unused API-key authority remains in the application schema

Severity: low  
Effort: S  
Area: crates/identity/project-state/src/lib.rs  
Evidence: `crates/identity/project-state/src/lib.rs:270`; `deploy/sql/app-schema.sql:308`; `crates/identity/project-state/tests/authority.rs:197`  
Problem: The application schema retains an API-key table even though runtime code does not read it. The model, schema, history, and tests preserve a second credential structure beside the working system PAT authority. This adds maintenance without providing authentication behavior.  
Proposal: Delete the unused application API-key substrate. Keep credential authority in the identity owner.  
Deletes: `API_KEY`, `platform.api_key`, its history scaffolding, and tests that only preserve the unused structure.  
Risk: Schema snapshots change. Schema and authority tests must pass without the table, while PAT and session authentication tests retain their behavior.

Owner review 2026-10-31: keep as a deletion, Group 1 In passing. It is a schema baseline change.

## Open questions for the owner

1. Must org isolation survive compromise of one native host or theft of its scheduler certificate? Answered 2026-10-31 by alignment R5: a compromised host or stolen runtime certificate reaches only its own environment. That is the boundary.
2. Does every deployed identity database connection have independently enforced transport encryption? Answered 2026-10-31: no. E1-5 is the fix.
3. Owner pick (E1-8): no trust deadline on the local key file (current) plus a revoked-list file when wanted, or a deadline and its length. Answered 2026-10-31: no trust deadline. A revoked-list file ships with the bundle when wanted.
4. Is every operator certificate intentionally a platform-wide credential, and what revokes one certificate without replacing the CA? Answered 2026-10-31: yes, the provisioning operator certificate is one platform-wide principal by design. Revocation is short certificate lifetimes under cert-manager (alignment R3), not a CRL (E2-2).
5. Are cache delays acceptable only after lost invalidation, or also after natural credential expiry? Answered 2026-10-31 under E1-6: only after a lost invalidation. Natural expiry refuses immediately.

<!-- report E2, bead wamn-hgdta.2 -->

## Part 2: secrets and transport

Read-only source audit. No files changed, and no builds, tests, or live probes ran. The tests below are proposed validation.

## Finding E2-1: Valid environment names share credential Secrets and ServiceAccounts

Severity: high  
Effort: M  
Area: crates/identity/credential-store  
Evidence: `crates/identity/credential-store/src/kubernetes.rs:21`, `crates/identity/credential-store/src/kubernetes.rs:26`, `crates/control/registry/src/identifiers.rs:87`, `crates/control/lib/src/environment/ensure.rs:866`, `crates/control/lib/src/release_chart.rs:490`  
Problem: Names join org, project, and environment with single hyphens, although each component permits single hyphens. Coordinates `a/b-c/d` and `a/b/c-d` therefore produce the same Secret, ServiceAccount, Role, and RoleBinding. Both environments receive access to the same complete credential Secret, so a compromised host crosses the project boundary.  
Merged into E1-1 (owner review 2026-10-31): one encoding, the double-hyphen convention, no fallback. Question 1 is answered: no current coordinates collide. The only deployed coordinates are `dkk--receiving--dev` and `dkk--wms--dev`.

## Finding E2-2: Org-targeted control credentials retain authority across organizations

Severity: high  
Effort: L  
Area: crates/control/provision/src/sql.rs; deploy/sql/system-schema.sql  
Evidence: `crates/control/provision/src/workload_role.rs:220`, `crates/control/provision/src/sql.rs:977`, `crates/control/provision/src/sql.rs:1034`, `deploy/sql/system-schema.sql:14`, `deploy/sql/system-schema.sql:1134`, `deploy/sql/system-schema.sql:1455`  
Problem: Provisioning describes each control login as org-targeted, but its inherited grants permit writes across entire membership and role tables. These tables have no row isolation between organizations. Owner-executed connection functions also trust caller-supplied org arguments, so a stolen control credential can alter another organization's authority and connection records.
Proposal: One control login per org, holding that org's environments and no other (the admin-hierarchy ruling). Authority tables in the system database carry the org key. The control login is one database role per org. Row security on the org key covers the membership, role and connection tables. Privileged functions derive the org from the login (`current_user`), never from an argument.  
Deletes: Broad control table grants and every org argument to a privileged function.  
Risk: Control operations require new database interfaces. Extend the live denial tests with two organizations and direct SQL attempts against the other organization's rows and functions.

Owner review 2026-10-31: keep, high, Group 3 Do. Question 2 is answered: no, an org control host is trusted for its own org only.

Owner review 2026-10-31 against origin/main 64dcf04: question 4 is answered: the provisioning operator certificate is one platform-wide principal, and revocation is short certificate lifetimes under cert-manager (alignment R3), no CRL.

## Finding E2-3: Postgres connections expose sensitive traffic without transport encryption

Severity: medium  
Effort: L  
Area: deploy/**/cnpg-cluster.yaml; native Postgres clients  
Evidence: `deploy/gcp/cnpg-cluster.yaml:51`, `deploy/platform/wamn-sysdb.yaml:64`, `services/identity/src/lib.rs:165`, `services/host/src/host.rs:782`, `services/cdc-reader/src/lib.rs:150`  
Problem: Database admission rules explicitly accept non-TLS connections, and deployed identity and control clients use `NoTls`. SCRAM password authentication does not encrypt subsequent SQL traffic, including identity state, signing material, and application data. The CDC reader also defaults its replication transport to `sslmode=disable`.  
Proposal: CNPG issues the server certificates already. One client connection owner validates the CA and the server name. The CDC default becomes `sslmode=verify-full`. Then the admission rule drops plaintext.  
Deletes: Production `NoTls` connections, plaintext admission rules, and the CDC plaintext default.  
Risk: Every database consumer requires trusted certificates. Deployment tests must reject plaintext, an untrusted CA, and a wrong server name while preserving identity, provisioning, and replication operations.

Owner review 2026-10-31: keep, medium, Group 3 Do. It rides no image rebuild of its own, because host and identity change together with One NATS.

## Finding E2-4: Event NATS sends credentials and events over plaintext connections

Severity: high  
Effort: M  
Area: deploy/gcp/nats-jetstream.yaml; deploy/infra/nats-jetstream.yaml  
Evidence: `deploy/gcp/nats-jetstream.yaml:16`, `deploy/gcp/cdc-reader.yaml:59`, `crates/platform/runtime/src/plugins/wamn_jetstream.rs:638`, `services/cdc-reader/src/lib.rs:1091`, `deploy/infra/nats-jetstream.yaml:57`  
Problem: The event broker has no configured TLS listener, while deployed clients connect with usernames and passwords over `nats://`. A network observer can obtain reusable broker credentials and event contents. The three-node manifest also exposes broker routes without configured peer authentication or TLS.  
Closed by [alignment](../plan/alignment.md) R1, R3 and R4 (owner addendum 2026-10-31): one NATS with TLS on, a cert-manager CA that renews, and certificate-mapped users with no passwords. The One NATS epic owns the change. The 365-day CA question is answered by R3.

## Finding E2-5: Shared development infrastructure publishes administrator credentials

Severity: low  
Effort: M  
Area: deploy/platform/postgres.yaml; deploy/infra/minio.yaml; crates/control/provision/src/backup.rs  
Evidence: `deploy/platform/postgres.yaml:35`, `deploy/platform/postgres.yaml:119`, `deploy/infra/minio.yaml:27`, `crates/control/provision/src/backup.rs:33`, `crates/control/provision/src/backup.rs:82`  
Problem: The Postgres fixture ships a known superuser password, and MinIO ships known root credentials. Backup declarations reuse the same MinIO administrator credentials across cluster prefixes. A pod with service reachability can administer these fixtures and read or delete backups stored there. These manifests describe development infrastructure, so this evidence does not establish production exposure.  
Proposal: Delete the standing Postgres fixture and create disposable databases for tests. Replace the shared backup root identity with credentials restricted to each cluster's backup prefix and an explicit encrypted endpoint.  
Deletes: `deploy/platform/postgres.yaml`, committed MinIO credential values, and shared root credential wiring in backup declarations.  
Risk: Existing condition and recovery procedures require replacement inputs. Prove backup restoration and deny one cluster's credentials access to another cluster's objects.

Owner addendum 2026-10-31, re-targeted: [integration-schema](../plan/integration-schema.md) drops `registry.credential_import`, `registry.import_connection_credentials` and `identity.credential.secret`, so credentials enter by UI or API only and plaintext secrets never sit in Postgres. `deploy/platform/postgres.yaml` goes with the prototype set (D1-10).

Owner review 2026-10-31, re-targeted again: low, Group 1 In passing. MinIO is a local drop-in only. Cloud backups use Cloud Storage through the cluster's service account (Workload Identity), with no root credential. Delete `deploy/infra/minio.yaml` with the prototype set (D1-10) and the MinIO root wiring in `backup.rs`. No restricted backup credentials are created.

## Finding E2-6: Command help can disclose database passwords

Severity: medium  
Effort: S  
Area: services/ctl; services/cdc-reader; services/scenario-worker  
Evidence: `services/ctl/src/provisioning_verbs.rs:590`, `services/ctl/src/provisioning_verbs.rs:594`, `services/cdc-reader/src/lib.rs:125`, `services/cdc-reader/src/lib.rs:131`, `services/scenario-worker/src/management.rs:445`, `services/identity/src/cli.rs:28`  
Problem: Several password-bearing environment arguments omit `hide_env_values`, so clap includes their values in help output. The affected inputs include superuser and replication database URLs. Help captured in support logs or deployment diagnostics can therefore disclose reusable credentials, while the identity CLI already demonstrates suppression.  
Proposal: Suppress environment values for every secret argument and redact their diagnostic representations. Remove password-bearing command arguments where mounted credential files can supply the same input.  
Deletes: Secret values from generated help and unnecessary password-bearing command arguments.  
Risk: Automation that passes URLs directly requires adjustment if flags disappear. Render each command's help with synthetic secret markers and prove that no marker appears.

Owner review 2026-10-31: keep, small, Group 3 Do.

## Finding E2-7: Standalone service images default to root execution

Severity: medium  
Effort: M  
Area: Dockerfile; deploy/platform/identity; deploy/gcp/cdc-reader.yaml  
Evidence: `Dockerfile:182`, `Dockerfile:189`, `Dockerfile:196`, `deploy/platform/identity/templates/deployment.yaml:29`, `deploy/gcp/cdc-reader.yaml:40`, `deploy/platform/scenario-worker.yaml:144`  
Problem: Shipping image stages declare no `USER`, and standalone identity, CDC, and scenario-worker manifests provide no non-root execution requirement. These services therefore default to container root unless an external admission policy changes them. A native service compromise gains unnecessary filesystem privileges and container capabilities.  
Proposal: Set an unprivileged numeric image user and require non-root execution in the standalone deployments. Drop capabilities, disable privilege escalation, and provide only the writable mounts each service needs.  
Deletes: Implicit root execution and unrestricted writable container filesystems.  
Risk: UID changes affect private certificate files and writable directories. Deployment smoke tests must start each service with its real Secret mounts under the restricted identity.

Owner review 2026-10-31: keep, medium, In passing. A `USER` line in the Dockerfile rebuilds every image, so it rides the One NATS build. The identity chart gets `runAsNonRoot` in the same change. `deploy/gcp/cdc-reader.yaml` and `scenario-worker.yaml` are already deleted under D1-6 and D1-10.

## Finding E2-8: Mutable base and infrastructure images weaken release provenance

Severity: low  
Effort: M  
Area: Dockerfile; deploy/gcp; deploy/infra  
Evidence: `Dockerfile:20`, `Dockerfile:158`, `deploy/gcp/nats-jetstream.yaml:96`, `deploy/gcp/cnpg-cluster.yaml:39`, `deploy/acceptance-tests/m1-postgres.Dockerfile:1`; measurements.md section 11
Problem: Build bases and deployed database and event-broker images use mutable tags without digests. The same source revision can therefore resolve different executable inputs across builds or node pulls. The shared measurements contain no container vulnerability scan, so this finding establishes an input-provenance gap.  
Proposal: Pin the two infrastructure images (NATS, CNPG postgres) by digest in the one NATS manifest and `cnpg-cluster.yaml` when alignment rewrites them.  
Deletes: Tag-only references for the two infrastructure images.  
Risk: Image updates become deliberate changes to two manifests. Deployment qualification exercises each replacement digest.

Owner review 2026-10-31: low, In passing, narrowed. The manifest check and the base-image digest pins are rejected. The Rust base follows `rust-toolchain.toml`, and a digest pin is churn on every toolchain bump.

## Open questions for the owner

All five questions were answered on 2026-10-31.

1. No current coordinates collide.
2. No. An org control host is trusted for its own org only (E2-2).
3. None. No NetworkPolicy exists. The cluster network is the boundary, and wamn-dev is a development cluster. E2-3 and One NATS close the plaintext part.
4. Alignment R3: cert-manager owns the CA and renews it.
5. Callers require the default server setting (`standard_conforming_strings` on). pg-core states that precondition in its doc comment. Nondefault parsing is unsupported. This closes the A2-11 question too.

<!-- report E3, bead wamn-hgdta.3 -->

## Part 3: sandbox and supply chain

This review used source inspection and the shared measurements. No files changed, and no builds or tests ran. Tests named below are proposed validation.

Application component admission refuses direct sockets, filesystem, and WASI HTTP imports. Granted platform capabilities still permit network effects through host bindings (`crates/platform/component-policy/src/lib.rs:181`, `crates/platform/component-policy/src/lib.rs:382`). Connector stores enforce an exact network authority independently of the application engine (`crates/platform/engine/src/connector.rs:138`, `crates/platform/engine/src/connector.rs:275`). Application workloads declare no filesystem volumes (`crates/platform/engine/src/operation/native_workload.rs:390`).

Execution uses deadlines bounded to 1–30,000 ms and native epoch interruption. WAMN grants no instruction fuel. Each linear memory has a 256 MiB ceiling, but aggregate guest memory uses count mode. A separate sampled process guard remains active (`crates/platform/engine/src/router_delivery.rs:475`, `docs/architecture/execution.md:371`).

## Finding E3-1: Registry descriptors can bypass byte admission

Severity: high  
Effort: S  
Area: `crates/catalog/package-artifact`, `crates/platform/engine`  
Evidence: `crates/catalog/package-artifact/src/lib.rs:5`, `crates/control/lib/src/publish_release.rs:1621`, `crates/catalog/model/src/component_facts.rs:460`, `crates/platform/engine/src/operation/native_workload.rs:254`, `crates/platform/engine/src/engine.rs:654`  
Problem: Package existence is treated as proof that publication admission ran. Publication accepts descriptor declarations, while runtime loading checks the component digest without rederiving its imports. A registry writer can therefore supply matching hashes and false admission facts. If that component reaches execution, the default socket policy permits undeclared connections.
Proposal (final, owner 2026-10-31): The host links application components against their declared world and nothing else. An undeclared import fails at instantiation, which `release.qualify` exercises. No descriptor comparison and no `wasmparser` check. The fork implements `wasi:sockets` unconditionally, which is the recorded egress bypass.  
Deletes: The unconditional `wasi:sockets` implementation for application components.  
Risk: Packages with undeclared imports refuse. A crafted package test must hide a socket import and prove refusal at instantiation.

Owner addendum 2026-10-31: [alignment](../plan/alignment.md) W1 to W3 change what admission matches (package family `0.2` and a build-derived ceiling). Admission work must not re-pin exact versions.

Owner review 2026-10-31: split. The rejected half is deriving admission facts from bytes at publication and comparing descriptors. Registry write access is the trusted boundary, and publication relies on OCI and WIT. Question 1 is answered: yes. The kept half is the runtime socket refusal above, Group 3 Do, small. It is independent of W1 to W3 and does not wait.

Owner review 2026-10-31 against origin/main 64dcf04: WASI version match closed on main (wamn-cqbb0; `component-policy` carries `WASI_FAMILY` and a build-derived `WASI_CEILING`). The runtime `wasi:sockets` refusal waits on nothing.

Owner review 2026-10-31 (developer-experience walk): final. The host links application components against their declared world and nothing else, and an undeclared import fails at instantiation, which `release.qualify` exercises. No descriptor comparison, no `wasmparser` check. Group 3 Do, small, unchanged.

## Finding E3-2: The native lockfile contains reported security vulnerabilities

Severity: high  
Effort: M  
Area: `Cargo.lock`  
Evidence: measurements.md section 5, `Cargo.lock:7260`, `Cargo.lock:7487`, `Cargo.lock:7512`, `docs/audit/raw/audit-root.txt:57`  
Problem: The refreshed measurement reports 17 vulnerabilities and 12 warnings in the root dependency graph. Wasmtime resolves to 48.0.7, despite the manifest requirement spelling 48.0.5. The recorded advisories name 48.0.8 as a patched version for the listed Wasmtime findings. Lockfile presence alone does not prove that every affected API is reachable.  
Proposal: Upgrade the affected native dependency families together. Record feature and API reachability for findings that remain. Preserve the single Wasmtime source and version contract.  
Deletes: Vulnerable dependency resolutions and obsolete version assertions.  
Risk: Runtime APIs and component behavior can change. Run the source-identity, admission, connector, deadline, and cancellation suites against the replacement graph.

Owner review 2026-10-31: keep, high. A dependency bump is the most expensive action, so the affected families (Wasmtime to 48.0.8 and the rest named in measurements section 5) are updated in the One NATS build, not in a build of their own. Record which advisories remain reachable after that update. No separate run.

## Finding E3-3: Package pulls and extraction have no resource budget

Severity: low  
Effort: S  
Area: `crates/catalog/package-artifact`, `crates/control/lib/src/package_artifact.rs`  
Evidence: `crates/catalog/package-artifact/src/registry_pull.rs:23`, `crates/catalog/package-artifact/src/lib.rs:391`, `crates/control/lib/src/package_artifact.rs:1223`, `crates/control/lib/src/package_artifact.rs:1267`  
Problem: Registry reads buffer complete layers, and extraction reads each complete entry into another buffer. The code imposes no aggregate byte, layer-count, or file-count limit before extraction and repacking. A malicious artifact can exhaust application memory or temporary storage before its final refusal. Regular-file and relative-path restrictions prevent archive links and ordinary traversal, but do not bound resource use.  
Proposal: A byte cap on blob reads and an entry cap on extraction, added when `package-artifact` is next touched. No streaming rewrite.  
Deletes: Uncapped blob reads and uncapped archive entry counts.  
Risk: Large legitimate packages can refuse. Boundary tests must cover oversized blobs, excessive entries, duplicate paths, traversal, and cleanup after failure.

Owner review 2026-10-31: low, In passing in package-artifact. Artifacts come from the project's own registry, which is the trusted boundary.

## Finding E3-4: Aggregate guest memory remains observational

Severity: high  
Effort: S  
Area: `crates/platform/engine/src/engine.rs`, host and edge configuration  
Evidence: `crates/platform/engine/src/engine.rs:119`, `crates/platform/engine/src/engine.rs:843`, `services/host/src/host.rs:275`, `services/edge/src/serve.rs:167`, `deploy/platform/release/values.yaml:59`  
Problem: Count mode records allocations beyond the shared guest budget and permits them. The 256 MiB ceiling applies to each linear memory, rather than the complete component or host. The sampled process guard and container limit mitigate exhaustion, but do not reserve aggregate guest memory before allocation. This behavior follows an existing owner ruling.  
Proposal: Use aggregate memory enforcement for serving hosts and connector engines. Keep count mode confined to explicit measurement environments.  
Deletes: Serving defaults that select `GuestMemoryMode::Count`.  
Risk: Workloads that currently exceed the budget refuse. Extend `enforce_mode_refuses_aggregate_growth_until_another_store_releases_memory` to cover deployed host, edge, and connector construction.

Owner review 2026-10-31: deferred. Count mode is an existing owner ruling. The condition that ends it is the post-A5 re-measurement with the build record's numbers, nothing before (question 2).

## Finding E3-5: Execution deadlines do not contain compilation

Severity: low  
Effort: S  
Area: `crates/platform/engine/src/component_admission.rs`, `operation/native_workload.rs`  
Evidence: `crates/platform/engine/src/component_admission.rs:195`, `crates/platform/engine/src/component_admission.rs:369`, `crates/platform/engine/src/lib.rs:85`, `crates/platform/engine/src/operation/native_workload.rs:400`  
Problem: Admission compiles the component before applying its import policy. Import discovery also invokes full compilation. Runtime loading moves compilation into `spawn_blocking`, whose work continues after its awaiting caller is cancelled. Guest memory ceilings and epoch deadlines do not bound compiler allocations or CPU use.  
Proposal: Import listing uses `wasmparser` instead of a full compile. That is cheaper, not a security mechanism, and nothing enforces on it. Fix detached compilation after caller cancellation when `component_admission.rs` is next changed.  
Deletes: Full compilation solely for import discovery and compilation that outlives a cancelled caller.  
Risk: Structural analysis must preserve named-interface and nested-component semantics. Adversarial compilation tests must prove bounded resource use and termination after caller cancellation.

Owner review 2026-10-31: low, In passing.

## Finding E3-6: Connector code remains retained after bindings retire

Severity: medium  
Effort: M  
Area: `crates/platform/engine/src/connector.rs`, `crates/platform/runtime/src/connectors.rs`  
Evidence: `crates/platform/engine/src/connector.rs:119`, `crates/platform/engine/src/connector.rs:126`, `crates/platform/engine/src/connector.rs:328`, `crates/platform/runtime/src/connectors.rs:27`, `crates/platform/runtime/src/connectors.rs:131`  
Problem: Process-wide connector engines retain a compiled template for every admitted digest. Runtime readers separately retain component bytes in an unbounded map. Binding invalidation removes linked instances but leaves these digest-indexed copies. Repeated connector upgrades therefore retain obsolete artifacts until their owning process or reader disappears.  
Reshaped by [integration-schema](../plan/integration-schema.md) "Runtime refresh" (owner addendum 2026-10-31): a version's component is compiled once per digest, its instance is linked per (connection, generation, credential), and calls in flight finish on what they started with. Retention bounds are set there. The audit files nothing separate.

## Finding E3-7: Engine construction mutates the environment after threads start

Severity: medium  
Effort: S  
Area: `crates/platform/engine/src/engine.rs`  
Evidence: `crates/platform/engine/src/engine.rs:242`, `services/host/src/main.rs:59`, `services/host/src/host.rs:1089`, `services/host/src/host.rs:1108`  
Problem: Engine construction calls unsafe `std::env::set_var` for the abandoned-call grace. Its safety argument covers Rust environment locking but assumes away concurrent native environment readers. The host creates its multithreaded runtime and starts connection work before this call. `Once` serializes the setter without establishing the required safety condition.  
Proposal: Set the grace before the runtime starts. No upstream issue.  
Deletes: Environment mutation from the shared engine constructor and its incomplete safety argument.  
Risk: Every executable that constructs an engine must retain zero-grace cancellation. Startup subprocess tests and the existing cancellation suites must prove that behavior.

Owner review 2026-10-31: keep, small, Group 3 In passing. It rides the One NATS host build.

## Finding E3-8: Dependency identity conditions do not provide a refresh policy

Severity: low  
Effort: S  
Area: `tools/repo-lint`, dependency locks, vendored dependencies  
Evidence: measurements.md sections 4 and 5, `tools/repo-lint:304`, `tests/conformance/tests/wasmtime_source_identity.rs:167`, `vendor/pg-walstream/WAMN-PATCH.md:1`, `vendor/pg-walstream/Cargo.lock:1142`  
Problem: The prior advisory database reported zero findings because it remained dated April 7. Repository lint runs no advisory refresh or scan. Shared measurements cover two lockfiles, while this checkout contains six first-party lockfiles plus the vendored lockfile. Source-identity conditions preserve selected versions but provide no security refresh cadence.
Proposal: Remove the confirmed unused runtime declarations for `percent-encoding` and `rand_core` (`crates/platform/runtime/Cargo.toml:17`, `crates/platform/runtime/Cargo.toml:70`), with A1-6. `cargo audit` runs by hand whenever a bump is planned.  
Deletes: The two unused dependency declarations.  
Risk: None beyond a rebuild of the runtime crate.

Owner review 2026-10-31: the advisory-refresh check and the release block are rejected. No owner and no refresh interval exist. Whoever plans the next bump runs `cargo audit` then. `vendor/pg-walstream` stays until upstream has the typed keepalive API. Low, In passing with A1-6.

Owner review 2026-10-31 against origin/main 64dcf04: the adapter-digest answer is verified against wamn-cqbb0.2 at filing. If the constant is gone, the row closes.

## Open questions for the owner

All five questions were answered on 2026-10-31.

1. Yes. Registry write access is the trusted boundary (E3-1).
2. After the A5 re-measurement, with the build record's numbers (E3-4).
3. No owner and no interval. Whoever plans the next bump runs `cargo audit` then. WASI-Virt and the adapter follow the Wasmtime bump.
4. No web advisory scan. The pnpm cooldown policy and the pinned wfe revision are the web supply-chain controls.
5. Alignment W2 keeps the adapter digest exact and its test fails on change, so the constant in component-virtualizer is redundant. Derive it at build in the W1 to W3 change.

