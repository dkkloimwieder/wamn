# WMS web application

The browser page of WMS. It places the generated WMS components on routes through the [app shell](../../../web/shell/README.md).
This directory is owned code, and the generator never writes it.
[`src/routes.tsx`](src/routes.tsx) is the route table: one screen for each WMS table.

## Run it

Stand up a local stack that serves the WMS release first. The notes of Beads `wamn-78or.6` hold the exact commands.
The stack prints a base URL. Keep it.
Install `web/ui` and `web/runtime` once, as [running tests](../../../docs/operations/running-tests.md#app-shell) describes.

```bash
cd apps/wamn_wms/web
npm install
WAMN_DEV_ENV_DIR=<the environment directory that holds dev.json> \
WAMN_ROUTE_URL=<the base URL the loop printed> \
  npm run dev
```

Open the address that Vite prints, and sign in with an account of that environment.

`npm run build` writes the static files to `dist/`. Nothing serves them yet.
