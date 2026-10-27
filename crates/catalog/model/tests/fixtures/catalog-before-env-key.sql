-- Reconstruct the catalog key spelling for historical upgrade tests.
DO $legacy_catalog_key$
DECLARE
    relation regclass;
BEGIN
    FOR relation IN
        SELECT c.oid::regclass
          FROM pg_class c
          JOIN pg_namespace n ON n.oid = c.relnamespace
          JOIN pg_attribute a ON a.attrelid = c.oid
         WHERE n.nspname = 'catalog' AND c.relkind = 'r'
           AND a.attname = 'env_key' AND NOT a.attisdropped
    LOOP
        EXECUTE format('ALTER TABLE %s RENAME COLUMN env_key TO tenant_id', relation);
    END LOOP;
END
$legacy_catalog_key$;
DROP FUNCTION catalog.connection_upgrade_bindings(text,text);
