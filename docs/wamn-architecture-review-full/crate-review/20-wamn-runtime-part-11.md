# 20 wamn-runtime, part 11: root files (last part)

Nothing was built or run. Every statement is from reading the code. **Fact** = read in the code. **Inference** = my conclusion.

## Files read

| File | Lines | Non-test lines |
| --- | --- | --- |
| `local_application.rs` | 1,109 | 719 |
| `component_artifact_source.rs` | 731 | 440 |
| `resource_lease.rs` (+ `resource_lease/tests.rs`, 250) | 786 | 534 |
| `release_manifest_source.rs` | 505 | 449 |
| `release_manifest_artifact.rs` | 198 | 149 |
| `pod_identity.rs` | 140 | 125 |
| `bin/docker-credential-wamn.rs` | 55 | 55 |
| `registry_transport.rs` | 41 | 41 |
| `lib.rs`, `plugins/mod.rs` | 40 | 40 |
| Total | 3,605 | 2,552 |

Callers read: `services/host/src/host.rs` (release load, local application load, artifact sources, leases), `services/ctl/src` (every `wamn_runtime::` use), `crates/control/dev/src/dev/coordinator.rs` (local facts, `selection`), `crates/control/environment/src/reconcile_package_data_access.rs`, the five local-application branches in `plugins/wamn_postgres`, `connection_facts.rs::exclusive_call`, `integrations.rs`, `wamn_package_artifact::registry_credentials` (metadata token reader).

## One purpose each

| File | Purpose | Verdict |
| --- | --- | --- |
| `local_application.rs` | Unpublished local application of `wamn dev` | Three things: file formats, database marker, a second runtime path |
| `component_artifact_source.rs` | Pull component bytes by digest | One purpose. Shares code by copy with the next file |
| `release_manifest_source.rs` | Pull release manifest bytes by digest | One purpose. Same copies |
| `release_manifest_artifact.rs` | Layout of the release artifact | One purpose |
| `registry_transport.rs` | Classify a registry error | One purpose |
| `resource_lease.rs` | Exclusive lease per external resource | One purpose |
| `pod_identity.rs` | Pod token and signer for GCS | One purpose. A second reader of the metadata token |
| `bin/docker-credential-wamn.rs` | Credential helper for wash-runtime pulls | One purpose |

## Findings: registry and artifacts

### 1. The registry client code is written twice

- **Fact.** `component_artifact_source.rs` and `release_manifest_source.rs` each have a `registry_client` function with the same body.
- **Fact.** Each reads CA bundles (one with `read_ca_bundles`, one with its own loop), each sets up the credential source (auth file or metadata), each turns it into `RegistryAuth::Basic`, each classifies transport errors with `transport_is_mismatched` (one helper function there, two inline copies here).
- **Fact.** The time limit is a parameter in one and a 30 second constant in the other.
- **Fact.** `wamn-package-artifact` already owns `registry_pull`, `registry_credentials` and `read_ca_bundles`.
- **Option.** One registry source type in `wamn-package-artifact`: base, insecure flag, credentials, CA bundles, time limit; with client, auth, manifest pull and blob pull. Both sources hold one. `registry_transport.rs` moves with it. The two error types stay: they belong to two boundaries.

### 2. Artifact transfer lives in the runtime, so the CLI links the runtime for it

- **Fact.** `services/ctl` uses `wamn_runtime` for `component_artifact_source` (4 uses), `release_manifest_source` (1) and `local_application` file names and types (3). Nothing else.
- **Fact.** `release_manifest_source.rs` and `release_manifest_artifact.rs` use no runtime type. Their one engine import is a plain SHA-256 function.
- **Fact.** `component_artifact_source.rs` implements the engine trait `ArtifactSource`. The engine has a comment that it carries no registry code.
- **Option.** Move `release_manifest_source.rs` and `release_manifest_artifact.rs` to `wamn-package-artifact` now. `component_artifact_source.rs` stays until the `wamn-ctl` tab, where its callers are read in full.

