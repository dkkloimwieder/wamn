-- Provisioning installs shared privilege roles before selecting the database owner.
DO $roles$
DECLARE role_name text;
BEGIN
    PERFORM pg_advisory_xact_lock(hashtext('wamn_role_bootstrap'));
    FOREACH role_name IN ARRAY ARRAY['wamn_system_reader', 'wamn_identity_issuer', 'wamn_control'] LOOP
        IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = role_name) THEN
            EXECUTE format('CREATE ROLE %I NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS', role_name);
        ELSIF EXISTS (SELECT FROM pg_roles WHERE rolname = role_name
                      AND (rolcanlogin OR rolsuper OR rolcreatedb OR rolcreaterole
                           OR rolinherit OR rolreplication OR rolbypassrls)) THEN
            RAISE EXCEPTION 'invalid ACL role %', role_name;
        END IF;
    END LOOP;
END
$roles$;
