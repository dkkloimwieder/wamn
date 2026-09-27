INSERT INTO product (product_code)
VALUES ($1::text)
RETURNING
    created_at,
    id,
    product_code,
    row_version;
