# WAMN session tokens (JWT) — proposal, rev 8

**Status:** owner-approved direction and constants · 2026-09-07 · implementation prerequisites remain
**Scope:** a short-lived session credential beside PATs. Companion to
`wamn_http_auth_proposal.md`; sequenced after its step 1.

## 1. Direction

Add a **short-lived signed session token** minted by the platform's identity
authority on PAT exchange (later, OIDC login). Hosts verify it offline. PATs stay
for machines and integrations. Environment membership is the first identity
prerequisite. The named session consumer is the TUI login (slice v).

What it is: **signed identity-and-role evidence, fresh tenant permissions, no
per-session database state.** What it is not: a way around the freshness
trade-off. A JWT is a signed snapshot; the owner-approved window is stated below.
Signing keys, key distribution, and client renewal are real added
complexity, justified by the TUI use case and the measured saving, not assumed.

The new `wamn-identity` deployable owns `POST /session` and
`GET /.well-known/jwks.json`, using the identity authority in
`crates/identity/platform`. It is a small, long-running service backed by the
system database. Signing keys remain with that authority; hosts receive
public keys only. It never shares a process with a host or the authoring Gate
(`POST /authoring`, system-principal mode).

The owner accepts the service's chart, inventory, and overlay cost.
OIDC federation later lands in `wamn-identity`; the identity epic needs no
further service. PAT minting stays on the provisioning CLI for now.
Moving PAT minting into the service is later work (`wamn-ctc8.20`), after
the service exists.

## 2. The revocation window, stated exactly

- **Next request:** changes to a role's operation permissions (the tenant
  permission table is read fresh on every request).
- **At session expiry:** PAT revocation, principal disablement, and **removal
  of project-environment membership or a role assignment**. The token carries
  `roles` as minted; the fresh permission read finds permissions for roles
  named in the token — it
  does not re-check membership. Test: remove a role assignment, the live
  session keeps that role's permissions until `exp + tolerance`, the next
  exchange does not.
- **The window includes the clock tolerance** (§3): expiry-bound revocations
  take effect at `exp` plus tolerance. The owner accepts a maximum identity
  revocation window of **15 minutes 30 seconds**, including tolerance.
- Operations declared fresh-only cannot accept a session token at all. This
  is checked at the **registered-operation boundary**, not the route: the
  host-owned caller context carries the originating credential kind through
  nesting, and a fresh-only callee refuses a session-originated caller even
  when the entry route accepted the session.

**Boundary for v1:** no session table, no denylist, no revocation feed, no
per-session database writes. `jti` is a token identifier only. Earlier session
revocation is a separate proposal with its own consistency rules.

## 3. Token and verification profile

- **Wire:** JWT; `alg: Ed25519` (RFC 9864's fully-specified identifier; the
  generic `EdDSA` is deprecated — confirm library support and state the exact
  value); `typ: wamn-session+jwt`.
- **Claims, all required:** `iss` (configured identity authority), `sub`
  (org-issued stable user id), `org` (org id), `aud` (the exact
  project-environment **id**, never a name), `roles` (role slugs for
  that scope only), `exp`, `iat`, `jti`.
- **Verification:** signature under the pinned algorithm with a key from the
  configured issuer's JWKS only (`kid` selects, never establishes trust);
  `iss`, `org`, `aud` match the host's own configured identity exactly;
  `typ` matches. **Age rules, separately:** `0 < exp − iat ≤ maximum_lifetime`;
  `iat` not further in the future than the tolerance; the token is expired when
  `now >= exp + tolerance` (RFC 7519: expiry is at-or-after). The owner-set
  constants are `maximum_lifetime = 15 minutes` and `tolerance = 30 seconds`.
  Any miss refuses; refusals are indistinguishable (`401`).
- **Evidence age — one rule for keys and minting.** A deadline is anchored
  at the **start** of the authoritative fetch or validation, never at
  completion; delay shortens what remains and never restarts a window.
  - *Keys:* a JWKS response is aged from the moment its request began, plus
    any HTTP `Age` it carries; a response arriving after its own deadline is
    discarded; one arriving in time takes only the window that remains.
  - *Minting:* the issuer sets `iat` to the actual signing time and bounds
    `exp <= validation_started_at + maximum_lifetime`; an exchange that
    cannot produce an unexpired token refuses. Delayed signing shortens the
    token, never lengthens the evidence.
  - *What this does not promise:* anchoring bounds the **age** of evidence,
    not its **currency** — a validation that read valid state, paused, and
    finished within its window may succeed even if a revocation landed
    during the pause; the revocation takes effect at the window's end. A
    validation that reads already-revoked state refuses. No coordination is
    added to close that gap; the window is the guarantee.
  - *Tests:* a refresh or exchange resumed **after** its deadline is
    discarded/refused; one resumed **within** the deadline succeeds with a
    token whose `exp` reflects the original start, not the resume; a JWKS
    response with prior `Age` is retired earlier by that amount.
