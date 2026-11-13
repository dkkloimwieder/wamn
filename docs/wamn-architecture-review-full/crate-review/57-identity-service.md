# 57 identity service (`services/identity`)

Scope: `services/identity/src` (3,492 lines: `lib.rs`, `password.rs` 1,634, `session.rs`, `pat.rs`, `mail.rs`, `cli.rs`); tests 4,418 lines. Nothing built or run.

## Earlier work checked first

- Tab 88: R-01 revocation and the auth cache; R-02 access tokens for user principals (`POST /session`). Tab 17 since ruled `moka` for the host token map, which keeps a host cache.
- Tabs 02, 05, 17, 27, 54–56 rulings (registry SQL, `jsonwebtoken`, `moka`, SQLx pool per environment, one error type, `SecretToken`, `hyper`).

## Files read

- `lib.rs` (config, state, connection handling, routing); `mail.rs` (Resend client, local file); item lists of `password.rs`, `session.rs`, `cli.rs`; actor binds and provisioning imports (grep).

## One purpose per file

- `lib.rs`: configuration, database handles, HTTPS listener, routing.
- `password.rs`: password, invitation, reset, login and environment discovery routes.
- `session.rs`: `POST /session` (PAT to session token).
- `pat.rs`: `POST /pats` (operator PAT issuance).
- `mail.rs`: invitation and reset mail (Resend or a local file).
- `cli.rs`: command line and environment.

## Findings

**1. Hand-managed database connections.**

Fact: `Inner` holds a shared `Database` behind a lock, a separate issuance connection, a signing connection, one reader per session target, and `reopen` logic after a server restart. Connections use `tokio-postgres` without TLS (`connect_database`).

Options:

- A: SQLx pools (U2; tab 27 finding 3 `moka` pool per environment for the per-target readers). Delete `Database`, `reopen` and the per-purpose mutexes.

**2. Routing by string match and one 500-line handler.**

Fact: `IdentityService::respond` matches method and path strings; `password.rs::handle` serves ten routes in one function with inline SQL.

Options:

- A: one route enum parsed once from method and path; one function per route; SQL in `wamn-platform-identity` (tab 55 finding 6) or `wamn-registry`.

**3. The service's configuration types live in the provisioning crate.**

Fact: `IdentityIssuerConnection`, `parse_identity_issuer_url`, `SessionTarget` come from `wamn-control-provision`; `pat.rs` uses its `bind_platform_principal_sql`; tests use its `SYSTEM_SCHEMA_SQL`.

Options:

- A: the types and the bind move to `wamn-platform-identity`, which provisioning and the service both link. The service no longer depends on provisioning.

**4. The actor bind is written by hand five times.**

Fact: `set_config('app.user_id', ...)` in `password.rs` (five places); `pat.rs` uses `bind_platform_principal_sql`.

Options:

- A: one bind function (finding 3's home); every write calls it.

**5. Registry SQL in the service.**

Fact: `session.rs` reads `registry.orgs` and `registry.project_envs`.

Options:

- A: the reads move to `wamn-registry` (tab 02 finding 5).

**6. Invitation and reset consume the token twice over.**

Fact (at 553511bbd): the consume block is written for invitation and again for reset.

Options:

- A: one consume function over `SecretToken` (tab 55 finding 2).

**7. Throttle, logging and timing defects.**

Fact (at 553511bbd): the `global` (120 per minute) and `source` buckets count renew and logout; the per-email bucket counts before the password check, so anyone can lock out a known user; refusal logs carry the email at info level; reset answers at once for an unknown email while recover is padded to 6 s.

Options:

- A: count `global` and `source` only on routes that hash a password; count the per-email bucket on failed checks only; log the principal id, not the email; pad reset like recover.

**8. User access tokens (R-02, open).**

Fact: no product code issues a PAT to a user principal; `POST /session`, `SessionAuthority::Pat` and the PAT arm of `session_is_active` exist for it (about 110 lines; about 1,450 lines of `tests/session_exchange.rs`).

Options:

- A: refuse `user` in `issue_pat`; delete `POST /session`, `SessionAuthority::Pat`, their arms and tests.
- B: keep them and throttle the exchange through `password::admit`.

**9. The operator certificate is reduced to yes or no.**

Fact (at 553511bbd): one operator certificate per cluster; the control host holds a copy for `/users` and `/invitations`; identity accepts it on `/pats` too.

Options:

- A: `serve` reads the certificate name into `Operator::Platform` or `Operator::Org(org)`; `/pats` admits `Platform` only; `/invitations` for `Org` requires that org.

## Follows from earlier rulings

- `reqwest` to `hyper`: `mail.rs` (owner direction 2026-11-11).
- Tab 05: signing through `jsonwebtoken`.
- Tab 55 finding 5: one error type (`IdentityServiceError` maps from it once).
- Tab 56 finding 6: NATS connect through the one function.

## Fine as is

- `mail.rs` local file mode for development; secrets redacted in `Debug`.

## Tests deleted, changed or moved

- Finding 8 A: `tests/session_exchange.rs` PAT exchange cases (about 1,450 lines).
- Finding 3: tests stop importing `wamn-control-provision` for schema SQL; they use the test database of the owning crate.

## Not read

- `password.rs` route bodies line by line; `session.rs` body; `cli.rs` body; tests.

## Rulings

- Finding 8 ruled A (2026-11-12; settles R-02): `issue_pat` refuses user principals; `POST /session`, `SessionAuthority::Pat`, the PAT arm of `session_is_active` and their tests go. Owner direction: API users are principals created with that explicit purpose (service principals); a web user signs in with the standard login.
- Checked against that direction (fact, grep of product code): service principals are created only by provisioning, for fixed purposes: `MANAGEMENT_AUTHOR` (role `project-author`) and `OPERATOR` (role `admin`) per environment (`pat_secrets/plan.rs`), one per application package (`environment/stage.rs`), and connection principals. No route or `wamn-ctl` verb found that lets an admin create an API service principal with chosen roles and issue its PAT. Not supported today.
- Findings 1–7 and 9 ruled A (2026-11-12).
