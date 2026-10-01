-- The user type `person` becomes `user` in an installed project-environment
-- database (owner naming ruling of 2026-10-01 on wamn-a40n.3). upgrade-schema
-- runs this file once in one transaction and records it. A fresh install
-- records it as applied, because app-schema.sql already says `user`.
-- The file binds wamn:provisioning, so each changed row stamps that principal
-- and this run. Old app_system.users_history rows keep `person` in their
-- JSON, because history is a record.
SELECT pg_catalog.set_config('app.user_id', '770df186-ac15-579e-b46b-c297cae2011b', true), pg_catalog.set_config('app.operation', 'wamn:provisioning', true);
ALTER TABLE app_system.users DROP CONSTRAINT users_type_check;
UPDATE app_system.users SET type = 'user' WHERE type = 'person';
ALTER TABLE app_system.users ADD CONSTRAINT users_type_check
    CHECK (type IN ('user', 'service', 'platform'));