- **Permission resolution:** the caller's exact operation permissions are the
  **union of the tenant grants for the verified role slugs**, read fresh from
  the existing permission table. A role slug the tenant does not know
  contributes nothing; there is no implicit default role; an empty union is
  `403` on any registered operation. No second role store.
- **Tests:** two organizations with identically named projects and
  environments; `dev` vs `prod`; a token for one `aud` refused by every other;
  two human principals with different roles resolving to different permission
  sets, and a principal whose roles carry no applicable grants refused.

## 4. Keys

- **Ownership:** `wamn-identity` holds signing keys in the system
  database and rotates the active signing generation by a generation flip,
  as credentials do. Private signing keys never reach a host or the
  authoring Gate. `GET /.well-known/jwks.json` exposes public verification
  keys only.
- **Rotation:** publish a new key before signing with it; retain a retiring
  key's verification through `maximum_lifetime + tolerance` counted from the
  **last token signed with it**; retire after.
- **Host key cache:** the whole key set has a bounded freshness
  (`jwks_max_age = 5 minutes`, owner-set). When it expires, the host refreshes —
  for known keys too, not only on an unknown `kid` — and **refuses all session tokens
  after that deadline if the refresh fails** (fail closed; cached keys past
  freshness are not evidence). Refreshes are bounded **per issuer**, not per
  request or per `kid`, so varying `kid` values cannot drive refresh load.
- **Compromised-key removal:** a distinct operation from rotation — the key is
  removed from the JWKS at once. The **maximum acceptance window** for tokens
  it signed is `jwks_max_age`: the owner accepts **at most 5 minutes**.
  Test: two hosts with warm caches, key removed, both refuse within
  `jwks_max_age`; repeat with the JWKS endpoint unavailable — both refuse at
  the deadline rather than keep trusting the stale key.

### Owner rulings, 2026-09-08

The owner approved these foundation choices in `wamn-ctc8.24` through `.28`.
They clarify this proposal without removing any §8 proof.

The retirement clock starts at a proven stop-signing barrier.
The barrier prevents successful issuance with the old key after its cutoff.
A successful database commit must precede the return of a signed token.
Retain the old public key for 930 seconds after that cutoff.
This bound requires no per-token database writes or durable last-signature timestamp.
Compromised-key removal remains immediate and does not activate a replacement.

The fixed JWT profile uses `ring` for Ed25519 signatures.
The wire algorithm remains exactly `Ed25519`, and the type remains `wamn-session+jwt`.
There is no generic JWT framework or alternate algorithm.

The service uses cluster-internal HTTPS with a configured issuer and trusted CA.
This foundation adds no external ingress or trust root.
The later TUI work owns the external connection path.

Each configured issuer permits one JWKS request at a time.
Request starts remain at least one second apart.
Each fetch has a five-second total timeout and a 65,536-byte response limit.
These limits never extend the 300-second freshness deadline.

`wamn-ctc8.15.1` owns the deployed service boundary, key lifecycle, and public-key cache proofs.
`wamn-ctc8.15.3` owns the actual two-host session-token acceptance and refusal proofs.
Those proofs include both reachable and unreachable JWKS.
Session activation remains blocked until those proofs and the fresh-only operation proof pass.

## 5. Minting

- `POST /session`: a valid **human** PAT → one token for one requested `aud`.
  Service PATs cannot be exchanged in v1; a session never converts a service
  principal into a human identity. Both kinds may be supported later by an
  explicit rule, not implicit conversion.
- **Mint eligibility, explicit:** `/session` issues for an `aud` only when
  the principal holds an explicit org-user → **project-environment**
  membership grant. The `wamn-ctc8` org-identity ruling controls this check.
  This membership is a system-database fact for the org user and exact
  project-environment, owned by `wamn-ctc8.19`. The provisioning CLI
  writes it. Route authentication and `/session` read the same authority-owned
  check. `app_system.users` receives the org-issued stable user id; it does
  not create another identity origin.
  A project role alone does not establish membership in an environment.
  The token carries only that environment's roles. `/session` refuses a
  requested `aud` in another organization, or without membership in that
  project-environment. All identity and membership checks happen here, fresh.
  Reuse that membership check; do not create a separate session-specific
  membership store.
- **Membership proof:** with membership in `dev` only, the same human PAT
  obtains a `dev` session and cannot obtain a `prod` session in that project.
  Removing membership prevents the next exchange. An already-issued session
  remains subject to §2's expiry window.
- No refresh tokens. The client re-exchanges its PAT at expiry; PAT and
  session lifetimes are separate and both stated.
