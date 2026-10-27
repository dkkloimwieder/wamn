-- Names are immutable authority identities. Readers use their authenticated login scope.
CREATE OR REPLACE FUNCTION registry.configure_connection(org_id text, operation text, payload jsonb) RETURNS jsonb
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog AS $$
#variable_conflict use_variable
DECLARE
 row_id uuid;
 integration_code text := payload->>'integration';
 row_name text := payload->>'name';
 row_status text := payload->>'status';
 params jsonb := payload->'parameters';
 system_id uuid;
 old_system registry.external_system;
 old_connection registry.connection;
 principal uuid;
 generation bigint;
 scope text := payload->>'scope_type';
 scope_id text := payload->>'scope_id';
 parent_scope text;
 parent_id text;
BEGIN
 PERFORM pg_advisory_xact_lock(hashtextextended('connections:' || org_id, 0));
 IF NOT EXISTS (SELECT FROM registry.orgs WHERE id = org_id) THEN
  RETURN jsonb_build_object('error','invalid_input','field','org');
 END IF;
 IF payload ?| ARRAY['id','connection_id','external_system_id'] THEN
  RETURN jsonb_build_object('error','invalid_input','field','id');
 END IF;
 IF operation IN ('external_system.write','connection.write','integration_allow.allow') THEN
  IF NOT EXISTS (SELECT FROM registry.integrations WHERE code = integration_code) THEN
   RETURN jsonb_build_object('error','integration_not_found','field','integration');
  END IF;
 END IF;
 IF operation IN ('external_system.write','connection.write') THEN
  IF row_name IS NULL OR btrim(row_name) = '' OR params IS NULL OR jsonb_typeof(params) <> 'object'
     OR row_status IS NULL OR row_status NOT IN ('active','disabled') THEN
   RETURN jsonb_build_object('error','invalid_input','field','connection');
  END IF;
 END IF;
 IF operation = 'external_system.write' THEN
  SELECT * INTO old_system FROM registry.external_system WHERE org = org_id AND name = row_name FOR UPDATE;
  IF FOUND THEN
   row_id := old_system.id;
   IF old_system.integration <> integration_code THEN RETURN jsonb_build_object('error','integration_mismatch','field','integration'); END IF;
   UPDATE registry.external_system SET parameters = params, status = row_status WHERE id = row_id
    AND (external_system.parameters,external_system.status) IS DISTINCT FROM (params,row_status);
  ELSE
   INSERT INTO registry.external_system(org,integration,name,parameters,status)
   VALUES(org_id,integration_code,row_name,params,row_status) RETURNING id INTO row_id;
  END IF;
  RETURN jsonb_build_object('name',row_name);
 ELSIF operation = 'connection.write' THEN
  IF payload->>'external_system' IS NOT NULL THEN
   SELECT * INTO old_system FROM registry.external_system WHERE org = org_id AND name = payload->>'external_system';
   IF NOT FOUND THEN RETURN jsonb_build_object('error','not_found','field','external_system'); END IF;
   IF old_system.integration <> integration_code THEN RETURN jsonb_build_object('error','integration_mismatch','field','external_system'); END IF;
   system_id := old_system.id;
  END IF;
  SELECT * INTO old_connection FROM registry.connection WHERE org = org_id AND name = row_name FOR UPDATE;
  IF FOUND THEN
   row_id := old_connection.id;
   principal := old_connection.service_principal_id;
   IF old_connection.integration <> integration_code THEN RETURN jsonb_build_object('error','integration_mismatch','field','integration'); END IF;
   generation := old_connection.current_generation;
   IF old_connection.parameters <> params THEN
    generation := generation + 1;
    INSERT INTO registry.connection_generation(connection_id,generation,parameters,secret_id)
    SELECT row_id,generation,params,g.secret_id FROM registry.connection_generation g
    WHERE g.connection_id = row_id AND g.generation = old_connection.current_generation;
   END IF;
   UPDATE registry.connection SET external_system_id = system_id,transport = payload->>'transport',
    exchange = payload->>'exchange',parameters = params,status = row_status,current_generation = generation WHERE id = row_id
    AND (connection.external_system_id,connection.transport,connection.exchange,connection.parameters,connection.status,connection.current_generation)
     IS DISTINCT FROM (system_id,payload->>'transport',payload->>'exchange',params,row_status,generation);
  ELSE
   row_id := gen_random_uuid();
   INSERT INTO identity.principals(type,subject,display_name) VALUES('service','connection-' || row_id::text,row_name) RETURNING id INTO principal;
   INSERT INTO registry.connection(id,org,integration,external_system_id,name,transport,exchange,parameters,status,service_principal_id)
   VALUES(row_id,org_id,integration_code,system_id,row_name,payload->>'transport',payload->>'exchange',params,row_status,principal);
   INSERT INTO registry.connection_generation(connection_id,generation,parameters) VALUES(row_id,1,params);
  END IF;
  RETURN jsonb_build_object('name',row_name,'principal_id',principal);
 ELSIF operation IN ('project_connection.enable','project_connection.disable') THEN
  SELECT id INTO row_id FROM registry.connection WHERE org = org_id AND name = payload->>'connection'
   AND (operation = 'project_connection.disable' OR status = 'active');
  IF NOT FOUND THEN
   RETURN jsonb_build_object('error','not_found','field','connection');
  END IF;
  IF NOT EXISTS (SELECT FROM registry.projects WHERE org = org_id AND id = payload->>'project') THEN
   RETURN jsonb_build_object('error','not_found','field','project');
  END IF;
  IF operation = 'project_connection.disable' THEN
   DELETE FROM registry.project_connection WHERE org = org_id AND project = payload->>'project' AND connection_id = row_id;
  ELSE
   INSERT INTO registry.project_connection(org,project,connection_id) VALUES(org_id,payload->>'project',row_id) ON CONFLICT DO NOTHING;
  END IF;
  RETURN jsonb_build_object('connection',payload->>'connection');
 ELSIF operation = 'connection.rotate' THEN
  RETURN registry.rotate_connection(org_id,payload->>'name',payload->>'secret');
 ELSIF operation = 'connection.import_credentials' THEN
  RETURN registry.import_connection_credentials(org_id,payload);
 ELSIF operation = 'integration_allow.allow' THEN
  IF scope = 'org' AND scope_id = org_id THEN NULL;
  ELSIF scope = 'project' THEN
   IF NOT EXISTS (SELECT FROM registry.projects WHERE org = org_id AND id = scope_id) THEN RETURN jsonb_build_object('error','not_found','field','scope_id'); END IF;
   parent_scope := 'org'; parent_id := org_id;
  ELSIF scope = 'environment' THEN
   IF position('/' in scope_id) = 0 THEN RETURN jsonb_build_object('error','invalid_input','field','scope_id'); END IF;
   IF NOT EXISTS (SELECT FROM registry.project_envs WHERE org = org_id AND project = split_part(scope_id,'/',1) AND env = split_part(scope_id,'/',2)) THEN
    RETURN jsonb_build_object('error','not_found','field','scope_id');
   END IF;
   parent_scope := 'project'; parent_id := split_part(scope_id,'/',1);
  ELSE RETURN jsonb_build_object('error','invalid_input','field','scope_type');
  END IF;
  IF parent_scope IS NOT NULL AND NOT EXISTS (SELECT FROM registry.integration_allow a WHERE a.org = org_id AND a.scope_type = parent_scope AND a.scope_id = parent_id AND a.integration = integration_code) THEN
   RETURN jsonb_build_object('error','integration_not_allowed','field','integration');
  END IF;
  INSERT INTO registry.integration_allow(org,scope_type,scope_id,integration) VALUES(org_id,scope,scope_id,integration_code) ON CONFLICT DO NOTHING;
  RETURN jsonb_build_object('integration',integration_code);
 END IF;
 RETURN jsonb_build_object('error','invalid_input','field','operation');
