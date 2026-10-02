-- The create-environment saga (wamn-zua8.3, docs/plan/platform-ui.md section 6
-- item 10, owner rulings of 2026-10-02). The environment.create route writes
-- one saga with its request in `input` and its org in `org`, and all of its
-- steps at once, so environment.list shows the whole chain from the start.
-- wamn-ctl serve runs the sagas of one org one at a time and alone updates a
-- saga and its steps. A saga can end abandoned, or wait for an operator at
-- its last step. The control family gains INSERT on both tables and reads
-- them. This file carries the whole rendered control surface, as 0008 did.
ALTER TABLE provisioning.sagas
    ADD COLUMN org text,
    ADD COLUMN input jsonb,
    DROP CONSTRAINT sagas_type_check,
    ADD CONSTRAINT sagas_type_check
        CHECK (type IN ('provision-org', 'provision-project-env', 'create-environment')),
    DROP CONSTRAINT sagas_status_check,
    ADD CONSTRAINT sagas_status_check
        CHECK (status IN ('pending', 'running', 'completed', 'failed',
                          'compensating', 'compensated', 'abandoned',
                          'awaiting-operator')),
    ADD CONSTRAINT sagas_create_environment_input
        CHECK (type <> 'create-environment'
               OR (org IS NOT NULL AND jsonb_typeof(input) = 'object'));

CREATE TABLE provisioning.saga_steps (
    saga_id     text NOT NULL REFERENCES provisioning.sagas (saga_id),
    step        int  NOT NULL CHECK (step > 0),
    name        text NOT NULL CHECK (name <> ''),
    status      text NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'running', 'completed', 'failed')),
    error       text,
    detail      jsonb,
    started_at  timestamptz,
    finished_at timestamptz,
    PRIMARY KEY (saga_id, step)
);

DO $control_surface$ BEGIN
  IF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'wamn_control') THEN
    REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA "identity", "provisioning", "registry" FROM "wamn_control"; REVOKE ALL PRIVILEGES ON SCHEMA "identity", "provisioning", "registry" FROM "wamn_control"; GRANT USAGE ON SCHEMA "identity", "provisioning", "registry" TO "wamn_control"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "identity"."org_memberships" TO "wamn_control"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "identity"."org_roles" TO "wamn_control"; GRANT SELECT ON TABLE "identity"."password_logins" TO "wamn_control"; GRANT SELECT ON TABLE "identity"."principals" TO "wamn_control"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "identity"."project_env_memberships" TO "wamn_control"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "identity"."project_roles" TO "wamn_control"; GRANT SELECT, INSERT ON TABLE "provisioning"."saga_steps" TO "wamn_control"; GRANT SELECT, INSERT ON TABLE "provisioning"."sagas" TO "wamn_control"; GRANT SELECT ON TABLE "registry"."project_envs" TO "wamn_control"; GRANT SELECT ON TABLE "registry"."projects" TO "wamn_control"; GRANT UPDATE ("status") ON TABLE "registry"."project_envs" TO "wamn_control";
  END IF;
END $control_surface$;
