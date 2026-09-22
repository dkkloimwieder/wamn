# Receiving demo

This package is disposable. It exists to judge the generated components once, and it is deleted after the Epic 4 review.
It has no routes, no navigation, and no layout system. Nothing here is the start of an operator product.

Delete it with two steps: remove `web/demo`, and remove its line in [the web README](../README.md).

## What it does

The page signs in with the existing password login, keeps the bearer token in memory, and builds one transport.
It then mounts the generated Receiving components under that transport.
Generated files are never edited.

## Run it

Stand up the local stack first. The notes of Beads `wamn-78or.6` hold the exact commands.
The stack prints a base URL and a route host. Keep both.

```bash
cd web/demo
npm install
WAMN_DEV_ENV_DIR=<the environment directory that holds dev.json> \
WAMN_DEMO_ROUTE_URL=<the base URL the loop printed> \
  npm run dev
```

Open the address that Vite prints. Sign in with the demo account.
`npm run check` runs the TypeScript compiler over this package and the generated components it imports.

## The account

The account is `receiving-demo@wamn.dev`, and its password is the same string.
The identity service requires a password of 15 characters, and this address carries 23.
The account is a demo credential in a disposable local environment.

## Why a proxy

The browser sees one origin, and the local stack has two.
The application host serves plain HTTP and selects its release by the `Host` header, which a browser cannot set.
The identity process serves HTTPS with a certificate it signed itself, which a browser does not trust.
`vite.config.ts` carries `/password` to the issuer and the four model prefixes to the release.

## Seed data

Receiving declares no operation that creates an item, a location, or a purchase order.
Apply the seed file before you expect a table to show rows. The file is named in Beads `wamn-78or.2`.

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
