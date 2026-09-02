//! Live-apply gate for the CDC capture builders (wamn-l5i9.9, D19 v3 §4).
//!
//! Set `WAMN_CDC_PG_URL` to a **superuser** URL of a throwaway Postgres running
//! with `wal_level=logical` (e.g. `docker run … postgres:18 -c
//! wal_level=logical`); skipped cleanly when unset. Applies the REAL builders
//! via psql and asserts the live substrate: the publication covers the schema
//! and auto-includes a table created later (`FOR TABLES IN SCHEMA`), the slot
//! is pgoutput + non-temporary + failover-enabled (the exact shape
//! pg_walstream's `ensure_replication_slot` tolerates), the role is
//! `REPLICATION` and otherwise least-privilege, the grants hold, and a
//! re-apply is a no-op (idempotent). Teardown drops the slot (releasing pinned
//! WAL), the database, and the role.

use std::io::Write as _;
use std::process::{Command, Stdio};

use wamn_control_provision::{cdc_object_name, project_env_database_name, sql};

const INSTANCE: &str = "k3m9x2p7";

/// Swap the database path segment of a libpq URL (the test controls the URL —
/// no query string).
fn swap_db(url: &str, db: &str) -> String {
    let (base, _) = url.rsplit_once('/').expect("url has a path");
    format!("{base}/{db}")
}

