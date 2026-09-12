SELECT json_build_object(
  'first_order', (SELECT id FROM receiving.purchase_order ORDER BY created_at, id LIMIT 1),
  'location_index', (SELECT position FROM (
    SELECT id, row_number() OVER (ORDER BY location_code, id) - 1 AS position
    FROM receiving.location) AS location WHERE id = '17cf6a33-bce0-466d-ad74-0898a139370c'),
  'location_count', (SELECT count(*) FROM receiving.location));
