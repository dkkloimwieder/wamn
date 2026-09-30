-- The service role `operator` becomes `admin` in an installed system database
-- (docs/plan/platform-ui.md §2.2, wamn-a40n.1). `operator` and `admin` held the
-- same application grants, so a service that held `operator` keeps its
-- authority. Route PAT admission accepts only `admin` after this change.
-- upgrade-schema runs this file once in one transaction and records it. A
-- fresh install records it as applied, because no install writes `operator`.

SELECT pg_catalog.set_config('app.user_id', '770df186-ac15-579e-b46b-c297cae2011b', true),
       pg_catalog.set_config('app.operation', 'wamn:provisioning', true);

INSERT INTO identity.project_roles (principal_id, org, project, role)
SELECT principal_id, org, project, 'admin'
  FROM identity.project_roles
 WHERE role = 'operator'
ON CONFLICT (principal_id, org, project, role) DO NOTHING;

DELETE FROM identity.project_roles WHERE role = 'operator';
