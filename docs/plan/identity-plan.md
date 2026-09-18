# Human identity and login

Owner-directed implementation sequence, updated 2026-09-18.
This revision incorporates the owner decision against operation-specific password reauthentication.
Password-first is settled. OIDC remains planned future work.
Beads owns scheduling, decisions, and implementation status.

## Goal and planning rule

An invited person signs into Receiving with a password and performs an authorized operation without obtaining a personal access token (PAT).
This is the first milestone, not the completed human-login release.
Renewal, recovery, and PAT-free sensitive operations follow in separate increments.
Their design must not block the first working password login.
People sign in. Integrations use separately provisioned credentials.
Ordinary onboarding and daily Receiving use must never require a person to create, copy, or understand a PAT.

Only the first epic receives detailed implementation issues now.
At every later epic start, review the preceding results and current implementation before planning that epic's issues.
Do not pre-plan the complete backlog or treat an epic dependency becoming clear as permission to skip reassessment.
OIDC has a future epic without implementation children.

## Existing authority

The [execution architecture](../architecture/execution.md#caller-authentication) defines the current authentication contract.
WAMN keeps its canonical principal, the stable identity shared across login methods.
Explicit membership grants control entry to an environment.
The environment's roles grant operation permissions.
Authentication never creates membership or permission by itself.

The existing `wamn-identity` service issues Ed25519-signed session tokens through password login or optional human PAT exchange.
The audience identifies the exact organization, project, environment, and environment instance.
Tokens last at most 900 seconds with 30 seconds of tolerance.
Hosts use the configured issuer's public keys, with a 300-second freshness limit, and read current role permissions per request.
New session admissions read current account, membership, login or source-PAT validity, and assigned roles.
Revocation blocks the next admission without waiting for token expiry.

Human creation, membership grants, and tenant-user provisioning already exist through `create-human`, `grant-project-env-membership`, and `reconcile-run-plane`.
Reuse these authorities and the existing token issuer.
The same person keeps the same identifier through password login and optional PAT access.
Service credentials never establish a human identity.
PATs remain available for existing clients, integrations, scripts, and optional developer access.
Password login issues a session directly. It never creates a PAT behind the scenes or requires PAT exchange.

## Receiving route policy

Receiving's existing authenticated application routes accept both session tokens and PATs.
Sessions are the default for people.
Both credentials use one set of routes, the same canonical caller, and one permission model.
Accepting a credential establishes the caller. It grants no additional membership, role, or operation permission.
Private event handlers remain private and gain no application route.

The first increment preserves the original PAT-only restriction during initial session enablement.
The [session-policy increment](#increment-3-renewable-sessions-for-all-permitted-operations) replaces that transitional restriction.
The owner rejects operation-specific password prompts and human PAT requirements.

## Delivery increments

| Increment | Usable outcome | Explicit limits |
| --- | --- | --- |
| 1. Password login | Operator invitation, first password, terminal login, existing session token, authorized Receiving operation. | No renewal or password recovery. Expiry requires another login. Logout clears local credentials. Fresh-only operations remain PAT-only. |
| 2. Everyday account use | Renewal, server-side logout, and password recovery. | No persistent terminal credentials, renewal retry grace, account linking, or self-service email changes. |
| 3. Consistent session access | Renewable sessions authorize all permitted application operations, including nested calls. | Normal login expiry and permission checks remain. No operation-specific password prompt. |

Complete the first milestone before adding the remaining session lifecycle.
An implementation issue cannot pull later-increment machinery into increment 1 for convenience.
The complete human-login release includes the agreed later increments; milestone 1 alone does not satisfy that larger scope.
The third increment removes the remaining human PAT requirement.
It does not add password reauthentication or a five-minute authentication window.

## Increment 1: password login

The first client is the existing operator terminal.
The existing identity service owns passwords and invitation credentials in the system database.
Hosts receive signed access tokens, never passwords or invitation secrets.
This increment adds no renewal credential, session table, authentication-time claim, or token-profile change.

Enrollment is invitation-based, with no public self-registration.
An authorized operator enters the person's email and creates or selects the canonical human principal.
The operator assigns application and environment access through the existing membership and role operations.
The operator issues an invitation for that explicitly selected, unenrolled principal.

The identity service sends an invitation to the principal's email address.
The email identifies Receiving and asks the person to set a password.
It contains a random, time-limited, single-use link or code, never a password or PAT.
The terminal accepts the emailed secret through a hidden prompt.
A future browser setup form calls the same enrollment and login functions.
Browser delivery remains outside this increment.

WAMN validates the invitation's account binding, stores the password hash, and consumes the invitation atomically.
Successful consumption establishes control of the invited mailbox and credentials for that account.
It grants no organizational membership or operation permission.
The random, expiring, single-use secret follows [OWASP email verification guidance](https://cheatsheetseries.owasp.org/cheatsheets/Email_Validation_and_Verification_Cheat_Sheet.html#email-ownership-verification).

After enrollment, the person signs in through the normal email-and-password path.
Do not add a separate automatic-login mechanism after enrollment.
`wamn-identity` checks the account and authorized environment, then issues the session directly.
With one authorized environment, Receiving opens it directly.
With several authorized environments, the person chooses one.
Every session remains bound to the selected environment and its current instance.
Application requests use that session automatically.

An invitation can establish only the first password.
It cannot overwrite an enrolled account, change the selected principal, or grant membership.
Granting another membership never authorizes replacing a person's global password.
The existing one-email-per-principal rule remains controlling.
An existing-account conflict requires an authorized operator disposition; automatic linking is outside this increment.
Already enrolled people use their existing password.
Another invitation must neither overwrite that password nor create a duplicate identity.
Subsequent normal logins need no email step. Email serves enrollment and later recovery.

Password login reuses the existing membership, current environment instance, active tenant-user, role, and token-minting rules.
The terminal keeps the access token only in process memory and discards password input after the request.
It never retains the password to renew access automatically.
Token expiry and process restart require another login.
Logout clears local credentials and states that issued tokens retain their existing validity.

The initial fresh-only restriction is transitional, not the intended human experience.
Password-issued tokens carry no claim of recent authentication beyond the existing profile.
A refused mutation is never replayed automatically after login.
The person explicitly submits it again.
Do not teach ordinary users to generate PATs or retry a partially executed workflow with another credential.

## Safety from the first endpoint

TLS, secure hashing, bounded password work, throttling, and secret-free logs are prerequisites for exposing password endpoints.
They are not a later hardening phase.
Use the existing identity service, logging, database, and test infrastructure.
Do not add a revocation service, event bus, or generalized authentication framework.

The identity service stores salted Argon2id password hashes with versioned parameters.
Choose parameters against the supported deployment's memory and concurrency limits before exposing the endpoint.
The password policy requires at least 15 characters and permits at least 64.
The owner deferred common and compromised password screening.
It permits password managers and paste, with no composition rules or routine password expiration.
The length policy follows [NIST guidance](https://pages.nist.gov/800-63-4/sp800-63b.html#passwordver).

Bound request sizes and concurrent expensive password work.
Apply throttling to accounts, request sources, and total work, including across identity-service replicas.
Preserve operator authentication on principal, membership, and invitation administration.
Unknown-account and wrong-password responses must not reveal account existence through distinct status codes or obvious response paths.
Do not introduce permanent attacker-triggered lockout or an elaborate constant-time networking requirement.
These protections follow [OWASP authentication guidance](https://cheatsheetseries.owasp.org/cheatsheets/Authentication_Cheat_Sheet.html).

Use one password-verification path for login.
Renewal uses its own credential and never asks for or retains the password.
Use one invitation/reset credential implementation with explicit purposes; increment 1 implements only invitation behavior.
Bind each credential to its purpose and principal, store only its hash, and consume it atomically with the permitted password change.
Invitation expiry is 24 hours; later reset expiry is 15 minutes.
Successful enrollment or password replacement invalidates outstanding invitation/reset credentials that can otherwise overwrite that password.

Human passwords and invitation/session secrets never appear in command arguments, environment variables, configuration files, logs, or generated artifacts.
Email delivery uses the selected deployment transport without a provider framework.
A delivery failure must not be reported as a delivered invitation.
The mail transport and concrete password-work limits must be resolved during the first epic, before the affected endpoints ship.
Use existing operational logging and retention controls without adding an authentication-log subsystem.

## Increment 2: everyday account use

Reassess increment 1 before filing this epic's implementation issues.
This increment adds renewal, server-side logout, password reset, and the operational recovery procedure.
It introduces revocable login records in the existing identity database, explicitly changing the earlier no-session-state design.
It does not create another identity authority.

A login record binds the principal, exact environment audience, authentication time, absolute expiry, and renewal-credential family.
A family is the sequence of replacement credentials for one login.
Store only hashes of opaque renewal credentials.
Every issue and renewal requires current principal status, membership, environment instance, tenant-user status, and assigned roles.
Keep access and renewal credentials only in terminal process memory.

The absolute deadline is eight hours after full interactive login.
Renewal expires after 30 minutes without successful renewal and never moves the absolute deadline or authentication time.
Clients renew during active use, not through an unattended timer.
Renewal activity does not establish physical user presence.
Cap every access token at the earlier of its normal expiry and the login's absolute deadline, with the existing clock tolerance applied separately.

Each successful renewal atomically consumes its credential and returns one replacement and a new access token.
Reusing a consumed credential revokes its family.
Clients serialize renewals; a lost replacement response requires another login.
Do not add a retry grace period.
Retain consumed-credential evidence through family expiry, then remove obsolete records through bounded cleanup.
This follows the rotation approach in [RFC 9700](https://www.rfc-editor.org/rfc/rfc9700.html#section-4.14.2).

Logout revokes the current family; logout-all and principal disablement revoke all families for the person.
Increment 2 alone leaves issued tokens valid until expiry. Increment 3 adds current revocation checks at admission.
If the issuer is unreachable, clear local credentials and report that server-side revocation was not confirmed.
Do not claim immediate revocation before increment 3 is deployed.

Password recovery uses an emailed single-use secret entered through a hidden terminal prompt.
Reset completion replaces the password, invalidates outstanding invitation/reset secrets, and revokes every renewal family for the person.
Notify the person after reset and require ordinary login instead of automatically opening a session.
This follows [OWASP recovery guidance](https://cheatsheetseries.owasp.org/cheatsheets/Forgot_Password_Cheat_Sheet.html).
If a person loses mailbox access, an authorized administrator updates the email on the existing human principal.
The person then uses normal password recovery at the replacement address.
There is no separate trusted-contact channel. The administrator owns approval of the email correction.
The [operator procedure](../operations/deployment.md#mailbox-loss-recovery) defines the transaction and remaining access-token limitation.
Existing PATs retain their explicit revocation path; password reset does not silently revoke them.

Implement revocation and issuance ordering with ordinary transactions in the existing identity database.
Concurrent renewal cannot escape logout or reset as a usable successor credential.
An in-flight login using the old password cannot create a surviving renewal family after password reset commits.
Increment 3 extends these revocations to issued tokens at the next admission.
Test these races against real PostgreSQL when renewal and reset land, not as a prerequisite for increment 1.

Self-service email changes, general account linking, persistent terminal credentials, and renewal retry grace remain outside this increment.

## Increment 3: renewable sessions for all permitted operations

Reassess the preceding epics before planning this work.
The owner rejects operation-specific password reauthentication, regardless of the operation.
A person uses the existing renewable session for every permitted application operation.
PATs remain supported for separately provisioned clients. Human workflows never require them.

Access-token expiry, renewal, and permission checks have separate purposes.
An access token supplies signed caller and environment evidence for a bounded lifetime.
Renewal uses a single-use credential to obtain replacement access without another password prompt.
The issuer checks current account, membership, environment, tenant-user status, and assigned roles before issuing that replacement.
Every operation still requires its normal permissions, including nested calls under the original caller.
Neither token acceptance nor renewal grants additional permissions.

Do not add a five-minute password window, an authentication-time claim, or a password-confirmation endpoint.
Do not treat successful renewal as evidence of physical user presence.
Keep the eight-hour absolute login deadline and the 30-minute renewal inactivity limit.
A failed or expired login requires normal login again. An operation classification does not trigger another password prompt.
The client never automatically replays an application mutation after a refusal or new login.

The owner selects current server-side validity checks for every human session admission.
Reuse the existing identity reader and login records. Do not cache an active-session result.
Signed tokens identify their password login or the source PAT of an optional exchange.
The host checks the record, principal status, and environment membership before admitting a new request.
It intersects signed roles with current assignments and reads current permissions for the active tenant user.
A revoked, expired, missing, or mismatched authority refuses admission.
Unavailable revocation state also refuses admission. The client never falls back to a PAT.

Logout revokes one password login. Logout-all, reset, and account disablement revoke all password logins for that person.
PAT exchange remains bound to its source PAT, including that PAT's revocation and expiry.
Password reset does not revoke independently provisioned PATs or their exchanged sessions.
Revocation blocks new admissions after the change commits. It does not undo work already admitted.
Nested calls retain the admitted caller and continue normal operation permission checks.
No extra password confirmation is required.

Coordinate host enforcement, direct route admission, client credential selection, generated clients, and their tests.
Remove obsolete PAT-only guidance and password challenges from the proposed design.
Preserve caller identity, private-handler boundaries, exact environment binding, and the existing PAT path.
Add the required signed `authority` claim and update issuer, verifier, and clients together.
Reject old tokens without that claim and require login again after deployment.
The controlled deployment permits coordinated restarts and login again. Do not build a prolonged compatibility rollout.

## OIDC and later methods

OIDC remains a planned future epic, without detailed implementation issues now.
It adds an external login adapter within `wamn-identity` and uses the same principal, membership, and session authority.
Reassess the completed password implementation before selecting its provider and client flow.
A terminal can use the system browser without requiring a browser application, as described in [RFC 8252](https://www.rfc-editor.org/rfc/rfc8252.html).

Map an approved provider's issuer and subject to the canonical principal.
Never link accounts using email alone, even if the provider marks it verified.
The stable identifier follows [OpenID Connect](https://openid.net/specs/openid-connect-core-1_0.html#ClaimStability).
Provider roles and groups do not automatically grant WAMN membership or permissions.
Automatic account creation, directory synchronization, and immediate upstream disablement detection are separate decisions.

Use a maintained protocol library and authorization code flow with PKCE, which binds code exchange to the initiating client.
Establish trusted issuer configuration and certificate roots explicitly.
Before implementation, define upstream disablement limits and the external login lifecycle.
A silent provider session is not automatically a new interactive login.
The [federation plan](identity.md) retains these boundaries.

Shared stations, passkeys, browser application delivery, SAML, and device certificates remain outside the three increments.
A station authenticates as a service; an operator authenticates as a person.
Station credentials alone cannot create human sessions, and badge identifiers alone do not establish authentication.
Future station work must define attribution, operator switching, idle locking, and isolation of earlier requests and responses.
No earlier request can acquire the next operator's authority.

## Acceptance by increment

Use existing tests, controlled clocks for time rules, and real PostgreSQL for transaction behavior.
Do not add a testing framework or broad benchmark campaign.

Increment 1 must demonstrate:

- Operator invitation, password establishment, terminal login, and a real authorized Receiving operation without a human PAT.
- Direct password-to-session issuance, with no hidden PAT creation or required exchange.
- Both credentials accepted on the existing authenticated routes, with no added permissions or exposed private event handlers.
- Direct opening of the sole authorized environment, or explicit choice among several authorized environments.
- The same canonical person identifier as optional PAT access, with existing membership and permission boundaries preserved.
- Expiring, correctly bound invitation secrets that cannot overwrite enrolled credentials or succeed twice, including concurrent consumption.
- Existing users sign in without a new email step, duplicate identity, or password replacement.
- TLS, bounded password work, throttling, protected operator endpoints, and public failure behavior that does not trivially enumerate accounts.
- No secrets in logs or persistent terminal state, re-login on token expiry, local-only logout, and unchanged PAT-only fresh operations.

Increment 2 must demonstrate:

- Last-minute renewal cannot extend access beyond absolute expiry plus tolerance; inactivity and replay rules hold.
- Concurrent renewal, renewal versus logout/reset, and old-password login versus reset follow the defined transaction ordering.
- Expired or consumed reset secrets refuse; successful reset invalidates other secrets, stops renewal, and notifies the person.
- Unreachable logout clears local credentials without falsely claiming server revocation.

Increment 3 must demonstrate:

- Sessions and PATs reach every permitted application operation without operation-specific password prompts.
- Direct and nested calls preserve the original caller, environment binding, and normal permission refusals.
- Logout, reset, disablement, membership removal, and source-PAT revocation affect the next admission using an already issued token.
- Missing authority and unavailable revocation state refuse admission without a cached approval.
- Renewal preserves absolute expiry and inactivity limits, and no authentication failure automatically replays an application mutation.
- Existing PAT access and private-handler boundaries remain intact.

## Remaining decisions and ownership

Password-first and the three-increment order are settled.
Resend is the initial email transport, with sender `WAMN <d@wamn.dev>`.
The [execution architecture](../architecture/execution.md#caller-authentication) records the implemented password protections and their limits.
Operation-specific password reauthentication is rejected.
Immediate revocation checks for new human session admissions are approved. No account-freshness decision remains open.
The mailbox-loss procedure belongs to increment 2; the concrete OIDC provider and upstream limits belong to its future epic.

The Beads epics carry this plan's scope and reassessment rule:

- `wamn-a045`: Invited password login into Receiving.
- `wamn-6uby`: Everyday account use and session lifecycle.
- `wamn-k3mu`: Renewable sessions for all permitted application operations.
- `wamn-jpx2`: Future OIDC login through the existing issuer.

Only increment 1 has implementation children at initial planning.
Later epic starts require another planning round informed by the preceding results.
