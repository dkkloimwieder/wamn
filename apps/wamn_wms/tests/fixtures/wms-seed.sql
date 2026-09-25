-- Fresh disposable WMS installation only. This creates baseline inventory.
-- Run: psql "$TARGET_DATABASE_URL" -v scale=10 -f wms-seed.sql
\set ON_ERROR_STOP on
\if :{?scale}
\else
  \set scale 10
\endif
BEGIN;
-- Administrative fixture actor for the platform audit stamps.
SET LOCAL app.user_id = '00000000-0000-4000-8000-0000000000f1';
SET LOCAL app.operation = 'admin:seed-wms-fixture';
INSERT INTO wms.product (id, product_code)
SELECT md5('wamn-wms-seed:product:' || n)::uuid, 'SKU-' || lpad(n::text, 5, '0')
FROM generate_series(1, :scale) AS n;
INSERT INTO wms.location (id, location_code)
SELECT md5('wamn-wms-seed:location:' || n)::uuid, 'LOC-' || lpad(n::text, 4, '0')
FROM generate_series(1, :scale) AS n;
INSERT INTO wms.packaging (id, type, code, location_id)
SELECT md5('wamn-wms-seed:packaging:' || n)::uuid, 'tote', 'PKG-' || lpad(n::text, 6, '0'),
       md5('wamn-wms-seed:location:' || n)::uuid
FROM generate_series(1, :scale) AS n;
INSERT INTO wms.inventory (id, packaging_id, product_id, location_id, disposition, quantity)
SELECT md5('wamn-wms-seed:inventory:' || n || ':' || k)::uuid,
       md5('wamn-wms-seed:packaging:' || n)::uuid,
       md5('wamn-wms-seed:product:' || (1 + (((n - 1) / 2 + k - 1) % :scale)))::uuid,
       md5('wamn-wms-seed:location:' || n)::uuid,
       CASE WHEN n % 10 = 0 THEN 'held' ELSE 'available' END,
       (10 + ((n * 7 + k * 13) % 91))::numeric
FROM generate_series(1, :scale) AS n,
     LATERAL generate_series(1, 1 + ((n - 1) % 3)) AS k;
COMMIT;
