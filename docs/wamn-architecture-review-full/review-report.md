# Review report: the wamn architecture review

As of 2026-11-12. This report checks the 91-tab architecture review against the code at `b94833fd` (`epic/dep7`). Only `docs/plan/deploy.md` changed in the repository after that commit, so every finding holds on main. The report lists corrections, proposes more deletion and restructure, records the user's rulings of 2026-11-12, and orders the work as a dependency list.

What goes into the doc, and when:

- The rows of section 1.1 go in as the rulings below decide them.
- The "Confirmed" rows of section 1.2 go in now.
- The other rows of section 1.2 go in after their check.
- The proposals of Part 2 go in as ruled.

## How this report was made

Eight parallel read-only review agents each took one area: shared crates, runtime, execution path, generator, model and events, identity and edge, deploy and CLI, and whole system and tests. Each agent read its tabs against the code, read across to other tabs, and proposed changes. The author and a second reviewer then read the code for each claim that the Basis column marks "confirmed".

Citations use `path::symbol`. The Basis column has four values.

- Confirmed: the cited code shows it.
- Confirmed, consequence inferred: the code shows the mechanism, and the effect is inference.
- Upstream: the source or documentation of an upstream project shows it.
- Not checked: an agent claim that no one read in the code yet.

Line counts are measured where a row says "measured". All other counts are estimates.

The rules for every proposal are the standing rules (greenfield, prefer delete) and the user's direction for this pass, verbatim: "single responsibility, single source of truth, no rewrite, restructure or rename is too large. preference for deletion of all accidental complexity".

The testing rule of 2026-11-12 applies to every step. Keep tests to a minimum. Test only interfaces. Do not test the presence of a string or a file, and do not compare strings or files exactly. Do not build complex mocks. A step that changes code deletes the tests of that code which break this rule.

## Part 1. Corrections to the review

### 1.1 Corrections that change a ruling

These rows needed a ruling first. The rulings follow the table.

| # | Tab | Fault | Basis |
| --- | --- | --- | --- |
| C1 | 88 R-03, R-06, Summary "Write path" | The route entry claims inside the operation transaction: `wamn_postgres/write_log.rs::OperationTransaction::begin_opaque` runs after `begin_operation_transaction` in `wamn_postgres/operation.rs::begin`. For an operation with only an external effect, a host stop after the effect rolls back the claim, and a retry repeats the effect. This is a gap in the proposed one write path, not an error in R-03. The two-step semantics stay. Only the host loop and the trait go. | Confirmed, consequence inferred |
| C2 | 88 R-06 | A late finish matches operation, key and `result IS NULL` only (`wamn_postgres/write_log.rs::FINISH_SQL`). After retention deletes a claim and the key comes again, an old finish can write over the new claim. | Confirmed, consequence inferred |
| C7, C8 | 03 ruling 6, 83, Summary 2, 3 and check 1 | `vendor/pg-walstream/src/stream.rs::LogicalReplicationStream::process_wal_message` passes `raw.wal_start` to each event, and `services/cdc-reader/src/lib.rs::drain` builds the message id from it. Postgres decodes the rows of one multi-insert at one record position (Postgres `decode.c`, multi-insert path). Broker deduplication can then drop rows of one `COPY`. The run key is `event:{sequence}:{source_event_id}` (`wamn_postgres/event_batch.rs::admit_event_batch`), so a republished event starts a second run. `environment/src/capture_gap.rs::last_cdc_event` parses the old prefix. Derived events have no LSN. | Confirmed for the code. Upstream for the shared LSN. Dropped rows inferred. |
| C10 | 60 A3, 71, 77 | Package apply uses `SET LOCAL ROLE` for the package owner, and `apply_package/roles.rs::reset_host_role` returns with `RESET ROLE`. With no allowlist, migration SQL can run `RESET ROLE` and act as the session login. A trigger on the migration role then misses later grants. | Confirmed, consequence inferred |
| C13 | 77 | History rendering has a second production caller: `apply_package/record_history.rs::create_history_tables`. A move into the generator makes deploy depend on build tools. | Confirmed |
| C18b | 57 | Tab 57 rules that API users are service principals, "not supported today". `PlatformRouteAuthenticator::authenticate` requires the `admin` role and the operator subject. Admitting a service principal by its grants is new support. | Not checked |
| C21 | 62, 65, 66 | `provision/src/secret.rs::WorkloadSecretBody` holds one `url`. CloudNativePG managed roles need `kubernetes.io/basic-auth` Secrets (CNPG documentation, managed roles). CNPG runs in `platform`, hosts in `hosts`, identity in a third namespace. A Secret cannot cross namespaces, so "one Secret per role" fails in this layout. | Confirmed for the format. Upstream for the Secret type. Conflict inferred. |
| C24 | 63, 64 | Two parts. `DROP OWNED BY` also revokes the grants that the migration role holds: this is a defect, and the fix (restore the grants) goes in directly. Today reset deletes application rows, manually created workflow definitions, queued runs and queued events, and the proposed reset keeps the runs and events: this needed a ruling. | Confirmed for today's reset (`environment/sql/reset.rs::reset_deployed_with_admin`, its warning text). Upstream for `DROP OWNED BY`. |
| C29 | 24, 31 A, 58 3 A | The tab 58 3 A wording says "the pool bound is the limit". In wash-runtime v2.12.1, `max_concurrency` is the number of calls per warm instance, overflow calls run in new stores, and nothing caps the calls. | Upstream |
| C31 | 29, 31 | Palette nodes have handler exports, node configuration, ports, retries and delays. No route contract covers them. | Not checked |

### Rulings on section 1.1

