-- The management surface reads the component facts of a package from its
-- verified package artifact, the way publish-release reads them
-- (wamn-vavs4.6, docs/plan/platform-deploy.md R2). It reads the digest and
-- verified_at that catalog.package_artifacts records on its control-author
-- connection, so the control author may SELECT that table and nothing more.
DO $control_author$ BEGIN
  IF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'wamn_control_author') THEN
    GRANT SELECT ON TABLE catalog.package_artifacts TO wamn_control_author;
  END IF;
END $control_author$;
