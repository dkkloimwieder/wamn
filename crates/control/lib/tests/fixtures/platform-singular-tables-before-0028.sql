-- Gives a fresh install of deploy/sql/app-schema.sql the plural role,
-- user_role, permission and api_key table names that a project database had
-- before project migration 0028, so a test can apply the earlier migrations
-- and 0028 to it. It reverses each rename of 0028.
ALTER TABLE platform.role RENAME TO roles;
ALTER TABLE platform.role_history RENAME TO roles_history;
ALTER TABLE platform.user_role RENAME TO user_roles;
ALTER TABLE platform.user_role_history RENAME TO user_roles_history;
ALTER TABLE platform.permission RENAME TO permissions;
ALTER TABLE platform.permission_history RENAME TO permissions_history;
ALTER TABLE platform.api_key RENAME TO api_keys;
ALTER TABLE platform.api_key_history RENAME TO api_keys_history;
ALTER SEQUENCE platform.role_history_position_seq RENAME TO roles_history_position_seq;
ALTER SEQUENCE platform.user_role_history_position_seq RENAME TO user_roles_history_position_seq;
ALTER SEQUENCE platform.permission_history_position_seq RENAME TO permissions_history_position_seq;
ALTER SEQUENCE platform.api_key_history_position_seq RENAME TO api_keys_history_position_seq;
ALTER POLICY role_tenant ON platform.roles RENAME TO roles_tenant;
ALTER POLICY role_platform ON platform.roles RENAME TO roles_platform;
ALTER POLICY role_history_tenant ON platform.roles_history RENAME TO roles_history_tenant;
ALTER POLICY role_history_platform ON platform.roles_history RENAME TO roles_history_platform;
ALTER POLICY user_role_tenant ON platform.user_roles RENAME TO user_roles_tenant;
ALTER POLICY user_role_platform ON platform.user_roles RENAME TO user_roles_platform;
ALTER POLICY user_role_history_tenant ON platform.user_roles_history RENAME TO user_roles_history_tenant;
ALTER POLICY user_role_history_platform ON platform.user_roles_history RENAME TO user_roles_history_platform;
ALTER POLICY permission_tenant ON platform.permissions RENAME TO permissions_tenant;
ALTER POLICY permission_platform ON platform.permissions RENAME TO permissions_platform;
ALTER POLICY permission_history_tenant ON platform.permissions_history RENAME TO permissions_history_tenant;
ALTER POLICY permission_history_platform ON platform.permissions_history RENAME TO permissions_history_platform;
ALTER POLICY api_key_tenant ON platform.api_keys RENAME TO api_keys_tenant;
ALTER POLICY api_key_platform ON platform.api_keys RENAME TO api_keys_platform;
ALTER POLICY api_key_history_tenant ON platform.api_keys_history RENAME TO api_keys_history_tenant;
ALTER POLICY api_key_history_platform ON platform.api_keys_history RENAME TO api_keys_history_platform;
ALTER INDEX platform.role_tkey RENAME TO roles_tkey;
ALTER INDEX platform.role_history_tkey RENAME TO roles_history_tkey;
ALTER INDEX platform.user_role_tkey RENAME TO user_roles_tkey;
ALTER INDEX platform.user_role_history_tkey RENAME TO user_roles_history_tkey;
ALTER INDEX platform.permission_tkey RENAME TO permissions_tkey;
ALTER INDEX platform.permission_history_tkey RENAME TO permissions_history_tkey;
ALTER INDEX platform.api_key_tkey RENAME TO api_keys_tkey;
ALTER INDEX platform.api_key_history_tkey RENAME TO api_keys_history_tkey;
DO $plural_constraints$ DECLARE target record; BEGIN
  FOR target IN
    SELECT c.conrelid::regclass AS relation, c.conname, names.plural, names.singular
      FROM pg_catalog.pg_constraint c
      JOIN (VALUES ('platform.roles'::regclass, 'roles', 'role'),
                   ('platform.roles_history'::regclass, 'roles', 'role'),
                   ('platform.user_roles'::regclass, 'user_roles', 'user_role'),
                   ('platform.user_roles_history'::regclass, 'user_roles', 'user_role'),
                   ('platform.permissions'::regclass, 'permissions', 'permission'),
                   ('platform.permissions_history'::regclass, 'permissions', 'permission'),
                   ('platform.api_keys'::regclass, 'api_keys', 'api_key'),
                   ('platform.api_keys_history'::regclass, 'api_keys', 'api_key'))
        AS names (relation, plural, singular) ON names.relation = c.conrelid
     WHERE starts_with(c.conname, names.singular || '_')
  LOOP
    EXECUTE format('ALTER TABLE %s RENAME CONSTRAINT %I TO %I',
      target.relation, target.conname,
      target.plural || substr(target.conname, length(target.singular) + 1));
  END LOOP;
END $plural_constraints$;
