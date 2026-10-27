-- C4: application-owned files pinned to the generation that stored their bytes.
CREATE TABLE platform.file (
    id                    uuid NOT NULL DEFAULT gen_random_uuid(),
    application           text NOT NULL CHECK (application <> ''),
    connection_id         uuid NOT NULL REFERENCES platform.connection (id),
    connection_generation bigint NOT NULL CHECK (connection_generation > 0),
    name                  text NOT NULL CHECK (name <> ''),
    media_type            text NOT NULL CHECK (media_type <> ''),
    size                  bigint NOT NULL CHECK (size >= 0),
    sha256                text NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    blob_key              text NOT NULL UNIQUE,
    exposure              text NOT NULL DEFAULT 'application'
                              CHECK (exposure IN ('private', 'application', 'public')),
    url                   text,
    width                 integer CHECK (width > 0),
    height                integer CHECK (height > 0),
    created_at            timestamptz NOT NULL,
    created_by            uuid NOT NULL,
    updated_at            timestamptz NOT NULL,
    updated_by            uuid NOT NULL,
    PRIMARY KEY (id),
    CHECK (blob_key = '_wamn_files/live/' || id::text),
    CHECK ((exposure = 'public') = (url IS NOT NULL)),
    CHECK (url IS NULL OR url <> '')
);

CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON platform.file
    FOR EACH ROW EXECUTE FUNCTION
    wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');
SELECT wamn_history.create_history_table('platform', 'file', false);
CREATE TRIGGER wamn_record_history_log
    AFTER INSERT OR UPDATE OR DELETE ON platform.file
    FOR EACH ROW EXECUTE FUNCTION wamn_history.log_row_change('unlimited');
GRANT SELECT ON platform.file TO wamn_app;


DO $workload_acl$ DECLARE role_name text := 'wamn_administration'; BEGIN PERFORM pg_advisory_xact_lock(hashtext('wamn_role_bootstrap')); IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = role_name) THEN EXECUTE format('CREATE ROLE %I NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS', role_name); ELSIF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = role_name AND (rolcanlogin OR rolsuper OR rolcreatedb OR rolcreaterole OR rolinherit OR rolreplication OR rolbypassrls)) THEN EXECUTE format('ALTER ROLE %I NOLOGIN PASSWORD NULL NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS', role_name); IF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = role_name AND rolsuper) THEN EXECUTE format('ALTER ROLE %I NOSUPERUSER', role_name); END IF; END IF; END $workload_acl$; REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA catalog, platform, "wamn_run" FROM "wamn_administration"; REVOKE ALL PRIVILEGES ON ALL ROUTINES IN SCHEMA catalog, platform, wamn_history, "wamn_run" FROM "wamn_administration"; REVOKE ALL PRIVILEGES ON SCHEMA catalog, platform, wamn_history, "wamn_run" FROM "wamn_administration"; REVOKE EXECUTE ON FUNCTION wamn_authority.tenant_key(text) FROM "wamn_administration"; GRANT USAGE ON SCHEMA catalog, platform, wamn_history TO "wamn_administration"; GRANT EXECUTE ON FUNCTION wamn_history.row_image(record) TO "wamn_administration"; GRANT SELECT ON TABLE catalog.releases TO "wamn_administration"; GRANT EXECUTE ON FUNCTION catalog.connection_upgrade_bindings(text, text) TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."user" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."user_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."role" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."role_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."user_role" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."user_role_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."permission" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."permission_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."environment" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."environment_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."external_system" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."external_system_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."connection" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."connection_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."file" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."file_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."project_connection" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."project_connection_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."integration_allow" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."integration_allow_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."number_sequence" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."number_sequence_history" TO "wamn_administration"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform."setting" TO "wamn_administration"; GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform."setting_history" TO "wamn_administration";
