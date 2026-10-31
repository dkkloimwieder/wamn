# Audit lane E: Security

Three read-only Codex gpt-6.1-sol runs at reasoning effort high wrote this file, one per part, on 2026-10-31 against commit 8c831349d. Root copied each run's final message verbatim and checked that every finding cites a file and line. Beads: wamn-hgdta.1, wamn-hgdta.2, wamn-hgdta.3. Shared tool output: [measurements.md](measurements.md).

<!-- lane E1, bead wamn-hgdta.1 -->

## Part 1: identity

### Threat model

This static review found risks in tenant isolation, grant revocation, and cached authority. No files changed, and no builds or tests ran. The final Git status was clean. Identity spans five libraries and one service, as recorded in measurements.md section 2.

Actors are anonymous clients, human users, service principals, organization administrators, project administrators, provisioning operators, and compromised workloads. A principal is global. Organization membership and environment access are separate grants. Deactivation must remove access within one organization while preserving access elsewhere (`docs/architecture/overview.md:175`).

Assets include application data, operation grants, credential secrets, password hashes, renewal credentials, signing keys, and service availability. Trust boundaries separate clients from HTTPS endpoints, identity from PostgreSQL, hosts from environment databases, hosts from Kubernetes Secrets, and hosts from the shared NATS cache.

PATs, or personal access tokens, contain 32 random secret bytes. PostgreSQL stores their digest, and authentication compares digests in constant time (`crates/identity/platform/src/lib.rs:163`, `crates/identity/platform/src/lib.rs:783`, `crates/identity/platform/src/lib.rs:1082`). Passwords use bounded Argon2id work, a password hashing method, with two concurrent jobs. Enrollment requires 15 characters (`crates/identity/platform/src/password.rs:24`, `crates/identity/platform/src/password.rs:209`). Renewal credentials rotate once, and replay revokes their login family (`crates/identity/platform/src/password_login.rs:193`).

Session tokens bind issuer, organization, audience, authority, roles, and time. Their maximum lifetime is 900 seconds, plus 30 seconds of tolerance (`crates/identity/session/src/token.rs:16`). Cloud verification uses an explicitly configured JWKS, a public signing key set. Fetches require HTTPS, reject redirects, and replace the complete cached set (`crates/platform/runtime/src/session_keys.rs:115`, `crates/platform/runtime/src/session_keys.rs:211`).

Mutual TLS authenticates provisioning operators through a dedicated certificate authority. Any accepted operator certificate grants the same issuance authority, and writes identify `wamn:provisioning` as their actor (`services/identity/src/lib.rs:389`, `services/identity/src/lib.rs:430`, `services/identity/src/pat.rs:75`). This credential is therefore a platform-wide trust anchor.

The inspected identity router protects operator writes and requires credentials for session issuance. Public keys, health, and password entry endpoints are intentionally public (`services/identity/src/lib.rs:250`). Application anonymity requires an explicit `none` policy. Unsupported policies refuse admission (`crates/platform/engine/src/flow_http_routing.rs:769`). No additional unintended anonymous route was established. Administration requires an explicit application `admin` row, but that row grants every served operation (`crates/platform/engine/src/flow_http_routing.rs:493`). The findings below describe paths that undermine these boundaries.

## Finding E1-1: Credential resource names collide across environments

Severity: high  
Effort: M  
Area: crates/identity/credential-store/src/kubernetes.rs  
Evidence: `crates/identity/credential-store/src/kubernetes.rs:21`; `crates/control/lib/src/environment/ensure.rs:826`; `crates/control/lib/src/environment/ensure.rs:866`  
Problem: Secret and ServiceAccount names join organization, project, and environment with single hyphens. Valid coordinates such as `acme/orders-eu/prod` and `acme/orders/eu-prod` produce identical names. Provisioning reuses existing resources, so these environments share a Secret and its read authority within the same namespace.  
Proposal: Use one collision-free coordinate encoding for Secrets, ServiceAccounts, Roles, and RoleBindings. Reuse the existing double-hyphen convention.  
Deletes: The two independent single-hyphen naming formulas.  
Risk: Existing resource references change. A test must provision both colliding coordinates and prove that neither account can read the other Secret.

## Finding E1-2: Shared NATS credentials can forge another environment’s grants

