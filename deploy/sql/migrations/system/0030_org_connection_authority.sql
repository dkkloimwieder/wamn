-- Org connection authority. Historical org_connections rows remain intact.
CREATE TABLE registry.external_system (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    org text NOT NULL REFERENCES registry.orgs(id),
    integration text NOT NULL REFERENCES registry.integrations(code),
    name text NOT NULL CHECK (btrim(name) <> ''),
    parameters jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(parameters) = 'object'),
    status text NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'disabled')),
    UNIQUE (org, name),
    UNIQUE (org, id, integration)
);
CREATE TABLE registry.connection (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    org text NOT NULL REFERENCES registry.orgs(id),
    integration text NOT NULL REFERENCES registry.integrations(code),
    external_system_id uuid,
    name text NOT NULL CHECK (name ~ '^[a-z][a-z0-9_-]{0,62}$'),
    transport text NOT NULL,
    exchange text NOT NULL DEFAULT 'request' CHECK (exchange IN ('request', 'event')),
    parameters jsonb NOT NULL CHECK (jsonb_typeof(parameters) = 'object'),
    status text NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'disabled')),
    service_principal_id uuid NOT NULL UNIQUE,
    principal_type text NOT NULL DEFAULT 'service' CHECK (principal_type = 'service'),
    current_generation bigint NOT NULL DEFAULT 1 CHECK (current_generation > 0),
    UNIQUE (org, name),
    UNIQUE (org, id),
    FOREIGN KEY (org, external_system_id, integration)
        REFERENCES registry.external_system(org, id, integration),
    FOREIGN KEY (service_principal_id, principal_type)
        REFERENCES identity.principals(id, type) DEFERRABLE INITIALLY DEFERRED
);
CREATE TABLE identity.credential (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    connection_id uuid,
    org text NOT NULL REFERENCES registry.orgs(id),
    project text,
    handle text NOT NULL,
    secret text NOT NULL,
    UNIQUE (connection_id, id),
    FOREIGN KEY (org, connection_id) REFERENCES registry.connection(org, id)
);
CREATE UNIQUE INDEX credential_project_handle ON identity.credential(org, project, handle) WHERE connection_id IS NULL;
CREATE TABLE registry.credential_import (
    org text PRIMARY KEY REFERENCES registry.orgs(id)
);
CREATE TABLE registry.connection_generation (
    connection_id uuid NOT NULL REFERENCES registry.connection(id),
    generation bigint NOT NULL CHECK (generation > 0),
    parameters jsonb NOT NULL CHECK (jsonb_typeof(parameters) = 'object'),
    secret_id uuid,
    PRIMARY KEY (connection_id, generation),
    FOREIGN KEY (connection_id, secret_id) REFERENCES identity.credential(connection_id, id)
);
ALTER TABLE registry.connection ADD CONSTRAINT connection_current_generation_fkey
    FOREIGN KEY (id, current_generation)
    REFERENCES registry.connection_generation(connection_id, generation)
    DEFERRABLE INITIALLY DEFERRED;
CREATE TABLE registry.project_connection (
    org text NOT NULL,
    project text NOT NULL,
    connection_id uuid NOT NULL,
    PRIMARY KEY (org, project, connection_id),
    FOREIGN KEY (org, project) REFERENCES registry.projects(org, id),
    FOREIGN KEY (org, connection_id) REFERENCES registry.connection(org, id)
);
CREATE TABLE registry.integration_allow (
    org text NOT NULL REFERENCES registry.orgs(id),
    scope_type text NOT NULL CHECK (scope_type IN ('org', 'project', 'environment')),
    scope_id text NOT NULL,
    integration text NOT NULL REFERENCES registry.integrations(code),
    PRIMARY KEY (org, scope_type, scope_id, integration)
);
CREATE FUNCTION registry.reject_connection_generation_change() RETURNS trigger
LANGUAGE plpgsql AS $$ BEGIN
    RAISE EXCEPTION 'connection-generation-is-immutable' USING ERRCODE = '55000';
