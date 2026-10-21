# Platform UI

This application ports the [shell rules](../../web/shell/README.md) to Solid 2.
[Execution](../../docs/architecture/execution.md) owns those rules.
The application uses wfe components from the `web/wfe` submodule and the shared session and transport in `web/runtime`.
Vite uses the `wfe-source` export condition, so it compiles the pinned source without a separate wfe build.
The build refuses Solid 1 modules and imports from the old UI packages.

The shell core owns addresses, permission filtering, navigation, and query values.
Its tests port the Solid 1 assertions and use stub screens for later Control and administration work.
Receiving and WMS retain their Solid 1 shell until M3.

The scope, address, permission, and query implementations therefore exist in both shells during the migration.
Session renewal and the cookie transport retain one owner in `web/runtime`.
DashboardShell and DashboardPanel frame each screen.
FormView and TableView render forms and record lists through their application-owned bindings.
Nested tables use TableView row details. The application owns expansion and child records.
The sign-in, invitation, recovery, and reset pages use `@wfe/form` with FormView.
Their rules also exist in both shells until M3.
After sign-in, the audience choice replaces the form in the centered identity panel.
It uses wfe's [SidebarGroup](../../web/wfe/docs/layout.md#icon-sidebar) for expanded org and project groups.
Each org lists its held Control entry first, then its projects and held environments.
One audience opens automatically.

Each dashboard uses `Theme` from wfe v0.0.4 with its unchanged light defaults.
WFE owns the theme properties, styles, fonts, and licenses.
This follows [Adopt the components](../../web/wfe/docs/adoption.md) and [Default theme](../../web/wfe/docs/theme.md).
The application owns page height and removes the browser's body margin.

The type check keeps `strict` enabled and omits `exactOptionalPropertyTypes`, which the wfe form source does not support.

From the repository root, run `pnpm --filter @wamn/platform-ui check`, `test`, or `build`.
The build writes `apps/platform_ui/dist`.
The page reads only the org from `/config.json` and offers the audiences that identity returns for that org.
The platform UI is a platform asset with no `wamn.k` or package artifact.
[Building](../../docs/operations/building.md#platform-ui) owns its build record.
[Deployment](../../docs/operations/deployment.md#platform-ui-files) owns upload and routing at `control.wamn.dev`.
