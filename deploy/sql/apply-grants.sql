-- Apply reads and writes system declarations through this exact surface.
DO $apply_connect$ BEGIN
    EXECUTE format('GRANT CONNECT ON DATABASE %I TO wamn_apply', current_database());
END $apply_connect$;
GRANT USAGE ON SCHEMA registry, identity, catalog, wamn_authority TO wamn_apply;
GRANT SELECT ON registry.orgs, registry.env_policies, registry.environment_projects,
    registry.target, registry.meta, identity.org_roles, identity.project_roles,
    identity.project_env_memberships, identity.principals,
    wamn_authority.author_login_projects TO wamn_apply;
GRANT SELECT (org, id), INSERT (org, id) ON registry.projects TO wamn_apply;
GRANT INSERT (org, env, project, secret_name, secret_namespace, instance_suffix) ON registry.environment_projects TO wamn_apply;
GRANT UPDATE (secret_name, secret_namespace, configuration)
    ON registry.environment_projects TO wamn_apply;
GRANT SELECT (id, principal_id, token_prefix, token_hash, revoked_at, expires_at)
    ON identity.pats TO wamn_apply;
GRANT INSERT (type, subject, display_name) ON identity.principals TO wamn_apply;
GRANT INSERT (principal_id, org, project, role) ON identity.project_roles TO wamn_apply;
GRANT INSERT (principal_id, org, project, env) ON identity.project_env_memberships TO wamn_apply;
GRANT INSERT (login_identity, project_env_key, org_id, project_id, environment)
    ON wamn_authority.author_login_projects TO wamn_apply;
GRANT UPDATE (org_id, project_id, environment)
    ON wamn_authority.author_login_projects TO wamn_apply;
GRANT SELECT, INSERT, UPDATE ON registry.event_readers TO wamn_apply;
GRANT EXECUTE ON FUNCTION
    registry.resolve_environment_connection(text,text,text,text,text,integer),
    registry.connection_projection(text,text,text) TO wamn_apply;
-- The grant projection locks the org membership row (wamn-ms1sn.2).
GRANT SELECT (principal_id, org, status), UPDATE (status)
    ON identity.org_memberships TO wamn_apply;

GRANT SELECT, INSERT ON registry.environments TO wamn_apply;
GRANT UPDATE (disposable, target_id) ON registry.environments TO wamn_apply;

GRANT EXECUTE ON FUNCTION identity.is_project_admin(uuid,text,text,text), identity.has_project_access(uuid,text,text,text) TO wamn_apply;
