-- The status of each project environment (wamn-zua8.2, docs/plan/platform-ui.md
-- §5.4). Identity offers no audience of an inactive environment, and its host
-- serves none of its routes. Every existing environment is active.
ALTER TABLE registry.project_envs
    ADD COLUMN status text NOT NULL DEFAULT 'active',
    ADD CONSTRAINT project_envs_status_check CHECK (status IN ('active', 'inactive'));