Severity: high  
Effort: L  
Area: crates/platform/runtime/src/auth_cache.rs  
Evidence: `deploy/gcp/auth-nats-clients.yaml:1`; `deploy/platform/host-environment-certs.example.yaml:28`; `crates/platform/runtime/src/auth_cache.rs:292`; `crates/platform/runtime/src/plugins/wamn_postgres/claims.rs:2359`  
Problem: Hosts and identity writers share the `wasmcloud-runtime` NATS identity. The inspected bundled NATS configuration grants that identity no subject restrictions. Hosts accept grant values from the shared bucket, so a stolen runtime certificate can publish an `admin` grant for a principal in another environment.  
Proposal: Keep authorization answers local and read them from PostgreSQL. Use NATS only for scoped invalidation messages, with separate identities for each environment and writer.  
Deletes: Shared serialized `PrincipalGrants` values and their replication path.  
Risk: PostgreSQL receives more reads. A two-environment test must prove that one runtime credential cannot alter another environment’s effective grants.

## Finding E1-3: A delayed grant can restore revoked administration

Severity: high  
Effort: L  
Area: crates/execution/host/src/control_route.rs  
Evidence: `crates/execution/host/src/control_route.rs:323`; `crates/execution/host/src/control_route.rs:337`; `crates/execution/host/src/control_route.rs:685`; `crates/execution/host/src/control_route.rs:1072`  
Problem: Administrative grants commit system rows before writing application rows, while revocation removes application rows before committing system rows. The mutex coordinates only one host process. Across two hosts, a delayed grant can write application `admin` after a revoke completes, leaving ordinary environment membership sufficient to use the restored privilege.  
Proposal: Give grant projection one serialized owner across hosts, CLI commands, and environment provisioning. Apply explicit environment rows from current committed authority, with durable recovery after interruption.  
Deletes: Independent route-owned grant and revoke fan-out.  
Risk: Administration completion and recovery behavior change. A test must pause a grant after its system commit, complete a revoke elsewhere, and prove that resumed work cannot restore `admin`.

## Finding E1-4: Inactive organization membership is not an admission boundary

Severity: high  
Effort: M  
Area: crates/identity/platform/src/org.rs  
Evidence: `crates/identity/platform/src/org.rs:27`; `crates/identity/platform/src/org.rs:75`; `crates/identity/platform/src/org.rs:470`; `crates/identity/platform/src/control.rs:52`; `crates/identity/platform/src/session_token.rs:152`  
Problem: Grant admission reads active membership without locking its row. A concurrent deactivation can remove existing grants and mark membership inactive before the admitted grant writes new rows. Control and application session admission inspect the resulting roles or environment membership without requiring active organization membership.  
Proposal: Make active organization membership a required authorization predicate. Serialize membership transitions and grant writes through the same membership row.  
Deletes: Role-only and environment-membership-only admission predicates for human principals.  
Risk: Previously inconsistent accounts lose access. A concurrent grant/deactivation test must prove that inactive membership cannot retain or regain authority, including control sessions.

## Finding E1-5: Identity database traffic carries signing material without TLS

Severity: high  
Effort: M  
Area: services/identity/src/lib.rs  
Evidence: `services/identity/src/lib.rs:156`; `services/identity/src/lib.rs:165`; `crates/identity/platform/src/session_keys.rs:182`; `crates/identity/platform/src/password.rs:555`  
Problem: Identity opens PostgreSQL connections with `NoTls`. Those connections return private signing keys and password hashes, so a deployment without independent transport encryption exposes these assets to network interception. HTTPS on the public listener does not protect this boundary.  
Proposal: Require authenticated TLS for identity database connections, including certificate and hostname validation. Apply the same transport contract to environment role readers.  
Deletes: Production `NoTls` connections in the identity service.  
Risk: Database certificates and connection inputs must change. Transport tests must reject plaintext, an untrusted certificate, and a certificate for another hostname.

## Finding E1-6: Cached PAT approval survives credential expiry

Severity: medium  
Effort: M  
Area: crates/platform/runtime/src/auth_cache.rs  
Evidence: `crates/platform/runtime/src/auth_cache.rs:72`; `crates/platform/runtime/src/auth_cache.rs:135`; `crates/platform/runtime/src/plugins/route_authentication.rs:504`; `crates/identity/platform/src/kv_memory.rs:11`  
Problem: Cached token approvals contain a receipt time but no credential deadline. A PAT accepted just before expiry remains accepted for up to 60 seconds without another identity read. A session’s cached authority approval can likewise outlive its source PAT, even while the signed session remains valid.  
Proposal: Cache explicit authority deadlines and expire approval at the earlier of that deadline and the cache deadline. Alternatively, remove identity approval caching.  
Deletes: Deadline-free `Token` approval entries.  
Risk: Requests near expiry begin to refuse earlier. A test with an active cache must cross natural PAT expiry without publishing an invalidation and expect refusal immediately.

