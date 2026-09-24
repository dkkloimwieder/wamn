INSERT INTO product (id, product_code)
VALUES ($1::uuid, $2::text)
RETURNING
    created_at,
    id,
    product_code,
    row_version;