1. Claim before an external effect (C1): A. A committed claim before the effect, inside the native operation capability, for operations with an external effect. Change the Summary "Write path" row: the two-step semantics stay, and only the host loop and the trait go. Work: S7.
2. Late finish (C2): B. No claim id. The invariant is that retention age is far above the longest call. Retention is 30 days (`provision/src/audit_retention.rs::WRITE_LOG_RETENTION_SQL`), and the longest call is 30 s (`engine/src/engine.rs::MAX_HOST_CALL_DURATION`). State this invariant in tab 88 R-06. Work: S7.
3. Event identity (C7, C8): A. CDC identity is database incarnation, commit LSN and row ordinal. The message id and the run key both use it, and the capture-gap parser changes in the same commit. Derived events keep their logical id. The stream sequence serves only the enable boundary. This changes tab 03 ruling 6 and closes Summary open points 2 and 3 and check 1. Work: S8.
4. Database incarnation: A. The Postgres system identifier plus the timeline id. Work: S8.
5. Replay horizon: A. Deduplication holds while the run row exists, and pruning ends it. Work: S8.
6. Migration login (C10): A. The migration connection logs in as the migration role, with no `SET ROLE`. Trusted catalog writes use a separate connection. This is a detail of tab 60 A3. Work: S6.
7. Reset scope (C24): A. Reset clears runs, queued events and authored workflows, as today, and it restores the migration role grants. Work: S6.
8. Concurrency limit (C29): no wamn limit. Keep tab 31 A and tab 58 3 A. Correct the tab 58 wording "the pool bound is the limit": the pool bound sets warm instances only, and nothing caps the calls. The edge device gets no connection cap. Work: S7, S11.
9. Namespace for CloudNativePG (C21): A. The CNPG cluster and its credential consumers share one org namespace, with one Secret per role. Work: S4, S10.
10. History DDL (C13): A. The build emits history DDL as migrations, and one `Migrator` applies them (P8). Work: S5, S6.
11. Service principals (C18b): A. S11 adds a verb that creates an API service principal and its PAT. It drops the operator-subject check in `PlatformRouteAuthenticator::authenticate`, because grants already read the same for a service and a user. Work: S11.
12. Palette nodes (C31): A. A private in-process entry into the same route implementation, with no public HTTP. Work: S7.

### 1.2 Corrections to apply directly

These rows are missed callers, missed copies or defects. Each one follows a ruling that already exists. "Apply" means: add the code and the tests that go with it to the owning tab, then to the Summary and Architecture tabs. A row marked "Not checked" is applied after a check.

| # | Tab | Fault | Action | Basis |
| --- | --- | --- | --- | --- |
| C3 | 09, 88 | The edge `intents` table is unique on `(tenant, idempotency_key)` (`run-state-sqlite/src/lib.rs::SCHEMA`). The `operation` column exists but is not in the key, so two operations with one key can return each other's result. | Key by operation, in the same change as P14. | Confirmed, consequence inferred |
| C4 | 58, 88 R-06 | `services/edge/src/intents.rs` (`intents list`, `intents resolve`) calls `IntentStore::uncertain` and `resolve`. `services/edge/src/samples.rs` imports `OperatorActionBasis`. | Add both to tabs 58 and 88 with the tests that go. Sample refusal gets its own local type. | Confirmed |
| C5 | 58, Architecture | `samples.rs::SampleIntents::finish` calls `finish_in` inside `store.transact`, so the sample and the outcome commit together. `services/edge/src/device.rs::run` calls `EdgeDelivery`. | State the requirement on the edge path: the sample and the outcome commit together. | Partly confirmed |
| C6 | Architecture | The crossing row "Platform code inside each application artifact", the crate rows "http-route guest" and "Composer and virtualizer", the route call path and both diagrams say that the route guest stays composed with the application. Tab 38 rulings 1 and C and tab 73 make the route table its own component. | Fix those rows and the Summary "Write path" row. The flow is: route component, host `wamn:operation` (JSON), host conversion, the typed `run` of the application. Only overlay and base composition stays (tab 73 finding 2 A). | Confirmed |
| C9 | 03 ruling 3 | `apps/platform/events/materializer/src/workflow.rs::SourceEvent::decode` finds derived events by `format-version`. | Record that a tagged enum in `wamn-event-wire` replaces the discriminator. | Confirmed |
| C11 | 07, Summary, Architecture | The read-only transaction for dynamic SQL is ruled (index, "Rulings that cross crates", tab 07). The Summary and Architecture tabs dropped it. The doc comment on `wamn_postgres/claims.rs::statement_mutates_session` names an escape: a `DO` block with `EXECUTE`. | Restore it in the Summary "Runtime and host" row, the Architecture crate row `wamn-runtime`, the route call "SQL" step and rules row 13. The `pg_query` check of tab 74 replaces the text filter. | Confirmed |
| C12 | 77 | Tab 77 lists "ALTER DEFAULT PRIVILEGES to check". Postgres fires event triggers for that command (Postgres 18 event trigger firing matrix). | Close the check and add the command to the trigger. | Upstream, not checked |
| C14 | 74, 75 | `serving_manifest.rs::ServingManifest::from_canonical_bytes` drops `workflow` on read. `ServingComponentOperation` copies the descriptor operations. | Delete the strip and the duplicate operation map with their tests. | Confirmed |
| C15 | 80 1 A | `package_artifact.rs::source_components` and `dev/coordinator.rs::ProductionDevStageRunner::admit` read `publication/wirings` through `wiring_components` for palette components and `store_alias`. | Workflow bindings own `store_alias`. | Confirmed |
| C16 | 79 1 B | The materializer guest prepares application consumers. Hosts already admit natively (`wamn_jetstream/admission.rs::WamnJetstream::route_event`). | Apply after a check: consumer preparation moves to provisioning. | Not checked |
| C17 | 83 | The commit arm of `cdc-reader/src/lib.rs::drain` calls `connect_system` and holds the environment lock. | Reset stops the reader, then purges. Then the system login goes. | Confirmed |
| C18a | 57 finding 8 A | `identity.pats` accepts `principal_type = 'user'` (`deploy/sql/system-schema.sql`), and route authentication keeps a user branch. | Add the rows, the branch and `SessionAuthority::Pat` to tab 57 as code that goes. Keep the audience and role helpers that password login uses. | Confirmed |
| C19 | 65 finding 4 A | `identity/platform/src/org.rs::materialize_admin_grants` copies org administration into project rows, and login role selection reads them. | Add the function and its readers to tab 65 as code that goes. | Confirmed |
| C20 | 58, Summary check 6 | `control/package/src/web_artifact.rs::upload_web_to_store` writes `SCOPE_FILE` from org and project into the web artifact. | It answers check 6. The fix goes with P26. | Confirmed for the write. Skipped uploads not checked. |
| C22 | 65 1 A | `managed.roles` is one list on the Cluster object. | Apply after a check: the org chart renders the full role list from registry facts. | Not checked |
| C23 | 62, 63 | `environment/analyse.rs` takes the newest `deployed` or `superseded` revision and skips failed ones. After A ok, B ok and C failed, "previous" is A, not B. | Fix the history reader after a read of the rollback caller (ruling 32). | Confirmed for the reader. Rollback effect inferred. |
| C25 | 63 | `environment/stage.rs::retire` keeps the digests of runs not in a terminal status. | Keep the pin condition in tab 63 (ruling 34). | Confirmed |
| C26 | Summary defect "Lifecycle lock" | `environment/lock.rs::LifecycleLock::connect`: the heartbeat loop breaks, and nothing tells the verb. | Merge into that defect. | Confirmed |
| C27 | Summary defect "NATS authorization Secret" | Two writers read, merge and apply the Secret: `platform/deploy.rs::infrastructure` and `environment/ensure.rs::provision_resources`. Neither carries `resourceVersion`. | Merge into that defect. | Confirmed |
| C28 | One-host ruling (2026-11-10) | `ConnectionFacts`, single-release plugins, one JetStream project and `workflow/src/queue.rs::QUEUE_CLAIM_SCOPE` are process-wide. | List as follow-on work of one host per environment (S9). | Confirmed for the scope constant. Others not checked. |
| C30 | 25, 26 | `sign-result` runs after commit, but signing reads authority from the open transaction. | Apply after a check, as a design note for S7: signing authority lives in the invocation record. | Not checked |
| C32 | 78 | `resource_lease.rs::database_session` connects with `tokio_postgres::NoTls`. The OAuth pool and lease cancel also connect. | List them as call sites of the tab 78 TLS ruling. | Confirmed for the lease session. Others not checked. |
| C33 | Summary defects | `auth_cache.rs::clear` does not change `identity_revision`, so a read in progress can store a stale token after a watch reset. Cached approvals outlive the credential deadline. | Add as a Summary defect. | Confirmed for `clear`, race inferred |
| C34 | Summary defects | An open `BoundContainer` keeps old blob credentials after a rotation. | Apply after a check: add as a Summary defect. | Not checked |
| C35 | 09 | `wamn_postgres/pool.rs::StaticCredentialProvider::resolve` checks `app_scope_hash`. `provision/src/tenant_key.rs` matches `_[ab]$` login suffixes. | Delete both with `CredentialGeneration`. | Confirmed |
| C36 | 06 | `control/environment/src/prune_record_history.rs` uses `QualifiedName`. | Callers use `postgres-protocol`, as ruled. | Confirmed |
| C37 | 34, 53 | `generator/src/describe.rs` refuses `INT2`, `FLOAT4` and `BPCHAR`. `wamn_postgres/statements.rs` maps `int2`, `float4`, `varchar`, `bpchar` and `name`. | Add the defect to tabs 34 and 53: one type table for build and run. | Confirmed |
| C38 | 74, 88 R-04 | Document checks sit in `PackageManifest::from_slice`, in `generate/validation.rs::validate_client_package_name` and in KCL. | All document checks move to `wamn-compiler` (ruling 25). | Partly confirmed |
| C39 | Architecture rules row 8 | `generator/src/materialize.rs` and `data_access.rs::grant_columns` derive grants. | Delete the grant subsystem, as rules row 8 rules. | Confirmed |
| C40 | 27 | `execution/host/src/control_route/operation_job.rs` (572 lines measured) adds deploy, rollback, diff, apply, reset and adopt Jobs. | Add the file to tab 27. It moves with the native control service. | Confirmed |
| C41 | Summary defect "`feed.limit`" | `http-route/src/entry.rs::validate_read_limits` accepts any limit when the route has no `read_limits`. | Keep the defect. Every list route gets a maximum at build. | Confirmed for the guard. Composed route not checked. |

