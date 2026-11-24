# wamn Epic 1: crate cleanup

This document is the work order for Epic 1 on branch `worktree-restructure`. It removes the trait layer and the `wamn-postgres` hub that the ports-and-adapters rule added, and it keeps the app, runtime and deployment planes isolated. Code facts were read at commit `4677cf7ed`. Before acting on a fact, check the cited file and symbol again.

## How to read this document

| Mark | Meaning |
| --- | --- |
| **F** | Read in the code. |
| **I** | Inferred from the code. Not reproduced. |
| **Not verified** | Stated by a source that was not read for this document. |

- Nothing in this document was built, run or measured.
- A rule in this document is decided. It is not a proposal.

## Constraints

- Breaking changes are acceptable.
- Never patch upstream code, and never maintain a copy of upstream code.
- Runs on kind or on GCP need explicit authorization before they start.
- Out of scope: any change of behaviour, new tests, chart changes beyond what a crate move needs, and any other work list item.
- Every change records: the commit, the owner removed or moved, the interface exercised, the cases that ran, and any claim left untested.

## Current state

**F, at `4677cf7ed`.**

- 58 crates. `wamn-postgres` has 16,673 lines and 22 normal wamn dependencies, one of them `wamn-record-history` from the apps workspace. `wamn-provision` uses 29 of its names, control 10, the host 6 and identity-service 3. `wamn-plugin` and `wamn-storage` depend on `wamn-postgres` only for tests. The CDC reader uses only `replication`, and links all 22.
- `wamn-postgres` depends on `wamn-platform-install`. `wamn-provision`'s system bootstrap calls `wamn-postgres` (`crates/provision/src/provision_system.rs`), and `wamn-provision` depends on `wamn-environment`.
- `wamn-kubernetes` depends on `wamn-connection`, `wamn-platform-install` and `wamn-routes`. `wamn-oci` depends on `wamn-codegen`, `wamn-platform-install` and `wamn-release`. `wamn-storage` depends on `wamn-release`, `wamn-blobstore`, `wamn-connection` and `wamn-application`, and holds web artifact publication (`crates/storage/src/web_artifact.rs`) and native file storage (`crates/storage/src/files.rs`).
- Shared database mechanics serve several owners. Project connection reads use `operation::open_executor_read` (`crates/postgres/src/project_connections/mod.rs`). The authorization store uses `WamnPostgres::executor_read` (`crates/postgres/src/authorization/store.rs`). Connection OAuth persistence and connection administration use `bind_transaction_identity`. `WamnPostgres` holds the per-project `HostProject` map and `HostCredentials` (`crates/postgres/src/pool.rs`), and `host_project.rs` and `checkout.rs` add inherent methods to it.
- `wamn-codegen` defines `BuildDatabase` (`crates/codegen/src/database.rs`) and uses it in 5 files. `wamn-build` depends on `wamn-codegen`. The Postgres implementation reads the catalogue through `catalog::read_catalog_excluding_relations` (`crates/postgres/src/build_database.rs`).
- I (code scan): about 77 public traits. Most have one production implementation, and almost none a test implementation. Most exist so that a domain crate can reach `wamn-postgres`, `wamn-kubernetes`, `wamn-oci` or `wamn-provision`: platform-install 11, authentication 10, release 7, connection 7, registry stores 6.
- `HostPolicy` is defined in `wamn-authority` and implemented in `wamn-plugin` by delegating to upstream `AllowedHost::matches` (`crates/plugin/src/host_policy.rs`).
- Session exchange carries one transaction through membership checks, signing and commit, through the boxed `ExchangeTransaction`. Renewal commits revocation in some refusal paths (`crates/authentication/src/session_exchange.rs`). The host keeps an operation transaction across guest callbacks and settles it afterwards (`crates/plugin/src/operation_transaction.rs`).
- Runtime binaries reach deployment crates. The host and the web proxy reach `wamn-platform-install` through `wamn-kubernetes`. The host and the CDC reader reach `wamn-codegen` through `wamn-postgres`. The host, the edge device and `wamn-plugin` reach `wamn-release` through `wamn-engine`, which uses it only for `ComponentAdmissionEngine`. The host also reaches `wamn-release` through `wamn-storage`.
- The host serves project administration: it uses `wamn_connection::administration` and `wamn_authentication::administration`. The host uses `wamn-identity-client`.
- `HostRouteSet::contract_files` reads files with `std::fs` (`crates/routes/src/host_route/contract_files.rs`). Its consumer is `crates/codegen/src/host_openapi.rs`.
- The dependency lint forbids sqlx and the other technology crates in the crates it lists as domains (`xtask/src/repo_policy/dependencies.rs`, `DOMAINS`).
- The apps workspace links only `wamn-digest`, `wamn-scalar`, `wamn-number-pattern`, `wamn-jsonata`, `wamn-guest` and `wamn-record-history` (`apps/Cargo.toml`).

