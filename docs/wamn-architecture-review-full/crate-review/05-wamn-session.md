# 05 `wamn-session`

Path `crates/identity/session`. Five files, 680 non-test lines, one test file of 134 lines. No workspace dependencies. Eight crates depend on it. Every file was read in full. Every public symbol's callers were searched, and the places that repeat a rule were opened. Nothing was built or run.

## One purpose

Check a session token: its fixed Ed25519 form, its signature, its issuer, org and audience, and its age. It needs no database and no HTTP client, so the edge box can link it. (fact)

The name fits. This is the smallest and cleanest crate read so far. It has no SQL.

## What it holds and who calls it

Callers are crates outside this one, test code excluded.

| Part | What it is | Callers |
| --- | --- | --- |
| `token.rs`: `SessionClaims`, `SessionAuthority`, `SessionHeader`, `validate_claim_shape`, `is_role_slug`, `is_principal_id` | The token's fields and their shape rules. The signer uses the same types. | `identity/platform` (the signer), `services/edge`, `services/identity` |
| `token.rs`: `verify_session_token`, `session_key_id`, `validate_session_age`, `SessionScope`, `TOLERANCE` | The verification steps | Only through `SessionVerifier` in production. Tests in other crates call them directly. |
| `verifier.rs`: `SessionVerifier`, `KeySource`, `KeyEvidence`, `VerifiedSession` | Verify a token with keys from some source, then check again just before admission | `platform/runtime`, `services/host`, `services/edge` |
| `keys.rs`: `PublicSessionKey`, `SessionJwks`, `decode_public_key` | The public key document | `identity/platform`, `platform/runtime` |
| `file_keys.rs`: `FileKeys` | Keys read from a file at start | `services/edge` |
| `PAT_TOKEN_PREFIX` | The text every personal access token starts with | `identity/client`, `identity/platform`, `platform/engine` |
| `SESSION_ALGORITHM`, `SESSION_TYPE`, `MAX_ROLE_LEN` | Constants | None outside the crate |

## Findings

| # | Finding | Mark |
| --- | --- | --- |
| 1 | `PAT_TOKEN_PREFIX` is about personal access tokens, not sessions. It is here because this is the only identity crate with no database dependency, and the client and the engine both need the text. One constant. | fact |
| 2 | The key document's fixed values are written twice. `keys.rs::decode_public_key` requires `OKP`, `Ed25519`, `Ed25519`, `sig`. `identity/platform/src/session_keys.rs::public_key` writes the same four strings to build the document. | fact |
| 3 | `is_principal_id` is a hand-written check that a string is a lowercase hyphenated UUID. A UUID is checked or respelled in at least five other places: `wamn-data-access/src/scalar.rs::uuid`, `schema/generator/src/cursor.rs::validate_uuid`, `schema/generator/src/generate/wit.rs::canonical_uuid`, `wamn_receiving` `record_receipt.rs::canonical_uuid`, `client_acme_receiving` `operation.rs::parse_uuid`. | fact |
| 4 | The role name rule exists in Rust and in SQL. `is_role_slug` here; `role_name_check` on `platform.role` in `deploy/sql/app-schema.sql` is `^[a-z0-9][a-z0-9-]*$` and at most 64 bytes. The two agree today. The SQL comment names the Rust function. No test gives both the same cases. | fact |
| 5 | Three constants are public with no outside user. Five verification items are public only so tests in other crates can call them. | fact |
| 6 | The token is parsed and checked by hand with `base64` and `ring`, about 200 lines, not with a JWT library. The crate accepts one algorithm, one type and a closed set of fields. | fact. Inference: this is a chosen design, not an accident. A general library accepts more than this profile allows. |

Found while searching, for other tabs (fact): `crates/execution/run-state/src/invocation_context.rs` has its own `is_slug` and its own `is_sha256`. They repeat the id rule of tab 02 and the digest form of tab 01.

## Options

| # | For finding | Option | What goes | What is lost |
| --- | --- | --- | --- | --- |
| 1 | 1 | Leave the constant here | Nothing | The crate holds one fact that is not about sessions. Moving it means a new crate for one line, or a database-linked crate under the client. |
| 2 | 2 | The key document is built in one place, in `keys.rs`, beside the function that checks it | The four literals in `identity/platform` | Nothing |
| 3 | 3 | One UUID check, in `wamn-scalar` (ruled in tabs 01 and 04). `is_principal_id` calls it. | The hand-written check here and the five others, as each crate is reached | Nothing. This crate gains a dependency on `wamn-scalar`, which has no database and no HTTP client. |
| 4 | 4 | Leave both. A rule the database enforces and a rule the edge box enforces without that database cannot be one piece of code. | Nothing | Nothing. They can drift. One test with shared cases would catch that; none exists. |
| 5 | 5 | Make the three constants private. Tests in other crates verify through `SessionVerifier`. | Three public constants; five items become crate-private | Tests in `platform/runtime`, `execution/host` and `tests/integration` that call the steps directly are rewritten or moved here |
| 6 | 6 | Leave as is | Nothing | Nothing |

### Option 6 reversed: adopt a JWT library

Owner direction (2026-11-11): we should probably adopt a JWT library. Option 6 (leave the hand-written parsing) is withdrawn. What was read and checked before writing this:

