//! Control portable-store schema and PostgreSQL 18 proofs (wamn-0h0g.9.9).

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::json;

fn ddl() -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../deploy/sql/control-portable-store.sql"),
    )
    .expect("read control portable-store DDL")
}

#[test]
fn portable_store_record_is_exact_and_storage_only() {
    let sql = ddl();
    assert_eq!(wamn_control_provision::CONTROL_PORTABLE_STORE_SQL, sql);
    assert_eq!(
        wamn_control_provision::CONTROL_BOOTSTRAP_SQL,
        [
            wamn_control_provision::SYSTEM_SCHEMA_SQL,
            wamn_control_provision::CONTROL_PORTABLE_STORE_SQL
        ]
    );
    for relation in [
        "catalog.catalogs",
        "catalog.flow_artifacts",
        "catalog.execution_bundles",
        "catalog.release_manifests",
        "catalog.release_flows",
        "catalog.catalog_heads",
        "catalog.flow_drafts",
        "catalog.validated_flow_drafts",
        "catalog.release_exposure_manifests",
        "catalog.release_sources",
        "catalog.release_attachments",
        "catalog.connection_requirements",
        "catalog.draft_safe_connection_grants",
        "catalog.authoring_command_audit",
        "wamn_run.authoring_test_sets",
        "wamn_run.authoring_test_run_reservations",
        "wamn_run.authoring_test_case_runs",
        "wamn_run.authoring_test_reports",
        "catalog.release_flow_test_evidence",
        "catalog.deployment_attestations",
    ] {
        assert!(
            sql.contains(&format!("CREATE TABLE IF NOT EXISTS {relation}")),
            "missing control portable relation {relation}"
        );
    }

    for excluded in [
        "catalog.attachment_activation",
        "catalog.attachment_tombstones",
        "catalog.connection_instances",
        "catalog.connection_generations",
        "catalog.connection_bindings",
        "catalog.connection_generation_retention",
        "catalog.schema_migrations",
        "catalog.entities",
        "catalog.event_registrations",
        "wamn_run.runs",
    ] {
        assert!(
            !sql.contains(&format!("CREATE TABLE IF NOT EXISTS {excluded}")),
            "project/runtime relation leaked into the control record: {excluded}"
        );
    }

    assert!(sql.contains("tested_resolution_map_bytes bytea NOT NULL"));
    assert!(sql.contains("sha256(tested_resolution_map_bytes)"));
    assert!(sql.contains("tested_resolution_map_bytes = p_tested_resolution_map_bytes"));
    assert!(
        sql.contains("REFERENCES catalog.validated_flow_drafts (tenant_id, validated_draft_hash)")
    );
    assert!(sql.contains("REFERENCES wamn_run.authoring_test_reports (tenant_id, report_id)"));
    assert!(
        sql.contains("REFERENCES catalog.execution_bundles (tenant_id, execution_bundle_hash)")
    );
    assert!(!sql.contains("REFERENCES registry."));
    assert!(!sql.contains("REFERENCES catalog.connection_generations"));
    assert!(!sql.contains("GRANT SELECT"));
    assert!(!sql.contains("GRANT INSERT"));
    assert!(!sql.contains("GRANT UPDATE"));
    assert!(!sql.contains("GRANT DELETE"));
    assert!(!sql.contains("guard_authoring_test_orchestration_write"));
    assert!(sql.contains(
        r#"con.contype::text || ':' || pg_get_constraintdef(con.oid, false),
        E'\n' ORDER BY (con.contype::text || ':'
        || pg_get_constraintdef(con.oid, false)) COLLATE "C""#
    ));
    assert!(sql.contains("7e6f31e287802d22eea4a7320a072471a793b94fe3882e4e8bbc30fd981bd7ed"));
    assert!(!sql.contains("ab4c8a54366eab426d72c31c81531e929a4b615d051f300be7c993c628699f78"));
    assert!(!sql.contains("06bf7790877f52c2094511dc368d605f7de4b112383fc6b857d8886844160c85"));
}

#[test]
fn tested_resolution_fixture_uses_the_landed_rfc8785_canonicalizer() {
    let value = json!({
        "flow-z": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "flow-a": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    });
    let bytes = wamn_flow::canonical_json_bytes(&value);
    assert_eq!(
        bytes,
        br#"{"flow-a":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","flow-z":"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}"#
    );
    assert_eq!(
        wamn_flow::canonical_json_sha256(&value),
        "sha256:1ce65570349393c45e6c5ab58405b960e24b6d0d8ece076a4e4b0947b52383a2"
    );
}

