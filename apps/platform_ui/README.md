# Platform UI

This application ports the [shell rules](../../web/shell/README.md) to Solid 2.
[Execution](../../docs/architecture/execution.md) owns those rules.
The application uses wfe components from the `web/wfe` submodule and the shared session and transport in `web/runtime`.
Vite uses the `wfe-source` export condition, so it compiles the pinned source without a separate wfe build.
The runtime and compiler use Solid `2.0.0-rc.16`.
The build refuses Solid 1 modules and imports from the old UI packages.

The shell core owns addresses, permission filtering, navigation, and query values.
Its tests port the Solid 1 assertions and use stub screens for later Control and administration work.
Receiving uses this shell with generated wfe descriptions. WMS retains the Solid 1 shell.
The shell passes permitted route actions through `ScreenProps.actions`, with route parameters filled.
Receiving renders those actions through the table `toolbarActions` binding.

The scope, address, permission, and query implementations therefore exist in both shells during the migration.
Session renewal and the cookie transport retain one owner in `web/runtime`.
DashboardShell and DashboardPanel frame dashboard screens.
Unknown addresses and missing details show one standalone EntryPanel with a heading, one sentence, and a return link.
These pages have no dashboard, sidebar, header, breadcrumbs, or form.
FormView and TableView render forms and record lists through their application-owned bindings.
Create project, Invite user, and Grant membership appear beside the table controls through `toolbarActions`.
Each action opens its own form screen and returns to the owning table after submission.
Cancel shares the form action layout and returns to the same table.
No data entry action opens a modal.

Records supplies wfe pagination for every table, with 25 rows per page and the total returned row count.
The application selects each page from the returned rows and limits the current page when rows disappear.
These APIs return complete arrays without server pagination: `project.list`, `user.list`, `environment.list`, `member.list`, `control.mine`, `environment.status`, and `release.profile`.
The identity endpoint `/password/environments` also lacks server pagination.
Workloads, conditions, and live releases come from `environment.status`.
Operations, roles, reaches, statements, and relations come from `release.profile`.
Membership and audience tables use the same pagination.
Refresh requests fresh data from each table’s owning API, including `control.mine` for project administrators.

The left pane follows the address through organization, project, and environment contexts, including direct links, history navigation, and reloads.
SidebarContextSwitcher shows the current context, searches sibling choices, and returns to the parent.
At the organization level, Projects expands to held projects from `control.mine`. Only org admins see Users.
Project context shows expandable Environments and Members. Environment context shows Workloads, Live releases, Operations, Roles, and Members.
Environment membership screens filter members and scope grants and revocations to the current environment.
A viewer with one held project opens that project directly when entering Control. The parent action still returns to the organization.
The Users group contains Project access and Memberships links for the addressed user.

Tables with actions retain separate screens with `scroll="content"` and no outer padding.
Read-only reference tables expand through wfe `rowDetails`: conditions under Workloads, operations under Roles, and reaches under Operations.
Reaches expand to operations and statements. Statements expand to relations, which expand to read and write fields.
These reference tables have no separate screens or routes.
Child tables use wfe defaults without controls or refresh. A child with more than 25 rows shows the shared pagination footer.
Each column shows one fact. Parent tables show counts for growing lists.
Record links use native anchors with the wfe accent color.

Project, User, Environment, and membership records place normal-sized actions beside the title in one shared group.
The visible lifecycle verbs are Activate and Inactivate. API identifiers retain their existing names.
State cells contain only the value. Pods cells contain only the count.

Sign-in, invitation, recovery, reset, and no-access pages use standalone wfe EntryPanel pages with distinct titles.
Theme and a main landmark wrap each default viewport panel. These pages contain no dashboard Frame.
Identity forms use `@wfe/form` and FormView. The no-access panel contains a return link.
Their rules also exist in both shells until M3.

After sign-in, an audience table replaces the form.
Its columns show the org, project, and environment.
Breadcrumbs appear on dashboard screens after audience selection. Every breadcrumb uses literal uppercase text, including record names.
Identity screens and audience choice omit breadcrumbs.

An audience with no available screens shows a named empty-state card and a link to audience selection.
One audience opens automatically.

Each dashboard uses `Theme` from wfe v0.0.6 with its unchanged light defaults.
Dependency pins remain at the v0.0.5 baseline.
WFE owns the theme properties, styles, fonts, and licenses.
This follows [Adopt the components](../../web/wfe/docs/adoption.md) and [Default theme](../../web/wfe/docs/theme.md).
The application owns dashboard height. EntryPanel owns the centered identity form layout.

The type check keeps `strict` enabled and omits `exactOptionalPropertyTypes`, which the wfe form source does not support.

From the repository root, run `pnpm --filter @wamn/platform-ui check`, `test`, or `build`.
The build writes `apps/platform_ui/dist`.
The page reads only the org from `/config.json` and offers the audiences that identity returns for that org.
The platform UI is a platform asset with no `wamn.k` or package artifact.
[Building](../../docs/operations/building.md#platform-ui) owns its build record.
[Deployment](../../docs/operations/deployment.md#platform-ui-files) owns upload and routing at `control.wamn.dev`.

To test these UI changes with existing generated clients, run the commands directly from the repository root:

```sh
pnpm --filter @wamn/platform-ui exec tsc --noEmit
pnpm --filter @wamn/platform-ui exec vitest run
pnpm --filter @wamn/platform-ui exec vite build
pnpm exec prettier --check 'apps/platform_ui/src/**/*.{ts,tsx,css}' 'apps/platform_ui/test/**/*.{ts,tsx,mjs}'
```

The package scripts also generate host clients through Cargo.
The direct commands above do not run Cargo or the live browser fixture.
