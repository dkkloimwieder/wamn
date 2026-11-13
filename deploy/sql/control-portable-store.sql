-- Targeted reader authority in the control database.
-- Apply after system-schema.sql as wamn_system.
CREATE SCHEMA IF NOT EXISTS catalog AUTHORIZATION wamn_system;
REVOKE ALL ON SCHEMA catalog FROM PUBLIC;

-- The control author is mapped to one project by its exact login generation.
CREATE SCHEMA IF NOT EXISTS wamn_authority AUTHORIZATION wamn_system;
REVOKE ALL ON SCHEMA wamn_authority FROM PUBLIC;

CREATE TABLE wamn_authority.author_login_projects (
    login_identity text        NOT NULL CHECK (login_identity <> ''),
    project_env_key        text        NOT NULL CHECK (project_env_key <> ''),
    org_id         text        NOT NULL CHECK (org_id <> ''),
    project_id     text        NOT NULL CHECK (project_id <> ''),
    environment    text        NOT NULL CHECK (environment <> ''),
    created_at     timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL,
    CONSTRAINT author_login_projects_pkey PRIMARY KEY (login_identity)
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON wamn_authority.author_login_projects
    FOR EACH ROW EXECUTE FUNCTION wamn_history.stamp_row(
        'created_at', 'created_by', 'updated_at', 'updated_by');
REVOKE ALL ON TABLE wamn_authority.author_login_projects FROM PUBLIC;

REVOKE ALL ON ALL TABLES IN SCHEMA catalog FROM PUBLIC;
REVOKE ALL ON ALL FUNCTIONS IN SCHEMA catalog FROM PUBLIC;
