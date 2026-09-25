// @generated from the client-contract IR; do not edit.
//
// Components of package `wamn_receiving`, one for each operation that has a role
// and a route.

export * from "./location.js";
export * from "./purchase_order.js";
export * from "./receipt.js";
export * from "./receiving.js";
export * from "./supplier.js";

// Every operation of this release has a screen role.

// These selectors read the first page and render no search, because the
// list they read declares no filter on its display field:
// wamn-receiving:purchase-order/update@1.0.0 change.supplier_id: wamn-receiving:supplier/query@1.0.0
// wamn-receiving:receiving/record-receipt@1.0.0 value.line[].location_id: wamn-receiving:location/list@1.0.0
// wamn-receiving:receiving/record-receipt@1.0.0 value.line[].purchase_order_line_id: wamn-receiving:receiving/load-receipt-screen@1.0.0

// These tables get no table definition, for the reason beside each:
// wamn-receiving:location/list@1.0.0: it declares no page limit
// wamn-receiving:receiving/load-purchase-order-history@1.0.0: it states no `lists`, so its rows have no row id
// wamn-receiving:receiving/load-receipt-screen@1.0.0: it declares no page limit

// These table columns show the record key, because the model they name
// serves no list whose rows open a record read that returns its text:
// wamn-receiving:purchase-order/query@1.0.0 supplier_id: supplier
