SELECT pg_catalog.pg_advisory_xact_lock(pg_catalog.hashtext('wamn_pooler_auth'));

DO $role$
BEGIN
    IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'wamn_pooler_auth') THEN
        CREATE ROLE wamn_pooler_auth;
    END IF;
END;
$role$;
ALTER ROLE wamn_pooler_auth
    LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS PASSWORD NULL;

CREATE SCHEMA IF NOT EXISTS wamn_pooler AUTHORIZATION postgres;

CREATE OR REPLACE FUNCTION wamn_pooler.lookup_login(text, text)
RETURNS TABLE (username text, verifier text)
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog
AS $lookup$
    SELECT login_role.rolname::text, login_role.rolpassword
      FROM pg_catalog.pg_authid AS login_role
      JOIN pg_catalog.pg_auth_members AS membership ON membership.member = login_role.oid
      JOIN pg_catalog.pg_authid AS group_role ON group_role.oid = membership.roleid
     WHERE login_role.rolname::text = $1
       AND group_role.rolname::text = $2
       AND $2 IN ('wamn_control', 'wamn_apply')
       AND login_role.rolname::text COLLATE "C" ~
           CASE $2 WHEN 'wamn_apply' THEN '^wamn_apply_(env_)?[0-9a-f]{40}$'
                   ELSE '^wamn_control_[0-9a-f]{40}$' END
       AND login_role.rolcanlogin
       AND (login_role.rolvaliduntil IS NULL OR login_role.rolvaliduntil >= CURRENT_TIMESTAMP)
       AND login_role.rolpassword LIKE 'SCRAM-SHA-256$%'
     LIMIT 1;
$lookup$;
ALTER FUNCTION wamn_pooler.lookup_login(text, text) OWNER TO postgres;

REVOKE CONNECT, TEMPORARY ON DATABASE postgres FROM PUBLIC;
REVOKE ALL ON DATABASE postgres FROM wamn_pooler_auth;
REVOKE ALL ON SCHEMA public FROM PUBLIC;
REVOKE ALL ON SCHEMA wamn_pooler FROM PUBLIC, wamn_pooler_auth;
REVOKE ALL ON FUNCTION wamn_pooler.lookup_login(text, text) FROM PUBLIC, wamn_pooler_auth;
GRANT CONNECT ON DATABASE postgres TO wamn_pooler_auth;
GRANT USAGE ON SCHEMA wamn_pooler TO wamn_pooler_auth;
GRANT EXECUTE ON FUNCTION wamn_pooler.lookup_login(text, text) TO wamn_pooler_auth;
