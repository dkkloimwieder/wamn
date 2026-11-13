# 16 wamn-runtime, part 7: connection and OAuth files

Nothing was built or run. Every statement is from reading the code unless marked *inference*.

## What was read

| File (under `crates/platform/runtime/src/`) | Lines without tests | Purpose |
| --- | --- | --- |
| `connection_authority.rs` | 675 | Decide the exact URL, host and pinned address of one outbound request |
| `connection_facts.rs` | 352 | Read connection facts, from host memory first and Postgres on a miss |
| `connection_memory.rs` | 299 | Host copy of the connection facts kept in NATS KV |
| `connection_credentials.rs` | 272 | Read connection rows and secrets under the reader login |
| `connection_health.rs` | 124 | Test candidate connection values without storing them |
| `connection_generation.rs` | 20 | Two hash helpers |
| `oauth_token.rs` | 321 | Stored OAuth token: read, refresh, write |
| `oauth_encryption.rs` | 362 | Envelope encryption of stored tokens |
| `oauth_authorization.rs` | 429 | Authorization code flow: start and callback |
| `integrations.rs` | 442 | Call a connection that runs as a wasm integration |

Callers were searched across `crates/` and `services/`.

`connection_authority.rs`, `oauth_encryption.rs` and `oauth_token.rs` each do one thing and have no second copy of their logic.

## Correction to tab 15, finding 10

I wrote that no other file in `wamn-runtime` uses `reqwest`. That was wrong. `oauth_encryption.rs`, `session_keys.rs` and `pod_identity.rs` use it as a client. The dependency stays. What remains of the finding: `connection_http.rs` can take its four header and method types from `hyper`, which it already uses.

## Findings, each with an option

### Dead code

**1. Proxy support has no production caller.** All three callers of `parse_http_connection_authority` pass `None` for the proxy. `transport.rs::target` refuses a proxy decision.

- Goes: `ProxyAuthority`, `parse_proxy`, `canonical_proxy_url`, the proxy parameter, `TransportDecision::Proxy`. The enum is then one value, so `AuthorityDecision.transport` becomes the pinned endpoint itself.
- Estimate: 70 lines, plus the proxy tests in `runtime/tests/connection_authority.rs`.
- Option: delete.

**2. `resolve_http_redirect` is called by tests only.** The transport never follows a redirect.

- Option: delete (35 lines) with its tests.

### Single truth

**3. The OAuth token request and reply are written three times.**

| Grant | Where | Its own copies |
| --- | --- | --- |
| Client credentials and refresh | `plugins/connection_http.rs` (about 300 lines of this file) | `Parameters`, `Credential`, `token_request`, `Oauth2Token`, `parse_oauth2_token` |
| Client credentials with a signed assertion | `plugins/connection_http/jwt.rs` | `Parameters`, `Credential` |
| Authorization code | `oauth_authorization.rs::exchange` | `Parameters`, `Credential`, `Response`, a POST built by hand |

- The reply checks differ. `parse_oauth2_token` checks the characters of the access token. `exchange` does not.
- Option: one OAuth module. One parameters type, one reply parser with one rule, one POST builder. The three grants call it. The 300 lines leave `connection_http.rs`.

**4. The token memory is changed from three files.** `oauth_token.rs::Cache` exposes its map. `connection_http.rs` locks it four times, `connection_facts.rs::observe` once, `connection_memory.rs` twice. Each place writes its own rule for what to drop.

- Option: the map is private, with five methods: get, put if the ticket still holds, drop one connection, keep one generation, clear. This is tab 15 finding 7 with the two more files.

**5. The HTTP definition is read by hand in three places, not two.** `oauth_authorization.rs::exchange` is the third reader of `primary-authority` and `tls-verification`.

- Option: as ruled in tab 15 finding 5, one typed parse. Three callers.

**6. Reading a secret from the credential store is written twice.** `connection_credentials.rs::material` and `oauth_authorization.rs::credential`. Each has its own way to put in a test store.

- Option: one function and one test entry.

**7. Pulling an integration component from the registry is written twice.** `connection_health.rs::guest` and `integrations.rs::resolve` have the same twelve lines, and each keeps its own map of bytes.

- Option: one function with one map.
- Check when `component_artifact_source.rs` is read: it may be a third copy.

**8. The last fact seen of a connection is kept twice.** `connection_facts.rs` has `seen` and `observe`. `integrations.rs::ReadState` has its own `seen` and `observe`. Both compare the new fact with the old and drop what depends on it.

- Option: `ConnectionFacts` tells the caller that a fact changed. `integrations.rs` drops its own map.

**9. One more digest function.** `connection_generation.rs::definition_hash` is SHA-256 over `serde_json::to_vec`. It has one caller, `local_application.rs`.

- Option: use the one digest function (ruled in tab 01, `wamn-digest`). The file then holds one helper and can join its caller.
- Check at change: the hash is stored by local admission, so stored values change.

**10. Results travel as strings and are parsed back.** `connection_http.rs::health` returns the HTTP status as error text. `connection_health.rs::check` parses the text as a number to decide whether to show it.