### 3. Two readers of the pod metadata token

- **Fact.** `pod_identity.rs::PodToken` reads `.../service-accounts/default/token` with `reqwest` and keeps the token until 30 seconds before expiry.
- **Fact.** `wamn_package_artifact::registry_credentials::read_metadata_registry_credentials` reads a token from the same metadata server with its own `reqwest` client, and keeps nothing.
- **Option.** One metadata client in `wamn-package-artifact`: token (with the expiry memory) and email. `pod_identity.rs` keeps only the two `object_store` adapters. The client uses the private `hyper` module ruled in tab 17 finding 13, moved to where both crates reach it.
- **Fact for the `reqwest` direction.** `wamn-package-artifact` also names `reqwest::Error` in `registry_pull.rs` to read `oci-client` errors. That use stays while `oci-client` uses `reqwest`.

## Findings: local application

### 4. One file holds three things

- **Fact.** File formats and their check: `LocalApplicationFacts`, `LocalBindingFacts`, `LocalRelease`, the two file names, `load_local_facts`, `validate_local_facts`. Written by `control/dev` and `wamn-ctl`, read by the host.
- **Fact.** Database marker: `local_target_marker`, `LocalTargetComment`, parse and read functions. Used by `control/dev`, `control/environment` and the host.
- **Fact.** Runtime behaviour: `LocalApplication` with `effect_snapshot`, `bindings_ready`, `require_instance`.
- **Option.** Formats and marker move next to `wamn_engine::release_manifest`, which already reads the same directory (`LoadedRelease::load_from`). The runtime keeps only `LocalApplication`.

### 5. The local mode is a second path inside production functions

- **Fact.** `plugins/wamn_postgres` branches on `local_application` in five places: `claims.rs` (credentials, effect snapshot), `workflow_resolution.rs`, `wiring_resolution.rs` (two).
- **Fact.** Three of them repeat by hand the same check: env\_key, environment, manifest digest, then `require_instance`.
- **Fact.** `effect_snapshot` builds the 26 fields of `ConnectionEffectSnapshot` again. `entry_facts` states the entry rule again; its comment says it is the same rule as the Postgres snapshot.
- **Option A (recommended now).** One method `LocalApplication::require_scope(client, env_key, environment, digest)`. The three hand copies go.
- **Option B (to decide in the `control/dev` tab).** Remove the second path: `wamn dev` writes the catalog rows the cluster path reads, and the host runs one path. I have not read `control/dev` in full, so I do not recommend B yet.
- This is the item carried from tab 14 finding 16.

### 6. The host reads and checks the local files twice

- **Fact.** `host.rs` calls `load_local_facts` and `require_local_target` when it loads the release. Later it calls `load_local_application`, which reads the facts file and the target marker again, on a second database connection.
- **Option.** `load_local_application` is the one call. The two earlier calls go.

### 7. Binding hashes are computed in three places

- **Fact.** Definition hash, requirement hash and validation hash are built in `local_application.rs::selection`, in `local_application.rs::read_binding`, and in the snapshot code of `claims.rs`.
- **Option.** One constructor of `CandidateConnectionBinding` from requirement, connection, generation, definition, handle and mode. The three call it.

### 8. Small repeats in the file

- **Fact.** The connection name query is written twice: in `read_local_connection_name` and inline in `read_binding`.
- **Fact.** `LocalApplication.credentials` is an `Option` behind a lock, set later through the Postgres plugin. `connection_reader` fails when it is not set.
- **Fact.** The file holds four SQL statements against `catalog` and `platform` tables.
- **Option.** One name query function. The reader is passed when the local application is built (the host builds it after the reader exists). The SQL joins the carried SQL list.

## Findings: resource lease

### 9. A default that production always replaces, and states as bare numbers