fn psql(url: &str, script: &str) -> std::process::Output {
    let mut child = Command::new("psql")
        .arg(url)
        .args(["-v", "ON_ERROR_STOP=1", "-q", "-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn psql (is it installed?)");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn run_ok(url: &str, script: &str) {
    let out = psql(url, script);
    assert!(
        out.status.success(),
        "psql failed:\n--- stderr ---\n{}\n--- script ---\n{script}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn cdc_substrate_applies_and_is_idempotent_on_postgres() {
    let Ok(url) = std::env::var("WAMN_CDC_PG_URL") else {
        eprintln!(
            "skipping cdc_substrate_applies_and_is_idempotent_on_postgres (set WAMN_CDC_PG_URL to run)"
        );
        return;
    };

    let (org, project, env) = ("acme", "billing", "dev");
    let db = project_env_database_name(org, project, env, INSTANCE);
    let cdc = cdc_object_name(org, project, env, INSTANCE);
    let schema = "app";

    // Maintenance DB: fresh database + the cluster-global replication role.
    // The role is dropped first so every run exercises the CURRENT builder —
    // a leftover healthy role would satisfy the IF NOT EXISTS guard and mask a
    // mutated builder (the M2 gate-blind-spot lesson).
    run_ok(
        &url,
        &format!(
            "{drop_db};\nDROP ROLE IF EXISTS \"{cdc}\";\n{create_db};\n{role}\n",
            drop_db = sql::drop_database_named_sql(&db),
            create_db = sql::create_database_named_sql(&db),
            role = sql::ensure_replication_role_sql(&cdc, "wamn_cdc"),
        ),
    );

    // Project-env DB: the CDC bundle (schema guard → publication → failover
    // slot → classification maps → grants — the exact order `cdc_sql_bundle`
    // emits; both maps precede the grants because the grants name them), then a
    // table created AFTER the publication, then the live assertions.
    let db_url = swap_db(&url, &db);
    let cdc_sql = format!(
        "{schema_guard};\n{publication}\n{slot}\n{entity_map};\n{exclusion_map};\n{grants}\n",
        schema_guard = sql::ensure_schema_sql(schema),
        publication = sql::create_publication_sql(&cdc, schema),
        slot = sql::create_failover_slot_sql(&cdc),
        entity_map = sql::ensure_entity_map_sql(schema),
        exclusion_map = sql::ensure_cdc_exclusion_map_sql(schema),
        grants = sql::grant_replication_access_sql(&db, &cdc, schema),
    );
    run_ok(&db_url, &cdc_sql);
    // Idempotency: a second apply of the same bundle is a clean no-op.
    run_ok(&db_url, &cdc_sql);

    run_ok(
        &db_url,
        &format!(
            r#"
CREATE TABLE {schema}.receipts (id uuid PRIMARY KEY, qty numeric(8,3));
DO $$ BEGIN
  ASSERT (SELECT count(*) FROM pg_publication WHERE pubname = '{cdc}') = 1,
    'the publication exists exactly once (idempotent re-apply)';
  ASSERT (SELECT puballtables FROM pg_publication WHERE pubname = '{cdc}') = false,
    'the publication is schema-scoped, never FOR ALL TABLES';
  ASSERT (SELECT count(*) FROM pg_publication_tables
            WHERE pubname = '{cdc}' AND schemaname = '{schema}' AND tablename = 'receipts') = 1,
    'FOR TABLES IN SCHEMA auto-includes a table created AFTER the publication';
  ASSERT (SELECT count(*) FROM pg_replication_slots WHERE slot_name = '{cdc}') = 1,
    'the failover slot exists exactly once (idempotent re-apply)';
  ASSERT (SELECT plugin FROM pg_replication_slots WHERE slot_name = '{cdc}') = 'pgoutput'
     AND (SELECT temporary FROM pg_replication_slots WHERE slot_name = '{cdc}') = false
     AND (SELECT two_phase FROM pg_replication_slots WHERE slot_name = '{cdc}') = false
     AND (SELECT failover FROM pg_replication_slots WHERE slot_name = '{cdc}') = true,
    'the slot is pgoutput, durable, single-phase, FAILOVER-enabled (the pg_walstream shape)';
  ASSERT (SELECT database FROM pg_replication_slots WHERE slot_name = '{cdc}') = '{db}',
    'the logical slot is bound to the project-env database';
  ASSERT (SELECT rolreplication FROM pg_roles WHERE rolname = '{cdc}') = true,
    'the role carries REPLICATION';
  ASSERT (SELECT rolsuper FROM pg_roles WHERE rolname = '{cdc}') = false
     AND (SELECT rolcreatedb FROM pg_roles WHERE rolname = '{cdc}') = false
     AND (SELECT rolbypassrls FROM pg_roles WHERE rolname = '{cdc}') = false,
    'the role is otherwise least-privilege (R8b tier)';
  ASSERT has_database_privilege('{cdc}', '{db}', 'CONNECT'),
    'the role may CONNECT to the project-env database';
  ASSERT has_schema_privilege('{cdc}', '{schema}', 'USAGE'),
    'the role has USAGE on the app schema';
  ASSERT has_table_privilege('{cdc}', '{schema}.receipts'::regclass, 'SELECT') = false,
    'a table created AFTER the grant is not retro-granted (decoding needs no SELECT)';
  ASSERT has_table_privilege('{cdc}', '{schema}.wamn_entities'::regclass, 'SELECT'),
    'the role reads the decode-time entity map';
  ASSERT has_table_privilege('{cdc}', '{schema}.wamn_cdc_exclusions'::regclass, 'SELECT'),
    'the role reads the explicit CDC-exclusion map';
END $$;
"#,
        ),
    );

    // Teardown: slot first (releases the pinned WAL deterministically; an
    // in-use slot would block DROP DATABASE, idle ones are dropped with it),
    // then the database (removes publication + grants), then the role.
    run_ok(&db_url, &sql::drop_replication_slot_sql(&cdc));
    run_ok(
        &url,
        &format!(
            "{drop_db};\nDROP ROLE IF EXISTS \"{cdc}\";\n",
            drop_db = sql::drop_database_named_sql(&db),
        ),
    );
}

/// The CDC role's table reads are the two classification maps and nothing else, and the
/// narrowing is SAFE: a tenant table it cannot `SELECT` still decodes.
///
/// The blanket `GRANT SELECT ON ALL TABLES IN SCHEMA` the old builder emitted
/// handed the same credential a plain-SQL read of every tenant table, with none
/// of the gates gating the replication path (the `REPLICATION` attribute, the
/// walsender protocol, slot ownership). This seeds an environment provisioned
/// UNDER that old builder and then applies the current one, so the retroactive
/// `REVOKE` is what the proof turns on — a narrowed grant alone would leave the
/// already-executed blanket grant standing, and every assertion below would pass
/// on new environments while production stayed wide open.
#[test]
fn cdc_role_reads_only_the_classification_maps_and_still_decodes_tenant_tables() {
    let Ok(url) = std::env::var("WAMN_CDC_PG_URL") else {
        eprintln!(
            "skipping cdc_role_reads_only_the_entity_map_and_still_decodes_tenant_tables (set WAMN_CDC_PG_URL to run)"
        );
        return;
    };

    let (org, project, env) = ("acme", "narrowing", "dev");
    let db = project_env_database_name(org, project, env, INSTANCE);
    let cdc = cdc_object_name(org, project, env, INSTANCE);
    let schema = "app";

    run_ok(
        &url,
        &format!(
            "{drop_db};\nDROP ROLE IF EXISTS \"{cdc}\";\n{create_db};\n{role}\n",
            drop_db = sql::drop_database_named_sql(&db),
            create_db = sql::create_database_named_sql(&db),
            role = sql::ensure_replication_role_sql(&cdc, "wamn_cdc"),
        ),
    );

    let db_url = swap_db(&url, &db);

    // An environment provisioned BEFORE the narrowing: a tenant table, the
    // entity map, and the blanket read the old builder granted. Asserted, so a
    // future refactor that silently stops seeding it turns the retroactive
    // REVOKE proof below vacuous rather than green.
    run_ok(
        &db_url,
        &format!(
            "{schema_guard};\n\
             CREATE TABLE {schema_ident}.receipts (id int PRIMARY KEY, tenant_secret text);\n\
             {entity_map};\n\
             {exclusion_map};\n\
             GRANT USAGE ON SCHEMA {schema_ident} TO \"{cdc}\";\n\
             GRANT SELECT ON ALL TABLES IN SCHEMA {schema_ident} TO \"{cdc}\";\n\
             DO $$ BEGIN \
               ASSERT has_table_privilege('{cdc}', '{schema}.receipts'::regclass, 'SELECT'), \
                 'the pre-narrowing blanket grant must really be in place'; \
             END $$;\n",
            schema_guard = sql::ensure_schema_sql(schema),
            schema_ident = format!("\"{schema}\""),
            entity_map = sql::ensure_entity_map_sql(schema),
            exclusion_map = sql::ensure_cdc_exclusion_map_sql(schema),
        ),
    );

    // The publication, the slot, and the REAL current grants builder applied
    // over that old state — twice, so the retroactive REVOKE is proven
    // idempotent rather than a one-shot that strips its own grant on replay.
    let narrow = format!(
        "{publication}\n{slot}\n{grants}\n{grants}\n",
        publication = sql::create_publication_sql(&cdc, schema),
        slot = sql::create_failover_slot_sql(&cdc),
        grants = sql::grant_replication_access_sql(&db, &cdc, schema),
    );
    run_ok(&db_url, &narrow);

    // Changes to decode, on the table the role must NOT be able to read.
    run_ok(
        &db_url,
        &format!("INSERT INTO {schema}.receipts VALUES (1, 'tenant-secret');\n"),
    );

    run_ok(
        &db_url,
        &format!(
            r#"
DO $$ BEGIN
  ASSERT has_table_privilege('{cdc}', '{schema}.receipts'::regclass, 'SELECT') = false,
    'the retroactive REVOKE removed the blanket read an old environment already had';
  ASSERT has_table_privilege('{cdc}', '{schema}.wamn_entities'::regclass, 'SELECT'),
    'the entity map survives the REVOKE that precedes it';
  ASSERT has_table_privilege('{cdc}', '{schema}.wamn_cdc_exclusions'::regclass, 'SELECT'),
    'the exclusion map survives the REVOKE that precedes it';
  ASSERT has_schema_privilege('{cdc}', '{schema}', 'USAGE'),
    'schema USAGE is untouched by the narrowing';
  ASSERT has_database_privilege('{cdc}', '{db}', 'CONNECT'),
    'the builder still grants database CONNECT — only the table read narrowed';
  ASSERT (SELECT rolreplication FROM pg_roles WHERE rolname = '{cdc}'),
    'the role still carries REPLICATION (the gate the narrowing leaves in place)';
END $$;

SET ROLE "{cdc}";
DO $$ BEGIN
  BEGIN
    PERFORM count(*) FROM {schema}.receipts;
    RAISE EXCEPTION 'the CDC credential must not be able to plain-SQL read a tenant table';
  EXCEPTION WHEN insufficient_privilege THEN NULL;
  END;
  PERFORM count(*) FROM {schema}.wamn_entities;
  PERFORM count(*) FROM {schema}.wamn_cdc_exclusions;
END $$;
DO $$ DECLARE frames bigint; BEGIN
  SELECT count(*) INTO frames FROM pg_logical_slot_peek_binary_changes(
    '{cdc}', NULL, NULL, 'proto_version', '1', 'publication_names', '{cdc}');
  ASSERT frames > 0,
    'decoding a table the role cannot SELECT still yields changes — the narrowing is SAFE';
END $$;
RESET ROLE;
"#,
        ),
    );

    run_ok(&db_url, &sql::drop_replication_slot_sql(&cdc));
    run_ok(
        &url,
        &format!(
            "{drop_db};\nDROP ROLE IF EXISTS \"{cdc}\";\n",
            drop_db = sql::drop_database_named_sql(&db),
        ),
    );
}
