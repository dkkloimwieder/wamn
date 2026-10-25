-- Restore the five administration tables before project migration 0038.
-- Existing rows and history keys take the tenant named by wamn.fixture_tenant.
ALTER TABLE platform.user_role DROP CONSTRAINT user_role_role_name_fkey;
ALTER TABLE platform.permission DROP CONSTRAINT permission_role_name_fkey;
ALTER TABLE platform.api_key DROP CONSTRAINT api_key_key_hash_key;
DO $administration_tenant_floor$ DECLARE relation text; target text; BEGIN
  FOREACH relation IN ARRAY ARRAY['role', 'user_role', 'permission', 'api_key', 'environment'] LOOP
    EXECUTE format('ALTER TABLE platform.%I DROP CONSTRAINT %I', relation, relation || '_pkey');
    EXECUTE format('ALTER TABLE platform.%I ADD COLUMN tenant_id text NOT NULL '
                   'DEFAULT current_setting(''wamn.fixture_tenant'') CHECK (tenant_id <> '''')',
                   relation);
    EXECUTE format('ALTER TABLE platform.%I ADD COLUMN tenant_id text NOT NULL '
                   'DEFAULT current_setting(''wamn.fixture_tenant'')', relation || '_history');
    FOREACH target IN ARRAY ARRAY[relation, relation || '_history'] LOOP
      EXECUTE format('ALTER TABLE platform.%I ALTER COLUMN tenant_id DROP DEFAULT', target);
      EXECUTE format('ALTER TABLE platform.%I ENABLE ROW LEVEL SECURITY', target);
      EXECUTE format('ALTER TABLE platform.%I FORCE ROW LEVEL SECURITY', target);
      EXECUTE format('CREATE POLICY %I ON platform.%I TO wamn_app '
                     'USING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key()) '
                     'WITH CHECK (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())',
                     target || '_tenant', target);
      EXECUTE format('CREATE POLICY %I ON platform.%I AS PERMISSIVE FOR ALL TO wamn_platform '
                     'USING (true) WITH CHECK (true)', target || '_platform', target);
      EXECUTE format('CREATE INDEX %I ON platform.%I ((wamn_authority.tenant_key(tenant_id)))',
                     target || '_tkey', target);
    END LOOP;
    IF relation = 'environment' THEN
      UPDATE platform.environment_history SET row_key = jsonb_build_object('tenant_id', tenant_id);
    ELSE
      EXECUTE format('UPDATE platform.%I SET row_key = jsonb_build_object(''tenant_id'', tenant_id) '
                     '|| row_key', relation || '_history');
    END IF;
  END LOOP;
END $administration_tenant_floor$;
ALTER TABLE platform.environment DROP COLUMN id;
ALTER TABLE platform.role ADD PRIMARY KEY (tenant_id, name);
ALTER TABLE platform.user_role ADD PRIMARY KEY (tenant_id, user_id, role_name);
ALTER TABLE platform.user_role ADD FOREIGN KEY (tenant_id, role_name)
    REFERENCES platform.role (tenant_id, name) ON DELETE CASCADE;
ALTER TABLE platform.permission ADD PRIMARY KEY (tenant_id, role_name, permission);
ALTER TABLE platform.permission ADD FOREIGN KEY (tenant_id, role_name)
    REFERENCES platform.role (tenant_id, name) ON DELETE CASCADE;
ALTER TABLE platform.api_key ADD PRIMARY KEY (tenant_id, id);
ALTER TABLE platform.api_key ADD UNIQUE (tenant_id, key_hash);
ALTER TABLE platform.environment ADD PRIMARY KEY (tenant_id);
