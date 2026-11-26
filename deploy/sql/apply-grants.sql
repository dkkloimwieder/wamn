-- Apply reads and writes system declarations through this exact surface.
DO $apply_connect$ BEGIN
    EXECUTE format('GRANT CONNECT ON DATABASE %I TO wamn_apply', current_database());
END $apply_connect$;
GRANT USAGE ON SCHEMA registry, identity, catalog, platform, wamn_authority TO wamn_apply;
GRANT INSERT ON platform.tx_identity TO wamn_apply;
GRANT EXECUTE ON FUNCTION wamn_authority.current_identity(), wamn_authority.current_principal_id(),
    wamn_authority.current_role(), wamn_authority.current_app() TO wamn_apply;
GRANT EXECUTE ON FUNCTION registry.login_org(), registry.login_principal(uuid),
    registry.project_capacity_available(int) TO wamn_apply;
GRANT SELECT ON registry.org, registry.project_env,
    registry.target, identity.org_role, identity.project_role,
    identity.project_env_membership, identity.principal TO wamn_apply;
GRANT SELECT ON registry.integration_type, registry.integration_type_version TO wamn_apply;
GRANT SELECT (platform_domain, max_projects) ON registry.meta TO wamn_apply;
GRANT SELECT (org, id), INSERT (org, id) ON registry.project TO wamn_apply;
GRANT INSERT (org, env, project, secret_name, secret_namespace, instance_suffix) ON registry.project_env TO wamn_apply;
GRANT UPDATE (secret_name, secret_namespace, instance_suffix, active, schema_mode)
    ON registry.project_env TO wamn_apply;
GRANT SELECT, INSERT, UPDATE ON registry.env_app TO wamn_apply;
GRANT SELECT, INSERT, UPDATE, DELETE ON registry.env_app_connection, registry.env_integration
    TO wamn_apply;
GRANT INSERT (principal_id, org, project, env) ON identity.project_env_membership TO wamn_apply;
GRANT DELETE ON identity.project_env_membership TO wamn_apply;
GRANT SELECT, INSERT, UPDATE, DELETE ON registry.event_reader TO wamn_apply;
GRANT SELECT, INSERT, UPDATE (resync_at), DELETE ON registry.capture_gap TO wamn_apply;

-- The grant projection locks the org membership row (wamn-ms1sn.2).
GRANT SELECT (principal_id, org, active), UPDATE (active)
    ON identity.org_membership TO wamn_apply;

GRANT SELECT, INSERT ON registry.env TO wamn_apply;
GRANT UPDATE (disposable, target_id, readiness_budget_seconds, drain_bound_seconds, revocation_seconds)
    ON registry.env TO wamn_apply;

GRANT USAGE ON SCHEMA provisioning TO wamn_apply;
GRANT INSERT, SELECT (command_id, outcome) ON provisioning.platform_event TO wamn_apply;

GRANT EXECUTE ON FUNCTION identity.is_project_admin(uuid,text,text), identity.has_project_access(uuid,text,text,text) TO wamn_apply;

GRANT USAGE ON SCHEMA wamn_history TO wamn_apply;
GRANT EXECUTE ON FUNCTION wamn_history.row_image(record) TO wamn_apply;

DO $apply_history$
DECLARE source record;
BEGIN
    FOR source IN
        SELECT n.nspname, c.relname
        FROM pg_trigger t
        JOIN pg_class c ON c.oid = t.tgrelid
        JOIN pg_namespace n ON n.oid = c.relnamespace
        WHERE t.tgfoid = 'wamn_history.log_row_change()'::regprocedure
          AND n.nspname IN ('registry', 'identity')
          AND (has_any_column_privilege('wamn_apply', c.oid, 'INSERT,UPDATE')
               OR has_table_privilege('wamn_apply', c.oid, 'DELETE'))
    LOOP
        EXECUTE format('GRANT INSERT ON %I.%I TO wamn_apply',
                       source.nspname, source.relname || '_history');
    END LOOP;

    IF to_regclass('registry._sqlx_migrations') IS NOT NULL THEN
        GRANT SELECT ON registry._sqlx_migrations TO wamn_apply;
    END IF;
END
$apply_history$;
