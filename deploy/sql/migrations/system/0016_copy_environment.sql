-- The copy-environment saga (wamn-zua8.4, docs/plan/platform-ui.md section
-- 5.3, owner rulings of 2026-10-04). The environment.copy route writes one
-- saga of the new type with its request in `input` and its org in `org`, as
-- environment.create does. The control surface does not change.
ALTER TABLE provisioning.sagas
    DROP CONSTRAINT sagas_type_check,
    ADD CONSTRAINT sagas_type_check
        CHECK (type IN ('provision-org', 'provision-project-env', 'create-environment',
                        'copy-environment')),
    DROP CONSTRAINT sagas_create_environment_input,
    ADD CONSTRAINT sagas_create_environment_input
        CHECK (type NOT IN ('create-environment', 'copy-environment')
               OR (org IS NOT NULL AND jsonb_typeof(input) = 'object'));