fn hex_from_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn control_portable_store_applies_twice_and_enforces_contract_on_postgres() {
    let Ok(url) = std::env::var("WAMN_CONTROL_PORTABLE_PG_URL") else {
        eprintln!(
            "skipping control_portable_store_applies_twice_and_enforces_contract_on_postgres \
             (set WAMN_CONTROL_PORTABLE_PG_URL)"
        );
        return;
    };

    let resolution = json!({
        "flow-z": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "flow-a": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    });
    let map_bytes = wamn_flow::canonical_json_bytes(&resolution);
    let map_hash = wamn_flow::canonical_json_sha256(&resolution);
    let map_hex = hex_from_bytes(&map_bytes);
    let bundle_hex = hex_from_bytes(b"plan");

    let mut script = String::from(
        "CREATE EXTENSION IF NOT EXISTS pgcrypto;\n\
         DROP SCHEMA IF EXISTS catalog CASCADE;\n\
         DROP SCHEMA IF EXISTS wamn_run CASCADE;\n\
         DO $$ BEGIN\n\
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_system') THEN\n\
             CREATE ROLE wamn_system NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               NOINHERIT NOREPLICATION NOBYPASSRLS;\n\
           END IF;\n\
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_portable_probe') THEN\n\
             CREATE ROLE wamn_portable_probe NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               NOINHERIT NOREPLICATION NOBYPASSRLS;\n\
           END IF;\n\
         END $$;\n\
         DO $$ BEGIN EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_system', current_database()); END $$;\n\
         SET ROLE wamn_system;\n",
    );
    script.push_str(&ddl());
    script.push('\n');
    // The additive artifact itself is the bootstrap reconciliation path.
    script.push_str(&ddl());
    script.push_str(&format!(
        r#"
DO $$ BEGIN
  ASSERT (SELECT count(*) FROM pg_constraint
          WHERE conrelid='catalog.release_flow_test_evidence'::regclass
            AND contype <> 'n') = 16,
         'release evidence must retain exactly 16 governed constraints';
  ASSERT (SELECT count(*) FROM pg_constraint
          WHERE conrelid='catalog.release_flow_test_evidence'::regclass
            AND contype = 'c') = 11,
         'release evidence must retain exactly eleven CHECK constraints';
  ASSERT (SELECT count(*) FROM pg_constraint
          WHERE conrelid='catalog.release_flow_test_evidence'::regclass
            AND contype = 'f') = 4,
         'release evidence must retain exactly four local foreign keys';
  ASSERT (SELECT count(*) FROM pg_constraint
          WHERE conrelid='catalog.release_flow_test_evidence'::regclass
            AND contype = 'p') = 1,
         'release evidence must retain exactly one primary key';
  ASSERT (SELECT encode(sha256(convert_to(string_agg(
            contype::text || ':' || pg_get_constraintdef(oid, false),
            E'\n' ORDER BY (contype::text || ':'
              || pg_get_constraintdef(oid, false)) COLLATE "C"
          ), 'UTF8')), 'hex')
          FROM pg_constraint
          WHERE conrelid='catalog.release_flow_test_evidence'::regclass
            AND contype <> 'n') =
         '7e6f31e287802d22eea4a7320a072471a793b94fe3882e4e8bbc30fd981bd7ed',
         'release evidence constraint fingerprint must be version-stable';
END $$;

SET app.tenant = 'tenant-a';
INSERT INTO catalog.catalogs
  (tenant_id,catalog_id,version,environment,schema_version,state)
VALUES ('tenant-a','cat',1,'dev','0.1','applied');
INSERT INTO catalog.flow_artifacts
  (tenant_id,flow_id,flow_version,schema_version,graph_json,graph_hash,artifact_hash)
VALUES ('tenant-a','flow-a',1,'0.1','{{}}','graph','artifact-a');
INSERT INTO catalog.execution_bundles
  (tenant_id,execution_bundle_hash,format_version,exact_bytes,byte_length)
VALUES ('tenant-a','sha256:'||encode(sha256(decode('{bundle_hex}','hex')),'hex'),
        '0.1',decode('{bundle_hex}','hex'),4);
INSERT INTO catalog.release_manifests
  (tenant_id,catalog_id,catalog_version,members_json)
VALUES ('tenant-a','cat',1,'[]');
INSERT INTO catalog.release_flows
  (tenant_id,catalog_id,catalog_version,flow_id,flow_version,execution_bundle_hash)
VALUES ('tenant-a','cat',1,'flow-a',1,
        'sha256:'||encode(sha256(decode('{bundle_hex}','hex')),'hex'));
INSERT INTO catalog.flow_drafts
  (tenant_id,draft_id,flow_id,revision,definition)
VALUES ('tenant-a','draft-a','flow-a',1,'{{}}');
INSERT INTO catalog.validated_flow_drafts
  (tenant_id,draft_id,draft_revision,draft_edited_at,draft_content_hash,
   catalog_id,catalog_version,environment,flow_id,runtime_flow_version,
   graph_json,graph_hash,draft_artifact_hash,execution_bundle_hash,
   binding_base_artifact_hash,validated_draft_hash)
VALUES ('tenant-a','draft-a',1,now(),'draft-content','cat',1,'dev','flow-a',1,
        '{{}}','graph','artifact-a',
        'sha256:'||encode(sha256(decode('{bundle_hex}','hex')),'hex'),
        'artifact-a','validated-a');
INSERT INTO wamn_run.authoring_test_sets
  (tenant_id,test_set_hash,schema_version,exact_bytes,byte_length)
SELECT 'tenant-a','sha256:'||encode(sha256(convert_to('{{"schema-version":"0.1"}}','UTF8')),'hex'),
       '0.1',convert_to('{{"schema-version":"0.1"}}','UTF8'),
       octet_length(convert_to('{{"schema-version":"0.1"}}','UTF8'));
INSERT INTO wamn_run.authoring_test_run_reservations
  (tenant_id,report_id,command_hash,validated_draft_id,test_set_hash,
   catalog_id,catalog_version,case_count,whole_deadline_at)
SELECT 'tenant-a','report-a','sha256:'||repeat('1',64),'validated-a',test_set_hash,
       'cat',1,1,clock_timestamp()+interval '1 hour'
FROM wamn_run.authoring_test_sets WHERE tenant_id='tenant-a';
INSERT INTO wamn_run.authoring_test_reports
  (tenant_id,report_id,validated_draft_id,test_set_hash,catalog_id,
   catalog_version,resolution_map,resolution_map_hash,passed,summary)
SELECT 'tenant-a','report-a','validated-a',test_set_hash,'cat',1,
       '{{}}'::jsonb,'sha256:'||encode(sha256(convert_to('{{}}'::jsonb::text,'UTF8')),'hex'),
       true,'{{}}'::jsonb
FROM wamn_run.authoring_test_sets WHERE tenant_id='tenant-a';

DO $$ DECLARE
  first_created_at timestamptz;
  retry_created_at timestamptz;
BEGIN
  first_created_at := catalog.register_release_flow_test_evidence(
    'tenant-a','cat',1,'flow-a','validated-a','report-a',
    (SELECT test_set_hash FROM wamn_run.authoring_test_sets WHERE tenant_id='tenant-a'),
    'artifact-a','sha256:'||encode(sha256(decode('{bundle_hex}','hex')),'hex'),
    decode('{map_hex}','hex'),'{map_hash}');
  retry_created_at := catalog.register_release_flow_test_evidence(
    'tenant-a','cat',1,'flow-a','validated-a','report-a',
    (SELECT test_set_hash FROM wamn_run.authoring_test_sets WHERE tenant_id='tenant-a'),
    'artifact-a','sha256:'||encode(sha256(decode('{bundle_hex}','hex')),'hex'),
    decode('{map_hex}','hex'),'{map_hash}');
  ASSERT first_created_at = retry_created_at,
         'exact evidence retry must return the original server timestamp';
  ASSERT (SELECT tested_resolution_map_bytes
          FROM catalog.release_flow_test_evidence
          WHERE tenant_id='tenant-a' AND flow_id='flow-a') = decode('{map_hex}','hex'),
         'RFC8785 bytes must round-trip byte-exactly';
END $$;

DO $$ BEGIN BEGIN
  PERFORM catalog.register_release_flow_test_evidence(
    'tenant-a','cat',1,'flow-a','validated-a','report-a',
    (SELECT test_set_hash FROM wamn_run.authoring_test_sets WHERE tenant_id='tenant-a'),
    'artifact-a','sha256:'||encode(sha256(decode('{bundle_hex}','hex')),'hex'),
    convert_to('{{"flow-a":"changed"}}','UTF8'),
    'sha256:'||encode(sha256(convert_to('{{"flow-a":"changed"}}','UTF8')),'hex'));
  ASSERT false, 'same evidence coordinate with different bytes must conflict';
EXCEPTION WHEN unique_violation THEN
  ASSERT SQLERRM = 'release-flow-test-evidence-content-conflict';
END; END $$;

DO $$ BEGIN BEGIN
  INSERT INTO catalog.release_flow_test_evidence
    (tenant_id,catalog_id,catalog_version,flow_id,validated_draft_id,report_id,
     test_set_hash,source_artifact_hash,execution_bundle_hash,
     tested_resolution_map_bytes,tested_resolution_map_hash)
  SELECT 'tenant-a','cat',1,'missing','validated-a','report-a',test_set_hash,
         'artifact-a','sha256:'||encode(sha256(decode('{bundle_hex}','hex')),'hex'),
         decode('{map_hex}','hex'),'sha256:'||repeat('0',64)
  FROM wamn_run.authoring_test_sets WHERE tenant_id='tenant-a';
  ASSERT false, 'bad tested map hash must fail';
EXCEPTION WHEN check_violation THEN NULL; END; END $$;

DO $$ BEGIN BEGIN
  INSERT INTO catalog.release_flow_test_evidence
    (tenant_id,catalog_id,catalog_version,flow_id,validated_draft_id,report_id,
     test_set_hash,source_artifact_hash,execution_bundle_hash,
     tested_resolution_map_bytes,tested_resolution_map_hash)
  SELECT 'tenant-a','cat',1,'missing-flow','validated-a','report-a',test_set_hash,
         'artifact-a','sha256:'||encode(sha256(decode('{bundle_hex}','hex')),'hex'),
         decode('{map_hex}','hex'),'{map_hash}'
  FROM wamn_run.authoring_test_sets WHERE tenant_id='tenant-a';
  ASSERT false, 'evidence coordinates must retain local foreign keys';
EXCEPTION WHEN foreign_key_violation THEN NULL; END; END $$;

DO $$ BEGIN BEGIN
  UPDATE catalog.release_flow_test_evidence SET source_artifact_hash='changed'
  WHERE tenant_id='tenant-a' AND flow_id='flow-a';
  ASSERT false, 'evidence must be immutable';
EXCEPTION WHEN SQLSTATE '55000' THEN NULL; END; END $$;

DO $$ DECLARE
  first_attested_at timestamptz;
BEGIN
  first_attested_at := catalog.register_deployment_attestation(
    'tenant-a','cat',1,'org-a','project-a','dev','sha256:'||repeat('2',64),
    '2026-08-15T12:00:00Z'::timestamptz);
  ASSERT first_attested_at = catalog.register_deployment_attestation(
      'tenant-a','cat',1,'org-a','project-a','dev','sha256:'||repeat('2',64),
      '2026-08-15T12:00:00Z'::timestamptz),
    'exact attestation retry must return the original row';
END $$;
DO $$ BEGIN BEGIN
  PERFORM catalog.register_deployment_attestation(
    'tenant-a','cat',1,'org-a','project-a','dev','sha256:'||repeat('4',64),
    '2026-08-15T12:00:00Z'::timestamptz);
  ASSERT false, 'same attestation coordinate with different content must conflict';
EXCEPTION WHEN unique_violation THEN
  ASSERT SQLERRM = 'deployment-attestation-content-conflict';
END; END $$;

DO $$ BEGIN
  ASSERT NOT has_schema_privilege('wamn_portable_probe','catalog','USAGE');
  ASSERT NOT has_table_privilege(
    'wamn_portable_probe','catalog.release_flow_test_evidence','SELECT');
  ASSERT NOT has_table_privilege(
    'wamn_portable_probe','catalog.release_flow_test_evidence','INSERT');
  ASSERT NOT has_table_privilege(
    'wamn_portable_probe','catalog.release_flow_test_evidence','UPDATE');
  ASSERT NOT has_table_privilege(
    'wamn_portable_probe','catalog.release_flow_test_evidence','DELETE');
  ASSERT (SELECT relrowsecurity AND relforcerowsecurity FROM pg_class
          WHERE oid='catalog.release_flow_test_evidence'::regclass);
  ASSERT NOT EXISTS (
    SELECT 1 FROM pg_constraint
    WHERE contype='f'
      AND conrelid IN ('catalog.release_flow_test_evidence'::regclass,
                       'catalog.deployment_attestations'::regclass,
                       'catalog.draft_safe_connection_grants'::regclass)
      AND confrelid::regclass::text LIKE 'registry.%'
  ), 'cross-plane coordinates must never become foreign keys';
END $$;
RESET ROLE;
"#
    ));

    let mut child = Command::new("psql")
        .arg(&url)
        .args(["-v", "ON_ERROR_STOP=1", "-q", "-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn psql");
    child
        .stdin
        .take()
        .expect("psql stdin")
        .write_all(script.as_bytes())
        .expect("write psql script");
    let output = child.wait_with_output().expect("wait for psql");
    assert!(
        output.status.success(),
        "portable-store PostgreSQL proof failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
