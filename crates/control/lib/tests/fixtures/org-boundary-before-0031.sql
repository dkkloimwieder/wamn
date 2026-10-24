-- Gives a fresh install of deploy/sql/app-schema.sql the tenant floor that
-- platform."user" and the platform configuration tables had before project
-- migration 0031, so a test can apply the earlier migrations and 0031 to it.
-- It reverses each change of 0031 to the tables: each table and its history
-- table get tenant_id, row security, the two policies and the tenant_key
-- index, and each key and foreign key gets tenant_id back, with the names
-- that app-schema.sql gave them. Each row that is there takes the tenant that
-- the setting wamn.fixture_tenant names, and each history entry also takes it
-- in its row_key. The counter functions stay as app-schema.sql has them.
ALTER TABLE platform.user_role DROP CONSTRAINT user_role_user_id_fkey;
ALTER TABLE platform.api_key DROP CONSTRAINT api_key_user_id_fkey;
ALTER TABLE platform.connection DROP CONSTRAINT connection_external_system_id_fkey;
ALTER TABLE platform.project_connection DROP CONSTRAINT project_connection_connection_id_fkey;
ALTER TABLE platform.number_counter DROP CONSTRAINT number_counter_sequence_id_fkey;
ALTER TABLE platform."user" DROP CONSTRAINT user_email_key;
ALTER TABLE platform.external_system DROP CONSTRAINT external_system_name_key;
ALTER TABLE platform.connection DROP CONSTRAINT connection_name_key;
ALTER TABLE platform.number_sequence DROP CONSTRAINT number_sequence_application_name_key;
DO $tenant_floor$ DECLARE relation text; history boolean; BEGIN
  FOR relation, history IN
    SELECT r.name, r.history
      FROM (VALUES ('user', true), ('external_system', true), ('connection', true),
                   ('integration_allow', true), ('project_connection', true),
                   ('number_sequence', true), ('number_counter', false), ('setting', true))
        AS r (name, history)
  LOOP
    EXECUTE format('ALTER TABLE platform.%I DROP CONSTRAINT %I', relation, relation || '_pkey');
    EXECUTE format('ALTER TABLE platform.%I ADD COLUMN tenant_id text NOT NULL '
                   'DEFAULT current_setting(''wamn.fixture_tenant'') CHECK (tenant_id <> '''')',
                   relation);
    EXECUTE format('ALTER TABLE platform.%I ALTER COLUMN tenant_id DROP DEFAULT', relation);
    EXECUTE format('ALTER TABLE platform.%I ENABLE ROW LEVEL SECURITY', relation);
    EXECUTE format('ALTER TABLE platform.%I FORCE ROW LEVEL SECURITY', relation);
    EXECUTE format('CREATE POLICY %I ON platform.%I TO wamn_app '
                   'USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key()) '
                   'WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())',
                   relation || '_tenant', relation);
    EXECUTE format('CREATE POLICY %I ON platform.%I AS PERMISSIVE FOR ALL TO wamn_platform '
                   'USING (true) WITH CHECK (true)',
                   relation || '_platform', relation);
    EXECUTE format('CREATE INDEX %I ON platform.%I ((wamn_authority.tenant_key(tenant_id)))',
                   relation || '_tkey', relation);
    IF history THEN
      EXECUTE format('ALTER TABLE platform.%I ADD COLUMN tenant_id text CONSTRAINT %I NOT NULL '
                     'DEFAULT current_setting(''wamn.fixture_tenant'')',
                     relation || '_history', relation || '_history_tenant_id_not_null');
      EXECUTE format('ALTER TABLE platform.%I ALTER COLUMN tenant_id DROP DEFAULT',
                     relation || '_history');
      EXECUTE format('UPDATE platform.%I SET row_key = jsonb_build_object(''tenant_id'', tenant_id) '
                     '|| row_key', relation || '_history');
      EXECUTE format('ALTER TABLE platform.%I ENABLE ROW LEVEL SECURITY', relation || '_history');
      EXECUTE format('ALTER TABLE platform.%I FORCE ROW LEVEL SECURITY', relation || '_history');
      EXECUTE format('CREATE POLICY %I ON platform.%I TO wamn_app '
                     'USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key()) '
                     'WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())',
                     relation || '_history_tenant', relation || '_history');
      EXECUTE format('CREATE POLICY %I ON platform.%I AS PERMISSIVE FOR ALL TO wamn_platform '
                     'USING (true) WITH CHECK (true)',
                     relation || '_history_platform', relation || '_history');
      EXECUTE format('CREATE INDEX %I ON platform.%I ((wamn_authority.tenant_key(tenant_id)))',
                     relation || '_history_tkey', relation || '_history');
    END IF;
  END LOOP;
END $tenant_floor$;
ALTER TABLE platform."user" ADD CONSTRAINT user_pkey PRIMARY KEY (tenant_id, id);
ALTER TABLE platform."user" ADD CONSTRAINT user_tenant_id_email_key UNIQUE (tenant_id, email);
ALTER TABLE platform.user_role ADD CONSTRAINT user_role_tenant_id_user_id_fkey
    FOREIGN KEY (tenant_id, user_id) REFERENCES platform."user" (tenant_id, id) ON DELETE CASCADE;
ALTER TABLE platform.api_key ADD CONSTRAINT api_key_tenant_id_user_id_fkey
    FOREIGN KEY (tenant_id, user_id) REFERENCES platform."user" (tenant_id, id) ON DELETE CASCADE;
ALTER TABLE platform.external_system ADD CONSTRAINT external_system_pkey
    PRIMARY KEY (tenant_id, id);
ALTER TABLE platform.external_system ADD CONSTRAINT external_system_tenant_id_name_key
    UNIQUE (tenant_id, name);
ALTER TABLE platform.connection ADD CONSTRAINT connection_pkey PRIMARY KEY (tenant_id, id);
ALTER TABLE platform.connection ADD CONSTRAINT connection_tenant_id_name_key
    UNIQUE (tenant_id, name);
ALTER TABLE platform.connection ADD CONSTRAINT connection_tenant_id_external_system_id_fkey
    FOREIGN KEY (tenant_id, external_system_id) REFERENCES platform.external_system (tenant_id, id);
ALTER TABLE platform.integration_allow ADD CONSTRAINT integration_allow_pkey
    PRIMARY KEY (tenant_id, scope_type, scope_id, integration);
ALTER TABLE platform.project_connection ADD CONSTRAINT project_connection_pkey
    PRIMARY KEY (tenant_id, project_id, connection_id);
ALTER TABLE platform.project_connection ADD CONSTRAINT project_connection_tenant_id_connection_id_fkey
    FOREIGN KEY (tenant_id, connection_id) REFERENCES platform.connection (tenant_id, id);
ALTER TABLE platform.number_sequence ADD CONSTRAINT number_sequence_pkey PRIMARY KEY (tenant_id, id);
ALTER TABLE platform.number_sequence ADD CONSTRAINT number_sequence_tenant_id_application_name_key
    UNIQUE (tenant_id, application, name);
ALTER TABLE platform.number_counter ADD CONSTRAINT number_counter_pkey
    PRIMARY KEY (tenant_id, sequence_id, counter, key_value);
ALTER TABLE platform.number_counter ADD CONSTRAINT number_counter_tenant_id_sequence_id_fkey
    FOREIGN KEY (tenant_id, sequence_id) REFERENCES platform.number_sequence (tenant_id, id)
    ON DELETE CASCADE;
ALTER TABLE platform.setting ADD CONSTRAINT setting_pkey
    PRIMARY KEY (tenant_id, scope_type, scope_id, key);
