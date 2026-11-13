# 55 identity, part 2: wamn-platform-identity core

Scope: `crates/identity/platform/src/` `lib.rs` (principals, PATs, memberships), `session_token.rs`, `session_keys.rs`, `password.rs`, `password_login.rs`, `control.rs` (3,277 lines of 7,149). Part 3 takes orgs, applications, configuration, connections and NATS. Nothing built or run.

## Files read

- `lib.rs` (SQL constants, types, errors, prepared reads, helpers); `control.rs` (SQL, function list); `session_keys.rs` (publication); `password.rs` (work budget, tokens, actor bind); `password_login.rs` (decode, renewal credential, lock).
- `crates/control/registry/src/identifiers.rs` (slug rule); `crates/control/provision/src/name.rs` (header); `crates/execution/run-state/src/invocation_context.rs::is_slug`.

## Callers read

- Counts of `set_config('app.user_id'`, the active-membership predicate and token prefixes (grep).
- Callers of the slug rules (grep).

## One purpose per file

- `lib.rs`: today principals, PATs, project roles, environment memberships, input rules and the error type. Finding 6.
- `session_token.rs`: sign a session token with the active key; read whether its authority is active.
- `session_keys.rs`: the signing key lifecycle (publish, activate, retire, remove).
- `password.rs`: password enrollment, reset and check; invitation and reset secrets.
- `password_login.rs`: logins and single-use renewal credentials.
- `control.rs`: who may hold a control session of an org.

## Findings

**1. Hand-written SQL strings over `tokio-postgres`, with ids and times as text.**

Fact: every query is a string constant run by `tokio-postgres`; rows decode by index (`decode_principal`, `decode_pat`, `password_login::decode`). Ids cross as text (`id::text`, `$1::text::uuid`). Times are formatted by `to_char(... AT TIME ZONE 'UTC', ...)` in SQL (nine copies) and carried as `Box<str>`; a comment says this avoids a calendar dependency.

Options:

- A: SQLx with build-time checked queries (U2 ruling) and typed rows; `PrincipalId(Uuid)` (tab 54 finding 1); `DateTime<Utc>` written as an ISO UTC string at the wire (owner datetime ruling). Delete the casts, the `to_char` copies and the decode functions.

**2. Four secret formats, each with its own mint, check and hash.**

Fact: PATs (`mint_token`, `lookup_prefix`, `digest_token`), invitations and resets (`password::token_hash`, `wamn_inv_`, `wamn_reset_`), renewal credentials (`password_login::hash`, `credential`, `wamn_renew_`). Each is a prefix plus 32 random bytes in hex, stored as SHA-256.

Options:

- A: one `SecretToken` type in `wamn-session`, beside `PatToken` (tab 54 finding 2): prefix, random hex, SHA-256 digest, constant-time match. PAT adds its lookup half. Delete the four copies.

**3. The active-member rule is written into many SQL strings.**

Fact: "an active user with an active org membership" (`p.type = 'user' AND p.status = 'active' AND EXISTS (... org_memberships ... status = 'active')`) appears in `control.rs` (all six reads) and `lib.rs` (route PAT, environment membership).

Options:

- A: one SQL function or view in `system-schema.sql` (for example `identity.is_active_member(principal, org)`); the queries call it.

**4. Org, project and environment names have several rules.**

Fact: `wamn_control_registry::identifiers::slug_reason` (the owner); `wamn_control_provision::name::slug_reason` (a copy); `wamn_run_state::invocation_context::is_slug` (weaker: no start or end rule); `wamn_platform_identity::checked_scope_segment` (1 to 40 bytes, any character). Role names have their own rule (tab 54 finding 1).

Options:

- A: `OrgName`, `ProjectName`, `EnvName` types in `wamn-control-registry`, parsed once; identity, provision and run-state take them. Delete the copies. Touches the held item "environment spelled many ways".

**5. Several error types and database mappers in one crate.**

Fact: `IdentityError` (type, message, refusal) and `PasswordError` (type, cause); database mappers `lib.rs::database_error`, `password.rs::database`, `password_login.rs::database`, `session_keys.rs::key_database_error`.

Options:

- A: one error type for the crate and one database mapping (with SQLx, one `From<sqlx::Error>`), per the repository error convention.