END $$;

ALTER FUNCTION registry.connection_org_exists(text) OWNER TO wamn_system;
REVOKE ALL ON FUNCTION registry.connection_org_exists(text) FROM PUBLIC;
ALTER FUNCTION registry.connection_integration_exists(text) OWNER TO wamn_system;
REVOKE ALL ON FUNCTION registry.connection_integration_exists(text) FROM PUBLIC;
ALTER FUNCTION registry.connection_system_rows(text) OWNER TO wamn_system;
REVOKE ALL ON FUNCTION registry.connection_system_rows(text) FROM PUBLIC;
ALTER FUNCTION registry.connection_rows(text) OWNER TO wamn_system;
REVOKE ALL ON FUNCTION registry.connection_rows(text) FROM PUBLIC;
ALTER FUNCTION registry.connection_project_rows(text) OWNER TO wamn_system;
REVOKE ALL ON FUNCTION registry.connection_project_rows(text) FROM PUBLIC;
ALTER FUNCTION registry.connection_allow_rows(text) OWNER TO wamn_system;
REVOKE ALL ON FUNCTION registry.connection_allow_rows(text) FROM PUBLIC;
ALTER FUNCTION registry.connection_principal_rows(text,text) OWNER TO wamn_system;
REVOKE ALL ON FUNCTION registry.connection_principal_rows(text,text) FROM PUBLIC;
ALTER FUNCTION registry.configure_connection(text,text,jsonb) OWNER TO wamn_system;
REVOKE ALL ON FUNCTION registry.configure_connection(text,text,jsonb) FROM PUBLIC;