### 1.3 Fixes to the Summary tab and the doc

- Fix the two doc errors first: C6 (the route component is separate, tabs 38 and 73, with both diagrams) and C11 (restore the read-only transaction in the Summary and Architecture tabs).
- Remove the "Correction (2026-11-12)" notes in tabs 86 and 88. The doc keeps no history.
- Open point 4: the reaper SQL (`run-state/src/queue/sql.rs::select_exhausted_production_sql`) filters by the mounted package-id set, not by digest. Reword the point to the narrower case, which ruling 34 now decides.
- Open point 5: write down the tables that `wamn_control` can change without limit. Ruling 33 decides the point.
- Open point 7: narrow it to guest memory. `services/host/src/host.rs::HostArgs::socket_egress` defaults to `enforce`.
- Open point 12: tab 11 finding 14 option A ("keep membership and current user") rules it. Close the point.
- Check 1: ruling 3 answers it. Check 2: `provision_project_env.rs` revokes `CONNECT` from `PUBLIC` and from `wamn_app`. Read the other roles, then close it. Check 4: a reloader sidecar sends SIGHUP (`deploy/infra/nats-jetstream.yaml`). Remove it. Check 5: `wamn_provisioner` is created but its deployed login is unused. Make it a deletion. Check 6: C20 answers it.
- The crate table omits five ruled owners: `wamn-scalar`, the cursor envelope, the NATS connection, the events lifecycle owner and KCL invocation.
- Rewrite the Crate review index as one table (tab, verdict, rulings), with no pending lines and no closed holds.
- Eight pages in `docs/architecture/` are wrong after the rulings: `overview.md`, `execution.md`, `data-access.md`, `components.md`, `capabilities.md`, `naming.md`, `native-alignment.md` and `edge.md`. Each step corrects the page that it changes.

### Rulings on the corrections

13. Overrides: A. The generated export always validates the input, and then it calls the override. Work: S5.
14. Routine effects: A. A statement that calls an authored routine counts as a write and keeps the command transaction. This applies with P9, which deletes `refuse_routines`. Work: S5, S6.
15. Application naming rule (Summary 10): B. No rule, the same as for model names (tab 26). Correct `naming.md`, which calls it mandatory. Work: S5.
16. `signed_reads` (Summary 8): A. Add `signed_reads` to the KCL `Result` and generate it. Today the generator writes an empty map, so no application can sign. This keeps tab 25 B1. Work: S5.

## Part 2. Overhaul beyond the rulings

