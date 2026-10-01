-- Org membership and org administration (docs/plan/platform-ui.md §4.4,
-- wamn-a40n.3): the two org tables, the control family's writes and reads,
-- and the identity issuer's reads of org roles and its insert of a user.
-- upgrade-schema runs this file once in one transaction as wamn_system and
-- records it. A fresh install records it as applied, because
-- system-schema.sql creates the tables and each credential prepare grants
-- its family's surface.
-- wamn_system cannot create a role, so each grant runs only when its role
-- exists. A role that does not exist yet gets its grants at its first
-- prepare.
-- The control grants are the output of
-- wamn_control_provision::sql::control_surface_grants_sql(), and a test in
-- crates/control/provision/src/schema_migrations.rs compares them.

-- The principal type `human` becomes `user` (owner naming ruling of
-- 2026-10-01 on wamn-a40n.3). The file binds wamn:provisioning, so each
-- changed row stamps that principal and this run. The foreign keys on
-- (id, type) have no ON UPDATE action, so the block drops them, changes the
-- rows and adds them again with their names. The authoring command audit is
-- immutable: its old rows keep `human`, and its check admits both values.
SELECT pg_catalog.set_config('app.user_id', '770df186-ac15-579e-b46b-c297cae2011b', true), pg_catalog.set_config('app.operation', 'wamn:provisioning', true);
ALTER TABLE identity.pats
    DROP CONSTRAINT pats_principal_id_principal_type_fkey,
    DROP CONSTRAINT pats_principal_type_check;
ALTER TABLE identity.password_credentials
    DROP CONSTRAINT password_credentials_principal_id_principal_type_fkey,
    DROP CONSTRAINT password_credentials_principal_type_check;
ALTER TABLE identity.password_tokens
    DROP CONSTRAINT password_tokens_principal_id_principal_type_fkey,
    DROP CONSTRAINT password_tokens_principal_type_check;
ALTER TABLE identity.password_logins
    DROP CONSTRAINT password_logins_principal_id_principal_type_fkey,
    DROP CONSTRAINT password_logins_principal_type_check;
ALTER TABLE identity.project_env_memberships
    DROP CONSTRAINT project_env_memberships_principal_id_principal_type_fkey,
    DROP CONSTRAINT project_env_memberships_human_check;
ALTER TABLE identity.principals
    DROP CONSTRAINT principals_type_check,
    DROP CONSTRAINT principals_email_check;
UPDATE identity.principals SET type = 'user' WHERE type = 'human';
UPDATE identity.pats SET principal_type = 'user' WHERE principal_type = 'human';
UPDATE identity.password_credentials SET principal_type = 'user' WHERE principal_type = 'human';
UPDATE identity.password_tokens SET principal_type = 'user' WHERE principal_type = 'human';
UPDATE identity.password_logins SET principal_type = 'user' WHERE principal_type = 'human';
UPDATE identity.project_env_memberships SET principal_type = 'user' WHERE principal_type = 'human';
ALTER TABLE identity.principals
    ADD CONSTRAINT principals_type_check
        CHECK (type IN ('user', 'service', 'platform')),
    ADD CONSTRAINT principals_email_check
        CHECK ((type = 'user') = (email IS NOT NULL)
               AND (email IS NULL
                    OR (email ~ '^[^@[:space:]]+@[^@[:space:]]+\.[^@[:space:]]+$'
                        AND char_length(email) <= 254)));
ALTER TABLE identity.pats
    ADD CONSTRAINT pats_principal_id_principal_type_fkey FOREIGN KEY (principal_id, principal_type)
        REFERENCES identity.principals (id, type) ON DELETE RESTRICT,
    ADD CONSTRAINT pats_principal_type_check
        CHECK (principal_type IN ('user', 'service'));
ALTER TABLE identity.password_credentials
    ALTER COLUMN principal_type SET DEFAULT 'user',
    ADD CONSTRAINT password_credentials_principal_type_check CHECK (principal_type = 'user'),
    ADD CONSTRAINT password_credentials_principal_id_principal_type_fkey FOREIGN KEY (principal_id, principal_type)
        REFERENCES identity.principals (id, type) ON DELETE RESTRICT;
ALTER TABLE identity.password_tokens
    ALTER COLUMN principal_type SET DEFAULT 'user',
    ADD CONSTRAINT password_tokens_principal_type_check CHECK (principal_type = 'user'),
    ADD CONSTRAINT password_tokens_principal_id_principal_type_fkey FOREIGN KEY (principal_id, principal_type)
        REFERENCES identity.principals (id, type) ON DELETE RESTRICT;
