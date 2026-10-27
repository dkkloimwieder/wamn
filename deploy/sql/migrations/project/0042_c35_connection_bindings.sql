-- Resolve each unscoped binding against the component owners in its exact release.
LOCK TABLE catalog.connection_bindings IN EXCLUSIVE MODE;
DO $package_binding_backfill$
DECLARE
 binding catalog.connection_bindings;
 scoped catalog.connection_bindings;
 owners text[];
 owner_name text;
BEGIN
 FOR binding IN SELECT * FROM catalog.connection_bindings WHERE package_id = '' LOOP
  SELECT array_agg(DISTINCT component.value->>'package-id')
   FILTER (WHERE coalesce(component.value->>'package-id','') <> '') INTO owners
  FROM catalog.releases release
  CROSS JOIN LATERAL jsonb_array_elements(convert_from(release.canonical_bytes,'UTF8')::jsonb->'components') component(value)
  WHERE release.tenant_id = binding.tenant_id AND release.manifest_digest = binding.manifest_digest
   AND component.value->'descriptor'->>'component-digest' = binding.component_digest;
  IF coalesce(cardinality(owners),0) = 0 THEN
   RAISE EXCEPTION 'connection_binding_package_owner_missing' USING ERRCODE = '55000';
  END IF;
  FOREACH owner_name IN ARRAY owners LOOP
   SELECT * INTO scoped FROM catalog.connection_bindings
   WHERE tenant_id = binding.tenant_id AND manifest_digest = binding.manifest_digest
    AND component_digest = binding.component_digest AND store_alias = binding.store_alias AND package_id = owner_name;
   IF FOUND AND (scoped.environment,scoped.instance_id,scoped.mode,scoped.binding_status,scoped.validation_status,scoped.validation_hash)
     IS DISTINCT FROM (binding.environment,binding.instance_id,binding.mode,binding.binding_status,binding.validation_status,binding.validation_hash) THEN
    RAISE EXCEPTION 'connection_binding_package_scope_conflict' USING ERRCODE = '55000';
   END IF;
   INSERT INTO catalog.connection_bindings
    (tenant_id,manifest_digest,component_digest,store_alias,environment,instance_id,package_id,mode,binding_status,validation_status,validation_hash,bound_at)
   VALUES (binding.tenant_id,binding.manifest_digest,binding.component_digest,binding.store_alias,
    binding.environment,binding.instance_id,owner_name,binding.mode,binding.binding_status,binding.validation_status,binding.validation_hash,binding.bound_at)
   ON CONFLICT DO NOTHING;
  END LOOP;
 END LOOP;
 DELETE FROM catalog.connection_bindings WHERE package_id = '';
END $package_binding_backfill$;
ALTER TABLE catalog.connection_bindings ALTER COLUMN package_id DROP DEFAULT;
ALTER TABLE catalog.connection_bindings ADD CONSTRAINT connection_bindings_package_id_nonempty CHECK (package_id <> '');
DROP FUNCTION IF EXISTS catalog.connection_is_referenced(text);


DO $workload_acl$ DECLARE role_name text := 'wamn_administration'; BEGIN PERFORM pg_advisory_xact_lock(hashtext('wamn_role_bootstrap')); IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = role_name) THEN EXECUTE format('CREATE ROLE %I NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS', role_name); ELSIF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = role_name AND (rolcanlogin OR rolsuper OR rolcreatedb OR rolcreaterole OR rolinherit OR rolreplication OR rolbypassrls)) THEN EXECUTE format('ALTER ROLE %I NOLOGIN PASSWORD NULL NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS', role_name); IF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = role_name AND rolsuper) THEN EXECUTE format('ALTER ROLE %I NOSUPERUSER', role_name); END IF; END IF; END $workload_acl$; REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA catalog, platform, "wamn_run" FROM "wamn_administration"; REVOKE ALL PRIVILEGES ON ALL ROUTINES IN SCHEMA catalog, platform, wamn_history, "wamn_run" FROM "wamn_administration"; REVOKE ALL PRIVILEGES ON SCHEMA catalog, platform, wamn_history, "wamn_run" FROM "wamn_administration"; REVOKE EXECUTE ON FUNCTION wamn_authority.tenant_key(text) FROM "wamn_administration"; GRANT USAGE ON SCHEMA catalog, platform, wamn_history TO "wamn_administration"; GRANT EXECUTE ON FUNCTION wamn_history.row_image(record) TO "wamn_administration"; GRANT SELECT ON TABLE catalog.releases TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."user" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."user_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."role" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."role_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."user_role" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."user_role_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."permission" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."permission_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."environment" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."environment_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."external_system" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."external_system_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."connection" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."connection_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."project_connection" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."project_connection_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."integration_allow" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."integration_allow_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."number_sequence" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."number_sequence_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."setting" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."setting_history" TO "wamn_administration";