These proposals go further than the review. Estimates exclude moved code and are not measured unless the row says so. The last column gives the ruling or the tab that the proposal follows.

| # | Proposal | Goes | Replaced by | Estimate | Status |
| --- | --- | --- | --- | --- | --- |
| P1 | Delete the local runtime subsystem after the `kind` loop measurement (`wamn-oeeon`) | `local_application.rs` (1,109 lines measured), host local modes, their branches | `kind` with the production path | 1,100 to 1,500 | Follows tabs 70 1 A and 72 1 A |
| P2 | One run persistence owner | Queue SQL in runtime `production_claim`, `event_admission`, `event_batch`, `workflow_resolution`, `wiring_resolution` (3,819 lines measured) | `wamn-run-state` admits, claims and settles runs. Workflow orchestrates. | 750 to 1,000 | Extends tabs 09, 14, 29 |
| P3 | One immutable authority source | Release-component JSON rebuilt from SQL, the `EffectKey` and `EffectFixed` caches, repeated manifest comparisons | The loaded release. Connection facts give only mutable state. | 250 to 400 | Extends tabs 11, 19 |
| P4 | Delete the custom guest log pipeline | Second SDK provider, front queue, drain task and context parser in `wamn_logging.rs` (855 lines measured) | wash-runtime `TracingLogger` and the host tracing export | 650 to 750 | Ruled yes (ruling 21), after a read of `TracingLogger` against tab 18 |
| P5 | One HTTP client and one metadata token service | Per-backend client and token code for registry, GCS and KMS | One `hyper` client and one token service. `oauth2` builds the protocol. | 250 to 450 | Extends tab 77 4 A |
| P6 | One operation controller | Parallel lifecycles in `NativePolicy::activate` and the Postgres `begin` | One authorized invocation record that dispatch borrows | 150 to 250 | Extends tab 25 1 A |
| P7 | Merge `wamn-router` into `wamn-workflow` | The crate boundary. Workflow is its only consumer. | Private graph modules | 30 to 60, plus the ruled 341 | Ruled yes (ruling 18). Reverses tabs 28 and 29. |
| P8 | History DDL as generated migrations | `physical-schema.json`, recipe fingerprints, admission re-rendering, live trigger convergence | Build emits history migrations. One `Migrator` applies them. | 800 to 1,300 | Ruled yes (ruling 10) |
| P9 | Introspect only what generation uses | Schema-wide refusal scans (`introspection/src/postgres.rs::refuse_routines` and its siblings) | A test that the selected columns fit the generated action | 400 to 700 | Ruled yes (rulings 14, 23) |
| P10 | One operation description in memory | Contract JSON assembly, parsing and repeated projections | One build-time description that WIT, Rust, routes, OpenAPI and clients read | 1,500 to 2,500 | Ruled yes (ruling 23) |
| P11 | The generator becomes a package compiler | Cargo calls, component copies, composition, the web build launch, the fingerprint build script | xtask owns orchestration. The compiler takes inputs and returns facts and artifacts. | 700 to 1,000 moved out | Ruled yes (ruling 23) |
| P12 | Fixed guest support apart from generated code | Wrapper layers in generated components | One guest support library. Generated exports validate, dispatch and translate. | 1,000 to 1,500 | Ruled yes (ruling 23) |
| P13 | Release publication owns route data | Route and application composition, generated dispatch (441 lines measured), composer CLI | Publication emits the route configuration. The composer joins overlay and base bytes. | about 600 | Ruled yes (ruling 23). Tab 31 gave the route table to the composer. |
| P14 | Delete the generic intent layer | `IntentStore` trait, operator vocabulary, the `wamn-run-state-sqlite` crate (656 lines measured) | Edge owns its SQLite store. Postgres owns its write log. WIT owns the contract. | 200 to 300 | Ruled yes after S7 (ruling 17). Reverses tab 09 finding 3. |
| P15 | Delete settings with one value | Both `DurabilityClass` types, the `standard` columns, queue `priority`, the old-schema fallback | Nothing | 150 to 250 | Extends tab 09 |
| P16 | One event contract owner | Two discriminators, `SourceEvent` decoding in the materializer, the `EventRegistration` carrier, the `wamn-event-reg` and `wamn-materializer` crates (1,790 lines measured) | `wamn-event-wire` owns the wire. The workflow owner holds triggers, matching and input. | 200 to 350 | Extends tabs 03, 79 |
| P17 | CDC with one coordinate and one lifecycle | Registration polling, shape checks, the per-commit system connection | The configuration file, and a reset that stops the reader | 350 to 500 | Extends tab 83 |
| P18 | Native CNPG credentials end to end | URL Secrets, generation conversion, five parser files and `secret.rs` (2,805 lines measured) | One basic-auth Secret per role. Connection data in component configuration. One SQLx constructor. | 1,500 to 2,300 | Extends tabs 59, 65 |
| P19 | Kubernetes Secrets as the only credential store | File backend, backend dispatch, local cleanup branches | Typed Secret calls in `wamn-kubernetes` | 120 to 200 | Ruled yes (ruling 19) |
| P20 | Upstream charts, not stored YAML | cert-manager, CNPG operator and barman YAML (35,435 lines measured), payload preparation in the image | Pinned chart dependencies and values | 35,000 | Extends tab 66 |
| P21 | The stamped chart is the whole installer input | Worker chart copy from the image, two overlay files, hashed input ConfigMaps | The worker fetches the immutable chart | 100 to 200 | Extends tab 66 |
| P22 | BuildKit runs the build | Hash labels, copy tags, process scans, spend counters and fake recorders in four platform-build files (3,868 lines measured) | One xtask command for build, push and stamp | 2,500 to 3,300 | Extends tab 67 |
| P23 | One publication owner | `apply_package` inside package control. Package and release publication in two crates. | `apply_package` moves to `wamn-environment`. One publication crate. The CLI needs no `Platform`. | under 100, but a clean client and worker split | Ruled yes with the renames (ruling 24) |
| P24 | One identity protocol crate | `wamn-project-state`, the database-linked dependency of the identity client | `wamn-session` widens to principals, roles, credential formats and wire shapes, and takes the name `wamn-identity-protocol` | 250 to 350 | Ruled yes (ruling 22). Tab 05 found the name right for today's scope, and the rename follows from the wider scope. |
| P25 | One broker owner | Two NATS change writers, KV and stream code in identity, provision and environment | `wamn-broker`. Identity says what changed, and the broker publishes. | 70 to 120 | Names the NATS owner that tabs 56 and 59 left open |
| P26 | One web front owner | The nginx sidecar, uploaded scope files (C20), the unbounded file cache | The Rust web front owns the password proxy, response headers and `config.json`. TLS stays at the ingress. | 80 to 140 | Ruled yes (ruling 20) |
| P27 | Delete test machinery instead of moving it | The `schema_drift` pair (438 lines measured), most of `journey.rs`, the `tests/integration` umbrella crate | Tests beside their owners. Root keeps cross-service journeys only. | 650 or more, plus the ruled 1,268 | Extends tab 86 |
| P28 | Remove the third Cargo workspace | `apps/platform/no-std`: a 92-line manifest and the 78-line `guest_runtime.rs` | Standard library guests on standard WASI | 170 | Follows rules row 6 |

