# 17 wamn-runtime, part 8: route authentication and host memory

Nothing was built or run. Every statement is from reading the code. **Fact** = read in the code. **Inference** = my conclusion.

## Files read

| File | Lines | Non-test lines |
| --- | --- | --- |
| `plugins/route_authentication.rs` | 857 | 686 |
| `auth_cache.rs` | 548 | 337 |
| `kv_memory.rs` | 326 | 326 |
| `session_keys.rs` | 459 | 459 |
| Total | 2,190 | 1,808 |

Callers read: `services/host/src/host.rs` (authenticator setup, both verifiers), `plugins/wamn_postgres/claims.rs` (grants, provisioned check, environment status, run callers), `crates/execution/workflow/src/queue.rs`, `router_driver.rs`, `connection_memory.rs`, `crates/identity/platform/src/kv_memory.rs`, `crates/platform/engine/src/flow_http_routing.rs` (credential selection, CSRF, refusals).

## One purpose each

| File | Purpose | Verdict |
| --- | --- | --- |
| `route_authentication.rs` | Turn a request credential into a caller | Also builds the caller of a workflow run, with its own SQL |
| `auth_cache.rs` | Host copy of auth facts | One purpose. Carries two small old shapes |
| `kv_memory.rs` | Local replica of one NATS KV bucket | One purpose. Clean |
| `session_keys.rs` | Fetch and hold the issuer's public keys | One purpose. Uses `reqwest` |

## Findings

### 1. Two builders for the caller of a workflow run

- **Fact.** `route_authentication.rs::queued_run_caller` reads the user, roles and permissions with `QUEUED_RUN_CALLER_SQL`, on a raw connection, with no host memory.
- **Fact.** `claims.rs::workflow_node_caller` answers the same question from `principal_grants` plus `role_caller`, through the host memory.
- **Fact.** Tab 11 already ruled that `QUEUED_RUN_CALLER_SQL` goes.
- **Option A (recommended).** Keep one function: `workflow_node_caller`. Delete the free function `queued_run_caller`, `QUEUED_RUN_CALLER_SQL` and `WamnPostgres::queued_run_caller`. `queue.rs` calls the one function. `role_caller` moves next to `principal_grants`.
- **Changes with A.** `queue.rs` today matches on an error. It will match on `None` (inactive or role not held) and still release its claim on `PgError::ConnectionUnavailable`.

### 2. The "no role" branch has no production caller

- **Fact.** `queued_run_caller(.., role: None)` builds an `"automation"` caller with every role. The only production caller, `queue.rs`, always passes `Some(role)`. All `None` calls are in tests.
- **Option.** Delete the branch. It goes with finding 1.
- **Stays.** `CredentialType::QueuedService`: `role_caller` and `services/edge/src/device.rs` use it.

### 3. Three structs hold the same handles

- **Fact.** `RouteAuthentication` (PAT), `SessionRouteAuthentication` and the signature tuple in `PlatformRouteAuthenticator` each hold the project. Two hold the identity reader. Two hold the Postgres plugin. The host passes the same values to each.
- **Option A (recommended).** One struct, one constructor: Postgres plugin, connection facts and project are required. PAT scope (identity reader, prepared reads, org, expected subject) is optional. Session verifier is optional. Delete `RouteAuthentication`, `SessionRouteAuthentication`, the three `with_*` functions and `Default`.
- **Kept.** The ruled answers of a host without a mechanism: session route 401, PAT route 503.
- **Option B.** Keep three structs. Finding 4 then needs its own fix.

### 4. Defect: a release with only signed routes answers 503

- **Fact.** The signature path takes the Postgres plugin from `self.authentication`, the PAT struct.
- **Fact.** `host.rs` builds that struct only when the release has a PAT route or a session route (`pat_routes || session_routes`).
- **Inference.** A release whose only protected routes are signed routes refuses every signed request with `authentication-unavailable`. Not run.
- **Option.** Fixed by finding 3 option A: the plugin handle is required and not tied to PAT. One new test at the lowest tier: a signed route authenticates with no PAT scope.

### 5. The subject of a connection's service principal is spelled twice

- **Fact.** `deploy/sql/system-schema.sql` writes the subject `'connection-' || id`. `route_authentication.rs` parses the same prefix to find the connection.
- **Option.** One function in `wamn-platform-identity`, next to the principal type: subject → connection id. The route file calls it. The SQL comment names it. The SQL literal stays, because SQL cannot import Rust.

### 6. The session steps are written twice

- **Fact.** `PlatformRouteAuthenticator::authenticate_session` and `ControlRouteAuthenticator::authenticate` both do: verify token, CSRF check, live check with a 5 second limit, verify again when `check_admission` fails.
- **Option.** One private function that takes the live check as a parameter. The application path adds its grants read. About 40 lines go.

### 7. Secret shape and id conversion in the route file