CREATE OR REPLACE FUNCTION registry.keep_connection_name() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog AS $$
BEGIN
 IF (NEW.org,NEW.name) IS DISTINCT FROM (OLD.org,OLD.name) THEN
  RAISE EXCEPTION 'connection_name_is_immutable' USING ERRCODE = '55000';
 END IF;
 RETURN NEW;
END $$;
REVOKE ALL ON FUNCTION registry.keep_connection_name() FROM PUBLIC;
CREATE TRIGGER connection_name_immutable BEFORE UPDATE OF org,name ON registry.connection
 FOR EACH ROW EXECUTE FUNCTION registry.keep_connection_name();
CREATE TRIGGER external_system_name_immutable BEFORE UPDATE OF org,name ON registry.external_system
 FOR EACH ROW EXECUTE FUNCTION registry.keep_connection_name();

CREATE OR REPLACE FUNCTION registry.rotate_connection(org_id text, connection_name text, secret text)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog AS $$
#variable_conflict use_variable
DECLARE
 c registry.connection;
 secret_id uuid;
 next_generation bigint;
BEGIN
 PERFORM pg_advisory_xact_lock(hashtextextended('connections:' || org_id,0));
 SELECT * INTO c FROM registry.connection WHERE org = org_id AND name = connection_name FOR UPDATE;
 IF NOT FOUND THEN RETURN jsonb_build_object('error','not_found','field','connection'); END IF;
 IF secret IS NULL THEN RETURN jsonb_build_object('error','invalid_input','field','secret'); END IF;
 next_generation := c.current_generation + 1;
 INSERT INTO identity.credential(connection_id,org,handle,secret)
 VALUES(c.id,org_id,connection_name,secret) RETURNING id INTO secret_id;
 INSERT INTO registry.connection_generation(connection_id,generation,parameters,secret_id)
 VALUES(c.id,next_generation,c.parameters,secret_id);
 UPDATE registry.connection SET current_generation = next_generation WHERE id = c.id;
 RETURN jsonb_build_object('name',connection_name,'generation',next_generation);
END $$;

CREATE OR REPLACE FUNCTION registry.import_connection_credentials(org_id text, document jsonb)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog AS $$
#variable_conflict use_variable
DECLARE
 org_entry record;
 project_entry record;
 credential_entry record;
 entry record;
 org_name text;
 connection_name text;
 material text;
 coordinate text;
 credentials jsonb := '{}';
 org_names text[];
