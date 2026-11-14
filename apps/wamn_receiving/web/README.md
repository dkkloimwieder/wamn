# Receiving web application

The Receiving web app renders generator-emitted wfe descriptions through the [platform UI shell](../../platform_ui/README.md).
Descriptions supply operation and field labels from the client contract.
Fields with `visible = True` in `wamn.k` appear in generated tables and details.
If a screen has no marked fields, it shows fields other than keys, IDs, audit fields, and timestamps.
They also name the display fields and lookup operations for references to other records.
Application bindings own requests, reference selectors, validation, and navigation.
This directory is owned code, and the generator never writes it.
[`src/routes.tsx`](src/routes.tsx) is the route table.

| Section | Routes |
| --- | --- |
| Purchase orders | The table, a record page, and below the record page its update form, its receiving screen and its history |
| Receipts | The table, a record page, and the receipt form |
| Suppliers | The table and the supplier form |
| Locations | The table |

The receipt form opens from the Receipts table, from a purchase order row, and from a purchase order record page, which fills in the order.
The receiving screen fills the order and line. A location row fills the location in the first receipt line.
Tables use wfe header actions and pagination, with 25 rows per page.
Query tables request server pages. Locations and receiving lines page the returned rows locally.
Forms use separate screens and return to their tables after submission or cancellation.
Record navigation uses links on order numbers and receipt references.
Actions use buttons and share one cell per row. The purchase order screen also offers the supplier change action.
Tables, details, and selectors show reference display text. A failed reference lookup shows `Unavailable`.
The shell shows literal uppercase breadcrumbs after sign-in.

## Run it

Deploy Receiving on a disposable `kind` platform with the [development loop](../../../docs/operations/development-loop.md).
Set `WAMN_ROUTE_URL` to its installed application URL, including the application hostname.
The browser and the development machine must trust the platform certificate.
Install the web packages once with `pnpm install` at the repository root.

```bash
cd apps/wamn_receiving/web
WAMN_ROUTE_URL=https://receiving.example.test pnpm run dev
```

Open the address that Vite prints. Sign in with an account from the installed platform.
Vite forwards `/config.json`, sign-in requests, and `/api` requests to that application URL.
It keeps the `/api` prefix. The installed web service supplies browser configuration.

`pnpm run build` writes the static files to `apps/target/wamn/wamn_receiving/web`.
Use [`wamn web upload`](../../../docs/operations/deployment.md#web-client-files) to publish the client files.

## Application data

Receiving has no operation that creates an item, a location, or a purchase order.
These tables need an imported dataset. The product import route is tracked by `wamn-eulfr.12.9`.
The browser tests retain their screen and paging assertions and fail at that missing import step.
The seed SQL files describe the small and large datasets; the browser tests do not apply SQL in place of the missing product route.
