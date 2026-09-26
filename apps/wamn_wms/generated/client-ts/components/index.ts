// @generated from the client-contract IR; do not edit.
//
// Components of package `wamn_wms`, one for each operation that has a role
// and a route.

export * from "./inventory.js";
export * from "./inventory_transaction.js";
export * from "./location.js";
export * from "./packaging.js";
export * from "./product.js";

// Every operation of this release has a screen role.

// These selectors read the first page and render no search, because the
// list they read declares no filter on its display field:
// wamn-wms:inventory/adjust@1.0.0 value.inventory_id: wamn-wms:inventory/query@1.0.0
// wamn-wms:inventory/merge@1.0.0 value.from_inventory_id: wamn-wms:inventory/query@1.0.0
// wamn-wms:inventory/merge@1.0.0 value.to_inventory_id: wamn-wms:inventory/query@1.0.0
// wamn-wms:inventory/move@1.0.0 value.inventory_id: wamn-wms:inventory/query@1.0.0
// wamn-wms:inventory/move@1.0.0 value.to_packaging_id: wamn-wms:packaging/query@1.0.0
// wamn-wms:inventory/split@1.0.0 value.from_inventory_id: wamn-wms:inventory/query@1.0.0
// wamn-wms:inventory/split@1.0.0 value.to_packaging_id: wamn-wms:packaging/query@1.0.0
// wamn-wms:packaging/close@1.0.0 value.packaging_id: wamn-wms:packaging/query@1.0.0
// wamn-wms:packaging/relocate@1.0.0 value.packaging_id: wamn-wms:packaging/query@1.0.0