BEGIN
 IF jsonb_typeof(document) IS DISTINCT FROM 'object'
    OR jsonb_typeof(document->'orgs') IS DISTINCT FROM 'object'
    OR jsonb_typeof(document->'projects') IS DISTINCT FROM 'object'
    OR document - ARRAY['orgs','projects'] <> '{}'::jsonb THEN
  RETURN jsonb_build_object('error','invalid_input','field','credentials_file');
 END IF;
 IF EXISTS (SELECT FROM jsonb_object_keys(document->'orgs') AS org_key(name) WHERE name IS DISTINCT FROM org_id) THEN
  RETURN jsonb_build_object('error','invalid_input','field','org');
 END IF;
 org_names := ARRAY[org_id];
 FOREACH org_name IN ARRAY org_names LOOP
  PERFORM pg_advisory_xact_lock(hashtextextended('connections:' || org_name,0));
  IF NOT EXISTS (SELECT FROM registry.orgs WHERE id = org_name) THEN
   RETURN jsonb_build_object('error','not_found','field','org');
  END IF;
  IF EXISTS (SELECT FROM registry.credential_import WHERE org = org_name) THEN
   RETURN jsonb_build_object('error','already_imported','field','org');
  END IF;
 END LOOP;
 FOR org_entry IN SELECT * FROM jsonb_each(document->'orgs') LOOP
  IF jsonb_typeof(org_entry.value) <> 'object' THEN
   RETURN jsonb_build_object('error','invalid_input','field','org');
  END IF;
  FOR credential_entry IN SELECT * FROM jsonb_each(org_entry.value) LOOP
   IF jsonb_typeof(credential_entry.value) <> 'string' OR NOT EXISTS (
    SELECT FROM registry.connection WHERE org = org_entry.key AND name = credential_entry.key
   ) THEN RETURN jsonb_build_object('error','not_found','field','connection'); END IF;
   coordinate := jsonb_build_array(org_entry.key,credential_entry.key)::text;
   credentials := credentials || jsonb_build_object(coordinate,credential_entry.value);
  END LOOP;
 END LOOP;
 FOR project_entry IN SELECT * FROM jsonb_each(document->'projects') LOOP
  IF jsonb_typeof(project_entry.value) <> 'object' THEN
   RETURN jsonb_build_object('error','invalid_input','field','project');
  END IF;
  FOR credential_entry IN SELECT * FROM jsonb_each(project_entry.value) LOOP
   IF jsonb_typeof(credential_entry.value) <> 'object'
      OR jsonb_typeof(credential_entry.value->'connection') IS DISTINCT FROM 'string'
      OR jsonb_typeof(credential_entry.value->'secret') IS DISTINCT FROM 'string'
      OR credential_entry.value - ARRAY['connection','secret'] <> '{}'::jsonb THEN
    RETURN jsonb_build_object('error','invalid_input','field','connection');
   END IF;
   connection_name := credential_entry.value->>'connection';
   material := credential_entry.value->>'secret';
   IF NOT EXISTS (SELECT FROM registry.connection c JOIN registry.project_connection p ON p.connection_id = c.id
     WHERE c.org = org_id AND c.name = connection_name AND p.org = org_id AND p.project = project_entry.key) THEN
    RETURN jsonb_build_object('error','not_found','field','connection');
   END IF;
   coordinate := jsonb_build_array(org_id,connection_name)::text;
   IF credentials ? coordinate AND credentials->>coordinate IS DISTINCT FROM material THEN
    RETURN jsonb_build_object('error','invalid_input','field','connection');
   END IF;
   credentials := credentials || jsonb_build_object(coordinate,material);
  END LOOP;
 END LOOP;
 -- All refusals precede writes, including conflicts across project and org maps.
 FOR entry IN SELECT * FROM jsonb_each_text(credentials) LOOP
  PERFORM registry.rotate_connection(entry.key::jsonb->>0,entry.key::jsonb->>1,entry.value);
 END LOOP;
 INSERT INTO registry.credential_import(org) SELECT unnest(org_names);
 RETURN jsonb_build_object('imported',true);
END $$;

-- No app.* claim supplies scope. SET ROLE does not change session_user.
-- PL/pgSQL delays mapping-table resolution until the portable store is installed.
DROP FUNCTION IF EXISTS registry.lookup_connection_credential(text,text,text,bigint);
CREATE OR REPLACE FUNCTION registry.lookup_connection_credential(
 org_id text, project_id text, connection_name text, requested_generation bigint DEFAULT NULL)
