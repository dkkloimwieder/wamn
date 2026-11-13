# 54 identity, part 1: session, credential store, project state, client

Scope: `wamn-session` (680 lines), `wamn-credential-store` (667), `wamn-project-state` (526), `wamn-identity-client` (942). Nothing built or run.

## Files read

- `crates/identity/session/src/*` (all).
- `crates/identity/credential-store/src/*` (all but the tail of `kubernetes.rs`).
- `crates/identity/project-state/src/*` (all); `tests/*` (headers and test list).
- `crates/identity/client/src/lib.rs` (PAT path in full; invitation and user paths in part).
- `deploy/sql/app-schema.sql` (`platform.role`); `deploy/sql/system-schema.sql` (role CHECKs).

## Callers read

- Every use of `is_principal_id`, `is_role_slug`, `valid_role`, `SessionAuthority`, `FileKeys`, `KeySource` (grep).
- `wamn_credential_store::open` callers; `KubernetesApi` users (`host/environment_status.rs`); `connections/administration.rs::open_store`.
- Every symbol imported from `wamn_project_state` (grep).
- `services/identity/src/pat.rs` (the PAT route); `PAT_TOKEN_PREFIX` users.

## One purpose per file

- `session`: verify a session token against an issuer's keys (`token.rs`, `keys.rs`, `verifier.rs`, `file_keys.rs`).
- `credential-store`: keep credential values out of the database (`file.rs`, `kubernetes.rs`); `kubernetes_api.rs` is a Kubernetes client that the host's environment status also uses.
- `project-state`: platform principal ids and the admin role name; plus a Rust copy of `app-schema.sql` (finding 7).
- `client`: the operator's HTTPS client for the identity service (PATs, users, invitations).

## Findings

**1. Principal ids and role names are strings checked by hand at each boundary.**

Fact: `wamn_session::token::is_principal_id` checks a lowercase UUID by bytes; `wamn_platform_identity::PrincipalId` wraps `Box<str>`; claims carry `String`. `is_role_slug` (session, used by platform identity and edge grants) matches the `role_name_check` in `app-schema.sql`. `wamn_runtime::plugins::wamn_postgres::claims::valid_role` uses a weaker rule (non-empty) and says the column is free-form text; it is not.

Options:

- A: `PrincipalId(Uuid)` and `RoleName` types in `wamn-session`, checked by their serde parse; claims use them; platform identity, edge and runtime use them. Delete `is_principal_id`, the free `is_role_slug` and `valid_role`. The database CHECKs stay as the database guard.

**2. The PAT token format is known in three places.**

Fact: `PAT_TOKEN_PREFIX` lives in `wamn-session`; `wamn_platform_identity::{mint_token, lookup_prefix}` write and read the format; `wamn_identity_client::decode_response` checks it again by hand (16 and 64 hex digits).

Options:

- A: one `PatToken` type in `wamn-session` (prefix, lookup, secret; parse and display). Platform identity mints and looks up through it; the client parses the returned token with it. Settles the held item "PAT\_TOKEN\_PREFIX home".

**3. Identity request and response shapes are written twice.**

Fact: the client builds bodies with `json!` and reads `PatResponse`, `UserReply`; `services/identity` has its own `IssueRequest`, `IssueResponse` and the user and invitation shapes.

Options:

- A: one typed set in `wamn-identity-client` (a `wire` module); the service serializes and parses with it.

**4. A hand-written Kubernetes client.**

Fact: `kubernetes_api.rs` reads the in-cluster config and calls the API over `reqwest`; `kubernetes.rs` edits Secret JSON by hand with a conflict loop; the host's environment status reuses it. Control environment runs `kubectl` as a subprocess (its tab).

Options:

- A: `kube` and `k8s-openapi` (hyper-based; in-cluster config; typed `Secret`; `resourceVersion` conflicts). Delete `KubernetesApi`; the host's environment status uses `kube`.
- B: keep the hand-written client, moved to `hyper` (owner ruling).

**5. The credential store has two constructions and a name that is not its own.**

