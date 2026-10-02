# Platform UI

**Design baseline:** `main` at `3a81dcfec`  
**Repository when Revision 9 was prepared:** `main` at `8a5182be3`  
**Revision:** 9, after the 2026-09-29 reviews and owner rulings  
**Status:** architecture accepted. Platform UI implementation remains gated on the accepted and completed `kind` → `type` precursor.

Section 3 records the reviewed `3a81dcfec` baseline deliberately. `main` has moved since, including `wamn-4afx` moving operation transaction ownership into the host. Issue 1 remeasures Section 3 against the `main` it starts from and commits that factual delta before changing behavior.

## 0. Required precursor — `kind` → `type`

The platform-wide `kind` → `type` migration is a **separate contract-migration epic and a hard dependency of this work**. It lands before Platform UI implementation begins.

The naming rule is platform-wide. Product vocabulary, Rust names, serialized fields and WAMN-owned schema columns use `type`, not `kind`, including the current `OperationKind`, `AttachmentKind`, `CredentialKind`, `FailureKind`, `principal_kind`, `placement_kind` and `*ErrorKind` families where WAMN owns the name.

This is a real contract migration, not a textual cleanup. `kind` occurs in authored and generated serialized surfaces, including `wamn.json`, generated source maps and attachment definitions. Renaming it changes canonical bytes and may therefore change definition hashes, artifact digests and package/release contents.

That cost is accepted.

The migration does **not** relax immutability or pinned-digest rules:

- an already sealed package coordinate is never rewritten;
- an immutable artifact is never replaced under its existing digest;
- old releases retain their old bytes and identities;
- affected applications are regenerated and republished under new immutable package/artifact/release identities as required;
- fixtures and frozen contract evidence are regenerated for the new vocabulary.

A disposable `ctl dev` target is not a sealed coordinate, so its local-target exception is outside these rules.

Before an agent implements the precursor, that epic gets its own accepted specification in the same form as this one. It must settle at least:

- every authored, generated, wire and persisted WAMN-owned surface that carries `kind`;
- which serialized changes move definition hashes, package/artifact digests or release bytes;
- which package versions must change;
- base/overlay regeneration order;
- the republish order for installed wamn-dev environments;
- database-column migrations;
- compatibility with already sealed releases and artifacts;
- whether the `*ErrorKind` Rust families migrate in the same epic or in a separately bounded step;
- the proofs that no immutable coordinate or artifact is rewritten.

The cost and ordering are accepted. The detailed migration shape is not delegated to an agent until that precursor specification is reviewed.

The remainder of this specification assumes the post-migration `type` vocabulary.

---

## 1. Goal

One administration surface at three levels, using the same contract, router, client IR, screen plan and shell architecture as applications:

- **Org** — users, projects, org admins, invitations.
- **Project** — environments, members, project admins.
- **Application** — users, roles, the operations each role holds, and the roles each user holds.

At the application level:

> `admin` is the one built-in role and means all current application authority. Every other role is authored data containing a selected set of stable operation references.

At the hierarchy above it:

> Org and project administrative authority is represented by **real stored grants at every level it covers**, not by an authorization rule inferred at call time.

---

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
- may hold additional permissions required by those selected operations;
- may be held alongside other roles.

A user may hold no application role.

`operator` is removed.

### 2.3 Permission identity and closure

A stored permission reference is:

```text
<package>:<interface>/<operation>
```

It is the versionless reference returned by `sealed_operation_reference`.

Package versions remain in the serving release and sealed runtime operation ids. They do not live in authored grant identity.

The existing router security invariant remains unchanged:

> **The caller must hold every operation the released call graph reaches.**

`authorize_released_operation` continues to check:

```text
entry operation
+
every permission folded into that released operation
```

The Platform UI permission model therefore materializes the required permission closure **in storage**, rather than synthesizing missing grants at request admission.

Conceptually one stored effective grant has:

```text
role
permission
required_by
```

where:

- `permission` is the effective stable operation reference the role holds;
- `required_by` is the directly selected root operation that requires it;
- `permission == required_by` means that permission was selected directly.

Example:

```text
grant X
→ X required_by X
→ Y required_by X
→ Z required_by X
```

If another selected operation `Q` also requires `Y`:

```text
Y required_by X
Y required_by Q
```

If `Y` itself is explicitly selected:

```text
Y required_by Y
```

Authorization reads the **distinct effective `permission` references** and resolves each one to its exact sealed operation id in the current serving release:

```text
current user roles
    ↓
distinct stored effective references
    ↓
current serving release
    ↓
current sealed operation ids
    ↓
AuthenticatedCaller
    ↓
authorize_released_operation
```

Admission does not add another permission closure.

The table therefore states what the caller is allowed to execute, while the router independently verifies the released call graph.

A stored reference that the current release does not serve grants nothing.

### 2.4 Permission grant and revoke

`permission.grant X`:

1. requires an existing authored role;
2. requires `X` to be a grantable operation in the current serving release;
3. reads `X`'s released permission closure;
4. writes `X required_by X`;
5. writes each required operation reference with `required_by X`.

The operation is idempotent.

`permission.revoke X` removes the direct selection `X required_by X` and every closure row owned by that selection (`required_by X`).

If another selected root still requires an affected permission, that permission remains effective through the other root and the reply says so.

Example:

```text
X selected
X requires Y
Y selected

revoke Y
→ remove Y required_by Y
→ preserve Y required_by X
→ reply: Y remains effective because X requires it

revoke X
→ remove X required_by X
→ remove Y required_by X
→ Y disappears unless another selected root still requires it
```

A revoke refuses only when the named permission is **not directly selected at all** and therefore has no `permission = required_by` row to remove.

Example:

```text
X selected
X requires Y
Y is not directly selected

revoke Y
→ refuse: Y is not directly granted; it is required by X
```

If several selected roots require it, the refusal names those roots.

This makes direct selection, effective authority, the administration grid and runtime authorization agree.

### 2.5 Non-grantable operations

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

---

## 3. Reviewed repository baseline

This section records the repository state reviewed at `main` `3a81dcfec`. It is a **design baseline, not a claim about current `main`**.

By Revision 9, `main` had advanced to `8a5182be3`, including `wamn-4afx` work that moved operation transaction ownership into the host. The mandatory `kind` → `type` precursor will move further contracts before Platform UI Issue 1 begins.

Issue 1 therefore remeasures this entire table against its starting `main` and commits the factual delta as its first commit, before making Platform UI behavior changes.

| Place | Reviewed baseline at `3a81dcfec` |
| --- | --- |
| Application authority | `app_system.users`, `roles`, `user_roles`, `permissions`. `wamn_app` has SELECT only. |
| Permission row | `permissions.permission` is text. Its only FK is `(tenant_id, role_name) → roles`. |
| Operation identity | `canonical_operation_identity` includes package version. `sealed_operation_reference` already removes the `@version`. |
| Built-in roles | `USER_ROLE_NAMES = [operator, admin]`. `apply-package` writes every public operation into both. |
| Role changes | `grant-role` / `revoke-role` accept only the two `USER_ROLE_NAMES`. |
| Application permission check | Session authorization intersects token roles with current `user_roles`; PAT authorization reads current roles. Both produce exact operation grants. |
| Call-graph authorization | Publish folds every operation reached by an export into `ServingComponentOperation.permissions`; `authorize_released_operation` requires the caller to hold every one. |
| Session target | A normal session target is one project-environment audience. |
| Serving route | A route names a component export. Application routes execute in a guest. |
| Operation type | The reviewed code calls it `OperationKind`; it is semantic: get, query, create, update, delete, command, projection, event handler. |
| Tenant | One project-env database may contain several packages of one effective application. |
| System identity | `identity.principals` is global. `identity.project_env_memberships` and `identity.project_roles` exist. There is no org-membership or org-role relation. |
| Project roles | Existing slugs include `project-author` and `project-admin`; meaning is attached by the management boundary. |
| Invitation | Invitation credentials are issued only for active users that have not yet enrolled a password. |
| Run-plane mirror | `reconcile-run-plane` creates application `users` rows from system identity but does not remove stale user rows. |
| Shell | Environment choice, invitation/recovery/reset, and application screens. No administration route. |
| Provisioning | Environment creation and release installation are CLI/runbook operations with infrastructure credentials. |
| Installed schema changes | No general installed-schema upgrade verb exists. Hand-applied wamn-dev statements are recorded in `docs/operations/gcp.md` §7 under `wamn-o8b9`. |

### 3.1 Remeasured at `4495e0d20` (issue 1, `wamn-a40n.1`)

Issue 1 starts from `main` `4495e0d20`, 163 commits after `3a81dcfec`. The kind → type migration (`wamn-ld93`) and `upgrade-schema` (`wamn-o8b9`) landed in that range. The table gives each row as measured there. Paths are relative to the repository root.

