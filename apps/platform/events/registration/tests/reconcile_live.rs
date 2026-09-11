//! Live PostgreSQL proof for package-coordinate registration reconciliation.
//!
//! `WAMN_EVENT_REGISTRATION_PG_URL` must name a fresh disposable database: the
//! test drops and recreates the `catalog` schema.

use std::io::Write as _;
use std::process::{Command, Stdio};

use wamn_event_reg::{DELETE_STALE_CATALOG_REGISTRATIONS_SQL, UPSERT_CATALOG_REGISTRATION_SQL};

#[test]
fn replay_is_a_no_op_with_base_and_overlay_registrations_applied() {
    let Ok(url) = std::env::var("WAMN_EVENT_REGISTRATION_PG_URL") else {
        eprintln!("WAMN_EVENT_REGISTRATION_PG_URL unset — skipping registration reconcile gate");
        return;
    };
    let mut child = Command::new("psql")
        .arg(url)
        .args(["-v", "ON_ERROR_STOP=1", "-q", "-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn psql");
    let script = format!(
        "DROP SCHEMA IF EXISTS catalog CASCADE; \
         CREATE SCHEMA catalog; \
         CREATE TABLE catalog.event_registrations ( \
           tenant_id text NOT NULL, package_id text NOT NULL, \
           registration_id text NOT NULL, \
           entity_id text NOT NULL, registration jsonb NOT NULL, \
           PRIMARY KEY (tenant_id, package_id, registration_id)); \
         CREATE TABLE catalog.registration_mutations (kind text NOT NULL); \
         CREATE FUNCTION catalog.note_registration_mutation() RETURNS trigger \
         LANGUAGE plpgsql AS $$ BEGIN \
           INSERT INTO catalog.registration_mutations VALUES (TG_OP); \
           RETURN COALESCE(NEW, OLD); \
         END $$; \
         CREATE TRIGGER registration_mutation AFTER INSERT OR UPDATE OR DELETE \
           ON catalog.event_registrations FOR EACH ROW \
           EXECUTE FUNCTION catalog.note_registration_mutation(); \
         PREPARE reconcile_upsert(text, text, text, text, text) AS {upsert}; \
         PREPARE reconcile_delete(text, text, text[]) AS {delete_stale}; \
         EXECUTE reconcile_upsert( \
           'tenant', 'wamn_receiving', 'base-audit', 'receipt', \
           '{{\"schema-version\":\"0.1\",\"registration-id\":\"base-audit\",\"package-id\":\"wamn_receiving\",\"source-package-id\":\"wamn_receiving\",\"entity\":\"receipt\",\"ops\":[\"insert\"],\"input\":\"event\"}}'); \
         EXECUTE reconcile_delete('tenant', 'wamn_receiving', ARRAY['base-audit']); \
         EXECUTE reconcile_upsert( \
           'tenant', 'client_acme_receiving', 'quality.create_inspection', 'receipt', \
           '{{\"schema-version\":\"0.1\",\"registration-id\":\"quality.create_inspection\",\"package-id\":\"client_acme_receiving\",\"source-package-id\":\"wamn_receiving\",\"entity\":\"receipt\",\"ops\":[\"insert\"],\"input\":\"event\"}}'); \
         EXECUTE reconcile_delete( \
           'tenant', 'client_acme_receiving', ARRAY['quality.create_inspection']); \
         DO $$ BEGIN \
           ASSERT (SELECT count(*) FROM catalog.event_registrations) = 2; \
           ASSERT (SELECT count(*) FROM catalog.registration_mutations) = 2; \
         END $$; \
         EXECUTE reconcile_upsert( \
           'tenant', 'wamn_receiving', 'base-audit', 'receipt', \
           '{{\"schema-version\":\"0.1\",\"registration-id\":\"base-audit\",\"package-id\":\"wamn_receiving\",\"source-package-id\":\"wamn_receiving\",\"entity\":\"receipt\",\"ops\":[\"insert\"],\"input\":\"event\"}}'); \
         EXECUTE reconcile_delete('tenant', 'wamn_receiving', ARRAY['base-audit']); \
         EXECUTE reconcile_upsert( \
           'tenant', 'client_acme_receiving', 'quality.create_inspection', 'receipt', \
           '{{\"schema-version\":\"0.1\",\"registration-id\":\"quality.create_inspection\",\"package-id\":\"client_acme_receiving\",\"source-package-id\":\"wamn_receiving\",\"entity\":\"receipt\",\"ops\":[\"insert\"],\"input\":\"event\"}}'); \
         EXECUTE reconcile_delete( \
           'tenant', 'client_acme_receiving', ARRAY['quality.create_inspection']); \
         DO $$ BEGIN \
           ASSERT (SELECT count(*) FROM catalog.event_registrations) = 2; \
           ASSERT (SELECT count(*) FROM catalog.registration_mutations) = 2, \
             'exact replay performed a write'; \
           ASSERT (SELECT count(*) FROM catalog.event_registrations \
                    WHERE package_id = 'client_acme_receiving' \
                      AND registration ->> 'source-package-id' = 'wamn_receiving') = 1; \
         END $$;",
        upsert = UPSERT_CATALOG_REGISTRATION_SQL,
        delete_stale = DELETE_STALE_CATALOG_REGISTRATIONS_SQL,
    );
    child
        .stdin
        .take()
        .expect("psql stdin")
        .write_all(script.as_bytes())
        .expect("write reconcile proof");
    let output = child.wait_with_output().expect("wait for psql");
    assert!(
        output.status.success(),
        "registration reconcile proof failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