The ruled deletions stay in force. Two of them are large: the scenario worker and authoring model (6,839 lines measured), and the stored copy of `pg_walstream` (60,210 lines measured). The generator can go from about 25,700 production lines to 12,000 to 15,000.

### Rulings on the overhaul

17. Intent store (P14): yes, after S7. C3 is fixed in the same change. Work: S7.
18. Router (P7): yes. Work: S7.
19. Credential store (P19): yes. `wamn-8qril` adds GCP Secret Manager only "if a target cannot run application-layer encryption" (`docs/plan/integration-schema.md`, Secrets and auth). No such target is named, so the file backend goes and the Kubernetes backend stays. `wamn-8qril` stays open as a conditional feature. Work: S2.
20. Web front (P26): A. The Rust front owns the headers, the proxy and `config.json`. TLS stays at the ingress with cert-manager. On the edge device, R-09 A stays as ruled. Work: S11.
21. Guest log pipeline (P4): A, after a read of `TracingLogger` against the tab 18 rulings. Work: S12.
22. Identity crate (P24): A. Widen `wamn-session`, delete `wamn-project-state`, and rename the crate. Work: S2.
23. Compiler proposals (P9 to P13): yes, as one direction for the compiler work. Work: S5.

## Part 3. Target crate map and layout

Ruling 25 moves the authored model into `wamn-compiler`. After every ruling and proposal, each crate has one purpose. "New" marks a crate that does not exist today.

| Crate | One purpose |
| --- | --- |
| `wamn-digest` (new) | Canonical JSON bytes, SHA-256 and the `sha256:<hex>` format. It replaces the copies in the execution contract, package artifact, schema connections, component admission, catalog, runtime connection definitions, local manifest identity, composer, generator and fixture digest. |
| `wamn-scalar` (new) | Scalar text and SQL value types, with one enum for the platform |
| `wamn-registry` | Org, environment and project coordinates, registry SQL, and one name module for every spelling. It keeps environment scope apart from project-database scope. |
| `wamn-event-wire` | The event envelope as one tagged enum, message identity, decode and identity checks |
| `wamn-postgres-statements`, `wamn-number-pattern` | Kept as ruled. Number pattern also takes the pure numbering logic of tab 13. |
| `wamn-data-access` | The guest data library (tabs 38, 87) |
| `wamn-identity-protocol` | Principals, roles, credential formats and session tokens |
| `wamn-identity-client` | The HTTPS client of the identity service |
| `wamn-platform-identity` | Identity authority and its SQL |
| `wamn-broker` (new) | NATS connection, streams and KV buckets |
| `wamn-kubernetes` (new) | One Kubernetes client, Job runner and Secret store on `hyper` |
| `wamn-catalog` | The served release model |
| `wamn-compiler` | The authored model, every document check (R-04), describe and the generated outputs |
| `wamn-schema-introspection` | Description of the selected relations |
| `wamn-package-artifact` | OCI transport of a package |
| `wamn-component-policy`, `wamn-component-composer` | Import admission. Overlay and base byte composition. |
| `wamn-engine` | Wasm dispatch on native wash-runtime |
| Runtime capability crates | One crate per capability (Postgres, HTTP connection, blob, authentication, events) on one scope registry (ruling 26) |
| `wamn-execution-host` | Invocation authority and native platform routes |
| `wamn-run-state` | The run store: admission, claim and settlement |
| `wamn-workflow` | Workflow lifecycle, graph walk, triggers and matching |
| `wamn-provision` | Database roles and credentials |
| `wamn-publish` | Package and release publication |
| `wamn-environment` | Environment lifecycle and package apply |
| `wamn-platform-install` | Cluster install from charts |
| `wamn-dev` | The dev loop: watch, publish, control route |
| Route component | The http-route guest, its own component (tab 38) |
| Services | `wamn-host`, `wamn-control`, `wamn-identity`, `wamn-web`, `wamn-edge`, `wamn-cdc-reader`, and the CLI `wamn` with the hidden worker `wamn-ctl` |
| Test support | `wamn-fixture-package` and the Postgres test support |
| `web/` | The TypeScript packages, out of scope for this report |
| `xtask` | Repository tasks, test selection and build |

Renames, with the tab of each crate: `wamn-control-registry` to `wamn-registry` (02), `wamn-session` to `wamn-identity-protocol` (05), `wamn-schema-generator` to `wamn-compiler` (32 to 53), `wamn-control-provision` to `wamn-provision` (59), `wamn-dev-control` to `wamn-dev` (69, 70), `wamn-environment-control` split into `wamn-environment` and `wamn-platform-install` (62 to 68), and `wamn-package-control` with `wamn-release-control` into `wamn-publish` (60, 61).

Merged: `wamn-credential-store` into `wamn-kubernetes`, `wamn-router` into `wamn-workflow`, `tools/bench` into `xtask`, and `wamn-runtime` into the capability crates.

Deleted: `wamn-execution-contract`, `wamn-pg-core`, `wamn-schema-control`, `wamn-authoring-model`, `wamn-client`, `wamn-run-state-sqlite` (its store moves into edge), `wamn-project-state`, `wamn-component-virtualizer`, `wamn-event-reg`, `wamn-materializer`, the materializer guest, `wamn-scenario-worker` and `wamn-gates`. The conformance, system and integration test crates go only where tab 86 findings 1 to 9 agree. Read them against tab 86 before the list is final.

