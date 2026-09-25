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

## The account and the seed

Sign in with `wms-demo@wamn.dev`. Its password is the same string, because the identity service requires 15 characters or more.
The notes of Beads `wamn-78or.6` hold the commands that create the account in a local environment.
It is a demo credential in a disposable local environment.

A table shows rows only after you apply a dataset. The by-hand checks run at 1000:

```bash
psql "$TARGET_DATABASE_URL" -v scale=1000 -f ../tests/fixtures/wms-seed.sql
```

The large size writes 1000 products, 1000 locations, 1000 pallets with 100 of them held, and 1999 quantity rows.
`wms-seed-small.sql` is the saved small size.