- Session tokens are not persisted server-side and never logged; the client
  holds its token in memory while using it.

### Exchange rulings, 2026-09-08

The owner approved the exchange choices in `wamn-ctc8.29` through `.31`.
These rulings clarify §§3 and 5 and preserve the §8 proof list.

The exact audience is `urn:wamn:project-env:{org}:{project}:{env}:{instance_suffix}`.
It includes the current registry instance suffix, not only the environment name.
The service refuses a target whose registry coordinates no longer match.
The existing registry does not guarantee that a suffix never repeats across all historical instances.

One configured issuer serves the explicitly provisioned organizations in a platform installation.
The provisioning CLI binds each audience to its organization, tenant, physical database, and dedicated read credential.
The service reads these bindings from mounted Secrets.
The HTTP request supplies only the audience, never an organization override, tenant, database URL, or roles.
The principal must hold current membership in that exact organization and project-environment.
The global principal record has no separate home-organization field.
The wrong-organization proof tests absent exact membership, not a home organization inferred from PAT text.

The issuer reads only the approved principal, PAT, membership, and registry columns in the system database.
A dedicated `SessionRoleReader` reads active users and their role assignments in the selected environment.
It adds no tenant writes, permission reads, catalog reads, or execution grants.
The role query includes the trusted tenant and authenticated principal explicitly.
The service takes roles only from that environment and refuses an empty role set.

WAMN supplies the initial login path through human PAT authentication.
Credential authentication remains separate from minting for an authenticated canonical principal.
Deferred `wamn-117` owns external login providers, including customer providers and outsourced login.
That work must map approved external identities to the same principals, environment memberships, and session profile.
This increment adds no unused provider framework, alternate principal store, session store, or revocation feed.
The accepted 900-second lifetime, 30-second tolerance, and 300-second key freshness limit remain unchanged.
Future upstream disablement and configurable limits belong to the federation design.

The owner approved retained readers in `wamn-ctc8.32`.
The service opens one scoped connection for each active configured environment and retains it between exchanges.
It reads current users and roles on every exchange, without a role cache.
Unused environments hold no connection.
Credential rotation retains the existing rule that the replacement generation must have a live service connection before retirement.
After a target Secret changes, restart the service and complete a replacement-generation exchange before retiring the old credential.
The service never reuses a connection for another audience or tenant.

## 6. Request path

```text
Bearer session token
→ offline verification (profile above), no database
→ the same tenant permission tables, fresh, by the role-union rule in §3
→ authenticated caller with exact operation permissions
```

**Already-admitted work:** session expiry and key-cache freshness govern
acceptance of **new requests**; they never cancel work already admitted.
Nested calls retain the original caller and still enforce operation
permissions and the fresh-only restriction. No per-node re-authentication,
no coordinated cancellation.

Route policy: `auth-policy.modes: [pat] | [session] | [pat, session]` — an
explicit list, one entry per landed mechanism; `none` stays incompatible with
registered operations. Nested calls carry the originating principal as today;
a fresh-only callee refuses a session-authenticated caller.

## 7. TUI login (v1)

Target the client crates on `main`; `lane/5-client` was integrated at
`e6300f3e`. Use the existing `CredentialProvider` boundary in
`crates/client/core`, with the receiving client as the concrete consumer.

The TUI obtains a PAT from environment/config (as today), exchanges it for a
session on start and at expiry, and uses the session for ordinary calls. This
is a PAT exchange, not a new human-login mechanism; OIDC login arrives with the
directory and issues the same token through the same verifier.

**Fresh-only rule for clients:** a call the client knows is fresh-only uses the
PAT path directly. A late nested refusal (`fresh-credential-required`) is
reported to the user, never answered by silently re-running the whole wiring
under the PAT — that would repeat already-committed work.

## 8. Sequencing and evidence

1. After the HTTP/auth proposal's step-1 baseline is measured. Two reads are
   the target, not a precondition. A measured three-read baseline retained
   under the stop rule also unblocks this work.
2. **Owner approval recorded on 2026-09-07, before session-token code:**
   `maximum_lifetime = 15 minutes`, `tolerance = 30 seconds`, and
   `jwks_max_age = 5 minutes`. The owner accepts §2's 15.5-minute identity
   revocation window, compromised-key acceptance of at most 5 minutes, and
   permission changes on the next request.
3. Membership (`wamn-ctc8.19`) is the first identity implementation bead and
   precedes the step-1 measurement. The session beads then deliver
   `wamn-identity` with key management + JWKS and the cache/refresh policy;
   `/session`; host verifier + `session` mode + credential-kind in the caller
   context; fresh-only as a registered-operation property; TUI login with the
   fresh-only client rule.