## Finding E1-7: Cache replication restarts the age of old authorization facts

Severity: medium  
Effort: M  
Area: crates/platform/runtime/src/kv_memory.rs  
Evidence: `crates/platform/runtime/src/kv_memory.rs:130`; `crates/platform/runtime/src/kv_memory.rs:178`; `crates/platform/runtime/src/kv_memory.rs:244`; `crates/identity/platform/src/auth_changes.rs:115`  
Problem: Watch delivery assigns `Instant::now()` to every received value and ignores its original age. A host that receives a nearly expired value gives it another full cache lifetime. This exceeds the documented 60-second backstop when an invalidation is lost.  
Proposal: Preserve one absolute freshness deadline across reads, publication, and replication. Removing shared authorization values, as proposed in E1-2, removes this replication failure.  
Deletes: Receipt-time renewal of replicated authorization values.  
Risk: Old watch entries become immediate misses. A delayed-delivery test must prove that replication never extends the original deadline.

## Finding E1-8: File-backed signing keys remain trusted indefinitely

Severity: medium  
Effort: M  
Area: crates/identity/session/src/file_keys.rs  
Evidence: `crates/identity/session/src/file_keys.rs:30`; `crates/identity/session/src/file_keys.rs:80`; `services/edge/src/authenticator.rs:52`  
Problem: File-backed keys load once and always report fresh evidence. After a signing key compromise, removing the key from cloud publication cannot stop an edge process from accepting newly forged tokens indefinitely. The short token lifetime does not bound this risk because the attacker can sign new issuance times.  
Proposal: Give offline key sets a finite trust deadline and require replacement before that deadline. Refuse new admission when the offline trust period ends.  
Deletes: The unconditional `true` freshness result.  
Risk: Disconnected devices eventually refuse sessions. A test must retain an old key set, advance beyond its trust deadline, and reject a newly signed token.

## Finding E1-9: CLI and HTTP routes enforce different grant rules

Severity: medium  
Effort: M  
Area: crates/control/lib/src/user_role.rs; crates/control/lib/src/role_permission.rs  
Evidence: `crates/control/lib/src/user_role.rs:152`; `crates/identity/platform/src/application.rs:436`; `crates/execution/host/src/application_route.rs:407`; `crates/execution/host/src/application_route.rs:365`; `crates/control/lib/src/role_permission.rs:49`  
Problem: The HTTP route refuses removal of application `admin` while `project-admin` covers it, but the CLI calls an unconditional delete. The HTTP permission route also rejects fixed host operations before calling an owner function that does not enforce that rule. The shared owner therefore permits states that another entry point declares invalid.  
Proposal: Put grantability and covering-grant rules in the owning mutation boundary. Make CLI and HTTP callers use the same authorized operation.  
Deletes: Transport-specific semantic guards and direct bypass paths.  
Risk: Some existing CLI commands begin to refuse. A parity test must submit the same covered revoke and fixed-operation grant through both entry points.

## Finding E1-10: An unused API-key authority remains in the application schema

Severity: low  
Effort: S  
Area: crates/identity/project-state/src/lib.rs  
Evidence: `crates/identity/project-state/src/lib.rs:270`; `deploy/sql/app-schema.sql:308`; `crates/identity/project-state/tests/authority.rs:197`  
Problem: The application schema retains an API-key table even though runtime code does not read it. The model, schema, history, and tests preserve a second credential structure beside the working system PAT authority. This adds maintenance without providing authentication behavior.  
Proposal: Delete the unused application API-key substrate. Keep credential authority in the identity owner.  
Deletes: `API_KEY`, `platform.api_key`, its history scaffolding, and tests that only preserve the unused structure.  
Risk: Schema snapshots change. Schema and authority tests must pass without the table, while PAT and session authentication tests retain their behavior.

## Open questions for the owner

1. Must tenant isolation survive compromise of one native host or theft of its scheduler certificate?
2. Does every deployed identity database connection have independently enforced transport encryption?
3. What maximum disconnected period justifies continued trust in an edge signing key set?
4. Is every operator certificate intentionally a platform-wide credential, and what revokes one certificate without replacing the CA?
5. Are cache delays acceptable only after lost invalidation, or also after natural credential expiry?

## Proposed fix epics

### Isolate credential and cache authority

