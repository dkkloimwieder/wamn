-- Gives a fresh install of deploy/sql/app-schema.sql the plural user table
-- names that a project database had before project migration 0027, so a test
-- can apply the earlier migrations and 0027 to it. It reverses each rename of
-- 0027.
ALTER TABLE platform."user" RENAME TO users;
ALTER TABLE platform.user_history RENAME TO users_history;
ALTER SEQUENCE platform.user_history_position_seq RENAME TO users_history_position_seq;
ALTER POLICY user_tenant ON platform.users RENAME TO users_tenant;
ALTER POLICY user_platform ON platform.users RENAME TO users_platform;
ALTER POLICY user_history_tenant ON platform.users_history RENAME TO users_history_tenant;
ALTER POLICY user_history_platform ON platform.users_history RENAME TO users_history_platform;
ALTER INDEX platform.user_tkey RENAME TO users_tkey;
ALTER INDEX platform.user_history_tkey RENAME TO users_history_tkey;
DO $plural_constraints$ DECLARE target record; BEGIN
  FOR target IN
    SELECT c.conrelid::regclass AS relation, c.conname
      FROM pg_catalog.pg_constraint c
     WHERE c.conrelid IN ('platform.users'::regclass, 'platform.users_history'::regclass)
       AND c.conname LIKE 'user\_%'
  LOOP
    EXECUTE format('ALTER TABLE %s RENAME CONSTRAINT %I TO %I',
      target.relation, target.conname, 'users' || substr(target.conname, 5));
  END LOOP;
END $plural_constraints$;
