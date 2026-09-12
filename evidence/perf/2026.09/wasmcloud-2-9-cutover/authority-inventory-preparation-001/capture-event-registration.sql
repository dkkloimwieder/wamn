-- Read-only, one-relation subset of protected_relations_live::generate_rows.
-- The denial-matrix owner has already installed its fresh database named wamn.
BEGIN TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY;
SET LOCAL search_path = pg_catalog;
SET LOCAL statement_timeout = '10s';
WITH target AS (
    SELECT c.oid, c.relowner, c.relacl, c.relrowsecurity, c.relforcerowsecurity
    FROM pg_class AS c
    JOIN pg_namespace AS n ON n.oid = c.relnamespace
    WHERE n.nspname = 'catalog' AND c.relname = 'event_registrations'
      AND c.relkind IN ('r', 'p')
), table_writes AS (
    SELECT CASE acl.grantee WHEN 0 THEN 'PUBLIC'
               ELSE pg_get_userbyid(acl.grantee) END AS role,
           lower(acl.privilege_type) AS operation
    FROM target AS t
    CROSS JOIN LATERAL aclexplode(COALESCE(t.relacl, acldefault('r', t.relowner))) AS acl
    WHERE acl.grantee <> t.relowner
      AND acl.privilege_type IN ('INSERT', 'UPDATE', 'DELETE', 'TRUNCATE')
), column_writes AS (
    SELECT CASE acl.grantee WHEN 0 THEN 'PUBLIC'
               ELSE pg_get_userbyid(acl.grantee) END AS role,
           lower(acl.privilege_type) || '(' || a.attname || ')' AS operation
    FROM target AS t
    JOIN pg_attribute AS a ON a.attrelid = t.oid
      AND a.attnum > 0 AND NOT a.attisdropped AND a.attacl IS NOT NULL
    CROSS JOIN LATERAL aclexplode(a.attacl) AS acl
    WHERE acl.grantee <> t.relowner
      AND acl.privilege_type IN ('INSERT', 'UPDATE')
)
SELECT json_build_object(
    'schema', 'wamn-event-registration-write-capture/v1',
    'database', current_database(),
    'server_version_num', current_setting('server_version_num')::integer,
    'transaction_read_only', current_setting('transaction_read_only')::boolean,
    'relation', 'catalog.event_registrations',
    'relation_count', (SELECT count(*) FROM target),
    'owner', (SELECT pg_get_userbyid(relowner) FROM target),
    'rls_enabled', (SELECT relrowsecurity FROM target),
    'rls_forced', (SELECT relforcerowsecurity FROM target),
    'nonowner_table_writes', COALESCE((
        SELECT json_agg(json_build_object('role', role, 'operation', operation)
                        ORDER BY role, operation) FROM table_writes
    ), '[]'::json),
    'nonowner_column_writes', COALESCE((
        SELECT json_agg(json_build_object('role', role, 'operation', operation)
                        ORDER BY role, operation) FROM column_writes
    ), '[]'::json),
    'guest_select', has_table_privilege('wamn_app', 'catalog.event_registrations', 'SELECT'),
    'guest_table_update', has_table_privilege('wamn_app', 'catalog.event_registrations', 'UPDATE'),
    'guest_any_column_update', has_any_column_privilege('wamn_app', 'catalog.event_registrations', 'UPDATE'),
    'guest_tenant_id_update', has_column_privilege('wamn_app', 'catalog.event_registrations', 'tenant_id', 'UPDATE')
);
ROLLBACK;