RETURNS TABLE(secret text, principal_id text, external_system text)
LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path = pg_catalog AS $$
#variable_conflict use_variable
BEGIN
 RETURN QUERY
 SELECT k.secret, c.service_principal_id::text, c.external_system_id::text
 FROM wamn_authority.author_login_tenants login
 JOIN registry.connection c ON c.org = login.org_id
 JOIN registry.project_connection p ON p.org = c.org AND p.connection_id = c.id AND p.project = login.project_id
 JOIN identity.principals principal ON principal.id = c.service_principal_id AND principal.status = 'active'
 JOIN registry.connection_generation g ON g.connection_id = c.id AND g.generation = coalesce(requested_generation,c.current_generation)
 LEFT JOIN identity.credential k ON k.connection_id = c.id AND k.id = g.secret_id
 WHERE login.login_identity = session_user AND login.org_id = org_id AND login.project_id = project_id
  AND c.name = connection_name AND c.status = 'active';
END $$;

CREATE OR REPLACE FUNCTION registry.connection_integrations()
RETURNS SETOF registry.integrations LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog AS $$
 SELECT * FROM registry.integrations
$$;
CREATE OR REPLACE FUNCTION registry.connection_binding_rows(org_id text, project_id text, environment_name text)
RETURNS TABLE(application text, alias text, integration text, mode text)
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog AS $$
 SELECT b.application,b.alias,c.integration,b.mode
 FROM registry.environment_connection_binding b JOIN registry.connection c ON c.id = b.connection_id
 WHERE b.org = org_id AND b.project = project_id AND b.env = environment_name
$$;

-- Requests contain application, alias, name, integration and mode. Results contain no secret.
CREATE OR REPLACE FUNCTION registry.analyse_connection_bindings(
 org_id text, project_id text, environment_name text, target_name text, requests jsonb)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog AS $$
#variable_conflict use_variable
DECLARE
 target uuid;
 previous_target uuid;
 request jsonb;
 c registry.connection;
 exclusive_medium boolean;
 seen_exclusive uuid[] := '{}';
 seen_bindings text[] := '{}';
 binding_key text;
 result jsonb := '[]';
 scope text;
 scope_id text;
BEGIN
 SELECT id INTO target FROM registry.target WHERE name = target_name AND status = 'active';
 IF NOT FOUND THEN RETURN jsonb_build_object('error','target_not_found','context',target_name); END IF;
 SELECT target_id INTO previous_target FROM registry.project_envs
 WHERE org = org_id AND project = project_id AND env = environment_name;
 IF previous_target IS NOT NULL AND previous_target <> target THEN
  RETURN jsonb_build_object('error','environment_target_changed','context',target_name);
 END IF;
 IF jsonb_typeof(requests) IS DISTINCT FROM 'array' THEN
  RETURN jsonb_build_object('error','invalid_input','context','bindings');
 END IF;
 FOR request IN SELECT * FROM jsonb_array_elements(requests) LOOP
  IF jsonb_typeof(request) <> 'object' OR coalesce(request->>'application','') = ''
     OR coalesce(request->>'alias','') = '' OR coalesce(request->>'name','') = ''
     OR coalesce(request->>'mode','') NOT IN ('read','write') THEN
   RETURN jsonb_build_object('error','invalid_input','context','binding');
  END IF;
  binding_key := jsonb_build_array(request->>'application',request->>'alias')::text;
  IF binding_key = ANY(seen_bindings) THEN
   RETURN jsonb_build_object('error','connection_declaration_conflict','context',binding_key);
  END IF;
  seen_bindings := array_append(seen_bindings,binding_key);
  IF NOT EXISTS (SELECT FROM registry.integrations WHERE code = request->>'integration') THEN
   RETURN jsonb_build_object('error','integration_not_found','context',request->>'integration');
  END IF;
  SELECT * INTO c FROM registry.connection WHERE org = org_id AND name = request->>'name' AND status = 'active';
  IF NOT FOUND OR NOT EXISTS (SELECT FROM registry.project_connection
    WHERE org = org_id AND project = project_id AND connection_id = c.id) THEN
   RETURN jsonb_build_object('error','connection_not_enabled','context',binding_key);
  END IF;
  IF c.integration <> request->>'integration' THEN
   RETURN jsonb_build_object('error','connection_integration_mismatch','context',binding_key);
  END IF;
  FOR scope,scope_id IN SELECT * FROM (VALUES
   ('org',org_id),('project',project_id),('environment',project_id || '/' || environment_name)) scopes LOOP
   IF NOT EXISTS (SELECT FROM registry.integration_allow a
     WHERE a.org = org_id AND a.scope_type = scope AND a.scope_id = scope_id AND a.integration = c.integration) THEN
    RETURN jsonb_build_object('error','integration_not_allowed','context',binding_key || ': ' || scope || ' ' || scope_id);
   END IF;
  END LOOP;
  SELECT exclusive INTO exclusive_medium FROM registry.integrations WHERE code = c.integration;
  IF exclusive_medium THEN
   IF c.id = ANY(seen_exclusive) OR EXISTS (SELECT FROM registry.environment_connection_binding
     WHERE target_id = target AND connection_id = c.id AND (org,project,env) <> (org_id,project_id,environment_name)) THEN
    RETURN jsonb_build_object('error','exclusive_connection_bound','context',binding_key);
   END IF;
   seen_exclusive := array_append(seen_exclusive,c.id);
  END IF;
  result := result || jsonb_build_array(jsonb_build_object(
   'application',request->>'application','alias',request->>'alias','name',c.name,
   'connection_id',c.id,'generation',c.current_generation,'integration',c.integration,
   'mode',request->>'mode','exclusive',exclusive_medium,
   'parameters',(SELECT parameters FROM registry.connection_generation WHERE connection_id = c.id AND generation = c.current_generation)));
 END LOOP;
 RETURN jsonb_build_object('bindings',result);