4. Proofs: §3's audience matrix and role-resolution cases; age rules (over
   lifetime, future `iat`, expired under tolerance); rotation overlap counted
   from last signing; compromised-key removal on two warm hosts, with and
   without JWKS reachable; role-assignment removal window; permission change
   next-request; service PAT refused at exchange; fresh-only callee refuses a
   session caller through a nested call — **paired with** the same human's
   valid PAT succeeding on that operation, and refusing on the next request
   after its project-role assignment is removed; minting refused for another
   organization's `aud` and for a project with no role; the evidence-age
   tests in §3 (late results discarded, in-window results keep the original
   start).
5. Measured with the existing bench: steady session requests (database reads
   from the measured step-1 baseline to one tenant permission read), exchange
   frequency and cost, cold-key (JWKS miss) behavior,
   cold traffic and failures. No unmeasured latency claims.

The owner resolved the membership and service questions in `wamn-ctc8.17`
and `wamn-ctc8.18`. The membership fact and live route reader belong to
`wamn-ctc8.19`; the new deployable, public JWKS, key management, chart,
inventory, overlay, and deployed boundary proof belong to `wamn-ctc8.15.1`.
`wamn-ctc8.15.2` adds `/session` and depends on both implementations.
These are implementation tasks, not claims that the prerequisites exist.
At `f100345a`, route PAT authentication accepts only the configured service
principal. The membership route reader must supply fresh human PAT admission;
the verifier work reuses it for item 4's paired proof.
Before enabling sessions, reconcile the owner-approved exception with the
fresh-authorization rule in `docs/exe-model.md`; fresh PAT checks and tenant
permission reads retain next-request revocation.

## PAT service addendum, 2026-09-10

The owner approved `wamn-ctc8.20` on 2026-09-10.
This addendum replaces the temporary CLI minting rule in §1 and preserves the earlier proposal as a historical snapshot.
The existing native `wamn-identity` process now owns `POST /pats` through the existing identity library and system database.
The authoring Gate and hosts do not gain PAT minting authority.

Only provisioning operators authenticate to `/pats`, through client certificates.
A dedicated certificate authority (CA) signs these operator certificates.
Configure its trust roots with `wamn-identity serve --operator-ca` or `WAMN_IDENTITY_OPERATOR_CA`.
Do not use this CA for host, application, or ordinary user certificates.

Every accepted certificate from this CA grants operator authority to mint for an existing active principal, whether human or service.
PATs, JWTs, and identity headers do not grant this authority.
Without operator trust roots, the service refuses PAT issuance.
The public JWKS and existing `/session` authentication rules remain unchanged.

The endpoint accepts `Content-Type: application/json` with exactly `principal_id`, `label`, and integer `lifetime_seconds`.
The service refuses extra or duplicate fields, bodies above 1,024 bytes, missing principals, and disabled principals.
The existing library limits the trimmed label to 1–200 bytes and the lifetime to 1 second–365 days.
The endpoint creates no principal, membership, or role assignment.

Success returns HTTP `201` with exactly `token`, `token_prefix`, `principal_id`, `created_at`, and `expires_at`.
The timestamps use RFC 3339 UTC text.
The service sends `Cache-Control: no-store` and never logs the raw token.

The `provision-project-env` command keeps its existing principal creation, role assignment, PAT authentication, and prefix-based revocation operations in the system database.
When either PAT Secret flag is present, the command requires the identity service, including during first-time provisioning.
The command requests the existing 30-day lifetime and authenticates the returned token against the expected principal before writing its Secret.
Secret output remains an atomic file replacement with mode `0600`, never stdout.
The command has no direct-database fallback for PAT issuance.

Configure the operator transport with these flags or their environment variables:

- Set `--pat-issuer` or `WAMN_PAT_ISSUER` to the HTTPS base URL.
- Set `--pat-client-cert` or `WAMN_PAT_CLIENT_CERT` to the PEM certificate chain.
- Set `--pat-client-key` or `WAMN_PAT_CLIENT_KEY` to the PEM private key.
- Set optional `--pat-server-ca` or `WAMN_PAT_SERVER_CA` to PEM roots that replace the default server trust roots.

The CLI appends `/pats` to the configured base path.
It refuses URL credentials, queries, fragments, and incomplete TLS configuration before provisioning writes.
It authenticates the server certificate, allows five seconds for the request, and refuses responses above 4,096 bytes.
It follows no redirects and retries no issuance request.

If the connection fails after issuance, a PAT can exist even when its raw token never reaches the operator.
The CLI reports that uncertainty without exposing response contents or credentials.
The stored digest cannot recover the lost raw token.

This increment runs correctness and security proofs, not benchmarks.
Both PATs and JWT sessions remain required.
The owner paused session measurements in `wamn-ctc8.15.6`.
