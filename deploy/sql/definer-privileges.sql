-- A definer function executes only with the database owner's authority.
DO $definers$
DECLARE routine record; grantee record; owner_name name;
BEGIN
    SELECT pg_get_userbyid(datdba) INTO owner_name
      FROM pg_database WHERE datname = current_database();
    FOR routine IN
        SELECT p.oid, p.oid::regprocedure AS signature
          FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
         WHERE p.prosecdef AND n.nspname IN
             ('platform', 'wamn_authority', 'wamn_history', 'identity', 'registry', 'catalog', 'wamn_run')
    LOOP
        EXECUTE format('ALTER FUNCTION %s OWNER TO %I', routine.signature, owner_name);
        EXECUTE format('ALTER FUNCTION %s SET search_path = pg_catalog', routine.signature);
        EXECUTE format('REVOKE ALL ON FUNCTION %s FROM PUBLIC', routine.signature);
        FOR grantee IN
            SELECT pg_get_userbyid(a.grantee) AS name
              FROM pg_proc p, LATERAL aclexplode(p.proacl) a
             WHERE p.oid = routine.oid AND a.grantee <> p.proowner
               AND a.grantee <> 0 AND a.is_grantable
        LOOP
            EXECUTE format('REVOKE GRANT OPTION FOR EXECUTE ON FUNCTION %s FROM %I CASCADE', routine.signature, grantee.name);
        END LOOP;
    END LOOP;
END
$definers$;
