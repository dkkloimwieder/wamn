-- Apply reads and writes system declarations through this exact surface.
DO $apply_connect$ BEGIN
    EXECUTE format('GRANT CONNECT ON DATABASE %I TO wamn_apply', current_database());
END $apply_connect$;
GRANT USAGE ON SCHEMA registry, identity, catalog, platform, wamn_authority TO wamn_apply;
GRANT INSERT ON platform.tx_identity TO wamn_apply;
GRANT EXECUTE ON FUNCTION wamn_authority.current_identity(), wamn_authority.current_principal_id(),
    wamn_authority.current_role(), wamn_authority.current_application() TO wamn_apply;
GRANT EXECUTE ON FUNCTION registry.login_org(), registry.login_principal(uuid),
    registry.project_capacity_available(int) TO wamn_apply;
GRANT SELECT ON registry.orgs, registry.environment_projects,
    registry.target, identity.org_roles, identity.project_roles,
    identity.project_env_memberships, identity.principals TO wamn_apply;
GRANT SELECT ON registry.integration_type, registry.integration_type_version TO wamn_apply;
GRANT SELECT (platform_domain, max_projects) ON registry.meta TO wamn_apply;
GRANT SELECT (org, id), INSERT (org, id) ON registry.projects TO wamn_apply;
GRANT INSERT (org, env, project, secret_name, secret_namespace, instance_suffix) ON registry.environment_projects TO wamn_apply;
GRANT UPDATE (secret_name, secret_namespace, schema_mode)
    ON registry.environment_projects TO wamn_apply;
GRANT DELETE ON registry.environment_projects TO wamn_apply;
GRANT INSERT, SELECT (org, project, env, instance_suffix)
    ON registry.retired_project_envs TO wamn_apply;
GRANT SELECT, INSERT, UPDATE, DELETE ON registry.environment_applications,
    registry.environment_application_connections, registry.environment_integrations TO wamn_apply;
GRANT INSERT (principal_id, org, project, env) ON identity.project_env_memberships TO wamn_apply;
GRANT SELECT, INSERT, UPDATE ON registry.event_readers TO wamn_apply;
GRANT SELECT, INSERT, UPDATE (resync_at) ON registry.capture_gap TO wamn_apply;

-- The grant projection locks the org membership row (wamn-ms1sn.2).
GRANT SELECT (principal_id, org, active), UPDATE (active)
    ON identity.org_memberships TO wamn_apply;

GRANT SELECT, INSERT ON registry.environments TO wamn_apply;
GRANT UPDATE (disposable, target_id, readiness_budget_seconds, drain_bound_seconds, revocation_seconds)
    ON registry.environments TO wamn_apply;

GRANT EXECUTE ON FUNCTION identity.is_project_admin(uuid,text,text), identity.has_project_access(uuid,text,text,text) TO wamn_apply;

DO $$ BEGIN
    IF to_regclass('registry._sqlx_migrations') IS NOT NULL THEN
        GRANT SELECT ON registry._sqlx_migrations TO wamn_apply;
    END IF;
END $$;