The proposed directory layout puts each type of code in one place.

- `services/<process>` for each binary
- `crates/<purpose>` with no group folders
- `guests/<component>` for the platform guests now under `apps/platform`
- `apps/<package>` for applications only
- `tests/journeys` for cross-service journeys
- `test-support/{postgres,fixtures,resources}`
- `xtask`
- `deploy/charts`

### Rulings on the crate map

24. Names: yes to the renames above. Work: S2, S5, S10.
25. Catalog: A. This reopens R-10: the authored model moves into `wamn-compiler`. Work: S5.
26. Runtime: A. One crate per capability after the scope registry lands. Work: S12.
27. Layout: yes, as the last step. Work: S13.

## Part 4. Order of work

The user's rule is "never propose new epics", and question 28 (lift the rule) is still open. This part therefore lists the work as steps in dependency order, with no epic names. Each step names its goal, its scope, what it deletes and what it needs first. Every step corrects the `docs/architecture/` pages that it changes and deletes the tests of the code that it deletes. Steps that share no files can run in parallel.

### S1. Ruled deletions

Goal: remove the code that a ruling deletes with no replacement.
Scope: the scenario worker and authoring model, `wamn-client`, `wamn-execution-contract`, `wamn-pg-core`, user PATs (C18a), `flow_id` and `flow_version`, `stream_seq`, `mint_evt_run_id`, the `ServingManifest` strip (C14), `wamn-gates` and `deploy/gates`, and the stored `pg_walstream` (replaced by a git dependency, with both Docker `COPY vendor` lines removed).
Needs: nothing.

### S1b. Proposed deletions

Goal: remove code with no product caller that no ruling covers yet.
Scope: queue `priority` and `DurabilityClass` (P15), `schema_drift` and most of `journey.rs` (P27), the unused WIT world `runner-http-effect-plugin` and the six unused workspace dependencies (Summary defect, default delete).
Needs: acceptance of P15 and P27.

### S2. Shared value owners

Goal: each value rule has one implementation.
Scope: `wamn-digest`, `wamn-scalar`, `jsonwebtoken` in session and runtime, `postgres-protocol` quoting, `wamn-identity-protocol`, `wamn-broker`, `wamn-kubernetes`.
Deletes: the digest copies, the scalar and error copies, the hand JWT, two NATS change writers, the file credential backend and `wamn-project-state`.
Needs: S1. It touches every crate manifest, so it merges alone.

### S3. Registry, hierarchy and authority

Goal: org, environment and project are one model, and every name comes from it.
Scope: `wamn-registry`, `system-schema.sql`, identity authorization, provision, release, host status and event names, and row security for control logins (ruling 33).
Invariants: environment scope and project-database scope are separate coordinates. Administration is derived and never copied (C19). Project admins manage the connections of their project (ruling 36).
Deletes: the registry document model, copied admin rows, `tenant_environments` and every hand `format!` name.
Needs: S2.

### S4. Database access

Goal: every connection uses SQLx with TLS and one login per role.
Scope: runtime pools, the OAuth pool, the lease session and lease cancel (C32), provision, CNPG managed roles with basic-auth Secrets in the org namespace (ruling 9), and the SELECT-only login in a read-only transaction (C11).
Deletes: deadpool, `CredentialGeneration` with `app_scope_hash` and the login suffixes (C35), URL Secrets, the text session guard and the unused provisioner login.
Needs: S3.

### S5. Compiler

Goal: `wamn.k` is the one source, and one build produces every fact once.
Scope: `wamn-compiler` with the authored model, catalog, KCL, introspection and apps, `signed_reads` in KCL (ruling 16), fixed typed list statements (ruling 37).
Invariants: one operation description, one type table (C37), every statement described at build, validation before any override (ruling 13), a statement that calls a routine is a write (ruling 14), and a maximum on every list route (C41).
Deletes: publication JSON, contract files, source maps, the verifier, the fingerprint script, the grant subsystem (C39), duplicate operation facts (C14), freshness hashing and `refuse_routines` with the other refusal scans.
Needs: S2 and the renames (ruling 24). It can run beside S3 and S4.

### S6. Migrations and schema lifecycle

Goal: one SQLx `Migrator` runs every migration under a login that SQL cannot escape.
Scope: package apply, platform migrations, history DDL as migrations (ruling 10), and reset (ruling 7).
Invariants: the migration connection logs in as the migration role (ruling 6). The event trigger covers GRANT, REVOKE and `ALTER DEFAULT PRIVILEGES` (C12). Reset restores the migration role grants and clears runs, events and authored workflows.
Deletes: `wamn-schema-control`, the hand runner, the ownership table, the allowlist, `physical-schema.json`, the adoption and resolve states and package owner roles.
Needs: S4, S5.

### S7. One invocation path

Goal: HTTP, workflows and edge reach the database through one route driver.
Scope: the scope registry, one operation controller, the separate route component with typed `run`, the host JSON converter, `sign-result` (C30), R-06 C with a committed pre-effect claim (ruling 1) and the retention invariant (ruling 2), the private palette entry (ruling 12), and the workflow and edge switch with the sample transaction (C5).
Deletes: the host loop, the two-step claim driver, delivery WIT, the bridge, codecs, `run-json`, `read-json`, the dispatch generator, `wiring_lowering.rs`, the virtualizer, edge intent repair (C4), `wamn-router` (ruling 18), and then the generic intent layer (ruling 17).
Needs: S4, S5. One owner holds the whole move, including its final deletion step.

### S8. Runs and events

Goal: one run store and one event identity.
Scope: `wamn-run-state`, `wamn-event-wire`, native admission, the CDC reader, capture gap, and reset coordination (C17), with the event identity of rulings 3 to 5.
Deletes: the materializer guest and its workload, the registration WIT and credential, `EventRegistration`, CDC polling and shape checks, the CDC system login, and runtime queue SQL.
Needs: S3, S7.

### S9. One host per environment

Goal: one host serves every project and release of its environment.
Scope: host startup, the scope registry, plugins, queue and scheduler, readiness, environment charts, the process-wide state of C28, and guest memory in enforce mode (ruling 35).
Invariants: project credentials stay apart. Queued runs use their pinned release, and a release stays loaded until its unfinished runs end (ruling 34). One failed release does not stop the others.
Deletes: process release flags, single-release plugin fields, the literal queue scope, release host groups and the host-only operator chart per release.
Needs: S3, S7, S8, and the memory budget value.

