# WAMN session tokens (JWT) — proposal, rev 6

**Status:** ACCEPTED for owner decision · 2026-09-07 · five external review rounds; no architectural blocker remaining
**Scope:** a short-lived session credential beside PATs. Companion to
`wamn_http_auth_proposal.md`; sequenced after its step 1.

## 1. Direction

Add a **short-lived signed session token** minted by the platform's identity
authority on PAT exchange (later, OIDC login). Hosts verify it offline. PATs stay
for machines and integrations. First deliverable of the identity epic; named
consumer: the TUI login (slice v).

What it is: **signed identity-and-role evidence, fresh tenant permissions, no new
database state.** What it is not: a way around the freshness trade-off. A JWT is
a signed snapshot; the window it creates is stated below and is the owner's to
accept. Signing keys, key distribution, and client renewal are real added
complexity, justified by the TUI use case and the measured saving, not assumed.

## 2. The revocation window, stated exactly

- **Next request:** changes to a role's operation permissions (the tenant
  permission table is read fresh on every request).
- **At session expiry:** PAT revocation, principal disablement, and **removal
  of a project-role assignment**. The token carries `roles` as minted; the
  fresh permission read finds permissions for roles named in the token — it
  does not re-check membership. Test: remove a role assignment, the live
  session keeps that role's permissions until `exp + tolerance`, the next
  exchange does not.
- **The window includes the clock tolerance** (§3): expiry-bound revocations
  take effect at `exp` plus tolerance. The owner approves that total.
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
  project-environment **id**, never a name), `roles` (project-role slugs for
  that scope only), `exp`, `iat`, `jti`.
- **Verification:** signature under the pinned algorithm with a key from the
  configured issuer's JWKS only (`kid` selects, never establishes trust);
  `iss`, `org`, `aud` match the host's own configured identity exactly;
  `typ` matches. **Age rules, separately:** `0 < exp − iat ≤ maximum_lifetime`;
  `iat` not further in the future than the tolerance; the token is expired when
  `now >= exp + tolerance` (RFC 7519: expiry is at-or-after). Both `maximum_lifetime` and `tolerance` are owner-set
  constants. Any miss refuses; refusals are indistinguishable (`401`).
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

- **Rotation:** publish a new key before signing with it; retain a retiring
  key's verification through `maximum_lifetime + tolerance` counted from the
  **last token signed with it**; retire after.
- **Host key cache:** the whole key set has a bounded freshness
  (`jwks_max_age`, owner-set). When it expires, the host refreshes — for known
  keys too, not only on an unknown `kid` — and **refuses all session tokens
  after that deadline if the refresh fails** (fail closed; cached keys past
  freshness are not evidence). Refreshes are bounded **per issuer**, not per
  request or per `kid`, so varying `kid` values cannot drive refresh load.
- **Compromised-key removal:** a distinct operation from rotation — the key is
  removed from the JWKS at once. The **maximum acceptance window** for tokens
  it signed is `jwks_max_age`, stated as the number the owner accepts.
  Test: two hosts with warm caches, key removed, both refuse within
  `jwks_max_age`; repeat with the JWKS endpoint unavailable — both refuse at
  the deadline rather than keep trusting the stale key.

## 5. Minting

- `POST /session`: a valid **human** PAT → one token for one requested `aud`.
  Service PATs cannot be exchanged in v1; a session never converts a service
  principal into a human identity. Both kinds may be supported later by an
  explicit rule, not implicit conversion.
- **Mint eligibility, explicit:** a principal holding a project role may
  obtain a session for **any environment of that project**; what the session
  may do in that environment is decided by that environment's fresh grants.
  No environment-specific eligibility store is added. `/session` refuses a
  requested `aud` in another organization, or in a project where the
  principal holds no role, and issues only the requested scope's roles. All
  identity reads happen here, fresh.
- No refresh tokens. The client re-exchanges its PAT at expiry; PAT and
  session lifetimes are separate and both stated.
- Session tokens are not persisted server-side and never logged; the client
  holds its token in memory while using it.

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

The TUI obtains a PAT from environment/config (as today), exchanges it for a
session on start and at expiry, and uses the session for ordinary calls. This
is a PAT exchange, not a new human-login mechanism; OIDC login arrives with the
directory and issues the same token through the same verifier.

**Fresh-only rule for clients:** a call the client knows is fresh-only uses the
PAT path directly. A late nested refusal (`fresh-credential-required`) is
reported to the user, never answered by silently re-running the whole wiring
under the PAT — that would repeat already-committed work.

## 8. Sequencing and evidence

1. After the HTTP/auth proposal's step 1 (two-read fresh baseline measured).
2. **Owner approval, recorded before code, of all three constants** —
   `maximum_lifetime`, `tolerance`, `jwks_max_age` — and of §2's
   identity-revocation window that they jointly define.
3. Beads under the identity epic: key management + JWKS with the cache/refresh
   policy; `/session`; host verifier + `session` mode + credential-kind in the
   caller context; fresh-only as a registered-operation property; TUI login
   with the fresh-only client rule.
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
   2 → 1), exchange frequency and cost, cold-key (JWKS miss) behavior,
   cold traffic and failures. No unmeasured latency claims.