| Fact | Source |
| --- | --- |
| JWT is written by hand twice in this repository. Here, for the session token (verify, and the signer in `identity/platform/src/session_token.rs`). And in `crates/platform/runtime/src/plugins/connection_http/jwt.rs`, 184 lines, which signs outbound client assertions with `ring` for `RS256`, `PS256` and `ES256`. | code read |
| `jsonwebtoken` 10.4.0 is already in `Cargo.lock`. No workspace crate asks for it. It arrives through `oci-client`, built on `aws-lc-rs`. | `Cargo.lock` |
| `jsonwebtoken` 10.4.0 has the algorithms `HS256/384/512`, `ES256/384`, `RS256/384/512`, `PS256/384/512` and `EdDSA`. It has no algorithm named `Ed25519`. | docs.rs page for `jsonwebtoken::Algorithm`, read this session |
| This crate writes and requires the header value `Ed25519`, the fully specified name from RFC 9864, and refuses `EdDSA`. `keys.rs` and four test files say so. | code read |
| The library's `Validation` checks `exp`, `nbf`, `aud`, `iss`, `sub` and the algorithm list, with a leeway. The docs page shows no check of the `typ` header, of a maximum lifetime, or of an `iat` in the future. | docs.rs page for `jsonwebtoken::Validation`, read this session |

What the library would replace here: splitting the token, base64, reading the header, the signature check, and the `exp`, `iss` and `aud` checks. Inference: about 70 of the 218 lines in `token.rs`, and `decode_public_key` if the library's key type reads the key document.

What stays hand-written with any library: the `typ` value, the 900-second maximum lifetime, the future-`iat` rule, the `org` claim, the shape of `sub`, `roles`, `authority` and `csrf`, the closed field set, and the second freshness check in `VerifiedSession::check_admission`.

What changes outside this crate: `connection_http/jwt.rs` can use the same library, so JWT has one implementation. The workspace uses `ring`; the library as locked uses `aws-lc-rs`.

One decision this forces: the header value.

|  | Option | What is lost |
| --- | --- | --- |
| 6a | Adopt `jsonwebtoken` and write `EdDSA` in the header | The fully specified name `Ed25519` from RFC 9864. `EdDSA` also covers Ed448; the key document still pins the curve. Tests that refuse `EdDSA` are inverted. |
| 6b | Adopt a library that has the `Ed25519` name | Not found yet. `jwt-compact` has a type called `Ed25519`; which header value it writes was not read. It would be a new dependency; `jsonwebtoken` is already locked. |
| 6c | Keep the header value `Ed25519` and keep the hand-written code | The owner's direction |

Not checked: whether `jsonwebtoken` reads an Ed25519 key from the key document form used here; whether it refuses a token with a repeated header field; its `rust_crypto` feature as an alternative to `aws-lc-rs`.

## Rulings (2026-11-11)

Owner ruling: adopt `jsonwebtoken`; the current version is 11.1.0; `EdDSA` in the header is fine. The other recommendations for this crate are accepted. Single owner, single purpose.

Checked after the ruling (fact, docs.rs page read this session): `jsonwebtoken` 11.1.0 is the latest version, and its `Algorithm` list is the same twelve as 10.4.0, with `EdDSA` and no `Ed25519`.

| # | Ruling |
| --- | --- |
| 1 | Accepted as written: `PAT_TOKEN_PREFIX` stays here. See the note below. |
| 2 | Accepted. The key document is built in one place, in `keys.rs`. |
| 3 | Accepted. One UUID check, in `wamn-scalar`; `is_principal_id` calls it. |
| 4 | Accepted. The Rust rule and the SQL CHECK both stay. |
| 5 | Accepted. The three constants and the five verification steps become private; outside tests use `SessionVerifier`. |
| 6 | 6a. `jsonwebtoken` 11.1.0 with `EdDSA`. The same library replaces the hand-written signing in `runtime/src/plugins/connection_http/jwt.rs`, so JWT has one implementation. |

What 6a changes besides the code: the header value becomes `EdDSA`; the tests in four files that refuse `EdDSA` are inverted; the lock file moves `jsonwebtoken` from 10.4.0 (through `oci-client`) to a direct 11.1.0, so two versions are locked until `oci-client` follows.

Still to check when the change is made: that the library reads the key document form used here; that it refuses a repeated header field; which crypto feature to enable, since the workspace uses `ring` and the locked build uses `aws-lc-rs`.

Note on ruling 1 against the single-purpose rule: the constant is the one item in this crate that is not about sessions. The owner of personal access tokens is `wamn-platform-identity`, which creates and looks them up. It links the database, so `identity/client` and `platform/engine` cannot import the constant from it. Raised with the owner; not changed.

## Not read

- `tests/file_keys.rs`, beyond its size.
- `IssuerKeys` in `platform/runtime/src/session_keys.rs`, the cloud key source. It belongs to the runtime tab.
- The signer in `identity/platform`, beyond the function named in finding 2.

## Tests deleted, changed or moved (added 2026-11-12)

Checked at `epic/dep7` (b94833fd) by grep. Test names are fact; which ruling each follows is from this tab.

Changed:

- `runtime/src/plugins/connection_http/jwt/tests.rs::fixed_clock_headers_claims_and_scope_separators` and `malformed_pem_and_algorithm_mismatch_refuse_before_dispatch`: signing by `jsonwebtoken` (ruling 6a).
- `identity/session/tests/file_keys.rs` (3 tests): its hand-written `sign` helper uses `jsonwebtoken`.
- Tests that call the verification steps or constants directly use `SessionVerifier` (ruling 5): `identity/platform/tests/session_token.rs`, `session_keys_live.rs`, `services/identity/tests/session_exchange.rs`, `session_exchange/password_lifecycle.rs`.

Moved:

- UUID check cases to `wamn-scalar` (ruling 3).

Deleted by a later tab: `tests/integration/src/host_session_test.rs`, `identity_session_test.rs` (tab 86 finding 2).
