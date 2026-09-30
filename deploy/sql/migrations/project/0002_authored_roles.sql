-- The application authority model of docs/plan/platform-ui.md §2.2 to §2.4 and
-- §4.1 in an installed project-environment database (wamn-a40n.1).
-- upgrade-schema runs this file once in one transaction and records it. A
-- fresh install records it as applied, because app-schema.sql already holds
-- the new shape.
--
-- 1. A holder of `operator` holds `admin`, and the role `operator` goes.
--    `operator` and `admin` held the same grants, so no holder loses authority.
-- 2. `admin` loses its permission rows. It holds every served operation.
-- 3. A surviving authored permission becomes a stable reference without its
--    `@<version>` and a direct selection (`required_by = permission`). The
--    closure rows of each selection are written when the next candidate
--    release is reconciled before activation.
-- 4. The permission key gains `required_by`, the closure rows follow their
--    root, and role names take the identity role slug.

SET LOCAL row_security = off;
SELECT pg_catalog.set_config('app.user_id', '770df186-ac15-579e-b46b-c297cae2011b', true),
       pg_catalog.set_config('app.operation', 'wamn:provisioning', true);

INSERT INTO app_system.roles (tenant_id, name)
SELECT DISTINCT tenant_id, 'admin' FROM app_system.roles WHERE name = 'operator'
ON CONFLICT (tenant_id, name) DO NOTHING;

INSERT INTO app_system.user_roles (tenant_id, user_id, role_name)
SELECT tenant_id, user_id, 'admin' FROM app_system.user_roles WHERE role_name = 'operator'
ON CONFLICT (tenant_id, user_id, role_name) DO NOTHING;

DELETE FROM app_system.roles WHERE name = 'operator';

DELETE FROM app_system.permissions WHERE role_name = 'admin';

-- Two versions of one operation in one role become one reference.
DELETE FROM app_system.permissions AS older
 USING app_system.permissions AS newer
 WHERE older.tenant_id = newer.tenant_id
   AND older.role_name = newer.role_name
   AND pg_catalog.regexp_replace(older.permission, '@[^@]*$', '')
       = pg_catalog.regexp_replace(newer.permission, '@[^@]*$', '')
   AND older.permission < newer.permission;

ALTER TABLE app_system.permissions ADD COLUMN required_by text;

UPDATE app_system.permissions
   SET permission = pg_catalog.regexp_replace(permission, '@[^@]*$', ''),
       required_by = pg_catalog.regexp_replace(permission, '@[^@]*$', '');

ALTER TABLE app_system.permissions ALTER COLUMN required_by SET NOT NULL;
ALTER TABLE app_system.permissions DROP CONSTRAINT permissions_pkey;
ALTER TABLE app_system.permissions
    ADD PRIMARY KEY (tenant_id, role_name, permission, required_by);
ALTER TABLE app_system.permissions
    ADD CONSTRAINT permissions_required_by_fkey
        FOREIGN KEY (tenant_id, role_name, required_by, required_by)
        REFERENCES app_system.permissions (tenant_id, role_name, permission, required_by)
        ON DELETE CASCADE,
    ADD CONSTRAINT permissions_admin_check CHECK (role_name <> 'admin'),
    ADD CONSTRAINT permissions_reference_check
        CHECK (permission ~ '^[a-z][a-z0-9]*(-[a-z0-9]+)*:[a-z][a-z0-9]*(-[a-z0-9]+)*/[a-z][a-z0-9]*(-[a-z0-9]+)*$'
           AND required_by ~ '^[a-z][a-z0-9]*(-[a-z0-9]+)*:[a-z][a-z0-9]*(-[a-z0-9]+)*/[a-z][a-z0-9]*(-[a-z0-9]+)*$');

ALTER TABLE app_system.roles
    ADD CONSTRAINT roles_name_check
        CHECK (name ~ '^[a-z0-9][a-z0-9-]*$' AND octet_length(name) <= 64);
