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
The sign-in, invitation, recovery, and reset pages use `@wfe/form` with wfe fields.
Their rules also exist in both shells until M3.

The type check keeps `strict` enabled and omits `exactOptionalPropertyTypes`, which the wfe form source does not support.

From the repository root, run `pnpm --filter @wamn/platform-ui check`, `test`, or `build`.
The build writes `apps/platform_ui/dist`.
The page reads the deployment scope from `/config.json` through the same contract as the Solid 1 shell.
`wamn web upload` currently accepts only web files from package artifacts that the deployed release names.
The platform UI needs that packaging connection before the command can upload it.