### S10. Install, deploy and control

Goal: Helm owns install state, and one control service runs every verb that changes deployed state.
Scope: platform, environment and org charts, the native control service with one Job runner and `operation_job.rs` (C40), the history reader and rollback to the latest successful revision (C23, ruling 32), lock loss (C26), the NATS Secret writer (C27), pin-aware retention (C25), the environment and install crates, publication, the CLI boundary of ruling 39, and the `kind` loop measurement followed by P1.
Deletes: Rust manifest renderers, 35,435 lines of stored YAML, selection ConfigMaps, nested SQL Jobs, public worker verbs, `wamn-ctl-ops`, pod-log polling, and the local host modes after the measurement.
Needs: S3, S4, S6.

### S11. Identity and web front

Goal: identity has stated rules, and the web front has one owner.
Scope: revocation bounds from project and application configuration (ruling 29), scheduled key rotation (ruling 30), a verb that creates an API service principal and its PAT (ruling 11), the web front with headers, proxy and `config.json` (ruling 20), throttling keyed by the forwarded client, and edge loopback ingress.
Deletes: the operator-subject check in route authentication, nginx, uploaded scope files (C20), the unbounded file cache, the `wamn-edge` feature split and the device limiter (ruling 8).
Needs: S2, S3, and the rotation cadence.

### S12. Runtime capabilities

Goal: each capability has one binding, one client and one log path.
Scope: P3, P4 and P5, blob generation fencing (C34), token cache fencing (C33), loopback denial, the 64 MiB upload limit (ruling 31), and the split into capability crates (ruling 26).
Deletes: the release JSON rebuild, the custom log pipeline (ruling 21), per-backend HTTP code and `reqwest`.
Needs: the S7 scope registry.

### S13. Build, tooling, tests and layout

Goal: one build path, one test selector, and a tree that shows ownership.
Scope: BuildKit through xtask, the justfile, a tool version file, test tiers from stated needs, the integration crate removal, the directory layout (ruling 27), the final `docs/architecture/` rewrite.
Deletes: platform-build Rust, scripts and their tests, the target roster, the third Cargo workspace if standard WASI allows it, and every remaining test that checks the presence of a string or a file, compares text or files exactly, or depends on a complex mock.
Needs: the BuildKit and xtask parts can start after S1. The layout move goes last.

### Dependency order

Steps on one line can run in parallel.

1. S1, S1b
2. S2
3. S3, S5
4. S4, S11
5. S6, S7
6. S8, S10, S12
7. S9
8. S13

### Rulings on the order of work

28. Epics: open. No ruling on "never propose new epics". The steps stay as steps.
29. Revocation (Summary 1): each project and application states its revocation bounds in its configuration. There are no fixed platform values. Each cache entry still ends at the credential deadline (C33). Work: S11.
30. Key rotation (Summary check 3): yes. The identity service rotates signing keys on a schedule with its existing commands. Work: S11.
31. Upload limit (R-12): 64 MiB, the value of `wamn_blobstore/intake.rs::MAX_OBJECT_BYTES`. Work: S12.
32. Rollback (C23): A. After a failed head, rollback restores the latest successful revision. Read the rollback caller first. Work: S10.
33. Control role (Summary 5): A. Postgres limits each control login to its org with row security. Work: S3.
34. Pinned runs (Summary 4, C25): A. A release stays loaded until its unfinished runs end, and stranded runs are reported. Work: S9, S10.
35. Guest memory (Summary 7): yes, enforce. Work: S9.
36. Connection authority (Summary 6): A. Project admins manage the connections of their project. Work: S3, S11.
37. List reads (Summary 9): A. Fixed typed statements per sort. Dynamic SQL covers other sorts. Work: S5.
38. Feature order (Summary 11): sync first. No step in this report covers screens or sync.
39. CLI boundary: B. Local build, dev and eject stay local, and only verbs that change deployed state go through control routes. This changes the tab 71 ruling. Work: S10.

### Still open

- Question 28: lift "never propose new epics" for the steps.
- The signing-key rotation cadence (ruling 30).
- The guest memory budget value (ruling 35).
- The R-06 timeout that separates `in-progress` from `outcome-unknown` (tab 88 R-06 C). Ruling 2 sets the retention invariant but not this value.
- Questions 40 to 43 on words (Part 5).

### Summary open points and their rulings

| Summary, needs a ruling | Ruling |
| --- | --- |
| 1 Revocation rule | 29 |
| 2 Event run key, 3 Message id | 3, 4, 5 |
| 4 Pinned runs | 34 |
| 5 `wamn_control` role | 33 |
| 6 Connection authority | 36 |
| 7 Sockets and guest memory | 35 |
| 8 `signed_reads` | 16 |
| 9 Typed list reads | 37 |
| 10 Manifest naming rules | 15 |
| 11 Feature order | 38 |
| 12 Privilege check | Closed by tab 11 finding 14 A |

## Part 5. One meaning per word

The user's direction of 2026-11-12: remove the banned words from the repository, and give each word one meaning. The banned words are "tenant", "kind" (except the `kind` tool), "ledger" (except an accounting ledger), "floor" (except a physical floor) and "gate" (except a physical gate). The direction also covers other words with more than one meaning.

### 5.1 Measured scope

The counts below are case-insensitive matches inside a word, measured with `rg` over the repository. They exclude `vendor/`, lockfiles, `web/wfe/` and this review directory.

| Word | Files | Matches | Main forms |
| --- | ---: | ---: | --- |
| tenant | 328 | 2,379 | `tenant_key`, `current_tenant_key()`, `tenant_id`, `tenant_environments`, `--tenant`, the fixtures `tenant-a` and `tenant-b` |
| kind | 499 | 5,297 | Rust `*Kind` types and `kind` fields (1,376 bare `kind` in Rust), Kubernetes `kind:` (273 in YAML), `relkind`, `platform_apply_kind` |
| gate | 327 | 2,206 | `wamn-gates`, `wamn-gate-harness`, `gate_reports`, "gate" as a word for a check. `gateway`, `aggregate` and `navigate` only contain the letters. |
| floor | 141 | 521 | `wamn_pg_floor`, `--pg-floor`, `revoke_public_connect_floor_sql`, "floor" for a minimum |
| ledger | 19 | 94 | Accounting ledgers in test fixtures. The other matches are in `docs/history/`. |

