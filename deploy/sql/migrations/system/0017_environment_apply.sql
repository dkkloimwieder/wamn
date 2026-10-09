-- The facts that `wamn-ctl env apply` reads from the system database
-- (wamn-snz0.1, docs/plan/platform-deploy.md §10.1, R2). The environment
-- policy gains the readiness budget, the drain bound and whether an apply
-- needs approval. The environment row gains its route host, which the
-- environment document declares. The control surface does not change.
ALTER TABLE registry.env_policies
    ADD COLUMN readiness_budget_seconds int NOT NULL DEFAULT 600
        CONSTRAINT env_policies_readiness_budget_check CHECK (readiness_budget_seconds > 0),
    ADD COLUMN drain_bound_seconds int NOT NULL DEFAULT 300
        CONSTRAINT env_policies_drain_bound_check CHECK (drain_bound_seconds > 0),
    ADD COLUMN approval_required boolean NOT NULL DEFAULT false;

ALTER TABLE registry.project_envs
    ADD COLUMN route_host text
        CONSTRAINT project_envs_route_host_check
        CHECK (route_host IS NULL OR route_host ~ '^[a-z0-9]([a-z0-9.-]*[a-z0-9])?$');
