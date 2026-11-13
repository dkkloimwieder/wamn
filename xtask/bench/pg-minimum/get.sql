-- Captured from pg_stat_statements: local 0.1.27, c4, 10 s, 2026-10-17.
-- Only bind parameters differ from the captured SELECT.
-- Random IDs use the unchanged 1000-order seed.
-- chr(58) prevents pgbench from treating seed-prefix colons as variables.
-- The parameter query is outside the replay and is included in pgbench latency and throughput.
-- search_path and statement_timeout are set once on the connection, like the
-- host's warm autocommit read pool. Use the prepared protocol.
\set order_number random(1, 1000)
SELECT md5(('wamn-receiving-seed' || chr(58) || 'order' || chr(58)) || :order_number::integer)::uuid AS id \gset
SELECT
    model.created_at,
    model.created_by,
    model.id,
    model.purchase_order_number,
    model.row_version,
    model.status,
    model.supplier_id,
    model.updated_at,
    model.updated_by
FROM purchase_order AS model
WHERE model.id = :id::uuid;
