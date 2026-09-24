# Demo

This package is disposable. It exists to judge the generated components, and the close of Epic 7 ends it.
Epic 4 used it for the evaluation, Epic 5 read the two fixes that only a browser shows, Epic 6 reads the authored labels, and Epic 7 reads the selectors and the prefilled form.
Epic 15 adds a WMS page, and one switch selects the application.
It has no routes, no navigation, and no layout system. Nothing here is the start of an operator product.

Delete it with two steps: remove `web/demo`, and remove its line in [the web README](../README.md).

## What it does

The page signs in with the existing password login and the cookie carrier of `@wamn/web-runtime`.
The identity service sets the session in an HttpOnly cookie, so the page holds no token and writes nothing to browser storage.
The page writes the selected environment into the address fragment, and a reload renews the session from the renewal cookie.
It then mounts the generated components of one application under one cookie transport.
`WAMN_DEMO_APP` selects the application when Vite starts: `receiving`, which is the default, or `wms`.
The switch selects the proxy prefixes, the page and the account the sign in offers.
Generated files are never edited.

## Run it

Stand up the local stack first. The notes of Beads `wamn-78or.6` hold the exact commands.
The stack prints a base URL and a route host. Keep both.
Run `npm install` in `web/ui` first, because the page renders through it.

```bash
cd web/demo
npm install
WAMN_DEMO_APP=wms \
WAMN_DEV_ENV_DIR=<the environment directory that holds dev.json> \
WAMN_DEMO_ROUTE_URL=<the base URL the loop printed> \
  npm run dev
```

Leave out `WAMN_DEMO_APP` for Receiving. The stack must serve the release of the application you select.

Open the address that Vite prints. Sign in with the demo account.
`npm run check` runs the TypeScript compiler over this package and the generated components of both applications.

## The account

The Receiving account is `receiving-demo@wamn.dev`, and its password is the same string.
The WMS account is `wms-demo@wamn.dev`, and its password is also the same string.
The identity service requires a password of 15 characters. The two addresses carry 23 and 17.
The account is a demo credential in a disposable local environment.

## Why a proxy

The browser sees one origin, and the local stack has two.
The application host serves plain HTTP and selects its release by the `Host` header, which a browser cannot set.
The identity process serves HTTPS with a certificate it signed itself, which a browser does not trust.
`vite.config.ts` carries `/password` to the issuer and the path prefixes of the selected release to that release.
Receiving has five prefixes. WMS has six: `/pallet`, `/inventory`, `/location`, `/product`, `/pallet_quantity` and `/inventory_movement`.

## Seed data

Receiving declares no operation that creates an item, a location, or a purchase order.
Apply a dataset before you expect a table to show rows.

```bash
psql "$TARGET_DATABASE_URL" -f ../../apps/wamn_receiving/tests/fixtures/receiving-seed-small.sql
```

That file is the saved small dataset: 10 items, 10 locations, 10 purchase orders, and 55 lines.
For a larger one, build it with `receiving-seed.sql` and the size you want.
Paging needs more than 100 orders, so the large size shows it.

For WMS, apply the size you want with `wms-seed.sql`. The WMS checklist runs at 1000:

```bash
psql "$TARGET_DATABASE_URL" -v scale=1000 -f ../../apps/wamn_wms/tests/fixtures/wms-seed.sql
```

The large size writes 1000 products, 1000 locations, 1000 pallets with 100 of them held, and 1999 quantity rows.
`wms-seed-small.sql` is the saved small size.

## The evaluation

Fill one row for each component. Write `yes`, `no`, or `n/a`.
The filled table belongs in the notes of Beads `wamn-78or`, because this package is deleted.

| Component | Mounts | Reads | Submits | Refusal on the right field | Revision conflict | Paging | Row link |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `LocationListTable` | | | | | | | |
| `PurchaseOrderQueryTable` | | | | | | | |
| `PurchaseOrderGetDetail` | | | | | | | |
| `PurchaseOrderUpdateForm` | | | | | | | |
| `ReceiptQueryTable` | | | | | | | |
| `ReceiptGetDetail` | | | | | | | |
| `ReceivingLoadReceiptScreenTable` | | | | | | | |
| `ReceivingLoadPurchaseOrderHistoryTable` | | | | | | | |
| `ReceivingRecordReceiptForm` | | | | | | | |

## The WMS checklist

Fill one row for each WMS component at the 1000 seed. Write `yes`, `no`, or `n/a`.
The filled table belongs in the notes of Beads `wamn-nq1b`, because this package is deleted.
A command form sends the revision of the pallet row you select, because the release binds no read that supplies it.

| Component | Mounts | Reads | Submits | Refusal on the right field | Revision conflict | Paging | Row link | Picker searches |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `PalletQueryTable` | | | | | | | | |
| `PalletGetDetail` | | | | | | | | |
| `PalletCreateForm` | | | | | | | | |
| `InventoryMoveForm` | | | | | | | | |
| `InventoryAdjustForm` | | | | | | | | |
| `InventoryMergeForm` | | | | | | | | |
| `InventorySplitForm` | | | | | | | | |
| `InventoryAggregateTable` | | | | | | | | |
| `LocationQueryTable` | | | | | | | | |
| `LocationGetDetail` | | | | | | | | |
| `LocationCreateForm` | | | | | | | | |
| `LocationUpdateForm` | | | | | | | | |
| `ProductQueryTable` | | | | | | | | |
| `ProductGetDetail` | | | | | | | | |
| `ProductCreateForm` | | | | | | | | |
| `ProductUpdateForm` | | | | | | | | |
| `PalletQuantityQueryTable` | | | | | | | | |
| `PalletQuantityGetDetail` | | | | | | | | |
| `InventoryMovementQueryTable` | | | | | | | | |
| `InventoryMovementGetDetail` | | | | | | | | |

## The Epic 5 checks

Two results need the running page, and Beads `wamn-iq82.7` records both.
Read the purchase order history of a selected order, and submit a receipt line with the quantity `0`.
The history draws its rows, and the refused line marks its own quantity control.

## The Epic 6 check

One result needs the running page, and Beads `wamn-c2y5.6` records it.
Read a purchase order table and open the record receipt form.
Each control and each column header states the text that `apps/wamn_receiving/wamn.json` authors, and a field with no authored text keeps its own name.

## The Epic 7 check

One result needs the running page, and Beads `wamn-rm14.7` records it.
Open the record receipt form and choose its purchase order, then its line, then its location, each from a list.
The line list offers the lines of the chosen order alone, the update form chooses a supplier from a list, and a purchase order row opens the receipt form already filled.
No control asks the operator to paste an identity.

## The Epic 10 check

Five results need the running page, and the notes of Beads `wamn-ut5e` record them.
Read one table, submit one form and read its toast, and choose one purchase order through its search and its next page.
Switch to dark mode once. The delete result reads "delete: no Receiving operation; covered by fixture test."
