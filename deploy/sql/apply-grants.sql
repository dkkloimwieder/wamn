-- Apply reads and writes system declarations through this exact surface.
DO $apply_connect$ BEGIN
    EXECUTE format('GRANT CONNECT ON DATABASE %I TO wamn_apply', current_database());
END $apply_connect$;
GRANT USAGE ON SCHEMA registry, identity, catalog, platform, wamn_authority TO wamn_apply;
GRANT INSERT ON platform.tx_identity TO wamn_apply;
GRANT EXECUTE ON FUNCTION wamn_authority.current_identity(), wamn_authority.current_principal_id(),
    wamn_authority.current_role(), wamn_authority.current_application() TO wamn_apply;
GRANT EXECUTE ON FUNCTION registry.control_org(), registry.control_principal(uuid) TO wamn_apply;
GRANT SELECT ON registry.orgs, registry.env_policies, registry.environment_projects,
    registry.target, registry.meta, identity.org_roles, identity.project_roles,
    identity.project_env_memberships, identity.principals,
    wamn_authority.author_login_projects TO wamn_apply;
GRANT SELECT (org, id), INSERT (org, id) ON registry.projects TO wamn_apply;
GRANT INSERT (org, env, project, secret_name, secret_namespace, instance_suffix) ON registry.environment_projects TO wamn_apply;
GRANT UPDATE (secret_name, secret_namespace, schema_mode)
    ON registry.environment_projects TO wamn_apply;
GRANT SELECT, INSERT, UPDATE, DELETE ON registry.environment_applications,
    registry.environment_application_connections, registry.environment_integrations TO wamn_apply;
GRANT INSERT (principal_id, org, project, env) ON identity.project_env_memberships TO wamn_apply;
GRANT INSERT (login_identity, project_env_key, org_id, project_id, environment)
    ON wamn_authority.author_login_projects TO wamn_apply;
GRANT UPDATE (org_id, project_id, environment)
    ON wamn_authority.author_login_projects TO wamn_apply;
GRANT SELECT, INSERT, UPDATE ON registry.event_readers TO wamn_apply;

-- The grant projection locks the org membership row (wamn-ms1sn.2).
GRANT SELECT (principal_id, org, status), UPDATE (status)
    ON identity.org_memberships TO wamn_apply;

GRANT SELECT, INSERT ON registry.environments TO wamn_apply;
GRANT UPDATE (disposable, target_id, policy_name) ON registry.environments TO wamn_apply;

GRANT EXECUTE ON FUNCTION identity.is_project_admin(uuid,text,text), identity.has_project_access(uuid,text,text,text) TO wamn_apply;

DO $$ BEGIN
    IF to_regclass('registry._sqlx_migrations') IS NOT NULL THEN
        GRANT SELECT ON registry._sqlx_migrations TO wamn_apply;
    END IF;
END $$;