**6. `lib.rs` holds four subjects.**

Fact: 1,561 lines: principals, PATs, project roles and environment memberships, input rules, errors.

Options:

- A: one file per subject (`principal.rs`, `pat.rs`, `membership.rs`, `error.rs`); `lib.rs` keeps the module list.

## Follows from earlier rulings

- Tab 54 finding 1: `PrincipalId` and `ProjectRole` become the `wamn-session` types; `canonical_role` goes.
- Tab 54 finding 2: the PAT format moves to `PatToken`.
- Tab 05 finding 6 (6a): `session_token.rs` signs with `jsonwebtoken` 11.1.0 and `EdDSA`.
- Tab 05 finding 2: `session_keys.rs::public_key` builds the key document through `wamn-session::keys`.
- Tab 05 finding 3 and tab 04: UUIDs and timestamps use `wamn-scalar` (tab 01). Finding 1 here writes times through it.
- Tab 02 and tab 03: the registry owns org, project and environment identifiers; the Architecture tab's hierarchy is org, environment, project, application. Finding 4 here follows both.
- Errors (inference from a search of tabs 01, 04, 05, 10, 22, 25 and 29): no earlier ruling on crate-internal error types. Rules row 11 of the Architecture tab and the tab 04 ruling cover application errors only. Finding 5 here follows the repository error convention in `CLAUDE.md`.

### Dependency check (2026-11-12)

Workspace dependencies of `wamn-platform-identity` and what it uses from each (fact, grep of `src/`):

- `wamn-session` (tabs 05, 54): `is_role_slug`, `is_principal_id`, `keys`, `SessionError`, `PAT_TOKEN_PREFIX`. Covered above.
- `wamn-credential-store` (tab 54): `open`, `KubernetesStore::in_cluster`, `Error` in `connections/administration.rs`. Tab 54 finding 5 A changes `open_store` there (part 3).
- `wamn-project-state` (tab 54): `PlatformComponent::Provisioning`, `ADMIN_ROLE`. Both kept by tab 54 finding 6.
- `wamn-execution-contract` (tab 01, crate dissolved): `canonical_json_bytes` in `paging.rs`. Tab 01 moves canonical JSON and SHA-256 to the new crate `wamn-digest`, with one digest function. This crate also hashes by itself (`sha2` and `hex` in `digest_token`, `password::token_hash`, `password_login::hash`); under tab 01 those use `wamn-digest`, inside the `SecretToken` of finding 2.
- `wamn-catalog` (no tab yet): `is_host_route_operation`, `is_granted_host_route_operation` in `application.rs`. Carried to the catalog tab.
- Dev only: `wamn-control-provision` (no tab yet), `wamn-test-infrastructure`, `wamn-test-postgres`.

Tables this crate reads or writes that another crate owns (fact):

- `registry.*` (system database, owner `wamn-control-registry`): `org.rs` 9 uses, `connection_projection.rs` 11, `control.rs` 4, `connections/administration.rs` 4, `application.rs` 1. Tab 02 finding 5 (my reading of the owner's ruling, not confirmed): registry SQL is gathered in one place, not spread over callers. This crate is such a caller. Option set for part 3.
- `catalog.*` (`connection_binding`, `release_connection`): `connections/administration.rs`, `connections/references.rs`. Carried to the catalog tab.

Also found: `paging.rs` builds its own page cursor (base64 of canonical JSON). The generator's cursor copy was deleted (tab 51 finding 1); `wamn-data-access::cursor` is the product cursor for guests. Option set for part 3.

## Fine as is

- `PasswordWork`: a fixed Argon2id profile behind a two-job semaphore on blocking threads.
- Session keys: private keys stay in the system database; signers hold the issuer row lock.

## Tests deleted, changed or moved

- Finding 1: tests that read text times or text ids change to typed values.
- Finding 2: token format tests move to `wamn-session`.
- Finding 4: the slug tests of provision and run-state go; registry keeps one set.

## Not read

- `password.rs` enrollment and reset flows in full; `session_token.rs`; `password_login.rs` rotation; the crate's tests.
- Other `set_config('app.user_id', ...)` binds outside this crate (28 in product code): a cross-crate question for the runtime tab.

## Rulings

- 2026-11-12: all findings A.