- Also: the runtime types `host`, `wasm`, `native` and the integration types `http`, `gcs`, `s3` are string literals here and in `integrations.rs`.
- Option: a typed health error. Enums for the two lists, in the same place as the auth type enum (tab 15 finding 6).

**11. `connection_memory.rs` has three pairs of functions that differ by one word.** `connection` and `keep_connection`, `release` and `keep_release`, `connections` and `keep_connections`.

- Option: one generic pair. About 40 lines less. Low priority.

### Single responsibility

**12. Two plugin constructors change a third plugin.** `ConnectionHttp::new` and the blob store plugin's constructor each call `postgres.set_connection_credentials(facts.reader(), ...)`. The same value is set twice, as a side effect of building something else.

- Option: the host sets it once, where it builds the Postgres plugin (`services/host/src/host.rs`).

**13. `ConnectionCredentials` has four optional fields that are present or absent together.** `org`, `oauth_pool`, `store`, `database`. Each method checks again. `store` is set by tests only.

- Option: the fields are required. The owner holds `Option<ConnectionCredentials>`. The test store goes with finding 6.

**14. `NetworkPolicy` is a trait with one production implementation.** `transport.rs::target` already calls the implementation by name.

- Option A: a plain function. `DnsResolver` stays a trait, because tests need a stand-in for DNS.
- Option B: keep both traits.

### Carried

**15. Database connections outside the plugin's pools.** The credential reader holds one shared connection, a second pool of four for OAuth under the same login, and the resource leases hold a session of their own (`services/host/src/host.rs` builds all three).

- Option: carried to the host tab. The question there: can the reader be one more login class of the existing pools.

**16. SQL for the identity schema lives in this crate.** Seven statements on `identity.oauth_token` and `identity.oauth_authorization` in `oauth_token.rs` and `oauth_authorization.rs`.

- Option: carried to the SQL scatter finding (tab 02).

## Tests that go or change

**Delete**

| Test | File | Goes with |
| --- | --- | --- |
| The proxy cases and the redirect cases | `runtime/tests/connection_authority.rs` | 1, 2 |
| `the_definition_hash_bytes_are_pinned` | `connection_generation.rs` | 9. The one digest function has its own test. |

**Change**

| Test | File | Why |
| --- | --- | --- |
| `mixed_answers_pin_allowed_peer_and_rebinding_refuses` | `plugins/connection_http/network_policy.rs` | 2. Its last step calls the redirect function. |
| Sites that match `TransportDecision::Direct` | `authentication.rs` tests, `network_policy.rs` tests, `runtime/tests/connection_authority.rs` | 1 |
| `connection_credentials/test_util.rs` |  | 13 |

Not read: test bodies, and the tests of `oauth_token.rs`, `oauth_encryption.rs`, `oauth_authorization.rs`, `connection_facts/tests.rs`. None of them names a removed symbol by a search.

## Not read

- `resource_lease.rs`, `kv_memory.rs`, `local_application.rs`, `component_artifact_source.rs`: later parts.
- `wamn_engine::integration`: engine tab.

## Finding 3, added 2026-11-11: an OAuth library

Owner's question: is an OAuth 2 library in use.

- Fact: no. `Cargo.lock` has no `oauth2`, `openidconnect` or `yup-oauth2`. Every request and reply is built and parsed by hand.
- Fact, from the `oauth2` crate documentation (version 5.0.0): it supports authorization code with PKCE, client credentials and refresh. It takes a custom async HTTP client as a function, so the pinned transport can stay. Its `reqwest` client is a default feature that can be turned off.
- Not confirmed: the signed assertion grant (RFC 7523). The documentation page does not mention it. *Inference:* it would be sent as extra request parameters, with the assertion made by `jsonwebtoken`.

**Options for finding 3**

- **Option A.** Adopt `oauth2` 5.0.0 with default features off. It replaces the hand-built request, the Basic header encoding, the reply parsing and the PKCE verifier and challenge. These stay ours: the pinned transport, the endpoint authority check (`token_endpoint`), the stored token and its encryption, the token memory.
- **Option B.** One hand-written OAuth module, as first listed.

Checks at change for A: the library does not apply our rule on access token characters; whether the assertion grant fits; that redirects stay off (our transport never follows them).

## Rulings (2026-11-11)

- Finding 3: **option A.** Adopt `oauth2` 5.0.0 with default features off. One OAuth module owns its use. The three checks listed above are made at change.
- Waiting: findings 1, 2, 4 to 13 (accept or hold), finding 14 (A or B).

* Findings 1, 2, 4 to 13: accepted.
* Finding 14: **option A.** `NetworkPolicy` becomes a plain function. `DnsResolver` stays a trait.
* Findings 15 and 16: carried, as written.
* Tab 16 is fully ruled.

**Correction, 2026-11-11, finding 9.** This finding named a crate `wamn-digest`. No crate of that name exists in the repository. The ruling "one function" stands. Tab 23 finding 6 proposes its home: `wamn-execution-contract`.

**Correction, 2026-11-11, finding 9.** An earlier note said the crate `wamn-digest` does not exist and moved the home to `wamn-execution-contract`. That note was wrong: tab 01 ruled `wamn-digest` as a new crate, and `wamn-execution-contract` stops existing. Finding 9 stands as first written.
