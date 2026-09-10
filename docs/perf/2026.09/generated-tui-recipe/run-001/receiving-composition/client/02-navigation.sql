SELECT json_build_object(
  'first_order', (SELECT id FROM receiving.purchase_order ORDER BY created_at, id LIMIT 1),
  'location_index', (SELECT position FROM (
    SELECT id, row_number() OVER (ORDER BY location_code, id) - 1 AS position
    FROM receiving.location) AS location WHERE id = '5e9a3e3c-5c62-4f89-b01d-c0b44b54eced'),
  'location_count', (SELECT count(*) FROM receiving.location));