ALTER TABLE identity.password_logins
    ALTER COLUMN principal_type SET DEFAULT 'user',
    ADD CONSTRAINT password_logins_principal_type_check CHECK (principal_type = 'user'),
    ADD CONSTRAINT password_logins_principal_id_principal_type_fkey FOREIGN KEY (principal_id, principal_type)
        REFERENCES identity.principals (id, type) ON DELETE RESTRICT;
ALTER TABLE identity.project_env_memberships
    ALTER COLUMN principal_type SET DEFAULT 'user',
    ADD CONSTRAINT project_env_memberships_principal_id_principal_type_fkey FOREIGN KEY (principal_id, principal_type)
        REFERENCES identity.principals (id, type) ON DELETE CASCADE,
    ADD CONSTRAINT project_env_memberships_user_check
        CHECK (principal_type = 'user');
CREATE OR REPLACE FUNCTION identity.lock_password_principal(principal uuid) RETURNS boolean
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog AS $$
DECLARE eligible boolean;
BEGIN
    SELECT status = 'active' AND type = 'user' INTO eligible
    FROM identity.principals WHERE id = principal FOR UPDATE;
    RETURN COALESCE(eligible, false);
END;
$$;
ALTER TABLE catalog.authoring_command_audit
    DROP CONSTRAINT authoring_command_audit_principal_type_check,
    ADD CONSTRAINT authoring_command_audit_principal_type_check
        CHECK (principal_type IN ('user', 'service', 'human'));

-- A user's membership of one org (docs/plan/platform-ui.md §2.7). The
-- principal stays global, and the membership status is org-local: an
-- inactive member keeps the row and holds no access in the org.
CREATE TABLE identity.org_memberships (
    principal_id   uuid NOT NULL,
    principal_type text NOT NULL DEFAULT 'user',
    org            text NOT NULL
        REFERENCES registry.orgs (id) ON DELETE CASCADE,
    status         text NOT NULL,
    created_at     timestamptz NOT NULL,
    created_by     uuid NOT NULL,
    updated_at     timestamptz NOT NULL,
    updated_by     uuid NOT NULL,
    PRIMARY KEY (principal_id, org),
    FOREIGN KEY (principal_id, principal_type)
        REFERENCES identity.principals (id, type) ON DELETE CASCADE,
    CONSTRAINT org_memberships_user_check
        CHECK (principal_type = 'user'),
    CONSTRAINT org_memberships_status_check
        CHECK (status IN ('active', 'inactive'))
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON identity.org_memberships
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

-- A user's administrative role in one org (docs/plan/platform-ui.md §4.4).
-- A role needs the user's membership of the org, and deleting the
-- membership deletes the role.
CREATE TABLE identity.org_roles (
    principal_id uuid NOT NULL,
    org          text NOT NULL,
    role         text NOT NULL,
    created_at   timestamptz NOT NULL,
    created_by   uuid NOT NULL,
    updated_at   timestamptz NOT NULL,
    updated_by   uuid NOT NULL,
    PRIMARY KEY (principal_id, org, role),
    FOREIGN KEY (principal_id, org)
        REFERENCES identity.org_memberships (principal_id, org) ON DELETE CASCADE,
    CONSTRAINT org_roles_role_check
        CHECK (role = 'org-admin')
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON identity.org_roles
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

DO $org_issuer_grants$ BEGIN
  IF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'wamn_identity_issuer') THEN
    GRANT SELECT (principal_id, org, role) ON TABLE identity.org_roles
      TO wamn_identity_issuer;
    GRANT INSERT (type, subject, email, display_name) ON TABLE identity.principals
      TO wamn_identity_issuer;
  END IF;
END $org_issuer_grants$;

DO $control_surface$ BEGIN
  IF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'wamn_control') THEN
    REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA "identity", "provisioning", "registry" FROM "wamn_control"; REVOKE ALL PRIVILEGES ON SCHEMA "identity", "provisioning", "registry" FROM "wamn_control"; GRANT USAGE ON SCHEMA "identity", "registry" TO "wamn_control"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "identity"."org_memberships" TO "wamn_control"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "identity"."org_roles" TO "wamn_control"; GRANT SELECT ON TABLE "identity"."password_logins" TO "wamn_control"; GRANT SELECT ON TABLE "identity"."principals" TO "wamn_control"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "identity"."project_env_memberships" TO "wamn_control"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "identity"."project_roles" TO "wamn_control"; GRANT SELECT ON TABLE "registry"."project_envs" TO "wamn_control"; GRANT SELECT ON TABLE "registry"."projects" TO "wamn_control";
  END IF;
END $control_surface$;