1. Replace ambiguous Kubernetes credential names
2. Remove shared authorization values from NATS
3. Scope scheduler identities and invalidations

### Give membership and grants one owner

1. Serialize grant projection across all writers
2. Require active organization membership at admission
3. Unify CLI and HTTP grant rules

### Bound credential trust

1. Require authenticated TLS for identity databases
2. Preserve credential and cache deadlines
3. Expire offline signing key trust

### Remove unused credential machinery

1. Delete the application API-key substrate

<!-- lane E2, bead wamn-hgdta.2 -->

## Part 2: secrets and transport

Read-only source audit. No files changed, and no builds, tests, or live probes ran. The tests below are proposed validation.

## Finding E2-1: Valid environment names share credential Secrets and ServiceAccounts

Severity: high  
Effort: M  
Area: crates/identity/credential-store  
Evidence: `crates/identity/credential-store/src/kubernetes.rs:21`, `crates/identity/credential-store/src/kubernetes.rs:26`, `crates/control/registry/src/identifiers.rs:87`, `crates/control/lib/src/environment/ensure.rs:866`, `crates/control/lib/src/release_chart.rs:490`  
Problem: Names join org, project, and environment with single hyphens, although each component permits single hyphens. Coordinates `a/b-c/d` and `a/b/c-d` therefore produce the same Secret, ServiceAccount, Role, and RoleBinding. Both environments receive access to the same complete credential Secret, so a compromised host crosses the project boundary.  
Proposal: Derive names from validated coordinates with the existing unambiguous `--` separator. Recreate affected Secrets and access resources without a fallback to ambiguous names.  
Deletes: The two independent single-hyphen name formats in `kubernetes.rs`.  
Risk: Existing references require replacement. A two-environment deployment test must prove distinct names and deny each ServiceAccount access to the other Secret.

## Finding E2-2: Org-scoped control credentials retain authority across organizations

Severity: high  
Effort: L  
Area: crates/control/provision/src/sql.rs; deploy/sql/system-schema.sql  
Evidence: `crates/control/provision/src/workload_role.rs:220`, `crates/control/provision/src/sql.rs:977`, `crates/control/provision/src/sql.rs:1034`, `deploy/sql/system-schema.sql:14`, `deploy/sql/system-schema.sql:1134`, `deploy/sql/system-schema.sql:1455`  
Problem: Provisioning describes each control login as org-scoped, but its inherited grants permit writes across entire membership and role tables. These tables have no row isolation between organizations. Owner-executed connection functions also trust caller-supplied org arguments, so a stolen control credential can alter another organization's authority and connection records.  
Proposal: Remove direct runtime writes to global authority tables. Expose narrow routines that derive permitted organization scope from the authenticated database login. Reserve global integration publication for the deployment owner.  
Deletes: Broad control table grants and caller-selected organization authority in privileged functions.  
Risk: Control operations require new database interfaces. Extend the live denial tests with two organizations and direct SQL attempts against the other organization's rows and functions.

## Finding E2-3: Postgres connections expose sensitive traffic without transport encryption

Severity: high  
Effort: L  
Area: deploy/**/cnpg-cluster.yaml; native Postgres clients  
Evidence: `deploy/gcp/cnpg-cluster.yaml:51`, `deploy/platform/wamn-sysdb.yaml:64`, `services/identity/src/lib.rs:165`, `services/host/src/host.rs:782`, `services/cdc-reader/src/lib.rs:150`  
Problem: Database admission rules explicitly accept non-TLS connections, and deployed identity and control clients use `NoTls`. SCRAM password authentication does not encrypt subsequent SQL traffic, including identity state, signing material, and application data. The CDC reader also defaults its replication transport to `sslmode=disable`.  
Proposal: Require authenticated TLS for database clients and replication. Give one connection owner responsibility for CA trust and server-name validation, then require encrypted connections in Postgres admission rules.  
Deletes: Production `NoTls` connections, plaintext admission rules, and the CDC plaintext default.  
Risk: Every database consumer requires trusted certificates. Deployment tests must reject plaintext, an untrusted CA, and a wrong server name while preserving identity, provisioning, and replication operations.

## Finding E2-4: Event NATS sends credentials and events over plaintext connections

