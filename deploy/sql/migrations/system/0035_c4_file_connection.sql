-- Native file routes resolve a declared binding or the generation pinned by a file.
-- Scope comes from the authenticated login, never caller-controlled app.* claims.
CREATE OR REPLACE FUNCTION registry.lookup_file_connection(
 org_id text, project_id text, environment_name text, application_id text,
 binding_name text, requested_connection_id text, requested_generation bigint)
RETURNS TABLE(connection_id uuid, connection_name text, generation bigint, parameters jsonb, mode text)
LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path = pg_catalog AS $$
#variable_conflict use_variable
BEGIN
 RETURN QUERY
 SELECT c.id, c.name, g.generation, g.parameters,
  CASE WHEN binding_name IS NOT NULL THEN binding.mode
   -- Multiple aliases may bind the same connection. Any current write binding
   -- permits mutations; a removed binding leaves existing files readable.
   WHEN EXISTS (
    SELECT FROM registry.environment_connection_binding current_binding
    WHERE current_binding.org = login.org_id AND current_binding.project = login.project_id
     AND current_binding.env = environment_name AND current_binding.application = application_id
     AND current_binding.connection_id = c.id AND current_binding.mode = 'write'
   ) THEN 'write' ELSE 'read' END
 FROM wamn_authority.author_login_tenants login
 JOIN registry.connection c ON c.org = login.org_id
 JOIN registry.project_connection p
   ON p.org = c.org AND p.connection_id = c.id AND p.project = login.project_id
 JOIN identity.principals principal ON principal.id = c.service_principal_id AND principal.status = 'active'
 JOIN registry.connection_generation g
   ON g.connection_id = c.id AND g.generation = coalesce(requested_generation,c.current_generation)
 LEFT JOIN registry.environment_connection_binding binding
   ON binding.org = login.org_id AND binding.project = login.project_id
    AND binding.env = environment_name AND binding.application = application_id
    AND binding.alias = binding_name AND binding.connection_id = c.id
 WHERE login.login_identity = session_user AND login.org_id = org_id AND login.project_id = project_id
  AND login.environment = environment_name AND c.status = 'active' AND application_id <> ''
  AND (
   (binding_name IS NOT NULL AND requested_connection_id IS NULL AND requested_generation IS NULL
    AND binding.connection_id IS NOT NULL)
   OR
   (requested_connection_id IS NOT NULL AND requested_generation > 0
    AND c.id::text = requested_connection_id
    AND (binding_name IS NULL OR (binding.connection_id IS NOT NULL AND binding.mode = 'write')))
  );
END $$;
ALTER FUNCTION registry.lookup_file_connection(text,text,text,text,text,text,bigint) OWNER TO wamn_system;
REVOKE ALL ON FUNCTION registry.lookup_file_connection(text,text,text,text,text,text,bigint) FROM PUBLIC;
DO $file_connection_reader_acl$ BEGIN
 IF to_regrole('wamn_conn_reader') IS NOT NULL THEN
  GRANT EXECUTE ON FUNCTION registry.lookup_file_connection(text,text,text,text,text,text,bigint) TO wamn_conn_reader;
 END IF;
END $file_connection_reader_acl$;