## Rule

It replaces the ports-and-adapters rule of `docs/plan/wamn-restructure.md`.

| Role | Crates | May depend on |
| --- | --- | --- |
| Portable | digest, registry, package, role, principal, scalar, routes, identity-protocol, connection-type, component-policy, event, workflow-graph, schema, number-pattern, oci-reference, application, edge-document, envelope, and in the apps workspace jsonata, guest and record-history | Other portable crates. No I/O library, and no file or network access. |
| Infrastructure | postgres (kernel), kubernetes, oci, broker, http-client, storage, gcp-metadata, key, identity-client | Portable and other infrastructure crates, and its own I/O library. It may hold WAMN identifiers and persistence or protocol conventions. It holds no domain decision and no lifecycle orchestration. |
| Runtime domain | engine, plugin, authority, http, blobstore, connection, authentication | Portable, infrastructure and other runtime domain crates, called directly, sqlx, and the host libraries it implements (wash-runtime, wasmtime, hyper, reqwest). It owns its SQL. |
| Deployment domain | environment, platform-install, release, build, codegen, identity | Any crate except a binary, called directly, and sqlx. It owns its SQL. |
| Binary | runtime: host, edge, web, cdc-reader. Deployment: control, cli, identity-service, xtask | Wiring, including their own Kubernetes, OCI and NATS client use, and SQL that only that binary uses. |

- Each logical operation has one transaction owner, which settles it. Helpers may borrow the transaction, and the host may keep it across guest callbacks. Preserve every existing transaction, lock, cancellation and commit boundary while replacing a transaction trait with concrete calls on the sqlx transaction. In particular, session exchange keeps membership checks, signing and commit in one transaction, and renewal keeps committing revocation in its refusal paths.
- A trait exists only when it has two production implementations, a test implementation in use, or it prevents a dependency cycle or keeps wasmtime and the engine out of a crate that does not need them. Before a trait is deleted, the location of its replacement call is named. Everything else is a direct call. `HostPolicy` stays: deleting it would make `wamn-authority` and `wamn-plugin` depend on each other, or copy upstream matching.
- The apps workspace links only portable crates.
- Among library crates, only the infrastructure crates link the Kubernetes, OCI and NATS client libraries. Binaries may link them for their own wiring: control, the host, identity-service and xtask do so today, and that stays.
- Runtime binaries and runtime domain crates reach no deployment domain crate. Deployment binaries may link runtime domain crates.
- `wamn-authentication` keeps what the runtime uses: token checks, authorization reads, route authentication and project administration. A new deployment crate, `wamn-identity`, takes sessions, passwords, account OAuth, service principals, workload identity, platform identity, logins and NATS signing, which identity-service and control use. The identity-service executable is also named `wamn-identity` (`Dockerfile`). The crate and the service hold the same subject, and both names stay. Connection OAuth token refresh is runtime and stays with `wamn-connection`.
- One known exception: `HostRouteSet::contract_files` keeps reading files until Epic 2 deletes the authored JSON shapes and their loader (R3 of the work list). It is not moved first.
- No lint enforces this rule. The dependency lint is deleted in step 1. After Epic 1, the rule lives in `docs/structure.md` and code review enforces it.

## Steps

Each step merges alone, with a green build. A step splits into several commits only where each commit builds alone. No temporary adapter is added to make a commit split work.

1. **Kernel first.** The shared database mechanics stay in, or move into, the `wamn-postgres` kernel: pool, the per-project pool registry with its credentials, checkout, executor reads, transaction identity binding, connection options and configuration. The configuration types the kernel takes from `wamn-platform-install` move into it, so the kernel depends on no domain crate. Each state moves with its inherent methods.
   - The migration runner (`migration_scope.rs`) stays in the kernel. Its `sqlx::migrate!` relative paths do not change.
   - All four `wamn-platform-install` imports in `crates/postgres/src` move. The database configuration types go into the kernel. `environment::connection_for`, `capture_configuration` and `project_migration_login` go into the kernel. `workload_role.rs` goes into `wamn-identity-protocol`.
   - This step fixes the public API of the kernel. Code that leaves `wamn-postgres` later calls that API through free functions or its own types, because another crate cannot add inherent methods to `WamnPostgres`. `OperationTransaction` keeps the transaction type and lifetime that it uses today.
   - `replication` moves into the CDC reader binary, its only user, with no Cargo feature. It keeps implementing `ReplicationInput`.
   - The dependency lint is deleted: `xtask/src/repo_policy/dependencies.rs`, `xtask/src/repo_policy/dependencies/`, the `RepoLintDependencies` task and every caller of `check_boundaries`. The other repo policy checks stay.
