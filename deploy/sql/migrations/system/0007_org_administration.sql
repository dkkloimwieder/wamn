-- Org membership and org administration (docs/plan/platform-ui.md §4.4,
-- wamn-a40n.3): the two org tables, the control family's writes and reads,
-- and the identity issuer's reads of org roles and its insert of a human.
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

-- A person's membership of one org (docs/plan/platform-ui.md §2.7). The
-- principal stays global, and the membership status is org-local: an
-- inactive member keeps the row and holds no access in the org.
CREATE TABLE identity.org_memberships (
    principal_id   uuid NOT NULL,
    principal_type text NOT NULL DEFAULT 'human',
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
    CONSTRAINT org_memberships_human_check
        CHECK (principal_type = 'human'),
    CONSTRAINT org_memberships_status_check
        CHECK (status IN ('active', 'inactive'))
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON identity.org_memberships
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

-- A person's administrative role in one org (docs/plan/platform-ui.md §4.4).
-- A role needs the person's membership of the org, and deleting the
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