| Place | At `4495e0d20` | Change since `3a81dcfec` |
| --- | --- | --- |
| Application authority | The four tables are in `deploy/sql/app-schema.sql` (`users` 192–237, `roles` 246–272, `user_roles` 279–309, `permissions` 316–344). `wamn_app` has SELECT and a tenant policy on each. `wamn_platform` has a `FOR ALL` policy on each (231–234, 266–269, 303–306, 338–341) but no table write grant. Writers use the owner or a BYPASSRLS URL (`crates/control/lib/src/user_roles.rs:24`, `crates/control/provision/src/operation_grants.rs:33`). | None. The `users` column `kind` is now `type`. |
| Permission row | `permission text NOT NULL`. The key is `(tenant_id, role_name, permission)`, and the one FK is `(tenant_id, role_name) → roles` with `ON DELETE CASCADE` (`app-schema.sql:316–327`). `roles.name` has no CHECK. The only role-slug CHECK is on `identity.project_roles.role` (`deploy/sql/system-schema.sql:323–324`). | None. |
| Operation identity | `canonical_operation_identity` returns `<package>:<interface>/<operation>@<version>`, and `sealed_operation_reference` strips the `@version` (`crates/schema/generator/src/manifest.rs:2242–2270`). Stored grants use the versioned form (`operation_grants.rs:205–208`). | None. |
| Built-in roles | `OPERATOR_ROLE`, `ADMIN_ROLE` and `USER_ROLE_NAMES = [operator, admin]` (`crates/identity/project-state/src/lib.rs:245–252`). `apply-package` writes both roles and every public operation of the package into both (`operation_grants.rs:249–318`). | None. |
| Role changes | `grant-role` and `revoke-role` (`services/ctl/src/role_verbs.rs:38`) accept only `USER_ROLE_NAMES` (`user_roles.rs:100–104`). The dev loop grants `operator` through `grant_role_on` (`crates/control/lib/src/dev/environment.rs:559–586`). | None. |
| Application permission check | A session reads the permissions of its signed roles that current `user_roles` still holds (`crates/platform/runtime/src/plugins/wamn_postgres/claims.rs:34–41`). A PAT reads the permissions of every current role (`claims.rs:51–59`). Both yield exact grants in `AuthenticatedCaller.permissions` (`crates/platform/engine/src/flow_http_routing.rs:388–452`). | `CredentialKind` is now `CredentialType`. |
| Service PAT admission | A service PAT is admitted only if the principal holds `identity.project_roles.role` in `USER_ROLE_NAMES` (`crates/identity/platform/src/lib.rs:87–97`, `crates/platform/runtime/src/plugins/route_authentication.rs:290`). `provision-project-env` gives the operator service PAT the project role `operator` (`crates/control/lib/src/provision_project_env/pat_secrets.rs:40–46`). | Not in the reviewed table. Removing `operator` changes system rows too. |
| Call-graph authorization | Publish folds each reached operation into `ServingComponentOperation.permissions` (`crates/catalog/model/src/serving_manifest.rs:148–329`, `crates/control/lib/src/publish_release/components.rs:351`). `authorize_released_operation` requires each one (`crates/platform/engine/src/router_delivery.rs:537–575`). | None. |
| Session target | One audience `urn:wamn:project-env:<org>:<project>:<env>:<instance>` (`crates/control/provision/src/session_target.rs:44–56`). | None. |
| Serving route | `AttachmentTarget::Route { component, operation }` names one component export (`serving_manifest.rs:565–576`). No host-run route exists. The host owns the operation transaction, and the guest cannot begin, commit or roll back (`crates/platform/runtime/src/plugins/wamn_postgres/operation_transaction.rs:1–29`). | The transaction moved into the host (`wamn-4afx`). |
| Operation type | `OperationType` with get, query, create, update, delete, command, projection and event handler (`serving_manifest.rs:352–361`). Contracts carry `"type"`. | Renamed from `OperationKind` (`wamn-ld93`). |
| Tenant | `ServingRelease.packages` holds several packages (`serving_manifest.rs:133`). Grant reconciliation deletes only one package's rows (`operation_grants.rs:236–238`). | None. |
| System identity | `identity.principals` is global (`system-schema.sql:257–292`). `project_roles` (310–325) and `project_env_memberships` (578–596) exist. No org membership or org role relation exists. | The principal column `kind` is now `type`. |
| Project roles | `project-author` and `project-admin` get meaning at the management boundary (`services/scenario-worker/src/management.rs:100–128`). `provision-project-env` writes `project-author` for the management PAT and `operator` for the operator PAT (`pat_secrets.rs:33–46`). | None. |
| Invitation | Issued only for an active user without a password (`crates/identity/platform/src/password.rs:326–418`, `system-schema.sql:755–763`). | Refusals are typed `RefusalCause` values. `Invitation` returns `expires_at`. |
| Run-plane mirror | `reconcile-run-plane` inserts service and user `users` rows and never deletes or updates stale rows (`crates/control/lib/src/reconcile_run_plane.rs:404–424`, 570–684). | It now also records the fresh-install migrations (632–641). |
| Shell | Routes `/`, `/invite`, `/recover`, `/reset` and `/:aud` with application screens (`web/shell/src/shell.tsx:215–239`). No administration route. | A Suspense root and direct navigation to the first screen (`wamn-28n8`). |
| Provisioning | CLI verbs in `services/ctl/src/main.rs` and the runbook `docs/operations/gcp.md` §3.6–3.8. `provisioning.sagas` runs in process (`crates/control/provision/src/saga.rs`). No separate worker. | `upgrade-schema` added. |
| Installed schema changes | `wamn-ctl upgrade-schema` applies numbered files in one transaction each (`crates/control/lib/src/upgrade_schema.rs`). The files are `deploy/sql/migrations/system/0001` to `0003` and `project/0001`. | The verb exists (`wamn-o8b9`). The owner ruled on 2026-09-30 that issue 1 ships its installed-database change as `system/0004` and `project/0002`, not as hand statements. |

---

## 4. Design

## 4.1 Authored application roles

The existing application authority tables remain the storage model, with the permission relation extended to preserve direct-selection provenance.

Changes:

1. `admin` becomes the only built-in role.
2. `operator` disappears.
3. `admin` has no permission rows.
4. Authored permission rows contain stable operation references.
5. Effective permission rows retain the directly selected root in `required_by`.
6. Any existing role name may be assigned by `grant-role`.
7. Administration writes are host-owned.

Conceptual permission key:

```text
tenant_id
role_name
permission
required_by
```

The exact DDL is owned by Issue 1, but the invariant is fixed:

> More than one selected root may require the same permission, and removing one root must not remove authority still required by another.

CLI operations:

```text
wamn-ctl create-role
wamn-ctl delete-role
wamn-ctl grant-permission
wamn-ctl revoke-permission
wamn-ctl grant-role
wamn-ctl revoke-role
```

`admin` cannot be created or deleted.

Role names use the same canonical slug rule everywhere:

```text
lowercase letters
digits
inner hyphens
maximum 64 bytes
```

The application schema gains the corresponding CHECK so storage cannot contain a role name that identity later refuses.

### Release reconciliation

`apply-package` no longer owns complete permission reconciliation.

It may create the built-in `admin` row if absent, but it cannot know the complete serving set when a package has been removed.

A candidate effective serving release is reconciled **before activation**.

For every authored role:

1. read its directly selected roots (`permission = required_by`);
2. remove a selected root the candidate release no longer serves;
3. recompute each surviving root's permission closure from the candidate serving release;
4. insert newly required closure rows;
5. remove closure rows no longer required by that root.

If reconciliation fails, activation fails with that error.

Only after reconciliation succeeds may the candidate release activate.

Therefore:

```text
package version changes
→ direct selected reference unchanged
→ closure reconciled against candidate

operation dependency changes
→ required rows updated before activation

operation disappears
→ selected root and its required rows removed

whole package disappears
→ its selected roots and required rows removed

admin
→ no permission rows to migrate
```

There is no window where a newly activated operation calls another operation the role was not granted.

Acceptance:

- authored role survives package application;
- version bump preserves its selected roots;
- newly added call-graph dependency adds its required permission before activation;
- removed dependency removes its no-longer-required row;
- a shared dependency remains while another selected root requires it;
- removing a direct grant preserves authority still required through another root;
- attempting to revoke a dependency that was never directly selected refuses and names its requiring roots;
- removed operation loses its root and closure rows;
- removed package loses its root and closure rows;
- stale unknown references grant nothing;
- `admin` reaches every current application operation without permission rows;
- `authorize_released_operation` remains unchanged and continues to verify every exact sealed operation in the released call graph.

---

## 4.2 Host-run routes