Severity: high  
Effort: M  
Area: deploy/gcp/nats-jetstream.yaml; deploy/infra/nats-jetstream.yaml  
Evidence: `deploy/gcp/nats-jetstream.yaml:16`, `deploy/gcp/cdc-reader.yaml:59`, `crates/platform/runtime/src/plugins/wamn_jetstream.rs:638`, `services/cdc-reader/src/lib.rs:1091`, `deploy/infra/nats-jetstream.yaml:57`  
Problem: The event broker has no configured TLS listener, while deployed clients connect with usernames and passwords over `nats://`. A network observer can obtain reusable broker credentials and event contents. The three-node manifest also exposes broker routes without configured peer authentication or TLS.  
Proposal: Require authenticated TLS on event clients and mutual TLS between broker peers. Keep environment subject permissions as the authorization boundary.  
Deletes: Plaintext broker listeners, unauthenticated peer routes, and plaintext event endpoint defaults.  
Risk: Existing clients require broker trust material. A broker integration test must reject plaintext clients and unauthorized peers, then prove publish, consume, and reconnect behavior.

## Finding E2-5: Shared development infrastructure publishes administrator credentials

Severity: medium  
Effort: M  
Area: deploy/platform/postgres.yaml; deploy/infra/minio.yaml; crates/control/provision/src/backup.rs  
Evidence: `deploy/platform/postgres.yaml:35`, `deploy/platform/postgres.yaml:119`, `deploy/infra/minio.yaml:27`, `crates/control/provision/src/backup.rs:33`, `crates/control/provision/src/backup.rs:82`  
Problem: The Postgres fixture ships a known superuser password, and MinIO ships known root credentials. Backup declarations reuse the same MinIO administrator credentials across cluster prefixes. A pod with service reachability can administer these fixtures and read or delete backups stored there. These manifests describe development infrastructure, so this evidence does not establish production exposure.  
Proposal: Delete the standing Postgres fixture and create disposable databases for tests. Replace the shared backup root identity with credentials restricted to each cluster's backup prefix and an explicit encrypted endpoint.  
Deletes: `deploy/platform/postgres.yaml`, committed MinIO credential values, and shared root credential wiring in backup declarations.  
Risk: Existing gate and recovery procedures require replacement inputs. Prove backup restoration and deny one cluster's credentials access to another cluster's objects.

## Finding E2-6: Command help can disclose database passwords

Severity: medium  
Effort: S  
Area: services/ctl; services/cdc-reader; services/scenario-worker  
Evidence: `services/ctl/src/provisioning_verbs.rs:590`, `services/ctl/src/provisioning_verbs.rs:594`, `services/cdc-reader/src/lib.rs:125`, `services/cdc-reader/src/lib.rs:131`, `services/scenario-worker/src/management.rs:445`, `services/identity/src/cli.rs:28`  
Problem: Several password-bearing environment arguments omit `hide_env_values`, so clap includes their values in help output. The affected inputs include superuser and replication database URLs. Help captured in support logs or deployment diagnostics can therefore disclose reusable credentials, while the identity CLI already demonstrates suppression.  
Proposal: Suppress environment values for every secret argument and redact their diagnostic representations. Remove password-bearing command arguments where mounted credential files can supply the same input.  
Deletes: Secret values from generated help and unnecessary password-bearing command arguments.  
Risk: Automation that passes URLs directly requires adjustment if flags disappear. Render each command's help with synthetic secret markers and prove that no marker appears.

## Finding E2-7: Standalone service images default to root execution

Severity: medium  
Effort: M  
Area: Dockerfile; deploy/platform/identity; deploy/gcp/cdc-reader.yaml  
Evidence: `Dockerfile:182`, `Dockerfile:189`, `Dockerfile:196`, `deploy/platform/identity/templates/deployment.yaml:29`, `deploy/gcp/cdc-reader.yaml:40`, `deploy/platform/scenario-worker.yaml:144`  
Problem: Shipping image stages declare no `USER`, and standalone identity, CDC, and scenario-worker manifests provide no non-root execution requirement. These services therefore default to container root unless an external admission policy changes them. A native service compromise gains unnecessary filesystem privileges and container capabilities.  
Proposal: Set an unprivileged numeric image user and require non-root execution in the standalone deployments. Drop capabilities, disable privilege escalation, and provide only the writable mounts each service needs.  
Deletes: Implicit root execution and unrestricted writable container filesystems.  
Risk: UID changes affect private certificate files and writable directories. Deployment smoke tests must start each service with its real Secret mounts under the restricted identity.

## Finding E2-8: Mutable base and infrastructure images weaken release provenance

