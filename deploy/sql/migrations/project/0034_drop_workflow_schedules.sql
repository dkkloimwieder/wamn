-- Drop catalog.workflow_schedules (docs/plan/workflows.md R9, epic 2) from an
-- installed project database. A package declares no schedule: a schedule is
-- a row of catalog.workflows, which the host's schedule task reads at each
-- tick. upgrade-schema runs this file once in one transaction and records it.
-- A fresh install records it as applied, because catalog-schema.sql no
-- longer creates the table.

DROP TABLE catalog.workflow_schedules;
