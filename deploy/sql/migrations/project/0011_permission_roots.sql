-- Permission roots only (docs/plan/platform-deploy.md R18). upgrade-schema runs
-- this file once in one transaction and records it. A fresh install records it
-- as applied, because app-schema.sql holds no closure row.
--
-- A stored permission is a root (`permission = required_by`). The closure rows
-- were derived from a serving release and rewritten at each release change.
-- Each host now expands the roots through the release it loaded, so the
-- derived rows go.

SET LOCAL row_security = off;
SELECT pg_catalog.set_config('app.user_id', '770df186-ac15-579e-b46b-c297cae2011b', true),
       pg_catalog.set_config('app.operation', 'wamn:provisioning', true);

DELETE FROM app_system.permissions WHERE permission <> required_by;