-- Read enabled bindings that survive the environment's release contraction.
CREATE OR REPLACE FUNCTION catalog.connection_upgrade_bindings(tenant text, environment_name text)
RETURNS TABLE(connection_name text, integration text)
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog
AS $$
    SELECT DISTINCT instance.org_connection_name,
           CASE WHEN instance.requirement_type = 'blobstore'
                THEN coalesce(generation.definition_json->>'provider', 's3')
                ELSE instance.requirement_type END
      FROM catalog.connection_bindings AS binding
      JOIN catalog.connection_instances AS instance
        ON (instance.tenant_id, instance.environment, instance.instance_id)
         = (binding.tenant_id, binding.environment, binding.instance_id)
      JOIN catalog.connection_generations AS generation
        ON (generation.tenant_id, generation.environment, generation.instance_id, generation.generation)
         = (instance.tenant_id, instance.environment, instance.instance_id, instance.active_generation)
      JOIN catalog.connection_requirements AS requirement
        ON (requirement.tenant_id, requirement.component_digest, requirement.store_alias)
         = (binding.tenant_id, binding.component_digest, binding.store_alias)
      JOIN catalog.releases AS release
        ON (release.tenant_id, release.manifest_digest) = (binding.tenant_id, binding.manifest_digest)
     WHERE binding.tenant_id = tenant AND binding.environment = environment_name
       AND binding.binding_status = 'active' AND binding.validation_status = 'valid'
       AND instance.lifecycle_status = 'enabled' AND instance.org_connection_name IS NOT NULL
     ORDER BY 1, 2
$$;
DO $upgrade_reader_owner$ BEGIN
    EXECUTE format('ALTER FUNCTION catalog.connection_upgrade_bindings(text, text) OWNER TO %I',
        (SELECT pg_get_userbyid(relowner) FROM pg_class
          WHERE oid = 'catalog.connection_bindings'::regclass));
END $upgrade_reader_owner$;
DO $upgrade_reader_policies$
DECLARE
    function_owner name;
    relation_name text;
BEGIN
    SELECT pg_get_userbyid(proowner) INTO function_owner FROM pg_proc
     WHERE oid = 'catalog.connection_upgrade_bindings(text,text)'::regprocedure;
    FOREACH relation_name IN ARRAY ARRAY[
        'connection_bindings', 'connection_generations', 'connection_instances',
        'connection_requirements', 'releases'
    ] LOOP
        EXECUTE format('DROP POLICY IF EXISTS connection_upgrade_reader_owner ON catalog.%I', relation_name);
        EXECUTE format('CREATE POLICY connection_upgrade_reader_owner ON catalog.%I FOR SELECT TO %I USING (true)',
                       relation_name, function_owner);
    END LOOP;
END $upgrade_reader_policies$;
REVOKE ALL ON FUNCTION catalog.connection_upgrade_bindings(text, text) FROM PUBLIC;
DO $upgrade_reader_grant$ BEGIN
    IF to_regrole('wamn_administration') IS NOT NULL THEN
        GRANT EXECUTE ON FUNCTION catalog.connection_upgrade_bindings(text, text) TO wamn_administration;
    END IF;
END $upgrade_reader_grant$;


DO $workload_acl$ DECLARE role_name text := 'wamn_administration'; BEGIN PERFORM pg_advisory_xact_lock(hashtext('wamn_role_bootstrap')); IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = role_name) THEN EXECUTE format('CREATE ROLE %I NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS', role_name); ELSIF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = role_name AND (rolcanlogin OR rolsuper OR rolcreatedb OR rolcreaterole OR rolinherit OR rolreplication OR rolbypassrls)) THEN EXECUTE format('ALTER ROLE %I NOLOGIN PASSWORD NULL NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS', role_name); IF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = role_name AND rolsuper) THEN EXECUTE format('ALTER ROLE %I NOSUPERUSER', role_name); END IF; END IF; END $workload_acl$; REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA catalog, platform, "wamn_run" FROM "wamn_administration"; REVOKE ALL PRIVILEGES ON ALL ROUTINES IN SCHEMA catalog, platform, wamn_history, "wamn_run" FROM "wamn_administration"; REVOKE ALL PRIVILEGES ON SCHEMA catalog, platform, wamn_history, "wamn_run" FROM "wamn_administration"; REVOKE EXECUTE ON FUNCTION wamn_authority.tenant_key(text) FROM "wamn_administration"; GRANT USAGE ON SCHEMA catalog, platform, wamn_history TO "wamn_administration"; GRANT EXECUTE ON FUNCTION wamn_history.row_image(record) TO "wamn_administration"; GRANT SELECT ON TABLE catalog.releases TO "wamn_administration"; GRANT EXECUTE ON FUNCTION catalog.connection_upgrade_bindings(text, text) TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."user" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."user_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."role" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."role_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."user_role" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."user_role_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."permission" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."permission_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."environment" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."environment_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."external_system" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."external_system_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."connection" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."connection_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."project_connection" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."project_connection_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."integration_allow" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."integration_allow_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."number_sequence" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."number_sequence_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."setting" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."setting_history" TO "wamn_administration";
