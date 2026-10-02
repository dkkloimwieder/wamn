-- The status of each project environment (wamn-zua8.2, docs/plan/platform-ui.md
-- §5.4). Identity offers no audience of an inactive environment, and its host
-- serves none of its routes. Every existing environment is active.
-- An installed database whose issuer was never prepared has no issuer role,
-- and its first prepare grants the read, so the grant runs only when the
-- role exists.
ALTER TABLE registry.project_envs
    ADD COLUMN status text NOT NULL DEFAULT 'active',
    ADD CONSTRAINT project_envs_status_check CHECK (status IN ('active', 'inactive'));

DO $environment_status$ BEGIN
  IF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'wamn_identity_issuer') THEN
    GRANT SELECT (status) ON TABLE registry.project_envs TO wamn_identity_issuer;
  END IF;
END $environment_status$;