END $$;
REVOKE ALL ON FUNCTION registry.reject_connection_generation_change() FROM PUBLIC;
CREATE TRIGGER connection_generation_immutable BEFORE UPDATE OR DELETE ON registry.connection_generation
FOR EACH ROW EXECUTE FUNCTION registry.reject_connection_generation_change();
CREATE TRIGGER credential_immutable BEFORE UPDATE OR DELETE ON identity.credential
FOR EACH ROW EXECUTE FUNCTION registry.reject_connection_generation_change();

INSERT INTO registry.external_system(org, integration, name, parameters)
SELECT org, type, name, '{}'::jsonb FROM registry.org_connections;
INSERT INTO registry.connection(org, integration, external_system_id, name, transport, parameters, service_principal_id)
SELECT o.org, o.type, s.id, o.name, 'http', o.definition, gen_random_uuid()
FROM registry.org_connections o JOIN registry.external_system s ON (s.org, s.name) = (o.org, o.name);
INSERT INTO identity.principals(id, type, subject, display_name)
SELECT service_principal_id, 'service', 'connection-' || id::text, name FROM registry.connection;
INSERT INTO registry.connection_generation(connection_id, generation, parameters)
SELECT id, current_generation, parameters FROM registry.connection;

-- Only non-secret connection authority crosses this interface.
CREATE OR REPLACE FUNCTION registry.connection_org_exists(org_id text) RETURNS boolean
LANGUAGE sql SECURITY DEFINER SET search_path = pg_catalog AS $$
 SELECT EXISTS (SELECT FROM registry.orgs WHERE id = org_id)
$$;
CREATE OR REPLACE FUNCTION registry.connection_integration_exists(integration_code text) RETURNS boolean
LANGUAGE sql SECURITY DEFINER SET search_path = pg_catalog AS $$
 SELECT EXISTS (SELECT FROM registry.integrations WHERE code = integration_code)
$$;
CREATE OR REPLACE FUNCTION registry.connection_system_rows(org_id text) RETURNS SETOF registry.external_system
LANGUAGE sql SECURITY DEFINER SET search_path = pg_catalog AS $$
 SELECT * FROM registry.external_system WHERE org = org_id
$$;
CREATE OR REPLACE FUNCTION registry.connection_rows(org_id text) RETURNS SETOF registry.connection
LANGUAGE sql SECURITY DEFINER SET search_path = pg_catalog AS $$
 SELECT * FROM registry.connection WHERE org = org_id
$$;
CREATE OR REPLACE FUNCTION registry.connection_project_rows(org_id text) RETURNS SETOF registry.project_connection
LANGUAGE sql SECURITY DEFINER SET search_path = pg_catalog AS $$
 SELECT * FROM registry.project_connection WHERE org = org_id
$$;
CREATE OR REPLACE FUNCTION registry.connection_allow_rows(org_id text) RETURNS SETOF registry.integration_allow
LANGUAGE sql SECURITY DEFINER SET search_path = pg_catalog AS $$
 SELECT * FROM registry.integration_allow WHERE org = org_id
$$;
CREATE OR REPLACE FUNCTION registry.connection_principal_rows(org_id text, project_id text)
RETURNS TABLE(id uuid, subject text, display_name text, status text, platform_domain text)
LANGUAGE sql SECURITY DEFINER SET search_path = pg_catalog AS $$
 SELECT p.id, p.subject, p.display_name, p.status, m.platform_domain
 FROM registry.connection c JOIN registry.project_connection e ON e.connection_id = c.id
 JOIN identity.principals p ON p.id = c.service_principal_id CROSS JOIN registry.meta m
 WHERE e.org = org_id AND e.project = project_id