2. **Infrastructure.** `wamn-kubernetes`, `wamn-oci` and `wamn-storage` lose their domain dependencies. Installation, publication and authorization orchestration moves to its domain owner: web artifact publication to `wamn-release`, connection-authorized file handling to `wamn-blobstore`, credential steps to `wamn-connection`, installation steps to `wamn-platform-install`. Concrete technology operations stay in the infrastructure crate. `ComponentAdmissionEngine` moves to `wamn-component-policy`, where `ComponentAdmissionRequest` and `ComponentDescriptor` already live, so `wamn-engine` drops `wamn-release`.
   - In `wamn-oci`, `platform_install.rs` goes to `wamn-platform-install`. These files go to `wamn-release`: `release_source.rs`, `release_package.rs`, `release_component.rs`, `package_source.rs`, `component_declaration.rs`, `deploy_packages.rs`, `web_artifact.rs` and `web_artifact/platform_ui.rs`. `integration.rs` and `package_artifact` stay as transport.
3. **`wamn-provision` goes.** Its environment adapters return to `wamn-environment`, which drops `Database`, `DatabaseSession`, `PackageMigrations` and `CaptureRepair`. Its org and system bootstrap goes to `wamn-platform-install`, which can call the kernel once step 1 has removed the reverse edge. Every consumer of `wamn-provision` is rewired: xtask, identity-service, control, cli, `wamn-environment`, `wamn-postgres`, `tests/receiving-tests`, `test-support/infrastructure` and `test-support/grant-tests`.
4. **Domain SQL returns to its owners**, each group with the references that would otherwise point back, and each group's traits go with it:
   - sessions, passwords, account OAuth, service principals, workload identity, platform identity, logins and NATS signing to `wamn-identity`; authorization reads and grants to `wamn-authentication`;
   - environment, capture, package apply, project setup, project migrations, release requirements, schema, package catalogue, record-history pruning and audit retention to `wamn-environment`;
   - platform database, platform migrations and org to `wamn-platform-install`;
   - operation-specific SQL, workflow, run store, file catalogue and file connections to `wamn-plugin`. `prune_run_history.rs` has no caller and is deleted;
   - project connections, including connection OAuth token refresh, to `wamn-connection`;
   - build verification and its catalogue introspection (`build_database`, `catalog`) to `wamn-codegen`, its only caller, so `BuildDatabase` can go without a codegen to build to codegen cycle;
   - control's own queries to the control binary.

   `wamn-record-history` does not move. It is a portable crate in the apps workspace, and only the `wamn-postgres` code that uses it moves. `crates/codegen/build.rs` lists `build_database` and `catalog` as generator inputs, and they become files of `wamn-codegen`.
5. **Ownership merges.** `wamn-application-chart` into `wamn-environment`, its only consumer. `wamn-run-state` into `wamn-plugin`: its types serve the host request path, and its consumers today are the host and `wamn-postgres`. `wamn-dev` into `wamn-build`: the CLI, its other consumer, already depends on `wamn-build`.
6. **Final check of the graph.** Check the whole graph against the rule, remove what was transitional, and rewrite the rule in `docs/plan/wamn-restructure.md`.

## Check

- Build each affected consumer alone, with its shipped features and targets: `cargo check --locked --all-targets -p <crate>`. Include the apps workspace when a portable crate it uses changes. Cargo unifies features across packages built together, so a workspace build is not enough.
- Record `cargo tree --locked -e normal` for the host, the CDC reader, the web proxy and the edge device, and `cargo tree --manifest-path apps/Cargo.toml --locked -e normal`, before and after. The runtime-to-deployment paths through `wamn-kubernetes`, `wamn-postgres`, `wamn-engine` and `wamn-storage` are gone.
- Existing tests move with their production owner, or have their imports adapted. Run the tests of moved code, among them `crates/postgres/tests/issuer_admission.rs` and `crates/postgres/tests/password_login_live.rs` (membership-gated issuance, concurrent renewal and logout, disable and re-enable), and the ignored run.
- In step 4, make sure that every early return in renewal that must commit a revocation still commits.
- The `Dockerfile` copies whole directories and builds by package name, so it needs no change in this epic.
- The write-log journey (`tests/journeys/tests/write_log.rs`) covers transaction settlement. The journey run on kind needs explicit authorization.
- No new tests.
- This forces a host image rebuild: `wamn-plugin`, the host and the CDC reader change.

## Expected end state

I, not measured.

- Measured after the epic, never a target: the crate count (58 before; `wamn-provision`, `wamn-application-chart`, `wamn-run-state` and `wamn-dev` go, and `wamn-identity` is added) and the trait count (about 77 before).
- `wamn-postgres` holds the kernel only.
- The CDC reader links the kernel and no domain crate.
