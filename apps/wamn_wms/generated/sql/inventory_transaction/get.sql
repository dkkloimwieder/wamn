SELECT
    model.created_at,
    model.from_disposition,
    model.from_inventory_id,
    model.from_lifecycle,
    model.from_location_id,
    model.from_packaging_id,
    model.from_product_id,
    model.from_quantity,
    model.id,
    model.inventory_id,
    model.occurred_at,
    model.operation_id,
    model.reason,
    model.to_disposition,
    model.to_inventory_id,
    model.to_lifecycle,
    model.to_location_id,
    model.to_packaging_id,
    model.to_product_id,
    model.to_quantity,
    model.type
FROM inventory_transaction AS model
WHERE model.id = $1::uuid;
