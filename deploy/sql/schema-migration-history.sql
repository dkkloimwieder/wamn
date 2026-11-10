-- This DDL is outside the frozen initialization digest.
CREATE TABLE @schema@.schema_migration (
    ordinal int PRIMARY KEY CHECK (ordinal > 0),
    relative_path text NOT NULL UNIQUE CHECK (relative_path ~ '^migrations/@scope@/[0-9]{4}_[a-z0-9_]+\.sql$'),
    sha256 text NOT NULL CHECK (sha256 ~ '^sha256:[0-9a-f]{64}$'),
    outcome text NOT NULL CHECK (outcome IN ('started', 'applied')),
    applied_at timestamptz,
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL,
    CHECK ((outcome = 'applied') = (applied_at IS NOT NULL))
);
ALTER TABLE @schema@.schema_migration OWNER TO @owner@;

CREATE FUNCTION @schema@.guard_schema_migration()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'schema-migration-immutable';
    END IF;
    IF ROW(NEW.ordinal, NEW.relative_path, NEW.sha256)
       IS DISTINCT FROM ROW(OLD.ordinal, OLD.relative_path, OLD.sha256)
       OR OLD.outcome <> 'started' OR NEW.outcome <> 'applied' THEN
        RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'schema-migration-immutable';
    END IF;
    RETURN NEW;
END
$$;
ALTER FUNCTION @schema@.guard_schema_migration() OWNER TO @owner@;
REVOKE ALL ON FUNCTION @schema@.guard_schema_migration() FROM PUBLIC;
CREATE TRIGGER schema_migration_immutable
    BEFORE UPDATE OR DELETE ON @schema@.schema_migration
    FOR EACH ROW EXECUTE FUNCTION @schema@.guard_schema_migration();
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON @schema@.schema_migration
    FOR EACH ROW EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');