Severity: medium  
Effort: M  
Area: Dockerfile; deploy/gcp; deploy/infra  
Evidence: `Dockerfile:20`, `Dockerfile:158`, `deploy/gcp/nats-jetstream.yaml:96`, `deploy/gcp/cnpg-cluster.yaml:39`, `deploy/gates/m1-postgres.Dockerfile:1`; measurements.md section 11  
Problem: Build bases and deployed database and event-broker images use mutable tags without digests. The same source revision can therefore resolve different executable inputs across builds or node pulls. The shared measurements contain no container vulnerability scan, so this finding establishes an input-provenance gap.  
Proposal: Pin shipping bases and infrastructure images by digest under one reviewed image declaration. Replace those pins through explicit image updates and qualification.  
Deletes: Tag-only shipping image references and duplicate image declarations.  
Risk: Image updates become deliberate release changes. A manifest gate must reject tag-only shipping references, and deployment qualification must exercise each replacement digest.

## Open questions for the owner

1. Do current environments contain colliding credential names? Inspect their coordinates before replacing names under E2-1.
2. Is every org control host intentionally trusted with all organizations' authority? The database grants currently establish that boundary.
3. Which external network controls protect the plaintext services and development fixtures? This source audit cannot establish the live network boundary.
4. Who owns the chart CA's renewal and expiry date? `deploy/infra/values-wamn.yaml:24` records a 365-day CA with no renewal path.
5. Must SQL literal quoting support nondefault string parsing? `crates/platform/pg-core/src/lib.rs:167` escapes apostrophes but does not state that server precondition.

## Proposed fix epics

Credential and organization isolation

1. Replace ambiguous credential resource names
2. Bind control database authority to authenticated organization scope
3. Prove isolation across organizations and credential stores

Authenticated internal transport

1. Require authenticated TLS for Postgres and replication
2. Require TLS for event clients and broker peers
3. Establish certificate renewal ownership and expiry tests

Secret and image boundaries

1. Retire shared administrator fixtures and restrict backup identities
2. Remove credential disclosure from command diagnostics
3. Require non-root services and reviewed image digests

<!-- lane E3, bead wamn-hgdta.3 -->

## Part 3: sandbox and supply chain

This review used source inspection and the shared measurements. No files changed, and no builds or tests ran. Tests named below are proposed validation.

Normal tenant admission refuses direct sockets, filesystem, and WASI HTTP imports. Granted platform capabilities still permit network effects through host bindings (`crates/platform/component-policy/src/lib.rs:181`, `crates/platform/component-policy/src/lib.rs:382`). Connector stores enforce an exact network authority independently of the application engine (`crates/platform/engine/src/connector.rs:138`, `crates/platform/engine/src/connector.rs:275`). Application workloads declare no filesystem volumes (`crates/platform/engine/src/operation/native_workload.rs:390`).

Execution uses deadlines bounded to 1–30,000 ms and native epoch interruption. WAMN grants no instruction fuel. Each linear memory has a 256 MiB ceiling, but aggregate guest memory uses count mode. A separate sampled process guard remains active (`crates/platform/engine/src/router_delivery.rs:475`, `docs/architecture/execution.md:371`).

## Finding E3-1: Registry descriptors can bypass byte admission

Severity: high  
Effort: M  
Area: `crates/catalog/package-artifact`, `crates/platform/engine`  
Evidence: `crates/catalog/package-artifact/src/lib.rs:5`, `crates/control/lib/src/publish_release.rs:1621`, `crates/catalog/model/src/component_facts.rs:460`, `crates/platform/engine/src/operation/native_workload.rs:254`, `crates/platform/engine/src/engine.rs:654`  
Problem: Package existence is treated as proof that publication admission ran. Publication accepts descriptor claims, while runtime loading checks the component digest without rederiving its imports. A registry writer can therefore supply matching hashes and false admission facts. If that component reaches execution, the default socket policy permits undeclared connections.  
Proposal: Derive admission facts from exact component bytes at the trusted publication boundary. Compare carried descriptors with those results. Enforce socket refusals at runtime.  
Deletes: The assumption that registry existence proves admission and the conversion of unchecked descriptors into admitted facts.  
Risk: Packages with stale or invented descriptors refuse. A crafted package test must hide a socket import in its descriptor and prove refusal before execution.

## Finding E3-2: The native lockfile contains reported security vulnerabilities

