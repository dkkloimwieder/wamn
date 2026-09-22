-- Receiving demo and test data, in three sizes.
--
-- Receiving declares no operation that creates an item, a location, or a
-- purchase order. Its migrations insert no rows. This file makes the rows that
-- a client reads and receives against.
--
-- The size is one variable. Small is 10. Medium is 100. Large is 1000 items,
-- locations, and purchase orders. An order carries 1 to that many lines, so
-- the large size writes about 500000 lines. The supplier count stays at five
-- at every size, because every order names one of five.
--
--   psql "$TARGET_DATABASE_URL" -v scale=10 -f receiving-seed.sql
--
-- Every value comes from a row number. One size always writes the same rows.
-- Build a size once. Save it with pg_dump. Apply the saved file afterward:
--
--   pg_dump "$TARGET_DATABASE_URL" --data-only --schema=receiving -f saved.sql
--
-- A saved file needs the three settings below, because the record history
-- trigger requires an actor. receiving-seed-small.sql carries them.
--
-- A second run of one size states a unique violation. The identifiers repeat
-- by design. To change the size, empty the tables in the same command:
--
--   psql "$TARGET_DATABASE_URL" -v reset=1 -v scale=100 -f receiving-seed.sql
--
-- The reset removes every receipt, because a receipt names a line.

\set ON_ERROR_STOP on
\if :{?scale}
\else
  \set scale 10
\endif

BEGIN;

SELECT set_config('app.user_id', '770df186-ac15-579e-b46b-c297cae2011b', true),
       set_config('app.tenant_id', 'receiving-route-auth', true),
       set_config('app.operation', 'admin:seed-receiving-fixture', true);

\if :{?reset}
TRUNCATE receiving.receipt_line, receiving.receipt, receiving.record_receipt_command,
         receiving.purchase_order_line, receiving.purchase_order,
         receiving.supplier, receiving.supplier_command,
         receiving.item, receiving.location;
\endif

INSERT INTO receiving.item (id, item_number)
SELECT md5('wamn-receiving-seed:item:' || n)::uuid,
       'ITEM-' || lpad(n::text, 4, '0')
FROM generate_series(1, :scale) AS n;

INSERT INTO receiving.location (id, location_code)
SELECT md5('wamn-receiving-seed:location:' || n)::uuid,
       'DOCK-' || lpad(n::text, 4, '0')
FROM generate_series(1, :scale) AS n;

-- Five suppliers at every size, because an order names one of five. They are
-- written before the orders, because purchase_order.supplier_id references
-- this table.
INSERT INTO receiving.supplier (id, name)
SELECT md5('wamn-receiving-seed:supplier:' || n)::uuid,
       'SUPPLIER-' || lpad(n::text, 4, '0')
FROM generate_series(1, 5) AS n;

-- Every fifth order is complete. Every seventh is cancelled. A filter and a
-- sort therefore have something to separate. The rest stay open.
INSERT INTO receiving.purchase_order (id, purchase_order_number, supplier_id, status)
SELECT md5('wamn-receiving-seed:order:' || n)::uuid,
       'PO-' || lpad(n::text, 4, '0'),
       md5('wamn-receiving-seed:supplier:' || (1 + (n % 5)))::uuid,
       CASE WHEN n % 5 = 0 THEN 'complete'
            WHEN n % 7 = 0 THEN 'cancelled'
            ELSE 'open' END
FROM generate_series(1, :scale) AS n;

-- An order carries one line for its own number, wrapped at the size. The
-- product of a line walks the item list, so two orders rarely repeat.
INSERT INTO receiving.purchase_order_line
  (id, purchase_order_id, line_number, item_id, ordered_quantity, received_quantity)
SELECT md5('wamn-receiving-seed:line:' || n || ':' || k)::uuid,
       md5('wamn-receiving-seed:order:' || n)::uuid,
       k,
       md5('wamn-receiving-seed:item:' || (1 + ((n * 7 + k) % :scale)))::uuid,
       (5 + ((n + k) % 20))::numeric,
       0::numeric
FROM generate_series(1, :scale) AS n,
     LATERAL generate_series(1, 1 + ((n - 1) % :scale)) AS k;

COMMIT;

SELECT 'items' AS relation, count(*) FROM receiving.item
UNION ALL SELECT 'locations', count(*) FROM receiving.location
UNION ALL SELECT 'suppliers', count(*) FROM receiving.supplier
UNION ALL SELECT 'orders', count(*) FROM receiving.purchase_order
UNION ALL SELECT 'lines', count(*) FROM receiving.purchase_order_line;