END $$;

CREATE OR REPLACE FUNCTION registry.reserve_connection_bindings(
 org_id text, project_id text, environment_name text, target_name text, requests jsonb)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog AS $$
#variable_conflict use_variable
DECLARE
 target uuid;
 result jsonb;
BEGIN
 -- Share the org writer lock so disable, rotate and apply cannot race.
 PERFORM pg_advisory_xact_lock(hashtextextended('connections:' || org_id,0));
 SELECT id INTO target FROM registry.target WHERE name = target_name FOR UPDATE;
 result := registry.analyse_connection_bindings(org_id,project_id,environment_name,target_name,requests);
 IF result ? 'error' THEN RETURN result; END IF;
 IF NOT EXISTS (SELECT FROM registry.project_envs WHERE org = org_id AND project = project_id AND env = environment_name) THEN
  RETURN jsonb_build_object('error','not_found','context','environment');
 END IF;
 UPDATE registry.project_envs SET target_id = target
 WHERE org = org_id AND project = project_id AND env = environment_name AND target_id IS DISTINCT FROM target;
 DELETE FROM registry.environment_connection_binding b
 WHERE b.org = org_id AND b.project = project_id AND b.env = environment_name
  AND NOT EXISTS (SELECT FROM jsonb_array_elements(result->'bindings') r
    WHERE r->>'application' = b.application AND r->>'alias' = b.alias
     AND (b.target_id,b.connection_id,b.generation,b.mode,b.exclusive) =
       (target,(r->>'connection_id')::uuid,(r->>'generation')::bigint,r->>'mode',(r->>'exclusive')::boolean));
 INSERT INTO registry.environment_connection_binding AS b
  (org,project,env,application,alias,target_id,connection_id,generation,mode,exclusive)
 SELECT org_id,project_id,environment_name,r->>'application',r->>'alias',target,
  (r->>'connection_id')::uuid,(r->>'generation')::bigint,r->>'mode',(r->>'exclusive')::boolean
 FROM jsonb_array_elements(result->'bindings') r
 ON CONFLICT (org,project,env,application,alias) DO NOTHING;
 RETURN result;
END $$;