Severity: high  
Effort: M  
Area: `Cargo.lock`  
Evidence: measurements.md section 5, `Cargo.lock:7260`, `Cargo.lock:7487`, `Cargo.lock:7512`, `docs/audit/raw/audit-root.txt:57`  
Problem: The refreshed measurement reports 17 vulnerabilities and 12 warnings in the root dependency graph. Wasmtime resolves to 48.0.7, despite the manifest requirement spelling 48.0.5. The recorded advisories name 48.0.8 as a patched version for the listed Wasmtime findings. Lockfile presence alone does not prove that every affected API is reachable.  
Proposal: Upgrade the affected native dependency families together. Record feature and API reachability for findings that remain. Preserve the single Wasmtime source and version contract.  
Deletes: Vulnerable dependency resolutions and obsolete version assertions.  
Risk: Runtime APIs and component behavior can change. Run the source-identity, admission, connector, deadline, and cancellation suites against the replacement graph.

## Finding E3-3: Package pulls and extraction have no resource budget

Severity: high  
Effort: M  
Area: `crates/catalog/package-artifact`, `crates/control/lib/src/package_artifact.rs`  
Evidence: `crates/catalog/package-artifact/src/registry_pull.rs:23`, `crates/catalog/package-artifact/src/lib.rs:391`, `crates/control/lib/src/package_artifact.rs:1223`, `crates/control/lib/src/package_artifact.rs:1267`  
Problem: Registry reads buffer complete layers, and extraction reads each complete entry into another buffer. The code imposes no aggregate byte, layer-count, or file-count limit before extraction and repacking. A malicious artifact can exhaust application memory or temporary storage before its final refusal. Regular-file and relative-path restrictions prevent archive links and ordinary traversal, but do not bound resource use.  
Proposal: Stream artifacts through fixed byte and entry budgets. Admit only the paths assigned to each layer before writing them.  
Deletes: Unbounded blob buffers and archive `read_to_end` calls.  
Risk: Large legitimate packages can refuse. Boundary tests must cover oversized blobs, excessive entries, duplicate paths, traversal, and cleanup after failure.

## Finding E3-4: Aggregate guest memory remains observational

Severity: high  
Effort: S  
Area: `crates/platform/engine/src/engine.rs`, host and edge configuration  
Evidence: `crates/platform/engine/src/engine.rs:119`, `crates/platform/engine/src/engine.rs:843`, `services/host/src/host.rs:275`, `services/edge/src/serve.rs:167`, `deploy/platform/release/values.yaml:59`  
Problem: Count mode records allocations beyond the shared guest budget and permits them. The 256 MiB ceiling applies to each linear memory, rather than the complete component or host. The sampled process guard and container limit mitigate exhaustion, but do not reserve aggregate guest memory before allocation. This behavior follows an existing owner ruling.  
Proposal: Use aggregate memory enforcement for serving hosts and connector engines. Keep count mode confined to explicit measurement environments.  
Deletes: Serving defaults that select `GuestMemoryMode::Count`.  
Risk: Workloads that currently exceed the budget refuse. Extend `enforce_mode_refuses_aggregate_growth_until_another_store_releases_memory` to cover deployed host, edge, and connector construction.

## Finding E3-5: Execution deadlines do not contain compilation

Severity: medium  
Effort: L  
Area: `crates/platform/engine/src/component_admission.rs`, `operation/native_workload.rs`  
Evidence: `crates/platform/engine/src/component_admission.rs:195`, `crates/platform/engine/src/component_admission.rs:369`, `crates/platform/engine/src/lib.rs:85`, `crates/platform/engine/src/operation/native_workload.rs:400`  
Problem: Admission compiles the component before applying its import policy. Import discovery also invokes full compilation. Runtime loading moves compilation into `spawn_blocking`, whose work continues after its awaiting caller is cancelled. Guest memory ceilings and epoch deadlines do not bound compiler allocations or CPU use.  
Proposal: Reject forbidden imports through bounded structural analysis before code generation. Run required compilation under explicit concurrency, memory, and execution budgets in a process that the caller can terminate.  
Deletes: Full compilation solely for import discovery and unrestricted detached compilation work.  
Risk: Structural analysis must preserve named-interface and nested-component semantics. Adversarial compilation tests must prove bounded resource use and termination after caller cancellation.

## Finding E3-6: Connector code remains retained after bindings retire