### 5.2 What the words mean today, and the proposed replacement

| Word | Meaning in this repository | Proposed replacement |
| --- | --- | --- |
| tenant | The project database scope: the key derived from `env_key` in `provision/src/tenant_key.rs` and `deploy/sql/postgres-init.sql`. In fixtures, an example org. | `project_key` and `current_project_key()` for the scope. `org-a` and `org-b` in fixtures. The `--tenant` flag goes with ruling 39 work. |
| kind | A type or variant name. `docs/history/kind-to-type.md` already records the owner decision that `type` replaces `kind`. | `type` (`ErrorType`, `FailureType`). Kubernetes `kind:` and Postgres `relkind` are names that the software defines, so they stay. |
| gate | A test target, a test image, or a condition that must pass. | "acceptance test" for the test image and targets, "condition" for the rest. `wamn-gates`, `deploy/gates` and `wamn-gate-harness` are deleted in S1, so they are not renamed. |
| floor | A minimum value or a base set of grants. | "minimum" (`--pg-minimum`, `wamn_pg_minimum`), and "base grants" for the connect revoke. |
| ledger | Outside `docs/history/`, an accounting ledger in test fixtures: the project `ledger` in `control_route_live.rs`, `retention_fixture.ledger` and `widget_ledger`. This is correct usage. | No rename. |

### 5.3 Other words with more than one meaning

These words carry two or more meanings in the code and the review. Each needs one meaning and a new word for the others.

| Word | Meanings found | Proposed rule |
| --- | --- | --- |
| claim | A JWT claim, a write log claim, a queue claim of a run | "JWT claim" stays, because the JWT standard defines it, and the text always says "JWT claim". "claim" alone means a write log claim. "lease" for the queue. |
| scope | The scope registry, `QUEUE_CLAIM_SCOPE`, OAuth scope, test scope | "scope" for the invocation scope record only. OAuth scope keeps its standard name. |
| environment | The middle level of the hierarchy, and `env_key`, which includes the project | "environment" for the level only. `env_key` becomes `project_key` with the tenant rename. |
| check | A test, a verification step, a CHECK constraint, a review item | "check" for a SQL `CHECK` constraint only, as the house style requires "make sure" for the verb. |
| class | `AuthorityClass`, a durability class, a test class | Decide with P15, which deletes the durability class. |

### 5.4 Plan

- Words step S0: one lane makes the rename across the repository. It goes after S1, so that it does not rename code that S1 deletes.
- Each later step uses only the new words.
- No lint rule or test looks for the words, because the testing rule forbids tests of string presence. Review keeps the words out.
- Names that the software defines stay: the `kind` tool, Kubernetes `kind:`, Postgres `relkind`, and `gateway`.
- `docs/history/` stays as written, as the `kind` to `type` record already rules.
- The SQL names change in place with no compatibility path (greenfield). Migration numbers are assigned at merge.

### 5.5 Questions on words

40. Tenant replacement: is `project_key` right for the scope that `tenant_key` derives? The alternative is `database_key`. Recommend `project_key`, because one database serves one project of an environment.
41. Order: rename after S1, as above, or now on the current tree? Recommend after S1, because it renames about 2,000 fewer matches.
42. Rust error convention: `CLAUDE.md` says that error structs carry "a kind". Rename the convention and the types to `type` (`ErrorType`)? Recommend yes, as `kind-to-type.md` decided.
43. Other words (5.3): accept the one-meaning rules for claim, scope, environment and check? Recommend yes.

## Part 6. Open beads that the work changes

Status does not change until code lands. The table is a proposal and was not checked against each bead.

| Bead | Disposition |
| --- | --- |
| `wamn-8vw7s` | Superseded by R-06 C (S7) |
| `wamn-eznu` | Obsolete when SQLx replaces rusqlite (S7) |
| `wamn-b71vg` | Obsolete when delivery WIT goes (S7) |
| `wamn-zvqi5` | Obsolete when the materializer guest goes (S8) |
| `wamn-xt0tg` | Subsumed by one operation representation (S5) |
| `wamn-pinbo` | Obsolete when `deploy/gates` goes (S1) |
| `wamn-plx8u` | Superseded by BuildKit (S13) |
| `wamn-prloy`, `wamn-qukfu` | Obsolete with `--record-digest` (S5) |
| `wamn-4rbo8`, `wamn-j1md8` | Obsolete with freshness and the virtualizer (S5, S7) |
| `wamn-vnvuc` | Superseded by one package output (S5) |
| `wamn-tztlm` | Obsolete when shell tools move to xtask (S13) |
| `wamn-kzgt7` | Superseded by deleting native dev provisioning (S10) |
| `wamn-8qril` | Stays open as a conditional feature (ruling 19). Its backend goes into `wamn-kubernetes`' Secret store design if a target needs it. |
| `wamn-film4` | Conflicts with greenfield. Close on the user's word. |
| `wamn-ldmaq` | Rescope to what remains in the install crate (S10) |
| `wamn-j68pg` | Keep the reclaim case through the run store change (S8) |
| `wamn-9p63w` | Acceptance case for one host per environment (S9) |
| `wamn-oeeon` | The `kind` loop measurement that comes before P1 (S10) |
| `wamn-b26q0`, `wamn-pmbuw`, `wamn-dxw9z`, `wamn-pbh27` | Prerequisites of the credential and installer work (S4, S10) |
| `wamn-3gqi1`, `wamn-h794p`, `wamn-nbw0t` | Depend on ruling 29 (S11) |
| `wamn-alrvy.19`, `wamn-z4vh`, `wamn-ib8ej` | Kept and moved into the event work (S8) |
| `wamn-p7c9a`, `wamn-5vue6` | Kept and moved into the web and identity work (S11) |
| `wamn-jd8wd`, `wamn-rab22`, `wamn-w3fft`, `wamn-7v9mt` | Requirements of the xtask selector (S13) |
| `wamn-rdlfn`, `wamn-t1rox` | Read against the code before you keep them |