Issue 2 (`wamn-a40n.2`) built this section; [execution](../architecture/execution.md#host-run-routes) holds the current behavior.

A serving route gains an execution target orthogonal to `OperationType`.

Conceptually:

```text
route.target =
    component { component, operation }
  | host      { handler }
```

`OperationType` remains semantic and unchanged by execution location.

A host-run route retains the normal:

- operation contract;
- request envelope;
- result and refusal shapes;
- request limits;
- CSRF treatment;
- client IR;
- screen plan;
- generated client/component behavior.

Only execution changes.

The router dispatches the operation to a fixed host handler instead of invoking a guest component.

A host-run write stamps the route's sealed operation id, `wamn-control:<noun>/<verb>@<version>`.

Control routes carry the package prefix in their path (`/wamn_control/…`) and attachment id (`wamn-control-…-http`), because every release serves them beside its application routes, which stay unprefixed.
Publish refuses that path prefix to an application.

The implementation uses the host transaction model present on the `main` Issue 1 starts from; it does not recreate a separate administration transaction path.

### Application host routes

The fixed application-administration contract is inserted into every application serving release as host-run routes.

Those handlers target that project-env database.

### Platform control routes

Org/project administration is not an application release.

The platform therefore has one immutable **control serving root**, built and versioned with the platform and loaded by the normal router stack independently of any application release.

It contains the `wamn_control` contracts and host route definitions, but:

- no application models;
- no component artifact;
- no project-env database;
- no guest connection.

This is the route source for the control UI.

---

## 4.3 Control authentication

Issue 2 built the control audience, discovery, login, renewal and `control.mine` for `project-admin`, and issue 3 added `org-admin`; [execution](../architecture/execution.md#control-sessions) holds the current behavior.

Control sessions are org-scoped.

The UI may label the destination simply **Control**, but the wire audience is unique per org, for example:

```text
urn:wamn:control:<org>
```

This preserves the existing session claim that carries one org and avoids making one token authoritative over every org of a principal.

Identity discovery returns a control audience only when the principal currently holds:

```text
org-admin in that org
or
project-admin in at least one project of that org
```

`project-author` and any other opaque project-role slug do **not** grant access to the Platform UI.

A control session carries no application roles.

Every control request rechecks current `wamn_system` authority:

```text
control session principal
+ token org
+ requested project when applicable
→ current org-admin/project-admin check
→ handler
```

The request cannot select another org than the token's org.

Revoking a control-plane role therefore takes effect on the next request.

### `control.mine`

One member-readable control operation returns the caller's current control authority.

Conceptually:

```text
org_admin: true | false

projects:
  - project: receiving
    project_admin: true
  - project: wms
    project_admin: true
```

It is available to every valid control session.

The shell uses it only for presentation:

```text
org-admin
→ org screens + all project administration screens

project-admin only
→ administration screens for the named projects only
```

Each actual control operation independently repeats its authoritative role check. `control.mine` never grants authority.

---

## 4.4 Org level

Issue 3 (`wamn-a40n.3`) builds the system-level part of this section.
Issue 4 (`wamn-a40n.6`) builds the application rows (`app_system.users` and `app_system.user_roles` in each environment's project database), through the administration logins of [application writes](#application-writes) (D's answer to [§9](#9-questions-for-the-owner) question 1, option A).

Add, in `wamn_system`:

```text
identity.org_memberships (
    principal_id,
    org,
    status        active | inactive
)

identity.org_roles (
    principal_id,
    org,
    role
)
```

Initial org role vocabulary:

```text
org-admin
```

`provision-org --owner-email <email>` is required.
It names an existing user principal and refuses an unknown email.
It writes that user's active org membership and `org-admin` row, stamped `wamn:provisioning` as every provisioning row is ([data access](../architecture/data-access.md#actors)).
An org provisioned before the flag gets its owner through `wamn-ctl invite` with `org-admin`.

### Control writes

The control host writes with the `control` family login, as the session user.
Each write transaction binds `app.user_id` to the caller's principal and `app.operation` to the route's sealed operation id.
The system identity tables record the actor only, so `app.operation` is bound but not stored.

The `control` family holds exactly these privileges in `wamn_system`:

```text
SELECT                          identity.principals, identity.password_logins
SELECT, INSERT, UPDATE, DELETE  identity.org_memberships, identity.org_roles,
                                identity.project_roles, identity.project_env_memberships
SELECT                          registry.projects, registry.project_envs
```

It holds nothing on `principals` beyond `SELECT`, nothing on password or invitation tokens, and nothing in any project database.
It holds nothing on `registry.orgs`, because the control host takes its org from `--org` and does not read that table.

### Application writes

The control host of an org holds the `wamn_administration` login of every environment in that org, and of no other org.
It is the login that the application host of the environment holds: one login per environment, the same family and the same privileges.
In each project database the login holds `SELECT`, `INSERT`, `UPDATE` and `DELETE` on `app_system.users`, `app_system.roles`, `app_system.user_roles` and `app_system.permissions`, and `SELECT` on `catalog.effective_release_heads`, and nothing else beside the history writes of those tables.
A grant reads the release head, so it knows whether the release it holds is the head (`wamn-a40n.9`).

The logins of an org are in one Secret, `wamn-control-administration-<org>`, with one key per environment named `<project>--<env>`.
The control host mounts the Secret as a volume and reads it at each write, so a new environment needs no restart.
`provision-project-env` adds the key of its environment with a patch, and each generation rotation patches the key again, so the rotation reaches both holders.
`delete-project-env` removes the key.

A write that reaches the application rows uses one transaction in each project database, after the system transaction for a grant and before it for a revoke or a deactivation ([§4.9](#49-consistency-and-convergence)).
The operation reports success only when every environment is done.
If an environment fails, the operation refuses and names the environments that completed.
`reconcile-run-plane` repairs an interrupted run.

The control host never creates a principal.
Identity creates or reuses the global user, issues the invitation credential and sends the mail, over the operator-certificate path that `wamn-ctl invite` uses.
The control host never holds the Resend key or a role that issues tokens.

### Org operations

Each org operation, except `control.mine`, requires a current `org-admin` row for the caller in the token's org.
The handler repeats that check in its write transaction.

```text
wamn-control:user/list@0.1.0
wamn-control:user/invite@0.1.0
wamn-control:user/activate@0.1.0
wamn-control:user/deactivate@0.1.0

wamn-control:project/list@0.1.0

wamn-control:org-admin/grant@0.1.0
wamn-control:org-admin/revoke@0.1.0

wamn-control:control/mine@0.1.0
```

### `user.list`

Returns active and inactive memberships of the org, with each principal's email and display name.

The global principal remains an identity fact. Membership status is org-local.

### `user.invite`

Input names:

- email
- display name
- optional project-env memberships
- optional `org-admin`
- optional `project-admin` grants.

All lists can be empty.
The invitation grants no authored application role.

The operation runs in three steps:

```text
identity: create or reuse the user by email     → principal id
control:  create or reactivate the org membership
          + write the requested system grants     (one transaction)
identity: issue the invitation and mail it        (only when no password credential exists)
```

The identity call that creates or reuses a user is idempotent on the email and returns the principal id.
Identity's database role gains `INSERT` on `identity.principals` for it.

#### New principal

Identity creates the user.
The invitation is issued and mailed.

#### Existing enrolled principal

The user already has a password credential.
Identity reuses it, and no invitation or informational email is sent.
This is the normal second-org case.

#### Existing unenrolled principal

The user exists but has no password credential.
Identity reuses it, and the invitation is issued and mailed.
Principal existence alone therefore does not suppress enrollment.

Only email and display name identify the global user. Org membership and access remain org-local.

An invitation with no effective role is valid. After enrollment the user sees:

> **No access has been granted.**

`wamn-ctl invite` accepts the same logical input and follows the same principal and enrollment rules.
It writes the membership and grants as its sibling `grant-project-env-membership` does: with `--system-database-url`, as `wamn_system`, stamped `wamn:provisioning`.
The operator path is provisioning, and the UI path is the `control` login as the session user.

An `org-admin` row is written one way only: `user.invite` and `wamn-ctl invite --org-admin` call the write of [`org_admin.grant`](#org_admingrant) in their transaction.

The invite's `project-admin` list and `wamn-ctl invite --project-admin <project>` call the write of [`project_admin.grant`](#project_admingrant): the row, then a membership in every environment of that project.

`user.invite` then writes the application rows of its grants, as each grant operation does.
`wamn-ctl invite` holds no administration login, so `reconcile-run-plane` writes the application rows of its grants.

### `user.deactivate`

Revokes from the leaves upward:

1. the user's `users` row in each environment of the org, which removes its role rows, one transaction per environment
2. then, in one system transaction, project-env memberships in the org, project roles in the org and org roles, and the org membership marked inactive.

It does not disable the global principal.

### `user.activate`

Marks the org membership active.

It restores no project, environment or role access.

### `org_admin.grant`

Requires an active org member.

Writes the actual `org-admin` row, then in the same transaction materializes:

```text
project-admin in every existing project of the org
membership in every existing environment of the org
```

Then it writes the user's `users` row and `admin` row in every existing environment of the org.

A later project/environment creation performs the same materialization for all current org admins.

### `org_admin.revoke`

Removes the user's `admin` row in every environment of the org first.
Then it removes `project-admin` rows throughout the org and the `org-admin` row, in one system transaction.

Ordinary memberships remain.

If lower administrative authority is still wanted after the revoke, it is explicitly granted again.

### Control admission

Control discovery, the control session's per-request check and `control.mine` accept `org-admin` beside `project-admin`.
Identity's database role gains `SELECT` on `identity.org_roles` for discovery.
`control.mine` reports `org_admin: true` for a holder, with every project of the org.

### Installed databases

System migration `0007` creates both tables and carries the grant changes of this section, as the rendered output of the provisioning builders.
`wamn-ctl upgrade-schema --system-database-url` applies it on an installed `wamn_system`.

---

## 4.5 Project level

Project authority uses the existing:

```text
identity.project_roles
```

with the existing administrative spelling:

```text
project-admin
```

Project operations:

```text
environment.list

member.list
member.grant
member.revoke

project_admin.grant
project_admin.revoke
```

### Membership

`member.grant` requires an active org membership.

It writes `identity.project_env_memberships` and converges the user row into the target application's `app_system.users`.

`member.revoke` removes the target environment membership and deletes that user's application `users` row. Existing FKs cascade its application role assignments.

It does not affect the user's org membership or another environment.

If the user still holds `org-admin` or `project-admin` covering that environment, `member.revoke` refuses. The covering authority must be revoked first.

`reconcile-run-plane` converges user membership in both directions:

```text
system membership exists + user row absent
→ add user row

user row exists + system membership absent
→ remove stale user row
```

This removal applies only to user rows. Service and platform rows retain their separate authoritative sources.

### `project_admin.grant`

Requires an active org member.

Writes an actual `project-admin` row and then materializes:

```text
membership in every existing project environment
admin in every existing application
```

A later environment creation performs the same materialization for every current project admin.

### `project_admin.revoke`

Refuses while the user remains `org-admin`.

Otherwise it removes application `admin` from every environment in the project, then removes `project-admin`.

Ordinary environment membership and authored application roles remain.

---

## 4.6 Application level

Issue 5 (`wamn-a40n.7`) built this section; [execution](../architecture/execution.md#host-run-routes) holds the current behavior.

One fixed platform contract is served under every application audience:

```text
wamn-control:<noun>/<verb>@<version>
```

It is one contract for the whole effective application, not one per package.

Host-run operations:

```text
wamn-control:user/list@<version>

wamn-control:role/list@<version>
wamn-control:role/create@<version>
wamn-control:role/delete@<version>

wamn-control:permission/list@<version>
wamn-control:permission/grant@<version>
wamn-control:permission/revoke@<version>

wamn-control:user-role/grant@<version>
wamn-control:user-role/revoke@<version>

wamn-control:permission/mine@<version>
```

All except `permission.mine` require `admin`.

### Users

`user.list` reads current user application users.

A user reaches this relation through project-env membership.

Membership remains authoritative; application administration cannot manufacture a user.

### Roles

`role.create` creates one empty authored role.

`role.delete` deletes an authored role and its assignments/grants through the existing FKs.

`admin` cannot be deleted.

### Permissions

`permission.list` reports for a role:

- every directly selected root;
- every effective permission row;
- the root or roots requiring each effective permission;
- whether each current operation is grantable;
- whether an operation is fixed admin-only.

Example:

```text
receiving.record_receipt
    selected

purchase_order.get
    required by receiving.record_receipt

inventory.query
    selected
    required by another_operation
```

`permission.grant` selects a root and materializes its current release closure.

`permission.revoke` removes that root's direct selection and the closure rows owned by it.

If an affected permission is also required by another root, it remains effective and the operation reports that fact.

A dependency that was never directly selected has no direct grant to revoke; attempting to revoke it refuses and names the roots requiring it.

Fixed administration operations are shown but disabled for authored roles.

### User roles

`user_role.grant` and `user_role.revoke` operate on current application roles.

A direct `admin` grant is permitted.

An `admin` revoke refuses while a covering `project-admin` or `org-admin` remains. Revoke the higher-level authority first.

### `permission.mine`

Returns the caller's effective current permission set for this application.

For an authored role, this is the distinct effective permission set already stored after release reconciliation.

For `admin`, it is every operation the current release serves.

It is an authenticated-member utility, not an authored grant.

---

## 4.7 Administration screens

Issue 6 (`wamn-a40n.10`) built this section; [execution](../architecture/execution.md#host-run-routes) holds the current behavior.

Generated operation screens remain available individually and remain the TUI baseline.

The web UI additionally has two composite screens in `web/ui`, built only from the operations above.

### Role grid

For one role:

```text
all current grantable operations
grouped by interface
search
directly selected state
effective state
required-by roots
one toggle per directly selectable operation
```

A dependency row that is effective only through another root is visible but has no direct selection to remove.

If a directly selected operation is also required through another root, its direct toggle can be turned off; the row remains effective and explains which root still requires it.

This makes:

> Why does this role have this permission?

answerable directly from the screen.

### User grid

For one user:

```text
all application roles
current role assignments
effective permission set
```

A lower administrative role covered by a higher-level grant is shown as hierarchy-controlled rather than independently revocable.

No grid writes a database directly.

---

## 4.8 Shell

The shell remains responsible only for:

```text
sign-in
audience choice
/invite
/recover
/reset
/:aud/<screen>
```

After credential validation:

- application audiences with no effective role are not offered;
- control audiences are offered only for current `org-admin` or `project-admin` authority;
- if no audience is available, the entire authenticated result is:

> **No access has been granted.**

No inaccessible environment names or screens are displayed.

Within an application audience, the shell reads `permission.mine` once and shows only screens/actions the caller can actually use.

Within a control audience, it reads `control.mine` once and shows:

```text
org administration
→ only for org-admin

project administration
→ only for projects where the caller is project-admin
```

This is presentation only. Server refusal remains authoritative.

---

## 4.9 Consistency and convergence

System and project databases cannot participate in one PostgreSQL transaction.

The hierarchy therefore uses a safe ordering.

### Grant

Authority is written from the source downward:

```text
higher-level authority
→ lower system grants
→ environment memberships
→ application grants
```

An interrupted grant may temporarily provide less access than requested. It never grants a lower authority without its authoritative higher-level source having committed first.

### Revoke / deactivate

Authority is removed from the leaves upward:

```text
application access
→ environment membership / project authority
→ org authority or org membership
```

An interrupted revoke may temporarily remove too much access. It must not report success while stale lower access remains.

`reconcile-run-plane` and the control reconciliation functions repair interrupted projection, but they are not the security mechanism for a successfully reported revoke.

Every write carries normal record-history attribution and an administrative operation identity.

---

## 5. Lifecycle — second epic

Lifecycle work begins only after the administration epic closes and teardown has a supported verb.

## 5.1 Project creation

`project.create` creates the control-plane project record.

It creates no infrastructure.

Existing org admins receive actual `project-admin` grants for the new project.

## 5.2 Environment creation

`environment.create` writes one provisioning saga.

The worker performs the existing supported chain, conceptually:

```text
provision-project-env
enable-cdc-project-env

apply selected packages
reconcile package data access
push required components
publish candidate release
reconcile authored role permission closures
activate release
reconcile run plane
upload selected client UI

materialize current org/project admin grants
```

The request names package/release inputs, not arbitrary executable provisioning commands.

`environment.list` exposes the saga and its steps.

## 5.3 Environment copy

`environment.copy` creates a new empty environment from the source's reusable installation inputs:

- selected package coordinates;
- component/artifact selections;
- reusable bindings;
- authored role definitions;
- directly selected stable permission roots;
- selected UI artifact/input.

It does **not** copy derived `required_by` rows. Those are recomputed against the target's candidate serving release.

It also does **not** copy:

- application data;
- users;
- session state;
- the source serving-manifest bytes;
- environment-specific release identity.

The target builds and publishes its own serving release.

Current org/project admins are materialized normally after creation.

## 5.4 Environment state

Add an environment status:

```text
active
inactive
```

Inactive means:

- identity does not offer or mint its application audience;
- router serves none of its application routes;
- shell does not list it;
- CDC remains attached and its slot remains maintained;
- data remains;
- activation is reversible.

`project.inactivate` inactivates every environment of the project.

It does not deactivate the user's org membership or alter org/project role rows.

## 5.5 Provisioning worker

`wamn-ctl serve` is the initial executable surface for a long-running provisioning worker.

Architecturally it is a service, not an interactive CLI holding an operator's omnibus credentials.

It receives a dedicated workload identity with the minimum provisioning capabilities required for the saga steps.

It:

- reads open `provisioning.sagas`;
- runs the same control-library functions as the CLI verbs;
- serializes one saga per org;
- records each completed step;
- leaves a failure at the failed step with its error;
- supports resume and abandon.

The router never receives these credentials.

---

## 6. Work sequence

One branch, one agent per issue. Workspace proofs first; cluster proofs are explicit exit gates where required.

### Required precursor

**P0. Specify platform `kind` → `type`.**

Before implementation, write and accept the dedicated migration specification required by Section 0.

It fixes:

- serialized surfaces;
- digest consequences;
- package-version changes;
- base/overlay ordering;
- wamn-dev republish sequence;
- installed-schema changes;
- compatibility with sealed history;
- the `*ErrorKind` decision;
- acceptance proofs.

**P1. Implement platform `kind` → `type`.**

Separate contract-migration epic and hard dependency.

It:

- implements the accepted P0 specification;
- changes the WAMN-owned vocabulary;
- migrates authored and generated serialization;
- preserves already sealed coordinates/artifacts;
- regenerates and republishes affected applications under new immutable identities as specified;
- regenerates frozen evidence;
- closes before Platform UI Issue 1 begins.

The migration cost is accepted.

### Administration epic

**1. Application authority model.**

The **first commit** of Issue 1 changes no Platform UI behavior. It remeasures Section 3 against the then-current `main`, including changes landed since `3a81dcfec`, and commits the factual baseline delta. Implementation proceeds from that measured state.

Then:

- remove `operator`;
- `admin` without permission rows;
- stable stored operation references;
- permission-root provenance;
- stored released permission closure;
- authored roles;
- role-name CHECK;
- candidate-release closure reconciliation before activation;
- CLI role/permission verbs;
- dev-loop and service-principal migration.

Installed databases move by `upgrade-schema` (owner ruling 2026-09-30, `wamn-a40n.1`), not by hand statements. Issue 1 ships two migration files:

- `deploy/sql/migrations/project/0002_authored_roles.sql`, for each project-environment database. A holder of `operator` holds `admin`, and the role `operator` goes. `admin` loses its permission rows. A surviving authored permission becomes a stable reference and a direct selection. The table gains `required_by`, the self-referencing key, the admin and reference CHECKs, and the role-name CHECK. The closure rows of each selection are written when the next candidate release is reconciled before activation.
- `deploy/sql/migrations/system/0005_admin_role.sql`, for `wamn_system`. A service that holds the project role `operator` holds `admin`, because route PAT admission accepts only `admin`.

A fresh install records both files as applied.

Exit includes:

- baseline remeasurement;
- version bump;
- added dependency;
- removed dependency;
- shared dependency;
- direct grant removed while dependency remains effective;
- non-selected dependency revoke refusal;
- removed operation;
- removed package;
- unchanged `authorize_released_operation`;
- admin-without-row proofs.

**2. Host-run routes and control serving root.**

- route execution target;
- host dispatch using the current host transaction path;
- fixed control serving root;
- application host routes;
- org-scoped control session target;
- only `org-admin` / `project-admin` admission;
- `control.mine`;
- one fixture operation;
- session/PAT/CSRF tests.

**3. Org membership and org administration.**

Issue 3 (`wamn-a40n.3`) is bounded by what lives in `wamn_system`.
§4.4 holds the design. Main at `216da8ce2` was measured on 2026-10-01.

Measured starting state:

- `identity.org_memberships` and `identity.org_roles` do not exist.
- The `control` family holds `SELECT` on `identity.principals`, `identity.project_roles` and `identity.password_logins` only (`CONTROL_RELATIONS`, `crates/control/provision/src/sql.rs`). Credential prepare converges it with `REVOKE ALL` first, so a grant outside the builder does not survive.
- `provision-org` writes `registry.orgs` and `registry.env_policies` only, and binds no actor.
- `wamn-ctl invite` sends `POST /invitations {principal_id}` over the operator certificate. The principal must already exist, and identity's database role cannot insert one.
- Control admission (`control_session_is_active`, `control_orgs`) and `control.mine` know `project-admin` only.
- `/password/environments` already returns an empty list for a user with no access.
- The system identity tables carry the stamp trigger, which records `app.user_id` only.

Commits, each one green:

1. System migration `0007` and the provisioning builders. The migration creates `identity.org_memberships` and `identity.org_roles` with their keys, checks and stamp triggers. It carries the grant changes of §4.4 as the rendered builder output. `system-schema.sql` gains the same tables for a fresh install. `family_denial_matrix` pins the new `control` writes and reads and the identity issuer's new `SELECT` and `INSERT`.
2. `provision-org --owner-email`, required, which writes the owner's membership and `org-admin` row.
3. The identity call that creates or reuses a user by email and display name. `wamn-ctl invite` takes the logical input of `user.invite`.
4. Control admission and `control.mine` accept `org-admin`.
5. The org host routes of §4.4: `user.list`, `user.invite`, `user.activate`, `user.deactivate`, `project.list`, `org_admin.grant` and `org_admin.revoke`. Each write runs in one transaction on the `control` login, with `app.user_id` and `app.operation` bound.
6. Documentation: execution, data access (the system identity tables record the actor only), and `gcp.md` §7, which records `0007` as a migration run and the dkk owner as a `wamn-ctl invite` with `org-admin`.

Exit includes:

- `0007` on an installed `wamn_system` gives the same ACL surface as a fresh install
- the denial matrix rows of the `control` family and the identity issuer
- `provision-org` with a known and an unknown owner email
- the new, enrolled and unenrolled `user.invite` cases, including mail only where no password credential exists
- an invitation with no access, whose login lists no environment
- `user.deactivate` and `user.activate` at the system level
- `org_admin.grant` materialization over every project and environment, and `org_admin.revoke`
- every org route refusing a caller without `org-admin`, and a revoked `org-admin` refused on the next request
- `wamn-ctl invite` parity

The application rows of `org_admin.grant`, `org_admin.revoke` and `user.deactivate` move to issue 4.

**4. Project administration and convergence.**

Issue 4 (`wamn-a40n.6`) builds §4.5 and the application rows that issue 3 left: `users` and `admin` rows for `org_admin.grant`, their removal for `org_admin.revoke` and `user.deactivate`, and the same rows for the project operations.

D answered [§9](#9-questions-for-the-owner) question 1 on 2026-10-01 with option A: the control host holds the `wamn_administration` login of every environment of its org ([application writes](#application-writes)). The rows stay stored, and nothing is read at request time.

Measured on main `ea471e14a` on 2026-10-01:

- The `control` family holds system tables only, and nothing in any project database (`CONTROL_RELATIONS`, `crates/control/provision/src/sql.rs`).
- `org_admin.grant` writes `org-admin`, `project-admin` in every project and a membership in every environment, all in `wamn_system`. It writes no application row.
- `app_system.users` rows of users come only from `reconcile-run-plane`, with the admin URL. It adds a missing row and never removes a stale one.
- `app_system.user_roles` rows come from `wamn-ctl grant-role` and `revoke-role`, with the admin URL. An application host holds the `wamn_administration` login (`WAMN_ADMINISTRATION_PG_URL`) for `permission.mine`.
- `provision-project-env` writes no `project-admin` row and no membership for the current org admins or project admins.
- The org operations of §4.4 run on the control serving root through `wamn_platform_identity::org`. No project operation of §4.5 exists.

Commits, each one green:

1. `reconcile-run-plane` removes a user row whose system membership is gone, as §4.5 states. Service and platform rows keep their own sources. The foreign keys remove the user's `user_roles` rows.
2. The project routes of §4.5 on the control serving root, system rows only: `environment.list`, `member.list`, `member.grant`, `member.revoke`, `project_admin.grant` and `project_admin.revoke`. A route needs `org-admin` in the org or `project-admin` in the named project, and checks it again in its write transaction. The writes are functions of `wamn_platform_identity::org`, beside `grant_project_admin`. `member.revoke` refuses while `org-admin` or `project-admin` covers the environment, and `project_admin.revoke` refuses while the user is `org-admin`.
3. This amendment of §4.4, this issue and §9.
4. Future materialization. `provision-project-env` writes `project-admin` for each current org admin in a new project, and a membership for each current org admin and project admin of the project in a new environment, in its provisioning transaction, through the same functions.
5. The administration logins of the control host. `provision-org` emits the empty Secret `wamn-control-administration-<org>`. `provision-project-env` emits the patch that adds or rotates the key of its environment, and `delete-project-env` emits the patch that removes it. The control host reads the mounted Secret at each write.
6. The application rows. `org_admin.grant`, `org_admin.revoke`, `user.deactivate`, `user.invite`, `project_admin.grant`, `project_admin.revoke`, `member.grant` and `member.revoke` write `users` and `admin` rows on a grant after the system rows, and remove them on a revoke or a deactivation before the system rows (§4.9). A failed environment refuses with the completed environments named.
7. `reconcile-run-plane` writes a missing `admin` row for each holder of `project-admin` in the environment's project, so it repairs an interrupted grant and the grants of `wamn-ctl invite`.
8. Documentation: execution, data access and the operations pages of the new routes, verbs and Secret.

Exit includes:

- `reconcile-run-plane` adding and removing user rows, and keeping service and platform rows
- every project route refusing a caller without `org-admin` or `project-admin` in the project
- `member.grant` and `member.revoke`, including the refusal under a covering `org-admin` or `project-admin`
- `project_admin.grant` materialization over every environment of the project, and the `project_admin.revoke` refusal under `org-admin`
- a new project and a new environment receiving the rows of the current org admins and project admins
- the application rows of `org_admin.grant`, `org_admin.revoke`, `user.deactivate`, `project_admin.grant` and `project_admin.revoke` in each environment, in the order of §4.9
- an interrupted revoke that never reports success while an application row remains
- a failed environment refused with the completed environments named
- a new environment's login reaching a running control host without a restart, and a removed environment's login gone

**5. Application administration contract.**

Issue 5 (`wamn-a40n.7`) builds §4.6: the application host routes of every application audience, which write through the `wamn_administration` login of the environment.

Measured on main `99e5b24d8` on 2026-10-01:

- The application set has one route, `permission.mine`, which needs a member only (`crates/catalog/model/src/host_route.rs`). Publish adds the set to every serving release. The local development release serves no host route.
- The route table keys every host route by its attachment id. An application `user.list` derives the id, path and sealed operation id of the control `user.list`, so the two collide in that table.
- An `Admin` host route requires its own sealed operation, and `AuthenticatedCaller::permits` admits `admin` or a caller that holds the stable reference. The schema refuses a permission row for `admin` only, so a stored `wamn-control:*` row would admit a caller without `admin`.
- The role and permission writes of `wamn-ctl` are in `crates/control/lib/src/role_permissions.rs` and `user_roles.rs`. They connect with the admin URL, turn row security off and bind `wamn:provisioning`. `ReleaseClosures` reads a serving manifest and needs only `wamn-catalog`.
- The application host holds the `wamn_administration` login for `permission.mine`. Its row policy gives that login every row of the four administration relations, and the login holds SELECT, INSERT, UPDATE and DELETE on them. `WamnPostgres` exposes no write on that login.
- The application host also holds the identity reader login of `wamn_system`, which reads `identity.project_roles`. Every `org-admin` holds `project-admin` in each project of the org (§2.8), so that row answers the covering check of an `admin` revoke.
- No generated client or component covers a host route. The generator projects package contracts only, and the control contract has no contract files.
- A host command route already requires the CSRF header.

Commits, each one green:

1. The routes. The application set gains `user.list`, `role.list`, `role.create`, `role.delete`, `permission.list`, `permission.grant`, `permission.revoke`, `user_role.grant` and `user_role.revoke`, all `Admin`. The route table keys a route by its set and its attachment id, so the two `user.list` routes keep the ids, paths and operation ids that §4.4 and §4.6 name. `permits` refuses a `wamn-control` reference to a caller without `admin` (§2.5), and `permission.grant` refuses it.
2. The shared writes. `ReleaseClosures` moves to `wamn-catalog`. The role, permission and user role writes move to `wamn_platform_identity::application` and take a prepared transaction. `wamn-ctl` keeps its own preparation and calls them, so the command line and the host write the same rows.
3. The handlers. A write takes the tenant lock that release reconciliation takes, binds the caller and the route's sealed operation id, and uses the closures of the loaded release. `permission.list` reports the roots, the effective rows, the roots that require each row, and whether each served operation is grantable or fixed to `admin`. `user_role.grant` refuses a user without an application user row. `user_role.revoke` of `admin` refuses while the user holds `project-admin` in the project, read through the identity reader.
4. The client. The application routes gain contract files, and the generator projects them through the client IR into a generated TypeScript client, as it does for every generated route. No client is written by hand.
5. Documentation: execution and data access.

Exit includes:

- each route refusing a caller without `admin`, and `permission.mine` admitting any member
- a stored `wamn-control` permission row that admits no caller without `admin`
- `role.create` and `role.delete`, with the refusals for `admin`
- `permission.grant` writing the closure of the loaded release, and refusing an unserved or a fixed operation
- `permission.revoke` keeping a row that another root requires and naming that root, and refusing a row that is not a root
- `permission.list` showing the roots, the required rows and their roots
- `user_role.grant` and `user_role.revoke`, including the refusal under a covering `project-admin`
- each write stamped with the caller and the route's sealed operation id
- the same rows from `wamn-ctl` and from the routes

The route tests call the routes through the generated client ([§9](#9-questions-for-the-owner) question 4).

**6. Web administration grids.**

Issue 6 builds the role grid and the user grid of §4.7 in `web/ui`, over the generated client `@wamn/control-client`.

Measured on main `a3283c7b7` on 2026-10-02:

- `web/ui` has two tables. `SetTable` (`web/ui/src/table/set-table.tsx`) takes its rows and columns as props. `QueryTable` (`query-table.tsx`) reads one operation through a `Transport`, and its row actions only open a record or fill a form.
- `ConfirmAction` (`confirm.tsx`) asks before an action, and `announceOutcome` (`outcome.ts`) shows one outcome as a toast. A generated delete screen joins them around one generated call (`check_client_components` output, tested by `web/components/test/delete.test.tsx`).
- No screen or component calls `@wamn/control-client`. Only `web/components/test/control-client.live.test.ts` calls it.
- `web/ui` has no test script. Its components are tested in `web/components` with vitest and jsdom, and shown in the gallery there.
- The web packages reach each other by path alias, not by package dependency (`pnpm-workspace.yaml`). The client modules import only `@wamn/web-runtime`, so a `web/ui` alias to the client makes no cycle.
- `permission.list` answers, for one role, each served operation and each stored row with `served`, `grantable`, `admin_only`, `selected` and `required_by`. That is every column of the role grid.
- `user.list` answers each application user with the roles it holds. No application route answers whether `project-admin` or `org-admin` covers a user's `admin`. `user_role.revoke` answers `admin_covered` only when an `admin` revoke is tried.
- No application route answers the effective permissions of another user. `permission.mine` answers the caller's own.

Commits, each one green:

1. The covering fact. Application `user.list` answers `admin_covered` for each user, read through the identity reader as `user_role.revoke` reads it ([§9](#9-questions-for-the-owner) question 5). The contract bytes change, so the version of the control contract moves with them, and the generated clients are written again.
2. The role grid. `RoleGrid` in `web/ui/src/admin/role-grid.tsx` takes a `Transport`. It reads `role.list` for its role choice and `permission.list` for the chosen role, and shows one row per operation on `SetTable`. The rows group by interface, the part of the reference between `:` and the last `/`, and a search filters them by reference. Each row shows the direct selection, the effective state and the roots that require it. A served, grantable row has one toggle, which calls `permission.grant` or `permission.revoke`. A row that is effective only through other roots shows those roots and has no toggle. A selected row that another root also requires keeps its toggle, and after a revoke it stays effective and names the roots in `still_required_by`. The role `admin` shows that it holds every operation, with no toggle.
3. The user grid. `UserGrid` in `web/ui/src/admin/user-grid.tsx` takes a `Transport`. It reads `user.list` for its user choice and `role.list` for the rows, with one toggle per role that calls `user_role.grant` or `user_role.revoke`. The effective permission set is the union of `permission.list` of each held role, or every served operation when the user holds `admin`. A user with `admin_covered` shows its `admin` row as hierarchy-controlled, with no toggle.
4. Tests and gallery. `web/components` tests each grid with a fake transport, in the pattern of `delete.test.tsx`: the rows, the toggles, a refusal with its contract text, and the state after each write. The gallery shows both grids. `control-client.live.test.ts` stays the test of the routes.
5. Documentation: the `web/ui` README export table, and execution for the screens and the `user.list` result.

Each write announces its outcome with `announceOutcome`, and a refusal shows the `text` of its contract. A grid reads again after each write, and writes no database directly.

Exit includes:

- a role grid that shows the direct and the effective state, and the roots of each required row
- a grant that adds the closure rows, and a revoke that keeps a row that another root requires and names that root
- a row effective only through another root, with no toggle
- `admin` with no toggle in the role grid
- a user grid that grants and revokes a role, and shows the effective permission set
- `admin_covered` for a user who holds `project-admin`, and a hierarchy-controlled `admin` row with no toggle
- each refusal shown with its contract text

**7. Shell.**

- control destinations;
- `control.mine`;
- `permission.mine`;
- empty-audience **No access has been granted** state;
- browser proof through control and application surfaces;
- architecture and `web-operator-client.md` updates.

Issue 7 builds §4.8 in `web/shell`. The text "No access has been granted." lands here ([§9](#9-questions-for-the-owner) question 3).

Measured on `worktree-table` `7daecf82f` on 2026-10-02:

- `environments()` posts the email, the password, the org and the project to `/password/environments` (`web/runtime/src/session.ts`). Identity lists an application audience only when `authorized_roles` succeeds, so an audience with no effective role is already left out (`services/identity/src/password.rs`, `services/identity/src/session.rs`).
- Identity adds the control audience `urn:wamn:control:<org>` only when the request names no project (`password.rs`). The shell always names its project, so it never receives a control audience. A control row has no `project` or `env`, but the TypeScript `Environment` type requires both.
- `signIn` already accepts a control audience. Identity checks the control authority and issues a session with no roles (`session.rs`).
- The shell `Session` refuses an address whose audience is not `urn:wamn:project-env:<org>:<project>:`, so a control audience in the address is refused (`web/shell/src/shell.tsx`).
- With no reachable environment, the shell shows "This account reaches no environment of this application." beside the sign in form (`shell.tsx`).
- A host serves the control set when it runs with `WAMN_CONTROL=true` (`services/host/src/host.rs`). On wamn-dev it is the ClusterIP hostgroup `control` with no edge entry. The edge lists only `receiving.wamn.dev` and `wms.wamn.dev` (`deploy/gcp/values-edge.yaml`). The dev loop starts no control host.
- No web page targets a control audience, and no page shows an org or project screen. `RoleGrid` and `UserGrid` (issue 6) are placed only in the gallery and the tests.
- `ShellScreen`, `ShellRoute` and `ShellAction` carry no operation. `routes.tsx` of each application imports only the labels. Each generated client module exports `*_ROUTE` with the sealed operation, and a query table shows a row button only for a reference it was given.
- `permission.mine` answers `admin` and `permissions`, the unversioned references the caller holds. `control.mine` answers `org_admin` and `projects[]` with `project_admin`. Nothing in `web/` reads either.
- The session cookie is HttpOnly, and nothing in `web/` reads the token's `roles`.
- `web/shell/test/shell.test.tsx` fakes identity with a `fetch` stub and needs no browser. No browser test of the shell exists. The gallery's visual test runs a local Chrome through Playwright. `control_client_live` serves both route sets from local processes with no kind cluster.
- `docs/plan/web-operator-client.md` says almost nothing about the shell. The shell rules are in `web/shell/README.md` and [execution](../architecture/execution.md).

[§9](#9-questions-for-the-owner) questions 6 to 10, answered 2026-10-02, set the commits. Each one is green:

1. No access. When `/password/environments` lists no audience, the sign in page shows only "No access has been granted.", with no environment, screen or other text, and mints no session.
2. Application screens. Each `ShellScreen` and `ShellAction` names its operation reference, which the route table takes from the generated `*_ROUTE` constant. The `Session` reads `permission.mine` once after sign in, and the navigation, the actions and the routes show only what the caller holds, or every one for `admin`. An address of a screen the caller does not hold shows the no page text. The route tables of Receiving and WMS name their operations.
3. Application administration. Each application shell has an Administration section with `RoleGrid` and `UserGrid`, shown only when `permission.mine` answers `admin`.
4. Control destinations. Identity lists the control audience `urn:wamn:control:<org>` of the shell's org when the caller holds `org-admin` or `project-admin` there, also when the request names a project. The shell offers it after sign in as Control, beside the environments. Under a control audience the shell reads `control.mine` once and shows the org destination only for `org-admin`, and a project destination only for each project that `control.mine` names. The org and project screens behind them are issue 8 of this epic.
5. Browser test. An ignored test serves an application host and a control host from local processes, as `control_client_live` does, and drives a headless Chrome through sign in, no access, a hidden screen, the application grids and the control destinations.
6. Documentation: the shell README address table and props, execution for the shell rules, and `web-operator-client.md`.

Presentation is never authority. Every route repeats its own check, and a refusal still shows its contract text.

Exit includes:

- an account with no audience that sees only "No access has been granted."
- an application user who sees only the screens and actions of the operations it holds
- an `admin` who sees every screen and the two grids
- an `org-admin` who sees the org screens and every project, and a `project-admin` who sees only its projects
- a browser test through a control surface and an application surface

**8. Org and project screens.**

Issue 8 (`wamn-a40n.12`) builds the screens behind the Control destinations of issue 7, from the operations of [§4.4](#44-org-level) and [§4.5](#45-project-level). The owner filed it on 2026-10-02 ([§9](#9-questions-for-the-owner) question 7).

Measured on `worktree-table` `27054619a` on 2026-10-02:

- `@wamn/control-org-client` has TypeScript bindings for all 14 org and project operations, and no generated components. `RoleGrid` and `UserGrid` in `@wamn/ui/admin` are hand-written over `@wamn/control-client` in the same way.
- Org `user.list` answers `principalId`, `email`, `displayName` and `status` for each member. It does not say who holds `org-admin`, so a screen cannot choose between grant and revoke before a click.
- `member.list` takes a project and answers each member with `environments`, `orgAdmin` and `projectAdmin`. `environment.list` takes a project and answers its environment names. `project.list` answers the project names of the org.
- Every org operation requires `org-admin`. A `project-admin` therefore cannot call org `user.list`, and `member.list` names only users who already have a membership or a role in the project. A `project-admin` has no list from which to choose a new member, and `member.grant` takes a `principalId`.
- `user.invite` takes `email`, `displayName`, `memberships` (project and environment pairs), `orgAdmin` and `projectAdmins`. It calls identity to create or reuse the user and to mail the invitation.
- A grant that reaches application rows reads the administration logins from a mounted directory. `control_route_live` gives the control host such a directory over its fixture project database. `shell_browser_live` and `control_client_live` give it none, so a write that reaches an environment refuses there, and the browser test cannot complete one.
- `ORG_SCREEN` and `PROJECT_ROUTE` in `web/shell/src/shell.tsx` render only "Org administration." and "Project administration of {project}.".

[§9](#9-questions-for-the-owner) questions 11 to 14, answered 2026-10-02, set the commits. Each one is green:

0. Org `user.list` answers `org_admin` for each member, and it admits a `project-admin` of any project in the org, read only.
1. Org screen. Under `org`, a members table from org `user.list` with activate, deactivate, `org-admin` grant and revoke, and an invite form with its memberships, `org-admin` and `project-admin` choices from `project.list` and `environment.list`.
2. Project screen. Under `projects/:project`, a members table from `member.list` with one membership toggle for each environment from `environment.list`, `member.grant` and `member.revoke`, and `project-admin` grant and revoke. A new member is chosen from org `user.list`.
3. Browser test. `shell_browser_live` grants and revokes through both screens.
4. Documentation: the shell README, execution and `web-operator-client.md`.

Each write reads its list again after it completes. A refusal shows its contract text, and a partial write names the environments that completed.

Exit includes:

- an `org-admin` who invites a user, activates and deactivates a member, and grants and revokes `org-admin`
- an administrator of a project who grants and revokes an environment membership and `project-admin` in that project
- a covered row, such as `project-admin` under `org-admin`, shown as covered with no revoke control
- each refusal shown with its contract text

### Lifecycle epic

Starts after the administration epic and the supported teardown verb.

The epic is `wamn-zua8`. Its first issue, `wamn-zua8.1`, closed on 2026-10-02 on evidence that `wamn-psss` closed at `fa04f19e1`. The items are numbered 9 to 11 after the administration issues, with the bead id after each ([§9](#9-questions-for-the-owner) question 15).

Measured on main `6eba50b90` on 2026-10-02:

- No table holds an environment status. `registry.project_envs` has `org`, `project`, `env`, `secret_name`, `secret_namespace`, `instance_suffix` and `disposable`. `catalog.tenant_environments` in the control store has no status either.
- `wamn-o8b9` closed. A system schema change is now a file in `deploy/sql/migrations/system/` (0001 to 0007), applied by `wamn-ctl upgrade-schema`.
- The application host holds only its project database login. It has no connection to the system database, where `registry.project_envs` lives.
- Identity offers an application audience for each `--session-target` file it reads at start. Helm builds those files from `sessionTargetSecrets`. A new environment therefore needs a new Secret and an identity upgrade, as `docs/operations/gcp.md` section 3.10 does by hand.
- A host serves the one release that its pod arguments pin. Nothing turns off the routes of one environment.
- `control.mine` answers projects only. `environment.list` reads `registry.project_envs`.
- `provisioning.sagas` exists with `type` in (`provision-org`, `provision-project-env`), one `step` number and `last_error`. It has no steps table, and no code writes it. The only saga writer is `copy-project-env` in `wamn-ctl-ops`, which copies data into `provisioning.copy_sagas`. That verb is not the environment copy of [§5.3](#53-environment-copy).
- `wamn-ctl` has no `serve` subcommand, and no loop reads a saga.
- `registry.projects` gets a row only inside `provision-project-env`. No control route creates a project, creates or copies an environment, or inactivates one.
- Each step of the [§5.2](#52-environment-creation) chain has a library function, except the client UI upload. `wamn web upload` is private to the `wamn_ctl` crate and runs `pnpm`. Role permission closures reconcile inside release selection, and `materialize_admin_grants` runs inside `provision-project-env`.
- `provision-org`, `provision-project-env` and `enable-cdc-project-env` hold no Kubernetes client. They write SQL and Kubernetes manifests that an operator applies with the cluster superuser and `kubectl`, in a set order. `deploy-release` runs `kubectl`. The release selection reads a qualification file. A new environment also needs its host workload, which the chain of [§5.2](#52-environment-creation) does not name.

[§9](#9-questions-for-the-owner) questions 15 to 27, answered 2026-10-02, set the items.

**9. Environment status (`wamn-zua8.2`).**

- System migration `0008` adds `registry.project_envs.status text NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'inactive'))`. `upgrade-schema` applies it on wamn-dev. No statement is applied by hand.
- Identity offers and mints no audience of an inactive environment, so the shell does not list it.
- The application host answers every request of an inactive environment with `environment-inactive` and status 503, after it reads its own row `app_system.environment (status)` in the project database as `wamn_app` (questions 28 to 31). No row means `active`. Each status route writes that row through the administration login, leaves first ([§9](#9-questions-for-the-owner) question 26). The host workload keeps running.
- The control family holds `UPDATE (status)` on `registry.project_envs` and nothing else on that table. The grant check and the denial matrix learn column grants (question 27).
- Control routes `environment.activate`, `environment.inactivate`, `project.activate` and `project.inactivate`, for `org-admin` only. The project routes change every environment of the project.
- CDC, data, memberships and role rows stay as they are.

**10. Provisioning worker and environment creation (`wamn-zua8.3`).**

- `environment.create` takes `{project, env, tenant, packages: [{package_id, version}], ui}`, with package coordinates already in the registry. Without `ui`, the saga has no UI step. A `ui` names a built UI artifact already pushed, and the worker never runs `pnpm`.
- `provisioning.sagas` gets the new saga type, and its own migration adds `provisioning.saga_steps`.
- `wamn-ctl serve` runs the [§5.2](#52-environment-creation) chain to the run plane and the UI. It holds an in-cluster ServiceAccount with RBAC on `Database` CRs in `platform` and Secrets in `hosts`, and nothing else. It runs the role and privilege SQL as the Postgres login `wamn_provisioner`, which has `CREATEROLE` and `CREATEDB` and is not a superuser. No key file exists. If a statement of the role SQL needs a superuser, the work stops and names it.
- The last step is `awaiting operator`, with the exact commands for the host workload and the identity restart, shown in `environment.list`. The worker never pauses for an operator before that step.
- Resume, abandon and the status UI.
- Exit: a local-process test with no Kubernetes.

**11. Project creation and environment copy (`wamn-zua8.4`).**

New-project grant materialization and deterministic empty-environment copy from reusable release inputs.

The epic closes on one kind run after B13 of the cutover.

---

## 7. Out of scope

- Org creation and issuer creation through the UI.
- Global principal disable from an org screen.
- Restoring old project/application grants automatically after org reactivation.
- Per-user permission rows.
- A permission-policy expression language.
- A second built-in application role.
- A second built-in project administrative role.
- `project-author` access to the Platform UI.
- Package/release management as a general standalone UI.
- Copying application data between environments.
- Copying derived permission-closure rows between environments.
- Service principals and PAT management in the UI.
- A control audience for a PAT. Control sessions are browser sessions only; `wamn-ctl` is the machine path.
- Performance/metrics screens.
- Making session role grants visible before renewal.
- Giving the router Kubernetes or provisioning credentials.
- Sending an informational email when an already enrolled principal is added to another org or receives additional access.

---

## 8. Accepted decisions

1. **Permission model — accepted.** Stable operation references are stored for application roles, with directly selected roots and their released permission closure materialized as provenance-bearing rows. Candidate-release reconciliation updates that closure before activation. Request admission resolves the resulting stored effective references to exact sealed ids. `authorize_released_operation` remains unchanged. Removing a direct selection does not remove authority still required by another selected root.

2. **Hierarchy — accepted.** `org-admin → project-admin → admin` is materialized as real grants. Higher-level role revocation removes subordinate administrative grants; org-user deactivation removes all lower access; lower-level changes never revoke higher authority.

3. **Org membership — accepted.** `identity.org_memberships` allows an invited user to exist in an org with zero effective access and gives org-local activation/deactivation without misusing global `identity.principals.status`.

4. **Control session — accepted.** The control audience is org-scoped and available only to `org-admin` and `project-admin`. Every org/project action rechecks current `wamn_system` roles. `control.mine` supplies the shell's current control view.

5. **Provisioning worker — accepted.** `wamn-ctl serve` is the initial binary surface for a dedicated, narrowly credentialed saga worker. The router remains unable to provision infrastructure.

6. **Naming precursor — accepted owner ruling.** The platform-wide `kind` → `type` contract migration is mandatory and completes before Platform UI Issue 1 begins. Its implementation is itself gated on an accepted dedicated migration specification.

7. **Baseline discipline — accepted.** Section 3 is the reviewed `3a81dcfec` baseline, not a permanent statement about `main`. Issue 1 remeasures it against its starting `main` in its first commit before changing Platform UI behavior.

---

## 9. Questions for the owner

1. Issue 4, answered 2026-10-01 with option A. The control host of an org holds the `wamn_administration` login of every environment in that org, the login the application host holds, in the Secret `wamn-control-administration-<org>` ([application writes](#application-writes)). A write reaches each project database in its own transaction, leaves first on a revoke, and reports success only when every environment is done.
2. Issue 3, answered 2026-10-01. `wamn-ctl invite` writes as its sibling does: `--system-database-url`, as `wamn_system`, stamped `wamn:provisioning`.
3. Issue 3, answered 2026-10-01. The shell text "No access has been granted." lands in issue 7 (owner ruling 2026-10-02, which corrected issue 6 to issue 7). The exit of issue 3 stays an invitation with no access, whose login lists no environment.
4. Issue 5, answered 2026-10-01 with option A. Issue 5 adds the contract files of the application routes and the generated TypeScript client, and tests the routes through that client. Issue 6 starts from that client.
5. Issue 6, answered 2026-10-02 with option A. Application `user.list` answers `admin_covered` for each user, so the grid shows the covering grant before any click. A route that changes its result shape changes its version, so the version of the control contract moves with it.
6. Issue 7, answered 2026-10-02 with option B. The accepted §4.8 shell is one shell with audiences, and Control is the audience `urn:wamn:control:<org>` that it offers after sign in, not a second page. The edge routing to the control host is a deployment change, filed for after the cutover, and issue 7 does not wait for it.
7. Issue 7, answered 2026-10-02. Issue 7 builds the entry point only, as its list says. The org and project screens are issue 8 of this epic, specified after issue 7 closes.
8. Issue 7, answered 2026-10-02 with option A. The route table names the operation of each screen from the generated route constants, with no generator change.
9. Issue 7, answered 2026-10-02. `RoleGrid` and `UserGrid` are an Administration section in each application, which only `admin` sees.
10. Issue 7, answered 2026-10-02. The browser test runs local processes and a headless Chrome, as `control_client_live` does.
11. Issue 8, answered 2026-10-02 with option A. Org `user.list` answers `org_admin` for each member. The control contract stays at 0.2.0, because 0.2.0 is not on main.
12. Issue 8, answered 2026-10-02 with option B. Org `user.list` also admits a `project-admin` of any project in the org, read only. Grant and revoke keep their authority as [§4.5](#45-project-level) states.
13. Issue 8, answered 2026-10-02 with option A. `OrgScreen` and `ProjectScreen` are hand-written in `@wamn/ui/admin`, built like the grids.
14. Issue 8, answered 2026-10-02 with option A. The browser test completes a membership grant and an `org-admin` grant through the control host's test database, as `control_route_live` does. The invite form is a stub in the component tests.

15. Lifecycle numbering, answered 2026-10-02. The lifecycle items are numbered 9 to 11, with the bead id in parentheses after each.
16. Issue 9, answered 2026-10-02. The status column comes in system migration `0008` through `upgrade-schema`. The plan names no hand-applied statement, and `docs/operations/gcp.md` §7 drops one if it records one, once `0008` runs on wamn-dev.
17. Issue 9, answered 2026-10-02. `registry.project_envs.status text NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'inactive'))`.
18. Issue 9, answered 2026-10-02. Identity offers no audience, and the router serves no routes, after each reads the status. The host workload keeps running in this epic. Stopping it is a Kubernetes action of the worker, filed as its own bead and not in scope.
19. Issue 9, answered 2026-10-02. Four routes: `environment.activate`, `environment.inactivate`, `project.activate` and `project.inactivate`. The project routes apply to every environment of the project. `org-admin` only.
20. Issue 10, answered 2026-10-02. The worker holds its credentials, narrowly, and never pauses for an operator. An in-cluster Kubernetes ServiceAccount has RBAC on `Database` CRs in `platform` and Secrets in `hosts`, and nothing else. A Postgres login `wamn_provisioner` with `CREATEROLE` and `CREATEDB`, not a superuser, runs the role and privilege SQL. If a statement in the role SQL needs a superuser, the work stops and names it. No key file exists anywhere.
21. Issue 10, answered 2026-10-02. `provisioning.sagas` gets the new type. `provisioning.saga_steps` comes in its own migration in issue 10.
22. Issue 10, answered 2026-10-02. The input is package coordinates already in the registry: `{project, env, tenant, packages: [{package_id, version}], ui}`. A release of another environment is `environment.copy`, issue 11.
23. Issue 10, answered 2026-10-02. The saga ends where [§5.2](#52-environment-creation) ends, at the run plane and the UI. For the host workload and the identity restart it records a last step `awaiting operator` with the exact commands, shown in `environment.list`. The deploy epic's `create-environment` automates those two, not this epic.
24. Issue 10, answered 2026-10-02. The input names a built UI artifact already pushed, and the worker never runs `pnpm`. Without `ui` in the input, the saga has no UI step.
25. Issue 10, answered 2026-10-02. Issue 10 exits on the local-process test. The epic closes on one kind run after B13.
26. Issue 9, answered 2026-10-02. The host gets no new login and never holds a system connection. Each status route mirrors the status into one row of `app_system.environment (status)` in the project database, through the administration login, as the control host writes application rows (`wamn-a40n.6`). The order is leaves first: inactivate writes the application row and then the system row, and activate writes the system row and then the application row. The host reads its own row for each request.
27. Issue 9, answered 2026-10-02. The control family gets `UPDATE (status)` on `registry.project_envs` and nothing else on that table, because `secret_name` and `instance_suffix` belong to provisioning. The grant check and the denial matrix learn column grants.
28. Issue 9, answered 2026-10-02 with option B. The host answers an inactive environment with the new code `environment-inactive` and status 503, in the existing `{"error":{"code"}}` body. A 404 would hide the state.
29. Issue 9, answered 2026-10-02. `wamn_app` reads `app_system.environment`, with SELECT only, as on every `app_system` authority table. The administration login writes it.
30. Issue 9, answered 2026-10-02. `app_system.environment` follows the full pattern of its sibling tables: the tenant key, the stamp columns and trigger, forced RLS with the tenant and platform policies, a `_history` table, and the project-state list that the schema tests check.
31. Issue 9, answered 2026-10-02. No row means `active`. The first status route writes the row.
32. Issue 10. Ruling 20 says to stop when the role SQL needs a superuser, and four statements do, measured on PostgreSQL 18 as a `CREATEROLE CREATEDB` login (notes of `wamn-zua8.3`): `CREATE PUBLICATION ... FOR TABLES IN SCHEMA`, the `NOSUPERUSER` clause of every role hardener, the reads of `pg_authid`, and `ALTER DATABASE ... OWNER TO` on a database that the cluster superuser owns. How does the worker run them?
