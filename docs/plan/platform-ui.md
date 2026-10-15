# Platform UI

**Design baseline:** `main` at `3a81dcfec`  
**Repository when Revision 9 was prepared:** `main` at `8a5182be3`  
**Revision:** 9, after the 2026-09-29 reviews and owner rulings  
**Status:** architecture accepted. Platform UI implementation remains gated on the accepted and completed `kind` → `type` precursor.

Sections 0, 1 and 3 to 9 are history: [platform UI history](../history/platform-ui-history.md). Section 5 is superseded by [platform deploy](platform-deploy.md) §12.

## 2. Fixed rules

### 2.1 Common UI and authorization

Every admin function is an operation in a contract and is served through the router.

Generated operation components remain the primitive UI. Composite screens may be hand-authored over those generated operations; they do not bypass the operation contracts.

Server authorization is authoritative. The UI may hide unavailable actions, but the router or host handler always enforces authority.

Every admin write runs in the host. `wamn_app` remains SELECT-only on:

```text
app_system.users
app_system.roles
app_system.user_roles
app_system.permissions
```

No guest component writes an authorization relation. No guest component receives a `wamn_system` connection.

### 2.2 Application roles

`admin` is the only built-in application role.

It has no permission rows. A current `admin` holder holds **every operation the current release serves, including the fixed application-administration operations**.

Every other role:

- is authored per tenant;
- starts empty;
- is identified by a role slug;
- contains stable operation references, not package-version-sealed operation ids;
- may contain several directly selected permissions;
- may be held alongside other roles.

A user may hold no application role.

`operator` is removed.

### 2.3 Permission identity

A stored permission reference is:

```text
<package>:<interface>/<operation>
```

It is the versionless reference returned by `sealed_operation_reference`.

Package versions remain in the serving release and sealed runtime operation ids. They do not live in authored grant identity.

The request check is: the caller holds the entry operation. Nothing derives into the ACL, on either side ([the ACL ruling](trx-pkg-plan.md#mechanism) (owner ruling 2026-10-11)). What the entry's code calls runs under the entry's authority.

```text
current user roles
    ↓
stored references (the role's rows)
    ↓
current serving release
    ↓
current sealed operation ids
    ↓
AuthenticatedCaller
    ↓
caller holds entry
```

A stored reference that the current release does not serve grants nothing.

### 2.4 Permission grant and revoke

`permission.grant X` requires an existing authored role and an operation `X` that the current serving release serves. It inserts one row. The operation is idempotent.

`permission.revoke X` deletes that row.

No row is derived from another, so no revoke keeps or refuses a derived row.

### 2.5 Non-grantable operations

Current behavior: the ACL-editing operations (`role.*`, `permission.*`, `user_role.*`, `user.*`) are reserved to `admin`, and Epic C of [trx-pkg-plan.md](trx-pkg-plan.md#spike-and-acceptance) does not change that. Direction, not shipped (owner ruling 2026-10-11): grantable administration, where an ACL-editing operation can confer only what its caller already holds, with `admin` treated as the full set. It is a follow-up after Epic C, rescoped then ([trx-pkg-plan.md](trx-pkg-plan.md#amendments-to-platform-uimd)). Until it lands, the rules below apply.

The fixed application-administration operations are **not grantable** to authored roles.

`permission.grant` refuses them.

`admin` receives them because `admin` holds every operation the release serves.

`permission.mine` is the exception: it is readable by any authenticated application session and is not itself an authored permission.

### 2.6 Grant timing

For a user session:

- permission-row changes take effect on the next request;
- role revocation takes effect on the next request because current `user_roles` still intersects the signed role names;
- role grant takes effect at renewal or new sign-in because the new role name is not in the existing token.

For a PAT caller, current roles are read per request, so grant and revoke both take effect on the next request.

### 2.7 Org membership and no-access users

A principal remains global, but org membership is explicit.

Add:

```text
identity.org_memberships
    principal_id
    org
    status        active | inactive
```

with primary key `(principal_id, org)`.

An invitation creates or reuses the global principal and creates an active org membership.

An active org member may have:

- no project membership;
- project/environment membership but no role;
- application roles;
- `project-admin`;
- `org-admin`.

A valid account with no effective audience is **not an authentication error**.

After password authentication, `/password/environments` returns only audiences in which the user currently has effective access. If that list is empty, the shell renders only:

> **No access has been granted.**

No project, environment, package, screen or administration metadata is shown. No application session is minted.

### 2.8 Materialized administrative hierarchy

There is no implied authorization.

An `org-admin` grant creates actual lower-level grants:

```text
identity.org_roles: org-admin
        ↓
identity.project_roles: project-admin
        ↓
identity.project_env_memberships
        ↓
app_system.user_roles: admin
```

for every existing project and environment in the org.

A `project-admin` grant creates actual lower-level grants:

```text
identity.project_roles: project-admin
        ↓
identity.project_env_memberships
        ↓
app_system.user_roles: admin
```

for every existing environment of that project.

New projects and environments materialize the corresponding grants for already-existing higher-level admins.

`reconcile-run-plane` repairs these stored projections.

Authorization never asks:

```text
is this user an org admin?
→ pretend they are an application admin
```

The lower grant row exists.

### 2.9 Downward revocation

Revocation flows downward. It never flows upward.

Revoking `org-admin` removes:

```text
org-admin
→ project-admin in that org
→ application admin in that org
```

It does not revoke ordinary authored application roles or ordinary project/environment membership.

Revoking `project-admin` removes:

```text
project-admin
→ application admin in that project
```

It does not revoke org membership, ordinary environment membership or authored application roles.

Because lower administrative grants are materialized, a lower-level revoke is refused while a covering higher-level administrative role still exists:

```text
org-admin present
→ project-admin cannot be independently revoked

org-admin or project-admin present
→ covered application admin cannot be independently revoked
```

Revoke at the scope that owns the authority.

Downward administrative revocation is deliberately destructive for the subordinate administrative grant. If a lower-level administrative grant should remain independently, it is granted again explicitly after the higher-level revoke.

### 2.10 Scoped user deactivation

Deactivating a user at the org level removes all authority below that org:

```text
application users / roles
project-env memberships
project roles
org roles
```

The global principal is not disabled and access in another org is untouched.

This direction is one-way:

```text
org deactivation
→ project + application access removed

project/environment removal
↛ org deactivation
```

Reactivation restores only the active org membership. It does **not** restore previous project memberships or roles.

The user sees:

> **No access has been granted.**

until access is explicitly granted again.

Revocation is performed from the leaves upward. A failed partial revoke may temporarily remove too much access, never retain lower application access after the higher authority has been reported successfully revoked.

### 2.11 Provisioning boundary

Ordinary org, project and application administration is handled through these contracts.

Infrastructure provisioning remains outside the router's credentials.

The router never receives Kubernetes provisioning authority.

Lifecycle requests that require infrastructure write a provisioning saga. A dedicated control worker executes it with narrowly scoped provisioning credentials.