Severity: medium  
Effort: M  
Area: `crates/platform/engine/src/connector.rs`, `crates/platform/runtime/src/connectors.rs`  
Evidence: `crates/platform/engine/src/connector.rs:119`, `crates/platform/engine/src/connector.rs:126`, `crates/platform/engine/src/connector.rs:328`, `crates/platform/runtime/src/connectors.rs:27`, `crates/platform/runtime/src/connectors.rs:131`  
Problem: Process-wide connector engines retain a compiled template for every admitted digest. Runtime readers separately retain component bytes in an unbounded map. Binding invalidation removes linked instances but leaves these digest-indexed copies. Repeated connector upgrades therefore retain obsolete artifacts until their owning process or reader disappears.  
Proposal: Give connector code and bytes one bounded cache with ownership tied to active bindings. Release obsolete entries after the last active instance retires.  
Deletes: Permanent compiled-template retention and the separate unbounded byte map.  
Risk: Eviction can cause recompilation. A rotation test must preserve reuse for active digests and prove that retired digests release their retained code and bytes.

## Finding E3-7: Engine construction mutates the environment after threads start

Severity: medium  
Effort: S  
Area: `crates/platform/engine/src/engine.rs`  
Evidence: `crates/platform/engine/src/engine.rs:242`, `services/host/src/main.rs:59`, `services/host/src/host.rs:1089`, `services/host/src/host.rs:1108`  
Problem: Engine construction calls unsafe `std::env::set_var` for the abandoned-call grace. Its safety argument covers Rust environment locking but assumes away concurrent native environment readers. The host creates its multithreaded runtime and starts connection work before this call. `Once` serializes the setter without establishing the required safety condition.  
Proposal: Set the grace before runtime or library initialization. Prefer an upstream constructor parameter when that API becomes available.  
Deletes: Environment mutation from the shared engine constructor and its incomplete safety argument.  
Risk: Every executable that constructs an engine must retain zero-grace cancellation. Startup subprocess tests and the existing cancellation suites must prove that behavior.

## Finding E3-8: Dependency identity gates do not provide a refresh policy

Severity: medium  
Effort: M  
Area: `tools/repo-lint`, dependency locks, vendored dependencies  
Evidence: measurements.md sections 4 and 5, `tools/repo-lint:304`, `tests/conformance/tests/wasmtime_source_identity.rs:167`, `vendor/pg-walstream/WAMN-PATCH.md:1`, `vendor/pg-walstream/Cargo.lock:1142`  
Problem: The prior advisory database reported zero findings because it remained dated April 7. Repository lint runs no advisory refresh or scan. Shared measurements cover two lockfiles, while this checkout contains six first-party lockfiles plus the vendored lockfile. Source-identity gates preserve selected versions but provide no security refresh cadence.  
Proposal: Gate fresh advisory results for every maintained dependency domain. Assign an owner to coordinated pin updates and the pg-walstream patch. Remove confirmed unused runtime declarations for `percent-encoding` and `rand_core` (`crates/platform/runtime/Cargo.toml:17`, `crates/platform/runtime/Cargo.toml:70`).  
Deletes: Unused dependency declarations and stale advisory results as release evidence. Retire `vendor/pg-walstream` when upstream supplies the required typed keepalive API.  
Risk: Fresh scans can block releases and expose fixture-only findings. A gate test must reject stale advisory data and enumerate every maintained lockfile.

## Open questions for the owner

1. Is registry write access intentionally equivalent to authority to bypass component admission? E3-1 depends on an attacker controlling a package that publication selects.
2. What measured condition ends count mode? The current removal condition requires another owner ruling (`docs/architecture/native-alignment.md:32`).
3. Who owns security refreshes for the immutable WASI-Virt revision, its adapter, and the pg-walstream patch? Their identities are recorded, but no refresh interval appears (`Cargo.toml:215`, `vendor/pg-walstream/WAMN-PATCH.md:1`).
4. Does another gate scan the web dependency graph? `web/wfe` is pinned at `f158f65c0f737ba92c3df0fc74003d28906a9c08`, and ancestry checks govern updates, but the supplied web measurement is unused-code analysis (`tools/submodule-check/src/main.rs:62`, measurements.md section 8).
5. Must WASI-Virt provenance output measure its adapter digest? The executable currently prints a constant (`crates/platform/component-virtualizer/src/main.rs:46`).

## Proposed fix epics

### Enforce artifact and runtime authority

1. Derive component facts from bytes at trusted publication
2. Enforce aggregate memory and runtime socket policy
3. Bound package transfer and extraction

### Contain compilation and connector retention

1. Bound and cancel component compilation
2. Release retired connector code and bytes
3. Move environment initialization before threads

### Refresh the supply chain

1. Upgrade vulnerable native dependency families
2. Gate fresh advisories across maintained dependency domains
3. Retire the pg-walstream patch and remove unused dependencies
