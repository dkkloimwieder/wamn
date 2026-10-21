# Platform UI

This application ports the [shell rules](../../web/shell/README.md) to Solid 2.
[Execution](../../docs/architecture/execution.md) owns those rules.
The application uses wfe components from the `web/wfe` submodule and the shared session and transport in `web/runtime`.
Vite uses the `wfe-source` export condition, so it compiles the pinned source without a separate wfe build.
The runtime and compiler use Solid `2.0.0-rc.16`.
The build refuses Solid 1 modules and imports from the old UI packages.

The shell core owns addresses, permission filtering, navigation, and query values.
Its tests port the Solid 1 assertions and use stub screens for later Control and administration work.
Receiving and WMS retain their Solid 1 shell until M3.

The scope, address, permission, and query implementations therefore exist in both shells during the migration.
Session renewal and the cookie transport retain one owner in `web/runtime`.
DashboardShell and DashboardPanel frame each screen.
FormView and TableView render forms and record lists through their application-owned bindings.
Create project and Invite user appear beside the table controls and refresh through `toolbarActions`.
Each action opens its own form screen and returns to the table after submission.
No data entry action opens a modal.

Records supplies wfe pagination for every table, with 25 rows per page and the total returned row count.
The application selects each page from the returned rows and limits the current page when rows disappear.
These APIs return complete arrays without server pagination: `project.list`, `user.list`, `environment.list`, `member.list`, `control.mine`, `environment.status`, and `release.profile`.
The identity endpoint `/password/environments` also lacks server pagination.
Workloads, conditions, and live releases come from `environment.status`.
Operations, roles, reaches, statements, and relations come from `release.profile`.
Membership and audience tables use the same pagination.

The Environment screen links to separate screens for Workloads, Live releases, Operations, and Roles.
Each of these screens holds one table in the full panel.
Rows open screens for memberships, workload conditions, reaches, statements, and relations.
Each screen gives its tables the full panel width. Membership grants stay inline, with one command per change.
The sign-in, invitation, recovery, and reset pages use wfe EntryPanel with `@wfe/form` and FormView.
Their rules also exist in both shells until M3.
After sign-in, an audience table replaces the form.
Its columns show the org, project, and environment.
Breadcrumbs appear on screens after audience selection, but identity screens and audience choice omit them.
One audience opens automatically.

Each dashboard uses `Theme` from wfe v0.0.5 with its unchanged light defaults.
WFE owns the theme properties, styles, fonts, and licenses.
This follows [Adopt the components](../../web/wfe/docs/adoption.md) and [Default theme](../../web/wfe/docs/theme.md).
The application owns page height. EntryPanel owns the centered identity form layout.

The type check keeps `strict` enabled and omits `exactOptionalPropertyTypes`, which the wfe form source does not support.

From the repository root, run `pnpm --filter @wamn/platform-ui check`, `test`, or `build`.
The build writes `apps/platform_ui/dist`.
The page reads only the org from `/config.json` and offers the audiences that identity returns for that org.
The platform UI is a platform asset with no `wamn.k` or package artifact.
[Building](../../docs/operations/building.md#platform-ui) owns its build record.
[Deployment](../../docs/operations/deployment.md#platform-ui-files) owns upload and routing at `control.wamn.dev`.