$$;
CREATE OR REPLACE FUNCTION registry.configure_connection(org_id text, operation text, payload jsonb) RETURNS jsonb
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog AS $$
#variable_conflict use_variable
DECLARE
 row_id uuid := (payload->>'id')::uuid;
 integration_code text := payload->>'integration';
 row_name text := payload->>'name';
 row_status text := payload->>'status';
 params jsonb := payload->'parameters';
 system_id uuid := (payload->>'external_system_id')::uuid;
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
  IF row_id IS NOT NULL THEN
   SELECT * INTO old_system FROM registry.external_system WHERE org = org_id AND id = row_id FOR UPDATE;
   IF NOT FOUND THEN RETURN jsonb_build_object('error','not_found','field','id'); END IF;
   IF old_system.integration <> integration_code THEN RETURN jsonb_build_object('error','integration_mismatch','field','integration'); END IF;
   IF old_system.name <> row_name AND EXISTS (SELECT FROM registry.connection WHERE external_system_id = row_id) THEN
    RETURN jsonb_build_object('error','referenced','field','name');
   END IF;
   UPDATE registry.external_system SET name = row_name, parameters = params, status = row_status WHERE id = row_id;
  ELSE
   INSERT INTO registry.external_system(org,integration,name,parameters,status)
   VALUES(org_id,integration_code,row_name,params,row_status) RETURNING id INTO row_id;
  END IF;
  RETURN jsonb_build_object('id',row_id);
 ELSIF operation = 'connection.write' THEN
  IF integration_code NOT IN ('http','gcs','s3') THEN RETURN jsonb_build_object('error','integration_not_found','field','integration'); END IF;
  IF integration_code IN ('gcs','s3') AND coalesce(params->>'provider','s3') <> integration_code THEN
   RETURN jsonb_build_object('error','integration_mismatch','field','parameters');
  END IF;
  IF system_id IS NOT NULL THEN
   SELECT * INTO old_system FROM registry.external_system WHERE org = org_id AND id = system_id;
   IF NOT FOUND THEN RETURN jsonb_build_object('error','not_found','field','external_system_id'); END IF;
   IF old_system.integration <> integration_code THEN RETURN jsonb_build_object('error','integration_mismatch','field','external_system_id'); END IF;
  END IF;
  IF row_id IS NOT NULL THEN
   SELECT * INTO old_connection FROM registry.connection WHERE org = org_id AND id = row_id FOR UPDATE;
   IF NOT FOUND THEN RETURN jsonb_build_object('error','not_found','field','id'); END IF;
   IF old_connection.integration <> integration_code THEN RETURN jsonb_build_object('error','integration_mismatch','field','integration'); END IF;
   IF (old_connection.name <> row_name OR row_status = 'disabled') AND EXISTS (SELECT FROM registry.project_connection WHERE connection_id = row_id) THEN
    RETURN jsonb_build_object('error','referenced','field','name');
   END IF;
   generation := old_connection.current_generation;
   IF old_connection.parameters <> params THEN
    generation := generation + 1;
    INSERT INTO registry.connection_generation(connection_id,generation,parameters,secret_id)
    SELECT row_id,generation,params,g.secret_id FROM registry.connection_generation g
    WHERE g.connection_id = row_id AND g.generation = old_connection.current_generation;
   END IF;
   UPDATE registry.connection SET external_system_id = system_id,name = row_name,transport = payload->>'transport',
    exchange = payload->>'exchange',parameters = params,status = row_status,current_generation = generation WHERE id = row_id;
  ELSE
   row_id := gen_random_uuid();
   INSERT INTO identity.principals(type,subject,display_name) VALUES('service','connection-' || row_id::text,row_name) RETURNING id INTO principal;
   INSERT INTO registry.connection(id,org,integration,external_system_id,name,transport,exchange,parameters,status,service_principal_id)
   VALUES(row_id,org_id,integration_code,system_id,row_name,payload->>'transport',payload->>'exchange',params,row_status,principal);
   INSERT INTO registry.connection_generation(connection_id,generation,parameters) VALUES(row_id,1,params);
  END IF;
  RETURN jsonb_build_object('id',row_id,'principal_id',principal);
 ELSIF operation = 'project_connection.enable' THEN
  row_id := (payload->>'connection_id')::uuid;
  IF NOT EXISTS (SELECT FROM registry.connection WHERE org = org_id AND id = row_id AND status = 'active') THEN
   RETURN jsonb_build_object('error','not_found','field','connection_id');
  END IF;
  IF NOT EXISTS (SELECT FROM registry.projects WHERE org = org_id AND id = payload->>'project') THEN
   RETURN jsonb_build_object('error','not_found','field','project');
  END IF;
  INSERT INTO registry.project_connection(org,project,connection_id) VALUES(org_id,payload->>'project',row_id) ON CONFLICT DO NOTHING;
  RETURN jsonb_build_object('connection_id',row_id);
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

