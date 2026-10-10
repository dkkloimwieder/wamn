-- verified_at is the time push-package verified the artifact
-- (wamn-vavs4.9, docs/plan/platform-deploy.md R11 (2)). push-package writes it
-- after every check and push succeeds; the column has no default, so a row
-- that no verification wrote holds NULL, and every reader refuses that row.
ALTER TABLE catalog.package_artifacts
    ALTER COLUMN verified_at DROP NOT NULL,
    ALTER COLUMN verified_at DROP DEFAULT;
