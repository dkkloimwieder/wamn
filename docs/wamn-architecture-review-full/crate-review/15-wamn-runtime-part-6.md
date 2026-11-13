# 15 wamn-runtime, part 6: HTTP connection plugin

Nothing was built or run. Every statement is from reading the code unless marked *inference*.

## What was read

| File (under `crates/platform/runtime/src/plugins/`) | Lines without tests |
| --- | --- |
| `connection_http.rs` | 1,671 |
| `connection_http/transport.rs` | 767 |
| `connection_http/jwt.rs` | 182 |
| `connection_http/authentication.rs` | 169 |
| `connection_http/network_policy.rs` | 23 |

Also read for callers: `crates/execution/host/src/operation.rs::component_invocation`, `operation/native_policy.rs` (bind and revoke), `deploy/sql/system-schema.sql` (the auth type rows).

## One purpose

Let a component send one HTTP request to an external system that an administrator bound, with the host adding the credential.

The plugin does this and nothing else. The transport file is sound: pinned peer, hard limits, no second copy elsewhere.

## Rulings that land here

| Ruling | What goes or changes |
| --- | --- |
| Candidate branch deleted (part 1) | `ConnectionExecutionClosure` is left with one value, so the type and the field go. Also: the candidate block in `bind_invocation`, three match arms in `attempt`, `authorize_candidate_closure`. About 120 lines. The blob store plugin uses the same function and loses its arm too. |
| `jsonwebtoken` 11.1.0 (tab 05) | `connection_http/jwt.rs` signs by hand with `ring`. The library covers its three algorithms (RS256, PS256, ES256). About 110 lines go. Check at change: the library reads both key formats the file accepts (PKCS#1 and PKCS#8 PEM). |
| Effect authorization moves here (tab 11, option A) | About 520 lines arrive from `wamn_postgres/claims.rs`. |
| `EffectOutcome` and the HTTP target check move to runtime (tab 01) | Two imports change. |

## Findings, each with an option

### Single truth

**1. The invocation carries its own identity twice.** `ConnectionInvocation` has `package_id`, `component_digest`, `component`, `operation`. It also has `origin: ConnectionOrigin` with the same four plus `interface_version`.

- Fact: the one production constructor, `execution/host/src/operation.rs::component_invocation`, fills both from the same values.
- Fact: `connection_http.rs::released_operation_reachable` refuses the call unless the two are equal.
- Fact: the effect lookup passes the five origin values to SQL as five more parameters.
- Option: delete `ConnectionOrigin`. Add `interface_version` to the invocation. `released_operation_reachable` goes. The lookup loses five parameters.

**2. One invocation record is copied into three plugin maps on every call.** `native_policy.rs` binds the same `ConnectionInvocation` into the Postgres plugin, this plugin and the blob store plugin, then revokes each. Each plugin has its own map, its own "already bound" check and its own revoke. In total the host makes about ten binds and nine revokes per call.

- Option: one scope record, held once and shared by the plugins. This widens the tab 11 ruling (one map for claims) to all plugins.
- Cost: the plugins get a shared handle at construction. Decide when `native_policy.rs` is read in full.

**3. The transport sends a request in two functions that are 65 lines alike.** `transport.rs::execute` (pooled client) and `send_once` (no pool, used by the health check). Preparing the request and collecting the response are the same code.

- Option: one function to prepare and one to collect. Both callers use them.

**4. Transport facts go to strings and are matched back.** `connection_http.rs::execute` turns the transport phase into `ConnectionError::Transport("connection-client-unavailable")` or `"connection-response-lost"`. `http_outcome` then compares those strings to find the phase again.

- Option: `send` returns the outcome with the error inside the plugin. The WIT error is made once, in `HostWithStore::send`. Three string constants go. This is the repository's own rule for errors.

**5. The connection definition is read by hand in two places.** `attempt` and `health` each read `primary-authority` and `tls-verification` from JSON and map the second to `TlsPolicy`. `wamn_catalog::connection` names the same two fields a third time.

- Option: one typed parse, next to `parse_http_connection_authority`.

**6. Auth type names are string literals in four files.** Six places here, plus `authentication.rs`, `oauth_token.rs::is_oauth`, `oauth_authorization.rs`. The list itself is rows in `deploy/sql/system-schema.sql`, each with a JSON schema of its credential. `authentication.rs` has a Rust struct for each of the same shapes.

- Option: one Rust enum for the names, with `is_oauth` as a method.
- The credential shapes stay in both languages: SQL checks the write, Rust reads the value. No option removes that without generated code.

**7. This file reaches into the OAuth token memory.** `self.oauth2.entries.lock()` appears four times, with the remove and insert rules written here.

- Option: three methods on `oauth_token::Cache`. Confirmed when `oauth_token.rs` is read.

**8. A digest is checked by hand next to its parser.** `bind_invocation` checks length 64 and hex characters twice by hand. One line above it calls `DefinitionHash::parse`. `authorize_release_closure` later calls `ArtifactHash::parse` on the same field.

- Option: use the parsers.

**9. The contract name is written three times.** `HTTP_CONTRACT` (compiled for tests only), the literal in `world()`, and `ConnectionTypeDescriptor::http_v1()`. `http_descriptor()` is a one-line wrapper. One test exists to compare two of the copies.

- Option: `world()` reads the descriptor. The constant, the wrapper and the test go.

### Dead weight

**10. `reqwest` is used for four type names only.** `HeaderMap`, `HeaderName`, `HeaderValue`, `Method`. The requests go through `hyper`. These are the same types under `hyper::header` and `hyper::Method`. No other file in `wamn-runtime` uses `reqwest`.

- Option: use the `hyper` names. Drop `reqwest` from this crate's dependencies. Check `Cargo.toml` at change.

**11. The release is optional, and without it every call is refused.** `ConnectionHttp.release` is `Option`. The comment says `None` is for tests and benches. After the candidate ruling, a plugin without a release can authorize nothing.

- Option: make it required. Four test constructors change.

**12. Proxy support is passed as a constant `None`.** `attempt` sets `let proxy = None`. `transport.rs::target` refuses any decision that is not direct.

- Carried to the part that reads `connection_authority.rs`: the proxy branch there may have no caller.

## Tests that go or change

**Delete**

| Test | File | Goes with |
| --- | --- | --- |
| `the_pinned_contract_literal_matches_the_descriptor` | `connection_http.rs` | 9 |
| Tests of the hand-written signature bytes | `connection_http/jwt/tests.rs` (283 lines) | `jsonwebtoken` ruling. Which ones: decided at change; bodies not read. |
| Candidate cases | `wamn_blobstore/plugin.rs` (one constructor at the candidate value) | candidate ruling |

**Change**

| Test | File | Why |
| --- | --- | --- |
| `a_timeout_never_reads_as_a_refusal_before_dispatch`, `the_two_details_that_dispatched_nothing_are_refused_before_dispatch`, `a_lost_response_never_reads_as_effect_uncertain`, `a_transport_failure_in_flight_never_reads_as_a_timeout` | `connection_http.rs` | 4 |
| Ten sites that build `ConnectionOrigin` | six test files and `execution/host/src/file_route.rs` | 1 |
| Sites that write `ConnectionExecutionClosure::Released` | six files | candidate ruling |
| Four sites that call `ConnectionHttp::new` with no release | three test files | 11 |

The other 30 tests in `connection_http.rs`, the transport tests (1,081 lines), the OAuth tests (1,884 lines) and the credential retry tests stay.

## Not read

- Test bodies.
- `oauth_token.rs`, `connection_authority.rs`, `connection_facts.rs`: next part.
- `native_policy.rs` beyond the bind and revoke lines.

## Rulings (2026-11-11)

- Findings 1 and 3 to 11: accepted.
- Finding 2: **held.** Come back to it when `execution/host/src/operation/native_policy.rs` is read in full.
- Finding 12: carried, as written.

**Correction, 2026-11-11, finding 10.** The statement that no other file in `wamn-runtime` uses `reqwest` was wrong. `oauth_encryption.rs`, `session_keys.rs` and `pod_identity.rs` use it. The dependency stays. The finding is reduced to: take the four type names from `hyper`.

**Ruled 2026-11-11, finding 10:** `connection_http.rs` uses the `hyper` types and no `reqwest` name. Owner's words: "use hyper drop reqwest". Whether the whole crate can drop `reqwest` depends on the three files that use it as a client (`oauth_encryption.rs`, `session_keys.rs`, `pod_identity.rs`). Each gets an option in the part that reads it.
