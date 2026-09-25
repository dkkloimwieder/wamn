# Receiving web application

The browser page of Receiving. It places the generated Receiving components on routes through the [app shell](../../../web/shell/README.md).
This directory is owned code, and the generator never writes it.
[`src/routes.tsx`](src/routes.tsx) is the route table.

| Section | Routes |
| --- | --- |
| Purchase orders | The table, a record page, and below the record page its update form, its receiving screen and its history |
| Receipts | The table, a record page, and the receipt form |
| Suppliers | The table and the supplier form |
| Locations | The table |

The receipt form opens from the Receipts table, from a purchase order row, and from a purchase order record page, which fills in the order.
The receiving screen and location rows do not open it yet, because their generated fill writes a receipt line in the wrong shape (`wamn-yviq`).

## Run it

Stand up a local stack that serves the Receiving release first. The notes of Beads `wamn-78or.6` hold the exact commands.
The stack prints a base URL. Keep it.
Install `web/ui` and `web/runtime` once, as [running tests](../../../docs/operations/running-tests.md#app-shell) describes.

```bash
cd apps/wamn_receiving/web
pnpm install
WAMN_DEV_ENV_DIR=<the environment directory that holds dev.json> \
WAMN_ROUTE_URL=<the base URL the loop printed> \
  pnpm run dev
```

Open the address that Vite prints, and sign in with an account of that environment.

`pnpm run build` writes the static files to `dist/`. Nothing serves them yet.