DO $connection_authority_acl$
DECLARE signature text;
BEGIN
 FOREACH signature IN ARRAY ARRAY[
  'connection_org_exists(text)', 'connection_integration_exists(text)',
  'connection_system_rows(text)', 'connection_rows(text)',
  'connection_project_rows(text)', 'connection_allow_rows(text)',
  'connection_principal_rows(text,text)', 'configure_connection(text,text,jsonb)',
  'rotate_connection(text,text,text)', 'import_connection_credentials(text,jsonb)',
  'connection_integrations()', 'connection_binding_rows(text,text,text)',
  'analyse_connection_bindings(text,text,text,text,jsonb)',
  'reserve_connection_bindings(text,text,text,text,jsonb)'
 ] LOOP
  EXECUTE 'ALTER FUNCTION registry.' || signature || ' OWNER TO wamn_system';
  EXECUTE 'REVOKE ALL ON FUNCTION registry.' || signature || ' FROM PUBLIC';
  IF to_regrole('wamn_control') IS NOT NULL THEN
   GRANT USAGE ON SCHEMA registry TO wamn_control;
   EXECUTE 'GRANT EXECUTE ON FUNCTION registry.' || signature || ' TO wamn_control';
  END IF;
  IF to_regrole('wamn_administration') IS NOT NULL THEN
   GRANT USAGE ON SCHEMA registry TO wamn_administration;
   EXECUTE 'GRANT EXECUTE ON FUNCTION registry.' || signature || ' TO wamn_administration';
  END IF;
 END LOOP;
END $connection_authority_acl$;
ALTER FUNCTION registry.lookup_connection_credential(text,text,text,bigint) OWNER TO wamn_system;
REVOKE ALL ON FUNCTION registry.lookup_connection_credential(text,text,text,bigint) FROM PUBLIC;
DO $credential_reader_acl$ BEGIN
 IF to_regrole('wamn_conn_reader') IS NOT NULL THEN
  REVOKE ALL ON identity.credential,registry.connection,registry.connection_generation,registry.project_connection FROM wamn_conn_reader;
  REVOKE ALL ON SCHEMA identity FROM wamn_conn_reader;
  GRANT USAGE ON SCHEMA registry TO wamn_conn_reader;
  GRANT EXECUTE ON FUNCTION registry.lookup_connection_credential(text,text,text,bigint) TO wamn_conn_reader;
 END IF;
END $credential_reader_acl$;

