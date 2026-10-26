ALTER TABLE catalog.connection_bindings ADD COLUMN mode text NOT NULL DEFAULT 'write' CHECK (mode IN ('read', 'write'));
ALTER TABLE catalog.connection_bindings ADD COLUMN package_id text NOT NULL DEFAULT '';
ALTER TABLE catalog.connection_bindings DROP CONSTRAINT connection_bindings_pkey;
ALTER TABLE catalog.connection_bindings ADD PRIMARY KEY (tenant_id, manifest_digest, component_digest, store_alias, package_id);
