# Platform UI application — spec

Status: live, `wamn-n8q9m`. M1 through M6 merged, and the shell runs on `@wfe/shell` at wfe v0.0.8.
Server paging, `wamn-n8q9m.20`, remains open.
[Execution](../architecture/execution.md#operator-clients) records current client and shell behavior.
Owner rulings of 2026-10-19 are applied.
**Baseline:** wamn `main` at `02e87bb4f`; wfe `main` at `45faa4aed7de92a7cd3e0a0b772bbd845e52ba63`.

## 1. Goal and scope

One web application, the platform UI, that lets an administrator run a tenant from a browser the day an environment is applied: sign in, see what is deployed, invite and manage users, grant access, and manage application roles and permissions.

It replaces the three partial UIs that exist today: `web/shell` (Solid 1, sign-in and app screens), `web/ui` (Solid 1, Zaidan copies), and `wfe/examples/platform` (Solid 2, mock-driven fixture).

In scope:
- Control of one org: projects, environments, environment status, users, memberships, org and project administration.
- Administration of one application environment: roles, permissions, user-role assignment.
- The application screens that the generator writes for each package, rendered through the same shell.
- A splash page per audience with status tiles.

Out of scope (own beads, after this plan):
- Traffic metrics (requests, latency, errors) from the OTel bucket.
- Org bootstrap. The first org admin comes from `wamn-ctl provision-org --admin`, as today.
- OIDC. Password identity only.
- Row-level or field-level ACL. Permissions stay per operation.

## 2. Decisions (proposed)

| # | Decision | Why |
| --- | --- | --- |
| D1 | wfe is the only UI dependency. `@wfe/components`, `@wfe/form`, `@wfe/generator`, Solid 2 pinned at exactly `2.0.0-rc.14`. | One component owner. wamn `web/ui` and `web/components` are deleted. |
| D2 | The platform UI is a new app at `apps/platform_ui/` in wamn, not an extraction of `wfe/examples/platform`. | The example is a fixture: query-string routing, mock scenarios and appearance controls in the app, no session. |
| D3 | wfe stays a separate repo. wamn consumes it by pinned git revision; tag when one exists. Generated clients are captured into wfe only for wfe's own examples. | wfe has no WAMN imports and must stay that way. |
| D4 | The shell is ported, not rewritten: `web/shell` addresses, session renewal, and the sign-in / invite / recover / reset pages keep their rules from `execution.md`. | The rules are tested. Only the rendering layer changes. |
| D5 | Every screen reads and writes through the existing `wamn-control:*` routes at 0.9.0. No new route. | The routes exist. The gap is the client. |
| D6 | `client_component.rs` emits wfe generator descriptions (`TableDescription`, `FormDescription`) instead of Solid 1 components. `client_ts.rs` bindings are unchanged. | The generator package renders descriptions. Emitting components again would be a second renderer. |
| D7 | Labels come from a `label` member on operations and fields in `wamn.k`, optional, defaulting to the field path. | [Web operator client history](../history/web-operator-client.md#8-the-epic-4-verdict) §8 "What hurts" item 3. `label` is an optional additive member of the manifest: the generator identity bumps, the manifest schema version does not. |
| D8 | No new route. `wamn-control:environment/status@0.9.0` gains three additive answer members: `users_with_role`, `roles`, `memberships` (nullable `int32`). A failed count read returns null without failing status. Additive members do not bump 0.9.0. | The splash tiles need one read per environment, not four. |

## 3. Audiences and access

The address decides the audience, as the shell does today.

| Address | Audience | Who sees it |
| --- | --- | --- |
| `/` | Sign in, then the list of audiences the account can reach | Everyone |
| `/urn:wamn:control:<org>` | Control of the org | `org-admin` sees everything; `project-admin` sees the projects `control.mine` names |
| `/urn:wamn:project-env:<org>:<project>:<env>:<id>` | One application environment | Any member of the environment; `administration/*` only for `admin` |

In §4, `/control` is short for `/urn:wamn:control:<org>`, and `/<env>` is short for `/urn:wamn:project-env:<org>:<project>:<env>:<id>`.

Authority is server-side. The client reads `control.mine` and `permission.mine` once per session and hides what the caller does not hold. A hidden screen that is reached by address shows "no page".

## 4. Screens

### 4.1 Control

| Screen | Address | Reads | Writes |
| --- | --- | --- | --- |
| Splash | `/urn:wamn:control:<org>` | `control.mine`, `project.list`, `environment.list` per project, `environment.status` per environment | — |
| Projects | `/urn:wamn:control:<org>/projects` | `project.list` | `project.create` |
| Project | `/urn:wamn:control:<org>/projects/<project>` | `environment.list`, `member.list` | `member.grant`, `member.revoke`, `project-admin.grant`, `project-admin.revoke`, `project.activate` / `project.inactivate` (one action) |
| Environment | `/urn:wamn:control:<org>/projects/<project>/<env>` | `environment.status`, `release.profile` | `environment.activate` / `environment.inactivate` (one action) |
| Users | `/urn:wamn:control:<org>/users` | `user.list` | `user.invite`, `user.activate`, `user.deactivate`, `org-admin.grant`, `org-admin.revoke` |
| User | `/urn:wamn:control:<org>/users/<principal>` | `user.list` (one), `member.list` per project | the four grant/revoke writes above, scoped to this principal |

### 4.2 Application administration

| Screen | Address | Reads | Writes |
| --- | --- | --- | --- |
| Splash | `/urn:wamn:project-env:<org>:<project>:<env>:<id>` | `permission.mine`, `environment.status` | — |
| Roles | `/urn:wamn:project-env:<org>:<project>:<env>:<id>/administration/roles` | `role.list`, `permission.list` per role | `role.delete` |
| Create role | `/urn:wamn:project-env:<org>:<project>:<env>:<id>/administration/roles/create` | None | `role.create` |
| Role Permissions (default Role screen) | `/urn:wamn:project-env:<org>:<project>:<env>:<id>/administration/roles/<role>/permissions` | `role.list`, `permission.list` for the role | `permission.grant`, `permission.revoke` |
| Role Users | `/urn:wamn:project-env:<org>:<project>:<env>:<id>/administration/roles/<role>/users` | `role.list`, `user.list` filtered by role | `user-role.revoke` |
| Grant to user | `/urn:wamn:project-env:<org>:<project>:<env>:<id>/administration/roles/<role>/users/grant` | `role.list`, `user.list` | `user-role.grant` |
| Users | `/urn:wamn:project-env:<org>:<project>:<env>:<id>/administration/users` | `user.list` | None |
| User | `/urn:wamn:project-env:<org>:<project>:<env>:<id>/administration/users/<principal>` | `user.list`, `permission.list` per held role | `user-role.revoke` |
| Grant role | `/urn:wamn:project-env:<org>:<project>:<env>:<id>/administration/users/<principal>/grant` | `user.list`, `role.list` | `user-role.grant` |
| Permissions overview | `/urn:wamn:project-env:<org>:<project>:<env>:<id>/administration/permissions` | `role.list`, `permission.list` per role | None |

These are `wamn-control:*` operations in the Application set. The application release serves `wamn-control:user/list@0.9.0`. Service principals have no route yet.

The environment hierarchy contains Roles, Users, and Permissions overview. Roles has one row per role, with a linked name and a granted-operation count. Each non-admin row has a Delete role action with a modal confirmation. Create role opens its own form and returns to Roles.

Each role has separate Permissions and Users screens. Permissions uses the served operations from `permission.list`, with one operation and one granted checkbox per row. Each change sends one `permission.grant` or `permission.revoke` command. A failed change restores the previous value and shows the error. The admin role is read-only, and `admin_only` rows are disabled. Role Users derives its rows from roles in `user.list` and offers modal revocation and a separate Grant to user form.

Users links each user name to a table of that user's roles. Each role name links to its Role screen, and each row has a read-only sub-table of granted operations. Revoke requires a modal confirmation. Grant role opens its own form and returns to the User screen.

Permissions overview is a read-only grid with one served operation per row and one role per column. Cells show granted marks without edit controls. The grid has no delete buttons and does not use `release.profile`. Each growing table fills its own screen and uses paged Records. Toolbar actions use `toolbarActions`, and forms use FormActions.

### 4.3 Application screens

Unchanged in rule from [web operator client history](../history/web-operator-client.md): one screen per operation role (table, detail, form, delete), rendered from the emitted description (D6), navigation from the package's route table.

## 5. Splash tiles

Per environment, from one `environment.status` read:

- release digest, short, and whether the rollout is complete
- users with a role, roles, memberships
- workload conditions: ready / not ready, one line

Per org, on the Control splash: the same tiles for every environment the caller administers, grouped by project.

No traffic numbers. A tile that would need the OTel bucket is not on this page.

## 6. wfe dependency

wamn consumes wfe by pinned git revision; tag when one exists. The pin is wfe `main` at `45faa4aed7de92a7cd3e0a0b772bbd845e52ba63`. The owner tags wfe after the wfe shell work, and the pin then moves to that tag. The platform UI needs these wfe exports: `@wfe/components` (with `layout` and `table`), `@wfe/form`, `@wfe/generator` (`TableDescription`, `FormDescription`, `GridDescription`, `LayoutView`), `@wfe/shell`, `StatusTile`, `TileRow`, and `label`/`format` on descriptions.

The wfe side is its own plan in the wfe repository. M1 starts on the exports that exist at the pin. Work that needs `@wfe/shell`, `GridDescription`, `StatusTile`, `TileRow` or `label`/`format` waits for the wfe tag, with one blocking bead per missing export. A wamn issue that finds a wfe gap files it against wfe and stops; it does not patch around it in wamn.

## 7. wamn track

Each is a wamn epic under the usual rules (worktree, ≤3 issues, build+clippy+fmt+unit per issue, report at merge). A wamn-dev deploy closes M1, M2, M4 and M5; the owner gives pool time once at each of those closes, and local checks run before that.

| # | Epic | Done when |
| --- | --- | --- |
| M1 | Three issues: pin wfe and port the shell core (D4); the sign-in, invite, recover and reset pages; delete `web/ui` and `web/components`. `web/runtime` and `web/host-route-clients.mjs` stay. | The shell tests pass on wfe; `wamn web upload` serves the shell; sign-in reaches the audience list on wamn-dev. |
| M2 | Control screens (§4.1) on real routes, with the project and environment activate/inactivate actions. | A fresh `env apply` + `provision-org --admin` lets the admin invite a user and grant a membership from the browser. |
| M3 | `client_component.rs` emits wfe descriptions (D6); `label` in `wamn.k` and the IR (D7). Receiving's web page renders from descriptions. | Receiving screens work in the browser; no Solid 1 output remains. |
| M4 | Application administration (§4.2): roles grid, users. | A role is created, a permission granted, and a user assigned from the browser; the server refuses what the client hides. |
| M5 | The three `environment.status` answer members (D8) and the splash pages (§5). | Both splashes render tiles from live data on wamn-dev. |
| M6 | Docs: `execution.md` shell rules, `web-operator-client.md` closed to history, operations page for `wamn web upload`. | Plan index updated. |

M1 → M2 → M3 → M4 → M5 → M6, one at a time.

## 8. What stays out of the platform UI

- Appearance review controls, mock scenarios, SSR hydration cases. These belong to wfe examples.
- Any client-side authorization rule. The client hides; the server refuses.
- Browser storage for anything but the session cookie (`__Host-wamn-session`).
- Traffic metrics. A named bead after M5.
