# Platform UI

**Design baseline:** `main` at `3a81dcfec`  
**Repository when Revision 9 was prepared:** `main` at `8a5182be3`  
**Revision:** 9, after the 2026-09-29 reviews and owner rulings  
**Status:** architecture accepted. Platform UI implementation remains gated on the accepted and completed `kind` → `type` precursor.

Sections 0, 1 and 3 to 9 are history: [platform UI history](../history/platform-ui-history.md). Section 5 is superseded by [environment lifecycle](../architecture/overview.md#environment-lifecycle).

## 2. Fixed rules

### 2.1 Common UI and authorization

See [caller authentication](../architecture/execution.md#caller-authentication).

### 2.2 Application roles

See [caller authentication](../architecture/execution.md#caller-authentication).

### 2.3 Permission identity

See [caller authentication](../architecture/execution.md#caller-authentication).

### 2.4 Permission grant and revoke

See [caller authentication](../architecture/execution.md#caller-authentication).

### 2.5 Non-grantable operations

See [current caller authorization](../architecture/execution.md#caller-authentication).

Direction, not shipped (owner ruling 2026-10-11): grantable administration, where an ACL-editing operation can confer only what its caller already holds, with `admin` treated as the full set. It is a follow-up after Epic C, rescoped then ([trx-pkg-plan.md](trx-pkg-plan.md#amendments-to-platform-uimd)).

### 2.6 Grant timing

See [caller authentication](../architecture/execution.md#caller-authentication).

### 2.7 Org membership and no-access users

See [organization membership and administration](../architecture/overview.md#organization-membership-and-administration).

### 2.8 Materialized administrative hierarchy

See [organization membership and administration](../architecture/overview.md#organization-membership-and-administration).

### 2.9 Downward revocation

See [organization membership and administration](../architecture/overview.md#organization-membership-and-administration).

### 2.10 Scoped user deactivation

See [organization membership and administration](../architecture/overview.md#organization-membership-and-administration).

### 2.11 Provisioning boundary

See [environment lifecycle](../architecture/overview.md#environment-lifecycle).
