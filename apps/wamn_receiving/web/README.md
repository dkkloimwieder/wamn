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

## The account and the seed

Sign in with `receiving-demo@wamn.dev`. Its password is the same string, because the identity service requires 15 characters or more.
The notes of Beads `wamn-78or.6` hold the commands that create the account in a local environment.
It is a demo credential in a disposable local environment.

Receiving declares no operation that creates an item, a location or a purchase order, so a table shows rows only after you apply a dataset.
The by-hand checks run at 1000:

```bash
psql "$TARGET_DATABASE_URL" -v scale=1000 -f ../tests/fixtures/receiving-seed.sql
```

The large size writes 1000 items, 1000 locations, 1000 purchase orders and about 500000 lines.
`receiving-seed-small.sql` is the saved small size: 10 items, 10 locations, 10 purchase orders and 55 lines.
Paging needs more than 100 orders, so only the large size shows it.
