-- The environment row owns the policy name its document declares
-- (wamn-snz0.1, owner ruling of 2026-10-09, docs/plan/platform-deploy.md R2,
-- R21). An existing row took its policy from its env name through the
-- (org, env) foreign key, so the migration records that name. The control
-- surface does not change.
ALTER TABLE registry.project_envs
    ADD COLUMN policy_name text,
    ADD CONSTRAINT project_envs_policy_name_fkey
        FOREIGN KEY (org, policy_name) REFERENCES registry.env_policies (org, name)
        DEFERRABLE INITIALLY IMMEDIATE;

UPDATE registry.project_envs SET policy_name = env WHERE policy_name IS NULL;
