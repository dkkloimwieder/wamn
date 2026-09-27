-- WMS demo and test data, in three sizes.
--
-- WMS migrations insert no rows. This file makes the products, locations,
-- packagings and stock that a client reads, moves, adjusts, merges and splits.
--
-- The size is one variable. Small is 10. Medium is 100. Large is 1000
-- products, locations and packagings. A packaging carries one to three quantity
-- rows, so the large size writes 1999 quantity rows.
--
--   psql "$TARGET_DATABASE_URL" -v scale=10 -f wms-seed.sql
--   psql "$TARGET_DATABASE_URL" -v scale=100 -f wms-seed.sql
--   psql "$TARGET_DATABASE_URL" -v scale=1000 -f wms-seed.sql
--
-- Every value comes from a row number. One size always writes the same rows.
-- The stamp trigger of the release sets the packaging stamps. A fresh schema has
-- no trigger, so the file also states them.
--
-- A second run of one size states a unique violation. The identifiers repeat
-- by design. To change the size, empty the tables in the same command:
--
--   psql "$TARGET_DATABASE_URL" -v reset=1 -v scale=100 -f wms-seed.sql
--
-- The reset also removes every transaction, because a transaction names a
-- packaging, and the write log rows of WMS.
--
-- Every packaging is a pallet. Every quantity row comes with the transaction
-- that made its stock appear: a row with no from side, so the balance equals
-- the sum of its transactions from the start.

\set ON_ERROR_STOP on
\if :{?scale}
\else
  \set scale 10
\endif

BEGIN;

-- The record history trigger requires an actor.
SELECT set_config('app.user_id', '770df186-ac15-579e-b46b-c297cae2011b', true),
       set_config('app.tenant_id', 'wms-route-auth', true),
       set_config('app.operation', 'admin:seed-wms-fixture', true);

\if :{?reset}
TRUNCATE wms.inventory_transaction,
         wms.packaging_quantity, wms.packaging,
         wms.product,
         wms.location;
DELETE FROM app_system.write_log WHERE operation LIKE 'wamn-wms:%';
\endif

INSERT INTO wms.product (id, product_code)
SELECT md5('wamn-wms-seed:product:' || n)::uuid,
       'SKU-' || lpad(n::text, 5, '0')
FROM generate_series(1, :scale) AS n;

INSERT INTO wms.location (id, location_code)
SELECT md5('wamn-wms-seed:location:' || n)::uuid,
       'LOC-' || lpad(n::text, 4, '0')
FROM generate_series(1, :scale) AS n;

-- Packaging n sits in location n. Every tenth packaging is held, so a filter on
-- status has something to separate. The rest are available.
INSERT INTO wms.packaging
  (id, packaging_code, type, location_id, status, created_at, created_by, updated_at, updated_by)
SELECT md5('wamn-wms-seed:packaging:' || n)::uuid,
       'PAL-' || lpad(n::text, 6, '0'),
       'pallet',
       md5('wamn-wms-seed:location:' || n)::uuid,
       CASE WHEN n % 10 = 0 THEN 'held' ELSE 'available' END,
       transaction_timestamp(),
       current_setting('app.user_id')::uuid,
       transaction_timestamp(),
       current_setting('app.user_id')::uuid
FROM generate_series(1, :scale) AS n;

-- Packaging n carries 1 to 3 rows, in the status of the packaging. Its products
-- are consecutive, and the first one is shared by packagings 2m-1 and 2m. Each
-- such pair is a merge whose stock adds to a row of the same product and
-- status. The pair 10m-1 and 10m differs in status.
INSERT INTO wms.packaging_quantity (id, packaging_id, product_id, status, quantity)
SELECT md5('wamn-wms-seed:quantity:' || n || ':' || k)::uuid,
       md5('wamn-wms-seed:packaging:' || n)::uuid,
       md5('wamn-wms-seed:product:' || (1 + (((n - 1) / 2 + k - 1) % :scale)))::uuid,
       CASE WHEN n % 10 = 0 THEN 'held' ELSE 'available' END,
       (10 + ((n * 7 + k * 13) % 91))::numeric
FROM generate_series(1, :scale) AS n,
     LATERAL generate_series(1, 1 + ((n - 1) % 3)) AS k;

INSERT INTO wms.inventory_transaction
  (id, product_id, quantity, to_packaging_id, to_status, occurred_at, created_at, created_by)
SELECT md5('wamn-wms-seed:transaction:' || quantity.id)::uuid,
       quantity.product_id,
       quantity.quantity,
       quantity.packaging_id,
       quantity.status,
       transaction_timestamp(),
       transaction_timestamp(),
       current_setting('app.user_id')::uuid
FROM wms.packaging_quantity AS quantity;

COMMIT;

SELECT 'products' AS relation, count(*) FROM wms.product
UNION ALL SELECT 'locations', count(*) FROM wms.location
UNION ALL SELECT 'packagings', count(*) FROM wms.packaging
UNION ALL SELECT 'held packagings', count(*) FROM wms.packaging WHERE status = 'held'
UNION ALL SELECT 'quantities', count(*) FROM wms.packaging_quantity
UNION ALL SELECT 'transactions', count(*) FROM wms.inventory_transaction;
