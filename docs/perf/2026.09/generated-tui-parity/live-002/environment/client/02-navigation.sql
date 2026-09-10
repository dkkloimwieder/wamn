SELECT json_build_object(
  'first_order', (SELECT id FROM receiving.purchase_order ORDER BY created_at, id LIMIT 1),
  'location_index', (SELECT position FROM (
    SELECT id, row_number() OVER (ORDER BY location_code, id) - 1 AS position
    FROM receiving.location) AS location WHERE id = 'a9dd51e7-c60e-4246-9bd2-0821e7ba6db7'),
  'location_count', (SELECT count(*) FROM receiving.location));
