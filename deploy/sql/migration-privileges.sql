-- The authenticated migration login cannot widen or remove another role's privileges.
-- session_user remains bound even inside a SECURITY DEFINER routine or RESET ROLE.
CREATE FUNCTION platform.refuse_migration_privileges()
RETURNS event_trigger
LANGUAGE plpgsql
SET search_path = pg_catalog
AS $$
BEGIN
    IF session_user ~ '^wamn_migration_[0-9a-f]{40}$' THEN
        RAISE EXCEPTION USING ERRCODE = '42501',
            MESSAGE = 'migration-privilege-change-refused';
    END IF;
END
$$;
ALTER FUNCTION platform.refuse_migration_privileges() OWNER TO wamn_db_owner;
REVOKE ALL ON FUNCTION platform.refuse_migration_privileges() FROM PUBLIC;
CREATE EVENT TRIGGER wamn_migration_privileges
    ON ddl_command_start
    WHEN TAG IN ('GRANT', 'REVOKE', 'ALTER DEFAULT PRIVILEGES')
    EXECUTE FUNCTION platform.refuse_migration_privileges();
