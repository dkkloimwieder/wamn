# External identity providers

Federated login connects an external identity provider to WAMN.
The deferred work belongs to `wamn-117`.
Current PAT issuance, session exchange, request authorization, and revocation limits belong in [execution](../architecture/execution.md).

The existing `wamn-identity` service will own the external login adapter.
Customer identity providers and outsourced login services must map approved external subjects to existing canonical principals.
They must use the same environment memberships, roles, and session profile.
This work adds no separate principal store, role model, session database, or identity service.

External authentication and session minting remain separate steps.
An external identity does not establish membership in an environment by itself.
The adapter must establish the canonical principal before the existing authority evaluates that membership.

The design must state how upstream account disablement affects an issued session.
Any configurable lifetime or freshness limits belong to that design.
The current 900-second lifetime, 30-second tolerance, and 300-second key freshness limit remain unchanged until a separate decision changes them.

The external connection path must establish its issuer and trusted certificate roots explicitly.
A browser login must not bypass the current request verifier or fresh-only operation restriction.
Federation remains deferred and does not block the existing PAT and session paths.