Fact: `open("k8s")` builds a store with an empty org, project and environment for `get` and `destroy`; `put` refuses that store; `connections/administration.rs` builds `in_cluster(org, project, env)` for `put`. `service_account_name` (the host ServiceAccount) lives in the credential store. `file.rs` matches `libc::ENOENT`.

Options:

- A: the store opens from its name alone; `put` takes the environment coordinate. `service_account_name` and `secret_name` move beside `project_env_key` in `wamn-control-registry`. `io::ErrorKind::NotFound` replaces the `libc` match; drop `libc`.

**6. A Rust copy of `app-schema.sql` that no product code reads.**

Fact: product code imports only `PlatformComponent`, `workflow_user_id`, `ADMIN_ROLE` and `test_database`. `Table`, `TABLES`, `HISTORY_EXEMPT`, `UserStatus`, `UserType`, the three claim name constants, `SCHEMA_NAME` and `SCHEMA_VERSION` are read only by `tests/schema.rs`, which also pins GRANT lines as text (test rule 2). Product code spells `app.user_id` in 96 files and `platform."user"` in 46. `PlatformComponent::principal_name` repeats the `as_str` match.

Options:

- A: delete the copy and the text pins. The crate keeps platform principals, the workflow user, the admin role name and the test database. `principal_name` is `wamn:` plus `as_str`. The live SQL tests (`tests/authority.rs`, the live tests in `tests/schema.rs`) stay.

## Follows from earlier rulings

- `reqwest` to `hyper` (owner direction 2026-11-11): `wamn-identity-client`, and `wamn-credential-store` under finding 4 B.
- Correction (2026-11-12): `wamn-session` was already reviewed in tab 05; this tab repeated it. Tab 05 rulings stand and take precedence:
  - Tab 05 finding 6 (6a): `jsonwebtoken` 11.1.0 with `EdDSA` replaces the hand-written token code (verify here, sign in `session_token.rs`, and `connection_http/jwt.rs`). The earlier "fine as is" line of this tab was wrong and is removed.
  - Tab 05 finding 3: one UUID check, in `wamn-scalar`. Finding 1 here parses `PrincipalId(Uuid)` through it.
  - Tab 05 finding 4: the role rule stays in Rust and in the SQL CHECK. Finding 1 here wraps the Rust rule in `RoleName`; the CHECK stays.
  - Tab 05 finding 1: `PAT_TOKEN_PREFIX` stays in `wamn-session`. Finding 2 here (`PatToken` in `wamn-session`) agrees.
  - Tab 05 findings 2 and 5: the key document is built once in `keys.rs`; the three constants and five verification steps become private.

## Fine as is

- `FileKeys`, `KeySource`, `SessionVerifier`.
- `async-trait` here: used in 69 files across the workspace; not a per-crate question.

## Tests deleted, changed or moved

- Finding 6: the `Table` drift guard and GRANT text pins in `tests/schema.rs` go.
- Finding 1: tests of `is_principal_id`, `is_role_slug`, `valid_role` become parse tests of the types.
- Finding 4 A: the `kubernetes.rs` mock tests go with the hand-written client.

## Not read

- `client/src/lib.rs` user and invitation bodies in full; the tail of `kubernetes.rs`.

## Rulings

- Finding 4 ruled B (2026-11-12): keep the hand-written Kubernetes client (about 300 lines), moved to `hyper`. Later option to explore: A (`kube` and `k8s-openapi`) for all Kubernetes callers, if the control environment tab removes the `kubectl` subprocess (233-line wrapper, about 70 call sites). Facts for that choice: `kube` is not in `Cargo.lock`; `kube-client` 5.1.0 always pulls `k8s-openapi` (0.29.0, 3.1 MB compressed), `kube-core` and `secrecy`, and a second `tower-http` (0.7 beside 0.6.11); `WorkloadDeployment` needs a dynamic object or a `CustomResource` derive.
- Findings 1, 2, 3, 5, 6 ruled A (2026-11-12).
