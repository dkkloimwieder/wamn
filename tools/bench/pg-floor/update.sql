-- Captured from pg_stat_statements: local 0.1.27, c4, 10 s, 2026-10-17.
-- Normalized constants are restored from their owning Rust/SQL statements.
-- Finish precedes emit, as operation_transaction.rs queues the commit batch.
-- Each client randomly selects from its own seed slice, as wamn-bench does.
-- The extra SELECT prepares UUID/text parameters and the current row version.
-- It runs before BEGIN and is included in pgbench latency and throughput.
-- The finish parameter uses the selected row as a representative JSON value;
-- its audit timestamps are the pre-update values. The hash is 32 fixed bytes.
-- Unique request IDs prevent retries. Every update changes the supplier.
-- operation and claim_operation come from the generated operation contract.
\set runner NULL
\set sequence :sequence + 1
\set order_number :client_id + 1 + :clients * random(0, (999 - :client_id) / :clients)
SELECT ''::text AS role, p.id, p.row_version AS expected_row_version,
       true AS supplier_present,
       (SELECT md5(('wamn-receiving-seed' || chr(58) || 'supplier' || chr(58)) || (n % 5 + 1))::uuid
          FROM generate_series(1, 5) AS n
         WHERE md5(('wamn-receiving-seed' || chr(58) || 'supplier' || chr(58)) || n)::uuid = p.supplier_id) AS supplier_id,
       format('bench-%s-%s-%s', :run::bigint, :client_id::int, :sequence::bigint) AS request_id,
       decode(repeat('ab', 32), 'hex') AS request_hash,
       json_build_object('value', row_to_json(p))::text AS result,
       json_build_object('run', format('bench-%s-%s-%s', :run::bigint, :client_id::int, :sequence::bigint),
                         'root', format('bench-%s-%s-%s', :run::bigint, :client_id::int, :sequence::bigint),
                         'depth', 0)::text AS causation
  FROM receiving.purchase_order AS p
 WHERE p.id = md5(('wamn-receiving-seed' || chr(58) || 'order' || chr(58)) || :order_number::integer)::uuid \gset
\startpipeline
BEGIN ISOLATION LEVEL READ COMMITTED;
SELECT set_config('statement_timeout', :statement_timeout, true), set_config('search_path', COALESCE(:schema, current_setting('search_path')), true), set_config('app.runner', COALESCE(:runner, current_setting('app.runner', true)), true), set_config('app.role', :role, true), set_config('app.user_id', :principal, true), set_config('app.operation', :operation, true);
INSERT INTO app_system.write_log (operation, idempotency_key, request) VALUES (:claim_operation::text, :request_id::text, :request_hash::bytea) ON CONFLICT (operation, idempotency_key) DO NOTHING RETURNING idempotency_key;
\endpipeline
WITH target AS MATERIALIZED (
    SELECT id, row_version
    FROM purchase_order
    WHERE id = :id::uuid
    FOR UPDATE
),
updated AS (
    UPDATE purchase_order AS model
    SET
        supplier_id = CASE WHEN :supplier_present::boolean THEN :supplier_id::uuid ELSE model.supplier_id END,
        row_version = CASE
            WHEN (:supplier_present::boolean AND :supplier_id::uuid::text IS DISTINCT FROM model.supplier_id::text)
            THEN model.row_version + 1
            ELSE model.row_version
        END
    FROM target
    WHERE model.id = target.id
      AND target.row_version = :expected_row_version::int4
    RETURNING
    model.created_at,
    model.created_by,
    model.id,
    model.purchase_order_number,
    model.row_version,
    model.status,
    model.supplier_id,
    model.updated_at,
    model.updated_by
)
SELECT
    CASE
        WHEN NOT EXISTS (SELECT 1 FROM target) THEN 'not_found'
        WHEN NOT EXISTS (SELECT 1 FROM updated) THEN 'concurrency_conflict'
        ELSE 'updated'
    END AS outcome,
    (SELECT target.row_version FROM target) AS observed_row_version,
    updated.created_at,
    updated.created_by,
    updated.id,
    updated.purchase_order_number,
    updated.row_version,
    updated.status,
    updated.supplier_id,
    updated.updated_at,
    updated.updated_by
FROM (SELECT 1) AS singleton
LEFT JOIN updated ON TRUE;
\startpipeline
WITH finished AS (UPDATE app_system.write_log SET result = :result::text WHERE operation = :claim_operation::text AND idempotency_key = :request_id::text AND result IS NULL RETURNING idempotency_key) SELECT 1 / count(*)::integer FROM finished;
SELECT pg_logical_emit_message(true, 'wamn.causation', :causation::text);
COMMIT;
\endpipeline