- **Fact.** `ConnectionMemory::with_ttl` builds `ResourceLeases::new(None)`. `host.rs` always replaces it with `with_resource_leases`.
- **Fact.** Lease states are four `u8` constants (`PENDING`, `HELD`, `LOST`, `RELEASED`) in an `AtomicU8`.
- **Option.** `ConnectionMemory` takes the leases in its constructor; `with_resource_leases` goes. The states become one enum stored in the atomic.

### 10. The lease session is one more database connection outside the pools

- **Fact.** `database_session` opens its own `tokio_postgres` session with `NoTls` and holds advisory locks on it.
- **Inference.** A session-level advisory lock needs a dedicated session, so a pool connection cannot serve here.
- **Option.** Keep the dedicated session. Add it to the carried list "database connections outside pools" (tab 16 finding 15) as a justified case, so the host tab does not count it again.

## Fine as is

- `resource_lease.rs` logic: one owner per host, a lost session marks every lease lost before reconnect, a call checks `held()` after it returns. I found nothing to delete. I could not check the concurrency by reading alone.
- `release_manifest_artifact.rs`: one layout, written and checked by the same file.
- `registry_transport.rs`: one list, used by both pulls.
- `bin/docker-credential-wamn.rs`: 55 lines, one token function.
- Each pull builds a new `oci-client`, on purpose: the client keeps the first credential it gets.

## Carried

- `control/dev/coordinator.rs` builds the credential handle text `org:<org>:<name>@<generation>` by hand; the runtime has `connection_handle`. To the `control/dev` tab.
- `host.rs` reads `WAMN_EXECUTOR_PLATFORM_PG_URL` in two places for the local application. To the host tab.

## `wamn-runtime` is read in full

- 11 parts, tabs 10 to 20.
- Still held for this crate: the scope record shared by plugins (tab 15 finding 2, with tabs 18 and 19 added), and the test lists of tabs 01 to 10.

## Tests

**Move**

- `component_artifact_source.rs::registry_pull_retries_transient_answers_only`, `registry_pull_does_not_retry_manifest_decode_errors`, `ca_bundles_read_in_order_and_an_unreadable_path_refuses_by_name`, `transport_integrity_errors_are_not_classified_as_unavailable`: to `wamn-package-artifact` with the shared registry source (finding 1)
- `release_manifest_source.rs` (2 tests) and `release_manifest_artifact.rs` (2 tests): with their files (finding 2)
- `local_application.rs::exact_local_facts_round_trip_and_refuse_missing_or_changed_closure`, `declared_local_requirement_cannot_disappear_from_readiness`, `local_facts_accept_the_http_descriptor`, `missing_local_admission_facts_refuse_before_loading`, `malformed_or_foreign_local_manifest_identity_refuses`, `local_ownership_is_bound_to_env_key_environment_and_database_creation`, `a_local_target_comment_reads_only_its_written_spelling`: with formats and marker (finding 4)

**Change**

- `component_artifact_source.rs::source_configuration_is_explicit_and_redacts_rejected_credentials`: uses the shared source (finding 1)
- `local_application.rs::local_readiness_and_dispatch_refuse_a_forged_binding_mode`, `a_route_entry_needs_only_its_origin_export_and_a_release_package`: the reader is passed at construction (finding 8)
- `resource_lease/tests.rs` and tests that build `ConnectionMemory`: constructor takes the leases (finding 9)
- `pod_identity.rs::pod_signing_credentials_never_contain_a_private_key`: built from the one metadata client (finding 3)

**Delete**

- None in this part.

## Not read

- Test bodies: names only.
- `wamn_package_artifact::registry_pull` and `registry_credentials`: signatures and the metadata reader only.
- `crates/control/dev`: the lines that call into this crate only.
- `wamn_engine::artifact_source`, `component_artifact`: signatures only.

## Rulings

None yet.

**Ruled 2026-11-11**

- Findings 1 to 4, 6 to 10: accepted.
- Finding 5: **option A.** One `require_scope` method. Option B stays open for the `control/dev` tab.
- Tab 20 is fully ruled.