DO $control_surface$ BEGIN
  IF EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_control') THEN
    REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA "identity", "provisioning", "registry" FROM "wamn_control"; REVOKE ALL PRIVILEGES ON SCHEMA "identity", "provisioning", "registry" FROM "wamn_control"; GRANT USAGE ON SCHEMA "identity", "registry" TO "wamn_control"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "identity"."org_memberships" TO "wamn_control"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "identity"."org_roles" TO "wamn_control"; GRANT SELECT ON TABLE "identity"."password_logins" TO "wamn_control"; GRANT SELECT ON TABLE "identity"."principals" TO "wamn_control"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "identity"."project_env_memberships" TO "wamn_control"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "identity"."project_roles" TO "wamn_control"; GRANT SELECT ON TABLE "registry"."project_envs" TO "wamn_control"; GRANT SELECT, INSERT ON TABLE "registry"."projects" TO "wamn_control"; GRANT UPDATE ("status") ON TABLE "registry"."project_envs" TO "wamn_control"; DO $connection_acl$ BEGIN IF to_regprocedure('registry.connection_org_exists(text)') IS NOT NULL THEN GRANT EXECUTE ON FUNCTION registry.connection_org_exists(text) TO "wamn_control"; END IF; END $connection_acl$; DO $connection_acl$ BEGIN IF to_regprocedure('registry.connection_integration_exists(text)') IS NOT NULL THEN GRANT EXECUTE ON FUNCTION registry.connection_integration_exists(text) TO "wamn_control"; END IF; END $connection_acl$; DO $connection_acl$ BEGIN IF to_regprocedure('registry.connection_system_rows(text)') IS NOT NULL THEN GRANT EXECUTE ON FUNCTION registry.connection_system_rows(text) TO "wamn_control"; END IF; END $connection_acl$; DO $connection_acl$ BEGIN IF to_regprocedure('registry.connection_rows(text)') IS NOT NULL THEN GRANT EXECUTE ON FUNCTION registry.connection_rows(text) TO "wamn_control"; END IF; END $connection_acl$; DO $connection_acl$ BEGIN IF to_regprocedure('registry.connection_project_rows(text)') IS NOT NULL THEN GRANT EXECUTE ON FUNCTION registry.connection_project_rows(text) TO "wamn_control"; END IF; END $connection_acl$; DO $connection_acl$ BEGIN IF to_regprocedure('registry.connection_allow_rows(text)') IS NOT NULL THEN GRANT EXECUTE ON FUNCTION registry.connection_allow_rows(text) TO "wamn_control"; END IF; END $connection_acl$; DO $connection_acl$ BEGIN IF to_regprocedure('registry.connection_principal_rows(text,text)') IS NOT NULL THEN GRANT EXECUTE ON FUNCTION registry.connection_principal_rows(text,text) TO "wamn_control"; END IF; END $connection_acl$; DO $connection_acl$ BEGIN IF to_regprocedure('registry.configure_connection(text,text,jsonb)') IS NOT NULL THEN GRANT EXECUTE ON FUNCTION registry.configure_connection(text,text,jsonb) TO "wamn_control"; END IF; END $connection_acl$; DO $connection_acl$ BEGIN IF to_regprocedure('registry.rotate_connection(text,text,text)') IS NOT NULL THEN GRANT EXECUTE ON FUNCTION registry.rotate_connection(text,text,text) TO "wamn_control"; END IF; END $connection_acl$; DO $connection_acl$ BEGIN IF to_regprocedure('registry.import_connection_credentials(text,jsonb)') IS NOT NULL THEN GRANT EXECUTE ON FUNCTION registry.import_connection_credentials(text,jsonb) TO "wamn_control"; END IF; END $connection_acl$; DO $connection_acl$ BEGIN IF to_regprocedure('registry.connection_integrations()') IS NOT NULL THEN GRANT EXECUTE ON FUNCTION registry.connection_integrations() TO "wamn_control"; END IF; END $connection_acl$; DO $connection_acl$ BEGIN IF to_regprocedure('registry.connection_binding_rows(text,text,text)') IS NOT NULL THEN GRANT EXECUTE ON FUNCTION registry.connection_binding_rows(text,text,text) TO "wamn_control"; END IF; END $connection_acl$; DO $connection_acl$ BEGIN IF to_regprocedure('registry.analyse_connection_bindings(text,text,text,text,jsonb)') IS NOT NULL THEN GRANT EXECUTE ON FUNCTION registry.analyse_connection_bindings(text,text,text,text,jsonb) TO "wamn_control"; END IF; END $connection_acl$; DO $connection_acl$ BEGIN IF to_regprocedure('registry.reserve_connection_bindings(text,text,text,text,jsonb)') IS NOT NULL THEN GRANT EXECUTE ON FUNCTION registry.reserve_connection_bindings(text,text,text,text,jsonb) TO "wamn_control"; END IF; END $connection_acl$;
  END IF;
END $control_surface$;

CREATE OR REPLACE FUNCTION registry.lookup_connection_binding_mode(
 org_id text, project_id text, environment_name text, application text, alias text)
RETURNS text
LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path = pg_catalog AS $$
#variable_conflict use_variable
BEGIN
 RETURN (
  SELECT binding.mode
  FROM wamn_authority.author_login_tenants login
  JOIN registry.environment_connection_binding binding
    ON binding.org = login.org_id AND binding.project = login.project_id
  WHERE login.login_identity = session_user
    AND login.org_id = org_id AND login.project_id = project_id
    AND login.environment = environment_name
    AND binding.env = environment_name
    AND binding.application = application AND binding.alias = alias
 );
END $$;
ALTER FUNCTION registry.lookup_connection_binding_mode(text,text,text,text,text) OWNER TO wamn_system;
REVOKE ALL ON FUNCTION registry.lookup_connection_binding_mode(text,text,text,text,text) FROM PUBLIC;
DO $binding_mode_reader_acl$ BEGIN
 IF to_regrole('wamn_conn_reader') IS NOT NULL THEN
  GRANT EXECUTE ON FUNCTION registry.lookup_connection_binding_mode(text,text,text,text,text) TO wamn_conn_reader;
 END IF;
END $binding_mode_reader_acl$;