- **Fact.** `SignatureSecret` and `SignatureHmac` parse a credential secret here. Tab 16 finding 6 ruled one secret read with typed shapes.
- **Fact.** The signature path turns a `Uuid` into a string and parses it back into a `PrincipalId`. The PAT path compares the same two ids as strings.
- **Option.** The secret type moves to the typed secret read of tab 16. `ConnectionFact.service_principal_id` becomes a `PrincipalId`, so both conversions go.

### 8. The provisioned check reads the same row a second time

- **Fact.** `claims.rs::require_provisioned_principal` reads `platform."user"` with `PROVISIONED_PRINCIPAL_SQL`. `principal_grants` reads the same row and records `present`.
- **Fact.** The memory value `Answer::Principal { grants: Option<..>, provisioned }` exists to hold both answers under one key. `KvMemory::replaced` exists only for that merge.
- **Fact.** The two reads use different logins: the guest login for the check, the callable-HTTP login for the grants.
- **Option A (recommended, if every host that binds claims has the callable-HTTP login).** The bind calls `principal_grants` and requires `present`. Delete `provisioned`, `keep_provisioned`, `PROVISIONED_PRINCIPAL_SQL`, the `provisioned` field, the `Option` around grants and `KvMemory::replaced`.
- **Option B.** Keep both reads.
- **Open fact.** Whether a host without the callable-HTTP login binds claims. To check in the host tab before A is applied.

### 9. An older stored shape is still accepted

- **Fact.** `PrincipalGrants.user` is `Option<bool>`. `AuthCache::grants` treats a value without it as a miss. The bucket is in memory and keeps a value 60 seconds.
- **Option.** `user: bool`. Delete the filter. This follows the repository rule "refuse old rows".

### 10. The token map repeats the age and cleanup code of `KvMemory`

- **Fact.** `auth_cache.rs::Tokens` keeps `seen`, checks the TTL and drops expired entries at `SWEEP_AT`, as `KvMemory` does. The token map never leaves the host, so it cannot be a `KvMemory`.
- **Option A.** A `moka` cache with a TTL (the crate is already used in `connection_facts.rs`). About 20 lines go. Dropping by principal then needs moka's closure invalidation.
- **Option B (recommended).** Keep. The saving is small and the drop by principal gets harder.

### 11. The watch is reached through a trait, `AsRef` and a function pointer

- **Fact.** `spawn_watch` takes a holder, plus `view: fn(&H) -> &dyn Watched`. `auth_cache::replicate` and `connection_memory::replicate` each add an `AsRef` bound. There are four `AsRef` impls for this.
- **Option.** `spawn_watch` takes `Arc<dyn Watched>`. `WamnPostgres` holds `Arc<AuthCache>`. The `view` parameter and the four `AsRef` impls go. The two `replicate` functions stay: their key filters differ.

### 12. Public surface used only by tests

- **Fact.** `KeyEvidence::issuer` has test callers only. Every evidence value clones the issuer string per request.
- **Fact.** `KeyEvidence::deadline` has test callers only (`tests/session_keys.rs`, `tests/integration/src/identity_keys_test.rs`).
- **Fact.** `AuthCache::clear` is `pub`. Outside the watch, only tests of this crate call it.
- **Option.** Delete `KeyEvidence::issuer` and its field. Put `deadline` behind `test-util`. Make `AuthCache::clear` `pub(crate)`.

### 13. `session_keys.rs` uses `reqwest`

- **Fact.** The file builds a `reqwest::Client`: HTTPS only, given roots only, no redirect, no proxy, no retry, 5 second limit. The config type holds `reqwest::Certificate`.
- **Fact.** The pinned transport cannot serve here as it is: `HttpTransport::execute` needs a connection scope and an authority decision.
- **Fact.** `reqwest` stays in the build either way. `object_store` and `oci-client` depend on it.
- **Option A (recommended, follows "use hyper drop reqwest").** One small private module: a `hyper` client over `tokio-rustls` with given roots, no redirect, a time limit and a body limit. `session_keys.rs` uses it now. `pod_identity.rs` and `oauth_encryption.rs` get the same option in their tabs. About 100 new lines replace three client builders.
- **Option B.** Keep `reqwest` in the three client files, because the build keeps it anyway.

## Carried

- `host.rs` derives the JWKS endpoint and builds the key cache twice (`session_verifier`, `control_connection`). To the host tab.
- `authenticate_route_pat(.., &[ADMIN_ROLE])`: the meaning of the accepted roles list. To the identity tab.
- Environment status is read with the guest login, grants with the callable-HTTP login. To the host tab, with finding 8.

## Fine as is

- `kv_memory.rs`: ticket, put with revision, named watch per filter, clear on a broken watch. No finding besides 8 and 11.
- `EnvironmentStatusReader`: the engine cannot depend on the runtime, so the trait and this small struct stay.
- `session_keys.rs` refresh rules (single fetch in flight, one attempt per second, refresh ahead of expiry) each cite an owner ruling.

## Tests

**Delete**

- `route_authentication/tests/connection_live.rs::queued_run_without_a_selected_role_uses_only_active_services_and_all_their_roles` (findings 1, 2)
- `auth_cache.rs::grants_without_principal_type_are_a_cache_miss` (finding 9)
- `auth_cache.rs::present_grants_answer_the_provisioned_check` (finding 8 option A)