DO $control_surface$ BEGIN
  IF EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_control') THEN
    REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA "identity", "provisioning", "registry" FROM "wamn_control"; REVOKE ALL PRIVILEGES ON SCHEMA "identity", "provisioning", "registry" FROM "wamn_control"; GRANT USAGE ON SCHEMA "identity", "registry" TO "wamn_control"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "identity"."org_memberships" TO "wamn_control"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "identity"."org_roles" TO "wamn_control"; GRANT SELECT ON TABLE "identity"."password_logins" TO "wamn_control"; GRANT SELECT ON TABLE "identity"."principals" TO "wamn_control"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "identity"."project_env_memberships" TO "wamn_control"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "identity"."project_roles" TO "wamn_control"; GRANT SELECT ON TABLE "registry"."project_envs" TO "wamn_control"; GRANT SELECT, INSERT ON TABLE "registry"."projects" TO "wamn_control"; GRANT UPDATE ("status") ON TABLE "registry"."project_envs" TO "wamn_control"; DO $connection_acl$ BEGIN IF to_regprocedure('registry.connection_org_exists(text)') IS NOT NULL THEN GRANT EXECUTE ON FUNCTION registry.connection_org_exists(text) TO "wamn_control"; END IF; END $connection_acl$; DO $connection_acl$ BEGIN IF to_regprocedure('registry.connection_integration_exists(text)') IS NOT NULL THEN GRANT EXECUTE ON FUNCTION registry.connection_integration_exists(text) TO "wamn_control"; END IF; END $connection_acl$; DO $connection_acl$ BEGIN IF to_regprocedure('registry.connection_system_rows(text)') IS NOT NULL THEN GRANT EXECUTE ON FUNCTION registry.connection_system_rows(text) TO "wamn_control"; END IF; END $connection_acl$; DO $connection_acl$ BEGIN IF to_regprocedure('registry.connection_rows(text)') IS NOT NULL THEN GRANT EXECUTE ON FUNCTION registry.connection_rows(text) TO "wamn_control"; END IF; END $connection_acl$; DO $connection_acl$ BEGIN IF to_regprocedure('registry.connection_project_rows(text)') IS NOT NULL THEN GRANT EXECUTE ON FUNCTION registry.connection_project_rows(text) TO "wamn_control"; END IF; END $connection_acl$; DO $connection_acl$ BEGIN IF to_regprocedure('registry.connection_allow_rows(text)') IS NOT NULL THEN GRANT EXECUTE ON FUNCTION registry.connection_allow_rows(text) TO "wamn_control"; END IF; END $connection_acl$; DO $connection_acl$ BEGIN IF to_regprocedure('registry.connection_principal_rows(text,text)') IS NOT NULL THEN GRANT EXECUTE ON FUNCTION registry.connection_principal_rows(text,text) TO "wamn_control"; END IF; END $connection_acl$; DO $connection_acl$ BEGIN IF to_regprocedure('registry.configure_connection(text,text,jsonb)') IS NOT NULL THEN GRANT EXECUTE ON FUNCTION registry.configure_connection(text,text,jsonb) TO "wamn_control"; END IF; END $connection_acl$;
  END IF;
END $control_surface$;
