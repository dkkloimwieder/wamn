-- Apply reads and writes system declarations through this exact surface.
DO $apply_connect$ BEGIN
    EXECUTE format('GRANT CONNECT ON DATABASE %I TO wamn_apply', current_database());
END $apply_connect$;
GRANT USAGE ON SCHEMA registry, identity, catalog, wamn_authority TO wamn_apply;
GRANT SELECT ON registry.orgs, registry.env_policies, registry.project_envs,
    registry.target, registry.meta, registry.schema_baseline, identity.org_roles, identity.project_roles,
    identity.project_env_memberships, identity.principals, catalog.project_environments,
    registry.generation_passwords, wamn_authority.author_login_projects TO wamn_apply;
GRANT SELECT (org, id), INSERT (org, id) ON registry.projects TO wamn_apply;
GRANT INSERT (org, project, env, secret_name, secret_namespace, instance_suffix,
    disposable, policy_name, target_id) ON registry.project_envs TO wamn_apply;
GRANT UPDATE (secret_name, secret_namespace, disposable, target_id, policy_name, route_host, pending_database)
    ON registry.project_envs TO wamn_apply;
GRANT SELECT (id, principal_id, token_prefix, token_hash, revoked_at, expires_at)
    ON identity.pats TO wamn_apply;
GRANT INSERT (type, subject, display_name) ON identity.principals TO wamn_apply;
GRANT INSERT (principal_id, org, project, role) ON identity.project_roles TO wamn_apply;
GRANT INSERT (principal_id, org, project, env) ON identity.project_env_memberships TO wamn_apply;
GRANT INSERT (project_env_key, org, project, env, instance_suffix, disposable, environment_instance)
    ON catalog.project_environments TO wamn_apply;
GRANT UPDATE (instance_suffix, disposable, environment_instance, projected_at)
    ON catalog.project_environments TO wamn_apply;
GRANT INSERT (role, sha256) ON registry.generation_passwords TO wamn_apply;
GRANT UPDATE (sha256, recorded_at) ON registry.generation_passwords TO wamn_apply;
GRANT DELETE ON registry.generation_passwords TO wamn_apply;
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
