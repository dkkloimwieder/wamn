-- The record_receipt replay (perf-arch.md M7). The statement sequence was
-- captured from pg_stat_statements: local f025f5876, c16, 60 s, 2026-10-18.
-- gcp.sh puts apps/wamn_receiving/command/record_receipt/record_receipt.sql
-- in place of the marker line, with its four parameters bound as below.
-- Finish precedes emit, as operation_transaction.rs queues the commit batch.
-- The slot SELECT runs before BEGIN and is part of pgbench latency.
-- receipt_slot reserves, per client, one order and one window of lines for
-- each receipt. A client owns its orders, and each line receives at most
-- 4 receipts of quantity 1, so no receipt exceeds the remaining quantity.
-- operation and claim_operation come from the generated operation contract.
\set runner NULL
\set sequence :sequence + 1
SELECT ''::text AS role, s.purchase_order_id, s.line,
       format('bench-%s-%s-%s', :run::bigint, :client_id::int, :sequence::bigint) AS request_id,
       decode(repeat('ab', 32), 'hex') AS request_hash,
       json_build_object('value', json_build_object('outcome', 'recorded'))::text AS result,
       json_build_object('run', format('bench-%s-%s-%s', :run::bigint, :client_id::int, :sequence::bigint),
                         'root', format('bench-%s-%s-%s', :run::bigint, :client_id::int, :sequence::bigint),
                         'depth', 0)::text AS causation
  FROM public.receipt_slot AS s
 WHERE s.client = :client_id AND s.slot = :sequence \gset
\startpipeline
BEGIN ISOLATION LEVEL READ COMMITTED;
SELECT set_config('statement_timeout', :statement_timeout, true), set_config('search_path', COALESCE(:schema, current_setting('search_path')), true), set_config('app.runner', COALESCE(:runner, current_setting('app.runner', true)), true), set_config('app.role', :role, true), set_config('app.user_id', :principal, true), set_config('app.operation', :operation, true);
INSERT INTO app_system.write_log (operation, idempotency_key, request) VALUES (:claim_operation::text, :request_id::text, :request_hash::bytea) ON CONFLICT (operation, idempotency_key) DO NOTHING RETURNING idempotency_key;
\endpipeline
SELECT status
FROM purchase_order
WHERE id = :purchase_order_id::uuid
FOR UPDATE;
-- record_receipt.sql
\startpipeline
WITH finished AS (UPDATE app_system.write_log SET result = :result::text WHERE operation = :claim_operation::text AND idempotency_key = :request_id::text AND result IS NULL RETURNING idempotency_key) SELECT 1 / count(*)::integer FROM finished;
SELECT pg_logical_emit_message(true, 'wamn.causation', :causation::text);
COMMIT;
\endpipeline