**Change**

- `claims/tests.rs::queued_caller_retains_typed_connection_unavailable`: calls the one function (finding 1)
- `crates/control/environment/tests/integration_principal_live.rs`: two calls of `queued_run_caller(.., None)` (findings 1, 2)
- `crates/execution/workflow/src/queue/automation_live.rs`: two calls of `queued_run_caller(.., None)` (findings 1, 2)
- `claims/tests.rs::live_a_removed_user_row_is_refused_once_the_auth_memory_clears`, `live_a_drop_refuses_a_removed_user_row_on_the_next_bind`: read `grants` in place of `provisioned` (finding 8 option A)
- Authenticator setup only (finding 3): `route_authentication.rs` tests `pat_mode_is_recognized_and_an_absent_backend_is_one_generic_outage`, `an_empty_authenticator_refuses_a_session_route_401_and_a_pat_route_503`, the fixtures of `tests/signature.rs` and `tests/connection_live.rs`, `claims/tests.rs::live_a_drop_refuses_a_revoked_pat_on_the_next_request`, `tests/session_route_authentication.rs`
- `tests/session_keys.rs` (15 tests): fixture only, the root certificate type (finding 13). `evidence.issuer()` assert goes (finding 12)
- `tests/integration/src/identity_keys_test.rs`: the `evidence.issuer()` check goes (finding 12)

**Move**

- `route_authentication.rs::a_queued_run_holds_the_operations_of_its_role_only`: moves with `role_caller` (finding 1)

**New, one**

- A signed route authenticates on a host with no PAT scope (finding 4).

## Not read

- Test bodies of `tests/signature.rs`, `tests/connection_live.rs`, `tests/session_keys.rs`, `tests/session_route_authentication.rs`, `auth_cache/test_util.rs`: names and call sites only.
- `wamn_platform_identity::session_token::session_is_active`, `control::control_session_is_active`, `PreparedIdentityReads`: signatures only.
- `wamn_session::verifier`: trait signatures only.
- `pod_identity.rs`, `oauth_encryption.rs` client code: only the `reqwest` lines.

## Rulings

None yet.

**Ruled 2026-11-11**

- Findings 1, 2, 4, 5, 6, 7, 9, 11, 12: accepted.
- Finding 3: **option A.** One authenticator struct, one constructor.
- Finding 10: **adopt `moka`.** The token map becomes a `moka` cache with a TTL. `Token.seen`, the TTL checks and the `SWEEP_AT` cleanup in `auth_cache.rs` go. Drop by principal uses moka's closure invalidation.
- Finding 13: **option A.** One small private `hyper` client module. `session_keys.rs` uses it.
- Finding 8: **option A, pending the check below.**

### Finding 8: result of the check

- **Fact.** A host has the callable-HTTP login only when its release has a PAT route, a session route or a connection requirement (`host.rs::demanded_http_admitter_url`).
- **Fact.** The claim bind runs on any host with the guest login. So some hosts that bind claims have no callable-HTTP login. Option A as first written would break them.
- **Fact.** A host with PAT routes and no guest SQL has only the callable-HTTP login (comment in `host.rs`). So neither login is on every host.
- **Fact.** The guest login `wamn_app` has `SELECT` on `platform."user"`, `platform.user_role` and `platform.permission` (`deploy/sql/app-schema.sql`). I found no row policy on these three tables in that file.
- **Option A2 (recommended).** One read, one SQL, one memory value; the caller names the login. The bind runs `PRINCIPAL_GRANTS_SQL` under the guest login and requires `present`. Route authentication runs the same function under the callable-HTTP login. Everything listed in option A still goes: `provisioned`, `keep_provisioned`, `PROVISIONED_PRINCIPAL_SQL`, the `provisioned` field, the `Option` around grants, `KvMemory::replaced`.
- **Option B.** Keep both reads.
- **Waiting for:** A2 or B.

### Found during the check, carried

- **Fact.** The caller of a queued workflow run is read under the callable-HTTP login (`claims.rs::queued_run_caller`, `workflow_node_caller`).
- **Fact.** Queue admission requires only the guest and executor logins (`host.rs::require_queue_admission`).
- **Inference.** A release with workflows, no PAT or session route and no connection cannot read the caller of a run. `queue.rs` releases the claim on `ConnectionUnavailable` and the run is claimed again. Not run.
- With A2 the run caller can name the guest login. To confirm in the workflow tab.

**Ruled 2026-11-11, finding 8: option A2.** One grants read; the caller names the login. Tab 17 is fully ruled.

**Note, 2026-11-11 (tab 31 finding 3 A).** The one authenticator (finding 3 A) stays, as the host identity capability. Its input changes: for an application route the guest passes the modes the route accepts (and the signature header and credential name, and whether CSRF is required) instead of an attachment id the host looks up. The host's own endpoints (control and file routes) keep the attachment lookup. The "environment inactive" check leaves `authenticate` for `authorize` / `begin-operation` (tab 31 check 2).
