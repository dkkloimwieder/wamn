-- The identity issuer reads that offer the control audience of an org
-- (docs/plan/platform-ui.md §4.3, wamn-a40n.2): the project roles of a
-- person, and the registered orgs.
-- upgrade-schema runs this file once in one transaction and records it. A
-- fresh install records it as applied, because no issuer role exists at
-- install and the first identity-issuer prepare grants these reads with the
-- rest of its surface.
-- An installed database whose issuer was never prepared has no issuer role,
-- and its first prepare grants the reads, so the grant runs only when the
-- role exists.
-- The control host login reads through its own family, `wamn_control`.
-- provision-org converges that grant set when it prepares the credential.

DO $control_audience_reads$ BEGIN
  IF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'wamn_identity_issuer') THEN
    GRANT SELECT (principal_id, org, project, role) ON TABLE identity.project_roles
      TO wamn_identity_issuer;
    GRANT SELECT (id) ON TABLE registry.orgs TO wamn_identity_issuer;
  END IF;
END $control_audience_reads$;
