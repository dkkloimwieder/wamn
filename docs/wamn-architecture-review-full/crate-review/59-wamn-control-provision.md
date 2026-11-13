# 59 wamn-control-provision

Scope: `crates/control/provision/src` (12,606 lines, 33 files; tests 10,758). Structure and callers; not every statement read. Nothing built or run.

Rechecked at `epic/dep7` (b94833fd) on 2026-11-12. Findings 1–6 still stand; the crate is unchanged except for the new `build.rs` and `schema_migrations.rs` (platform migration history). Those are in tab 60 findings 5 and 6.

## Earlier work checked first

- Earlier proposal (not ruled): it runs nothing; it mixes names and login-role identity, the SQL and manifest text that creates them, NATS declarations (`events.rs`) and a Kubernetes status reader; services use mostly names, URL parsers and role constants. Recommendation: names and roles move to the registry; `bind_platform_principal_sql` to project state; `OPERATION_GRANT_LOCK_SQL` to the catalog.
- Owner ruling 2026-11-11 (constraint row 7): one login and one Secret per role, host restart on change; the A/B credential generations go.
- Owner 2026-11-10: per-function logins (separation of function) are liked.
- Tab 06: `wamn-pg-core` deleted. Tab 01: `wamn-execution-contract` split. Tab 41: GRANT text is a named U2 exception. Tab 02: registry SQL in one place; crate renamed `wamn-registry`. Tab 55 finding 4: name types in `wamn-registry`. Tab 57 findings 3 and 4: `IdentityIssuerConnection`, `SessionTarget` and the actor bind move to `wamn-platform-identity`. Tab 58 finding 6: `wamn-kubernetes`. Tab 56 finding 6: one NATS connect function, home open.
- Owner 2026-11-05: KCL, Helm and the wasmCloud and CNPG controllers own deployment state.

## Files read

- `lib.rs` header; `Cargo.toml`; public items of the credential files (`identity_issuer.rs`, `system_reader.rs`, `session_role_reader.rs`, `control_author.rs`, `management_admitter.rs`, `host_credentials.rs`, `workload_role.rs`); file headers of the renderers; counts of `generation`.
- Every `wamn_control_provision::` path used outside the crate (grep, with counts).

## One purpose per file (today)

- Names: `name.rs`, `tenant_key.rs`, `workload_role.rs` (role constants and families).
- Login contracts and URL parsers: `identity_issuer.rs`, `system_reader.rs`, `session_role_reader.rs`, `control_author.rs`, `management_admitter.rs`, `host_credentials.rs`.
- SQL text builders: `sql.rs` (1,936), `sql/*`, `apply.rs`, `audit_retention.rs`, `operation_grants.rs`, `platform_principals.rs`.
- Kubernetes manifests as JSON: `secret.rs`, `org.rs` and `backup.rs` and `recovery.rs` (CNPG `Cluster`), `database.rs` (CNPG `Database`), `materializer_workload.rs`.
- NATS stream declarations: `events.rs`. Kubernetes status: `environment_state.rs`. Test databases: `test_database.rs`.

## Findings

**1. Five connection URL parsers, each with its own error type.**

Fact: `parse_identity_issuer_url`, `parse_system_reader_url`, `parse_session_role_reader_url`, `parse_control_authoring_url`, `parse_management_admission_url`, each with an `...UrlErrorType` and `...UrlError`, each checking that a URL names its expected role and database. `host_credentials.rs::HOST_CREDENTIALS` lists eight credentials.

Options:

- A: one check: parse with SQLx `PgConnectOptions`, then compare role and database with the one table of logins. Delete the five parsers and their error types. The identity service (tab 57 finding 3) uses the same check.

**2. Names and role constants that every service reads live here.**

Fact: outside the crate, `project_env_key` (22 uses), `workload_secret_name`, `package_owner_role`, `project_env_database_name`, `event_stream_name`, `cdc_object_name`, the role constants and `validate_project_env` are imported by host, workflow, runtime, environment, dev, ctl. `name.rs::slug_reason` repeats the registry rule (tab 55 finding 4).

Options:

- A: names and role names move to `wamn-registry`; the statements that create the roles stay here.

**3. SQL text built here and run by other crates.**

Fact: the crate holds no database client ("pure core"). Statement builders (`sql.rs`, `sql/*`, `apply.rs`, `audit_retention.rs`, `platform_principals_sql`, ...) are executed by `control/environment` (7 files), `control/dev` (3), `control/package` (2) and `wamn-platform-identity`.

Options:

- A: this crate runs its own statements over SQLx: each function takes a connection and does the work; callers hold no SQL text. GRANT and DDL text stays inside, as the named U2 exception (tab 41).
- B: keep text builders; callers execute.

**4. NATS stream declarations in the provisioning crate.**

Fact: `events.rs` (548 lines, `async-nats`) declares the source and advisory streams and consumers; used by `control/environment` broker code and dev. `wamn-event-wire` is a guest-linked crate and cannot take `async-nats`.

Options:

- A: `events.rs` moves to the crate that owns the one NATS connect function (tab 56 finding 6), so all NATS setup has one owner.

**5. Kubernetes manifests written in Rust.**

Fact: `secret.rs`, `org.rs`, `backup.rs`, `recovery.rs`, `database.rs` and `materializer_workload.rs` render Secrets, CNPG `Cluster` and `Database` objects and the materializer workload as JSON; `materializer_workload.rs` says it is rendered from `deploy/platform/materializer.example.yaml`. Not read: where each is applied, and whether a Helm chart renders the same object.

Options:

- Decide in the control environment tab, where they are applied: Helm charts (owner 2026-11-05: Helm and the controllers own deployment state) or typed objects in `wamn-kubernetes`.

**6. Two statements with other owners.**

Fact: `bind_platform_principal_sql` (66 uses) binds an actor; `operation_grants::OPERATION_GRANT_LOCK_SQL` (8 uses) locks package grants.

Options:

- A: the bind is the one actor bind of `wamn-platform-identity` (tab 57 findings 3 and 4); the grant lock moves to the catalog.

## Follows from earlier rulings

- Row 7 (one login per role): delete the generation machinery: `*_generation_role`, `*_scope_hash`, `prepare_*_generation_sql`, `retire_*_generation_sql`, `normalize_workload_generation_membership_sql`, `CredentialGeneration` (`generation` appears in 12 files; most in `workload_role.rs`, `sql/credentials.rs`, the five credential files).
- Tab 06: `wamn-pg-core` quoting becomes the client library's.
- Tab 01: `wamn-execution-contract` uses become `wamn-digest` or `wamn-scalar`.
- Tab 57 finding 3: `identity_issuer.rs` types and `session_target.rs` move to `wamn-platform-identity`.
- Tab 58 finding 6: `environment_state.rs` moves to `wamn-kubernetes`.

## Tests deleted, changed or moved

- Row 7: generation tests go.
- Finding 1: the five parsers' tests become one table test.
- Finding 3 A: text-shape tests of builders go (test rule 2); live tests stay.

## Not read

- `sql.rs` statements one by one; renderer bodies; `test_database.rs`; tests.

## Rulings

- 2026-11-12: findings 1, 2, 3, 4, 6 A (finding 3: this crate runs its own statements over SQLx; callers hold no SQL text). Finding 5 carried to the control environment tab.
