# Human identity and login

Review draft, 2026-09-17. Specification task: `wamn-mlsg`.
This plan replaces `identity-notes.md` and proposes human login without mandatory personal access tokens (PATs).
It does not authorize implementation or change the current authentication contract.
The review questions below require owner decisions before implementation tasks are scheduled.

## Purpose and boundaries

A person can sign in, enter an authorized environment, continue working, and perform sensitive operations without creating a PAT.
Every login method resolves to the existing canonical principal, the stable identity used across authentication methods.
WAMN remains the session issuer, the service that signs access tokens.
Applications receive authenticated callers through the existing host boundary.

PATs remain available for integrations, scripts, stations, and optional developer access.
This plan does not remove the current PAT path or silently convert human credentials into PATs.
Machine authentication remains separate from human authentication.

## Current implementation

The [execution architecture](../architecture/execution.md#caller-authentication) defines current behavior and its limits.
The existing `wamn-identity` service exchanges a human PAT through `POST /session` after establishing membership and environment roles.
Service PATs cannot mint human sessions.
The host authenticates Ed25519 signatures using the configured issuer's public keys and reads current role permissions for each request.

Access tokens last at most 900 seconds, with 30 seconds of time tolerance.
Cached public keys remain usable for at most 300 seconds.
The audience identifies the exact organization, project, environment, and environment instance.
The current client keeps access tokens in memory and uses its PAT to obtain another token.
There is no refresh token, login-session table, or access-token denylist.

Fresh-only operations currently require a PAT, including nested calls.
Membership removal, assigned-role removal, and principal disablement affect existing sessions through their remaining lifetime.
Changes to permissions granted by a role apply on the next request.
These rules remain in force until an approved replacement ships.

Human creation, explicit environment membership grants, and production tenant-user provisioning already exist.
They use `create-human`, `grant-project-env-membership`, and `reconcile-run-plane`.
The first human-login release must reuse these authorities rather than create another directory or membership model.

## Proposed first release

The first release provides invited email-and-password login, recovery, logout, session renewal, and recent authentication for sensitive operations.
The existing identity service owns these functions and their storage in the system database.
Application hosts hold no passwords, recovery secrets, or private signing keys.
Outbound email is a required dependency for invitations and recovery.

The first client is the existing operator terminal.
Browser login is a later delivery unless the owner selects it during review.
The terminal stores access and renewal credentials only in process memory for the first release.
Closing the process requires another login.
Passwords never enter command arguments, environment variables, configuration files, or logs.

## Identity and access

Authentication establishes a person but grants no environment membership by itself.
Every token issue and renewal requires an active principal, explicit membership, the current environment instance, an active tenant-user row, and current assigned roles.
The host continues to resolve operation permissions from those roles.
Changing environments requires a separately authorized token with that environment's exact audience.

The same person retains the same identifier through password login, SSO, and optional PAT use.
Email remains a contact and login attribute, not the immutable identity key.
The existing rule that one email identifies one platform principal remains controlling.
Invitation acceptance must not create a duplicate principal or silently attach credentials to an existing account.
An email conflict requires authenticated account recovery or an authorized linking process.

An authorized platform operator creates or selects the principal and grants membership through existing operations.
The invitation establishes the person's password and control of the invited address.
It does not grant additional membership or roles.
Public self-registration and organization-admin delegation are outside the first release.

## Passwords and recovery

The identity service stores salted Argon2id password hashes with versioned parameters.
Implementation must choose parameters against the supported deployment's memory and concurrency limits.
The initial policy requires at least 15 characters, permits at least 64, and rejects common or compromised passwords.
It permits password managers and paste, and imposes no composition rules or routine password expiration.
These password rules follow [NIST guidance](https://pages.nist.gov/800-63-4/sp800-63b.html#passwordver).

Login, invitation, and recovery endpoints apply bounded request sizes and coordinated rate limits across service instances.
Limits cover the account, request source, and total expensive password work.
Unknown accounts receive equivalent public responses without exposing account existence.
Failure handling must not provide an attacker with a permanent account-lockout operation.
Exact rate limits and hash parameters require implementation review before deployment.

Invitation and reset credentials are random, short-lived, single-use secrets stored only as hashes.
Consumption and password replacement occur atomically.
Proposed expiry is 24 hours for invitations and 15 minutes for password reset links.
Reset completion revokes all renewal credentials for that person and requires a new login.
It does not silently create an authenticated session.
Existing access tokens retain the bounded validity described below.

Email changes require recent authentication, verification of the new address, and notification to the old address.
The principal identifier and memberships remain unchanged.
Loss of the recovery mailbox requires an explicitly authorized support process, not an undocumented bypass.
Automatic PAT revocation during password recovery is outside this proposal; operators retain the existing explicit revocation path.

## Session renewal

This plan proposes a revocable login record and an opaque renewal credential, separate from the signed access token.
This is new identity state and explicitly changes the earlier no-session-database design.
The record binds the principal, environment audience, authentication time, expiry, and renewal-credential family.
A family is the sequence of replacement credentials for one login.
The database stores credential hashes, never their bearer values.

Proposed limits are eight hours from interactive login and 30 minutes without successful renewal.
Renewal never extends the eight-hour limit or updates the interactive authentication time.
Clients renew only during active use, not through an unattended timer that defeats the inactivity limit.
This measures renewal activity, not proof of physical user presence.

Each successful renewal atomically consumes its credential and returns a replacement plus a new access token.
Reusing a consumed credential revokes its family.
Clients serialize renewals; a lost replacement response requires a new login in the initial design.
This favors simple replay refusal over a retry grace period.
Rotation and replay detection follow the approach described in [RFC 9700](https://www.rfc-editor.org/rfc/rfc9700.html#section-4.14.2).

Logout revokes the current family and clears local credentials.
Logout-all and principal disablement revoke every family for that person.
Already issued access tokens remain usable for at most their remaining 900-second lifetime plus 30-second tolerance.
Immediate access-token revocation is not promised.
Credential records require bounded cleanup without removing consumed-credential evidence before its family's absolute expiry.

## Recent authentication

This plan proposes extending fresh-only admission to accept recent human authentication as an alternative to the existing PAT path.
It does not silently weaken existing operation declarations.
A long-lived PAT proves possession of that credential, not recent interactive authentication by a person.

The proposed human freshness window is five minutes after successful interactive authentication.
The issuer records that time from its own authentication process, never from a client-supplied timestamp.
Renewal preserves that time and cannot make an old login recent.
The host enforces freshness at the registered-operation boundary, including nested calls under the original caller.
Freshness does not grant a permission or replace normal authorization.

The token contract needs a reviewed extension carrying authenticated evidence and its time.
Legacy tokens without that evidence remain valid for ordinary operations but remain insufficient for fresh-only operations.
Deployment must install compatible verifiers before the issuer emits the new profile.
Exact claims, profile compatibility, and refusal literals require a separate contract review before code changes.

When evidence is too old, the client requests another login challenge.
The challenge binds the same principal and intended environment.
Success supplies recent evidence but does not replay a previously refused mutation automatically.
The person explicitly submits the operation again.
No new per-operation identity lookup is proposed, so recent evidence retains its own bounded revocation window, including clock tolerance.

## External identity providers

OIDC is a later login adapter inside `wamn-identity`, not a second identity authority.
OIDC connects WAMN to an external authentication provider.
The provider authenticates the person.
WAMN establishes membership and issues its own environment-scoped token.
Provider roles and groups do not automatically become application permissions.

An approved connection maps the provider's issuer and subject to an existing canonical principal.
Email alone must never link accounts, including when the provider marks it verified.
Issuer and subject form the stable identifier described by [OpenID Connect](https://openid.net/specs/openid-connect-core-1_0.html#ClaimStability).
Automatic principal creation on first login requires a separate owner decision.
The initial proposal requires invited or explicitly linked accounts.

The adapter uses authorization code flow with PKCE, which binds code exchange to the initiating client.
It enforces issuer, audience, redirect target, state, nonce, signature, and expiry requirements through a maintained protocol library.
The configured connection establishes the trusted issuer and certificate roots.
SSO does not imply directory synchronization, provisioning, or immediate upstream disablement detection.

Federation design must define upstream revalidation before renewal and how recent authentication evidence is established.
A silent provider session cannot be assumed to represent a new interactive login.
Federation cannot ship until its upstream disablement window is explicit and approved.
The [existing federation plan](identity.md) remains subject to these proposed review decisions.

## Shared stations and later methods

A station authenticates as a service principal; its operator authenticates as a person.
The station credential alone cannot mint or renew a human session.
A badge identifier selects a person but is not automatically an authenticator.
PIN or badge authentication needs a separate policy for guessing resistance, enrollment, and recovery.

Application row history continues to identify the acting person.
Station attribution requires a separately authenticated context and audit contract.
The station design must define operator switching, idle locking, credential clearing, and requests admitted before a switch.
An earlier request must never acquire the next operator's authority or deliver its response into the next operator's screen.
This work is outside the first release.

Passkeys remain a later authentication method with explicit enrollment and recovery design.
SAML and device certificates require a concrete customer need.
OAuth client credentials remain a separate machine-token flow, not a rename of PATs.
None requires another principal directory or permission model.

## Audit and acceptance

Authentication records include principal identifiers, outcomes, and permitted operational context, never passwords or bearer secrets.
The identity service records enrollment, login, recovery, linking, renewal replay, logout, and revocation events.
Record writes continue to stamp the canonical actor identifier.
Authentication logs need access controls and a retention decision before deployment.

The first release must demonstrate these behaviors with focused tests:

- An invited person logs in and works without any PAT.
- Missing membership, inactive users, and stale environment instances refuse token issue and renewal.
- Renewal rotates credentials atomically, rejects replay, and preserves the original authentication time and absolute expiry.
- Recovery tokens expire and cannot be consumed twice, including concurrent attempts.
- Logout and recovery stop renewal while access-token validity stays within the documented bound.
- Recent authentication permits an authorized fresh-only operation, while stale or absent evidence refuses direct and nested calls.
- A challenge cannot change the principal, widen the audience, or replay a refused mutation.
- Permission changes affect the next request; role and membership changes follow the documented token lifetime.
- Existing PAT clients continue to work, and service PATs cannot establish a human session.
- Credentials remain absent from logs, generated artifacts, and persistent terminal state.

Browser delivery additionally requires protected credential storage and protection against forged cross-site requests.
It must not persist renewal credentials in JavaScript-readable browser storage.
Those requirements do not authorize building a browser client in the first release.

## Review questions

1. Approve invited password login in the operator terminal as the first human-login release, with no mandatory human PAT?
2. Approve revocable renewal records, an eight-hour absolute limit, a 30-minute renewal inactivity limit, and login after process restart?
3. Approve recent human authentication within five minutes as an alternative to PATs for fresh-only operations?
4. Accept remaining access-token validity after logout or recovery, bounded to 15 minutes plus 30 seconds?
5. Approve operator-issued invitations, the proposed password and recovery rules, and an explicitly authorized mailbox-loss recovery process?
6. Which outbound email service and authentication-log retention policy will the deployment use?
7. Keep federation, automatic account creation, shared-station login, passkeys, and browser delivery outside the first release?

Beads owns implementation status and scheduling.
After review, approved work must receive implementation issues before coding starts.
Closed historical issues `wamn-117` and `wamn-0h0g.9` do not supply an active implementation owner.
