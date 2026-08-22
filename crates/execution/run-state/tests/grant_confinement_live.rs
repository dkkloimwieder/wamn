//! Ignored live gate: what the guest-visible `wamn_app` role may and may NOT do
//! to `wamn_run.runs` and `wamn_run.invocation_admissions`
//! after the wamn-0h0g.12.37 / .12.40 / .12.41 / .12.128 confinements.
//!
//! Every denial here is asserted TWICE and deliberately so. A statement
//! rejected by the tenant policy also raises SQLSTATE 42501, so an outcome
//! assertion alone passes for the wrong reason; each probe therefore states the
//! exact privilege with `has_table_privilege` / `has_column_privilege` as well,
//! and every statement expected to be DENIED is otherwise RLS-LEGAL for the
//! probe tenant.
//!
//! The three probes share the fixed `wamn_run` and `catalog` schema names, so
//! they serialize on `INSTALL` rather than running concurrently.

use std::io::Write as _;
use std::process::{Command, Output, Stdio};
use std::sync::Mutex;

static INSTALL: Mutex<()> = Mutex::new(());

/// Columns `wamn_app` may INSERT into `runs` — the callable admission's set.
const RUN_INSERT_COLUMNS: &[&str] = &[
    "admission_context_version",
    "attachment_id",
    "catalog_id",
    "catalog_version",
    "environment",
    "event_depth",
    "event_root_run_id",
    "event_source_run_id",
    "execution_bundle_hash",
    "flow_id",
    "flow_version",
    "idempotency_key",
    "input_json",
    "invocation_context",
    "platform_revision",
    "registration_id",
    "response_deadline_at",
    "run_deadline_at",
    "run_id",
    "status",
    "tenant_id",
    "trigger_source",
];

/// Columns `wamn_app` may UPDATE on `runs` — the claim, park, release, and
/// terminalize statements' union.
const RUN_UPDATE_COLUMNS: &[&str] = &[
    "caller_http_status",
    "caller_outcome_hash",
    "caller_outcome_json",
    "caller_outcome_kind",
    "caller_release_node_id",
    "caller_released_at",
    "fail_kind",
    "manifest_digest",
    "release_version",
    "result_json",
    "state_json",
    "status",
    "terminal_reason",
    "updated_at",
];

/// Every column `wamn_run.runs` carries.
const RUN_COLUMNS: &[&str] = &[
    "tenant_id",
    "run_id",
    "flow_id",
    "flow_version",
    "catalog_id",
    "catalog_version",
    "environment",
    "execution_bundle_hash",
    "attachment_id",
    "registration_id",
    "event_source_run_id",
    "event_root_run_id",
    "event_depth",
    "status",
    "trigger_source",
    "capture_mode",
    "durability_class",
    "release_version",
    "manifest_digest",
    "input_json",
    "result_json",
    "state_json",
    "invocation_context",
    "admission_context_version",
    "platform_revision",
    "idempotency_key",
    "caller_outcome_kind",
    "caller_outcome_json",
    "caller_http_status",
    "caller_release_node_id",
    "caller_outcome_hash",
    "caller_released_at",
    "response_deadline_at",
    "run_deadline_at",
    "terminal_reason",
    "fail_kind",
    "created_at",
    "updated_at",
];

/// Every column `wamn_run.invocation_admissions` carries.
const ADMISSION_COLUMNS: &[&str] = &[
    "tenant_id",
    "catalog_id",
    "environment",
    "attachment_id",
    "definition_hash",
    "principal_digest",
    "client_key_digest",
    "client_request_fingerprint",
    "admitted_catalog_version",
    "admitted_flow_version",
    "run_id",
    "created_at",
];

const EMPTY_HASH: &str = "sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a";

fn psql(url: &str, script: &str) -> Output {
    let mut child = Command::new("psql")
        .args(["-X", "-v", "ON_ERROR_STOP=1", "-Atq", url])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start psql");
    let mut stdin = child.stdin.take().expect("open psql stdin");
    if let Err(error) = stdin.write_all(script.as_bytes()) {
        eprintln!("psql closed stdin before the full script was written: {error}");
    }
    drop(stdin);
    child.wait_with_output().expect("run psql")
}

fn success(url: &str, script: &str) -> String {
    let output = psql(url, script);
    assert!(
        output.status.success(),
        "psql failed\nscript:\n{script}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("psql stdout is utf-8")
}

fn url() -> String {
    std::env::var("WAMN_RUN_STORE_PG_URL")
        .expect("set WAMN_RUN_STORE_PG_URL to the throwaway superuser database")
}

/// Install the DDL of record into a database with NO residue.
///
/// Roles are DROPPED and recreated rather than reused: a leftover role that
/// still carries a grant from an earlier install would satisfy a probe without
/// the DDL under test having granted anything.
fn install(url: &str) {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../..");
    let read = |name: &str| {
        std::fs::read_to_string(format!("{root}/deploy/sql/{name}"))
            .unwrap_or_else(|error| panic!("read {name}: {error}"))
    };
    let catalog = read("catalog-schema.sql");
    let run_state = read("run-state.sql");
    let run_queue = read("run-queue.sql");

    success(
        url,
        &format!(
            "DROP SCHEMA IF EXISTS wamn_run CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DO $$ DECLARE role_name text; BEGIN \
               FOREACH role_name IN ARRAY ARRAY['wamn_app','wamn_scenario_author', \
                                                'wamn_effect_writer', \
                                                'wamn_run_projection_writer'] LOOP \
                 IF EXISTS (SELECT FROM pg_roles WHERE rolname = role_name) THEN \
                   EXECUTE format('DROP OWNED BY %I', role_name); \
                   EXECUTE format('DROP ROLE %I', role_name); \
                 END IF; \
               END LOOP; \
             END $$; \
             CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' \
               NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS; \
             CREATE ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               NOINHERIT NOREPLICATION NOBYPASSRLS; \
             CREATE ROLE wamn_effect_writer NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               NOINHERIT NOREPLICATION NOBYPASSRLS; \
             CREATE ROLE wamn_run_projection_writer NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               NOINHERIT NOREPLICATION NOBYPASSRLS; \
             {catalog} {run_state} {run_queue} \
             INSERT INTO catalog.catalogs \
               (tenant_id,catalog_id,version,environment,schema_version,state) \
             VALUES ('t1','cat',1,'prod','0.1','draft'); \
             INSERT INTO catalog.execution_bundles \
               (tenant_id,execution_bundle_hash,format_version,exact_bytes,byte_length) \
             VALUES ('t1','{EMPTY_HASH}','0.1',decode('7b7d','hex'),2); \
             INSERT INTO catalog.release_manifests (tenant_id,catalog_id,catalog_version) \
             VALUES ('t1','cat',1);"
        ),
    );
}

fn app_preamble() -> &'static str {
    "BEGIN; SET LOCAL ROLE wamn_app; SET LOCAL search_path TO wamn_run; \
     SET LOCAL app.tenant = 't1';"
}

/// A `DO` block asserting `has_column_privilege` matches `expected` exactly for
/// every column of `relation`, so a column joining or leaving a set is named.
fn assert_column_set(relation: &str, privilege: &str, all: &[&str], expected: &[&str]) -> String {
    let rows = all
        .iter()
        .map(|column| format!("('{column}',{})", expected.contains(column)))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "DO $probe$ DECLARE drift text; BEGIN \
           SELECT string_agg(format('%s(want %s)', c.name, c.want), ', ' ORDER BY c.name) \
             INTO drift \
             FROM (VALUES {rows}) AS c(name, want) \
            WHERE pg_catalog.has_column_privilege('wamn_app','{relation}',c.name,'{privilege}') \
                  IS DISTINCT FROM c.want; \
           ASSERT drift IS NULL, '{relation} {privilege} column drift: ' || drift; \
         END $probe$;"
    )
}

/// A `DO` block asserting the exact table-level privileges of `wamn_app`.
fn assert_table_privileges(relation: &str, held: &[&str]) -> String {
    let all = [
        "SELECT",
        "INSERT",
        "UPDATE",
        "DELETE",
        "TRUNCATE",
        "REFERENCES",
        "TRIGGER",
    ];
    let rows = all
        .iter()
        .map(|privilege| format!("('{privilege}',{})", held.contains(privilege)))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "DO $probe$ DECLARE drift text; BEGIN \
           SELECT string_agg(format('%s(want %s)', p.name, p.want), ', ' ORDER BY p.name) \
             INTO drift \
             FROM (VALUES {rows}) AS p(name, want) \
            WHERE pg_catalog.has_table_privilege('wamn_app','{relation}',p.name) \
                  IS DISTINCT FROM p.want; \
           ASSERT drift IS NULL, '{relation} table privilege drift: ' || drift; \
         END $probe$;"
    )
}

/// A `DO` block asserting PUBLIC holds nothing on `relation`, table or column.
fn assert_public_holds_nothing(relation: &str) -> String {
    format!(
        "DO $probe$ BEGIN \
           ASSERT NOT EXISTS ( \
             SELECT 1 FROM pg_catalog.pg_class relation \
              CROSS JOIN LATERAL pg_catalog.aclexplode( \
                COALESCE(relation.relacl, \
                         pg_catalog.acldefault('r', relation.relowner))) acl \
              WHERE relation.oid = pg_catalog.to_regclass('{relation}') \
                AND acl.grantee = 0), 'PUBLIC holds a table grant on {relation}'; \
           ASSERT NOT EXISTS ( \
             SELECT 1 FROM pg_catalog.pg_attribute attribute \
              CROSS JOIN LATERAL pg_catalog.aclexplode(attribute.attacl) acl \
              WHERE attribute.attrelid = pg_catalog.to_regclass('{relation}') \
                AND acl.grantee = 0), 'PUBLIC holds a column grant on {relation}'; \
         END $probe$;"
    )
}

#[test]
#[ignore = "requires WAMN_RUN_STORE_PG_URL and a throwaway PostgreSQL database"]
fn environment_policy_is_read_only_to_admission_roles() {
    let _serialize = INSTALL.lock().unwrap_or_else(|poison| poison.into_inner());
    let url = url();
    install(&url);

    success(
        &url,
        &format!(
            "{} {} \
             DO $probe$ BEGIN \
               ASSERT pg_catalog.has_table_privilege( \
                 'wamn_scenario_author','wamn_run.environment_policies','SELECT'); \
               ASSERT NOT EXISTS (SELECT FROM unnest(ARRAY['INSERT','UPDATE','DELETE']) p \
                 WHERE pg_catalog.has_table_privilege( \
                   'wamn_scenario_author','wamn_run.environment_policies',p)); \
               ASSERT NOT EXISTS (SELECT FROM unnest(ARRAY['SELECT','INSERT','UPDATE','DELETE']) p \
                 WHERE pg_catalog.has_table_privilege( \
                   'wamn_effect_writer','wamn_run.environment_policies',p)); \
               ASSERT NOT EXISTS (SELECT FROM unnest(ARRAY['SELECT','INSERT','UPDATE','DELETE']) p \
                 WHERE pg_catalog.has_table_privilege( \
                   'wamn_run_projection_writer','wamn_run.environment_policies',p)); \
             END $probe$;",
            assert_table_privileges("wamn_run.environment_policies", &["SELECT"]),
            assert_public_holds_nothing("wamn_run.environment_policies"),
        ),
    );

    success(
        &url,
        "INSERT INTO wamn_run.environment_policies \
           (tenant_id,expected_environment,durability_class) \
         VALUES ('t1','dev','durable'), ('t2','dev','standard');",
    );
    let visible = success(
        &url,
        &format!(
            "{} SELECT tenant_id || '|' || expected_environment || '|' || durability_class \
               FROM environment_policies; ROLLBACK;",
            app_preamble()
        ),
    );
    assert_eq!(
        visible.trim(),
        "t1|dev|durable",
        "tenant RLS must hide another tenant's projected policy"
    );

    let refused = success(
        &url,
        &format!(
            "{} DO $probe$ BEGIN \
               BEGIN \
                 INSERT INTO environment_policies \
                   (tenant_id,expected_environment,durability_class) \
                 VALUES ('t1','prod','durable'); \
                 RAISE EXCEPTION 'probe-not-refused'; \
               EXCEPTION WHEN insufficient_privilege THEN NULL; \
               END; \
             END $probe$; ROLLBACK; SELECT 'refused';",
            app_preamble()
        ),
    );
    assert!(refused.contains("refused"));
}

// ---------------------------------------------------------------------------
// wamn-0h0g.12.40 — runs writes are confined to the two ratified column sets.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires WAMN_RUN_STORE_PG_URL and a throwaway PostgreSQL database"]
fn app_run_writes_are_confined_to_the_ratified_column_sets() {
    let _serialize = INSTALL.lock().unwrap_or_else(|poison| poison.into_inner());
    let url = url();
    install(&url);
    success(
        &url,
        "INSERT INTO wamn_run.environment_policies \
           (tenant_id, expected_environment, durability_class) \
         VALUES ('t1', 'prod', 'standard');",
    );

    success(
        &url,
        &format!(
            "{} {} {} {}",
            // SELECT and DELETE stay table-wide; every other table privilege is
            // gone, so no column grant can be laundered into a table one.
            assert_table_privileges("wamn_run.runs", &["SELECT", "DELETE"]),
            assert_column_set("wamn_run.runs", "INSERT", RUN_COLUMNS, RUN_INSERT_COLUMNS),
            assert_column_set("wamn_run.runs", "UPDATE", RUN_COLUMNS, RUN_UPDATE_COLUMNS),
            assert_public_holds_nothing("wamn_run.runs"),
        ),
    );

    // A run the application role authored itself, through exactly the columns
    // the callable admission names.
    success(
        &url,
        &format!(
            "{} INSERT INTO runs \
               (tenant_id, run_id, flow_id, flow_version, catalog_id, catalog_version, \
                environment, execution_bundle_hash, attachment_id, status, trigger_source, \
                input_json, invocation_context, admission_context_version, platform_revision, \
                idempotency_key, response_deadline_at, run_deadline_at) \
             VALUES ('t1','r1','f',1,'cat',1,'prod','{EMPTY_HASH}','http-a','dispatched','http', \
                     '{{}}'::jsonb,'{{}}'::jsonb,'0.1','rev','k1', \
                     now() + interval '1 hour', now() + interval '1 hour'); \
             DO $probe$ BEGIN \
               ASSERT (SELECT capture_mode FROM runs WHERE run_id='r1') = 'off', \
                      'the withheld capture carrier took its fail-closed default'; \
             END $probe$; COMMIT;",
            app_preamble()
        ),
    );

    // The ratified UPDATE set is exercised positively, and the row lock the
    // outage rule is about is proven to still work under a 14-column grant.
    success(
        &url,
        &format!(
            "{} UPDATE runs SET status='running', updated_at=now() WHERE run_id='r1'; \
             DO $probe$ DECLARE locked text; BEGIN \
               SELECT run_id INTO locked FROM runs WHERE run_id='r1' FOR UPDATE; \
               ASSERT locked = 'r1', 'FOR UPDATE needs UPDATE on at least one column'; \
               SELECT run_id INTO locked FROM runs WHERE run_id='r1' FOR KEY SHARE; \
               ASSERT locked = 'r1', 'FOR KEY SHARE needs UPDATE on at least one column'; \
             END $probe$; COMMIT;",
            app_preamble()
        ),
    );

    // Every denial below writes tenant t1's own row, so the tenant policy
    // admits it and only the withheld column privilege can refuse.
    for (name, statement) in [
        (
            "insert-capture_mode",
            "INSERT INTO runs (tenant_id, run_id, flow_id, flow_version, catalog_id, \
               catalog_version, environment, execution_bundle_hash, status, capture_mode) \
             VALUES ('t1','r2','f',1,'cat',1,'prod','{EMPTY_HASH}','dispatched','off')",
        ),
        (
            "insert-fail_kind",
            "INSERT INTO runs (tenant_id, run_id, flow_id, flow_version, catalog_id, \
               catalog_version, environment, execution_bundle_hash, status, fail_kind) \
             VALUES ('t1','r3','f',1,'cat',1,'prod','{EMPTY_HASH}','dispatched','terminal')",
        ),
        (
            "insert-created_at",
            "INSERT INTO runs (tenant_id, run_id, flow_id, flow_version, catalog_id, \
               catalog_version, environment, execution_bundle_hash, status, created_at) \
             VALUES ('t1','r4','f',1,'cat',1,'prod','{EMPTY_HASH}','dispatched',now())",
        ),
        (
            "update-trigger_source",
            "UPDATE runs SET trigger_source='cron' WHERE run_id='r1'",
        ),
        (
            "update-input_json",
            "UPDATE runs SET input_json='{\"x\":1}'::jsonb WHERE run_id='r1'",
        ),
        (
            "update-idempotency_key",
            "UPDATE runs SET idempotency_key='stolen' WHERE run_id='r1'",
        ),
        (
            "update-created_at",
            "UPDATE runs SET created_at=now() WHERE run_id='r1'",
        ),
        (
            "update-tenant_id",
            "UPDATE runs SET tenant_id='t1' WHERE run_id='r1'",
        ),
    ] {
        let statement = statement.replace("{EMPTY_HASH}", EMPTY_HASH);
        let refused = success(
            &url,
            &format!(
                "{} DO $probe$ BEGIN \
                   BEGIN \
                     {statement}; \
                     RAISE EXCEPTION 'probe-not-refused'; \
                   EXCEPTION WHEN insufficient_privilege THEN NULL; \
                   END; \
                 END $probe$; ROLLBACK; SELECT 'refused';",
                app_preamble()
            ),
        );
        assert!(refused.contains("refused"), "{name} was not refused");
    }
}

// ---------------------------------------------------------------------------
// wamn-0h0g.12.41 — the admissions ledger is append-only but still lockable.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires WAMN_RUN_STORE_PG_URL and a throwaway PostgreSQL database"]
fn admission_ledger_is_append_only_and_still_key_share_lockable() {
    let _serialize = INSTALL.lock().unwrap_or_else(|poison| poison.into_inner());
    let url = url();
    install(&url);
    success(
        &url,
        "INSERT INTO wamn_run.environment_policies \
           (tenant_id, expected_environment, durability_class) \
         VALUES ('t1', 'prod', 'standard');",
    );

    success(
        &url,
        &format!(
            "{} {} {} {}",
            assert_table_privileges("wamn_run.invocation_admissions", &["SELECT", "INSERT"]),
            // `tenant_id` alone — the minimum PostgreSQL demands for a row lock.
            assert_column_set(
                "wamn_run.invocation_admissions",
                "UPDATE",
                ADMISSION_COLUMNS,
                &["tenant_id"]
            ),
            assert_column_set(
                "wamn_run.invocation_admissions",
                "INSERT",
                ADMISSION_COLUMNS,
                ADMISSION_COLUMNS
            ),
            assert_public_holds_nothing("wamn_run.invocation_admissions"),
        ),
    );

    // The ledger's own writer path: a run, then its admission, both authored by
    // the application role, then the FOR KEY SHARE the admission statement takes.
    success(
        &url,
        &format!(
            "{} INSERT INTO runs \
               (tenant_id, run_id, flow_id, flow_version, catalog_id, catalog_version, \
                environment, execution_bundle_hash, attachment_id, status, trigger_source) \
             VALUES ('t1','a1','f',1,'cat',1,'prod','{EMPTY_HASH}','http-a','dispatched','http'); \
             INSERT INTO invocation_admissions \
               (tenant_id, catalog_id, environment, attachment_id, definition_hash, \
                principal_digest, client_key_digest, client_request_fingerprint, \
                admitted_catalog_version, admitted_flow_version, run_id) \
             VALUES ('t1','cat','prod','http-a','d','p','ck','fp',1,1,'a1'); \
             INSERT INTO run_queue (tenant_id, run_id) VALUES ('t1','a1'); \
             DO $probe$ DECLARE locked text; BEGIN \
               SELECT run_id INTO locked FROM invocation_admissions \
                 WHERE run_id='a1' FOR KEY SHARE; \
               ASSERT locked = 'a1', \
                 'FOR KEY SHARE needs UPDATE on at least one column of this table'; \
             END $probe$; COMMIT;",
            app_preamble()
        ),
    );

    // The single writable column carries NO semantic authority: the policy's
    // WITH CHECK admits only the value the USING clause already required.
    success(
        &url,
        &format!(
            "{} UPDATE invocation_admissions SET tenant_id = 't1' WHERE run_id='a1'; \
             DO $probe$ DECLARE detail text; BEGIN \
               BEGIN \
                 UPDATE invocation_admissions SET tenant_id = 't2' WHERE run_id='a1'; \
                 RAISE EXCEPTION 'probe-not-refused'; \
               EXCEPTION WHEN insufficient_privilege THEN \
                 GET STACKED DIAGNOSTICS detail = MESSAGE_TEXT; \
                 ASSERT detail LIKE '%row-level security%', \
                   'the cross-tenant rewrite must be refused BY THE POLICY, not by a grant: ' \
                   || detail; \
               END; \
             END $probe$; COMMIT;",
            app_preamble()
        ),
    );

    // Neither withheld statement survives. Both target tenant t1's own row, so
    // the policy admits them and only the revoked privilege can refuse.
    for (name, statement) in [
        (
            "update-client_key_digest",
            "UPDATE invocation_admissions SET client_key_digest='forged' WHERE run_id='a1'",
        ),
        (
            "update-run_id",
            "UPDATE invocation_admissions SET run_id='a1' WHERE run_id='a1'",
        ),
        (
            "delete",
            "DELETE FROM invocation_admissions WHERE run_id='a1'",
        ),
    ] {
        let refused = success(
            &url,
            &format!(
                "{} DO $probe$ BEGIN \
                   BEGIN \
                     {statement}; \
                     RAISE EXCEPTION 'probe-not-refused'; \
                   EXCEPTION WHEN insufficient_privilege THEN NULL; \
                   END; \
                 END $probe$; ROLLBACK; SELECT 'refused';",
                app_preamble()
            ),
        );
        assert!(refused.contains("refused"), "{name} was not refused");
    }

    // The app role needs table-wide DELETE for retention, but the ordinary
    // trigger makes the prune statement's terminal predicate
    // caller-independent. This is a database refusal, not an API convention.
    let nonterminal_refused = success(
        &url,
        &format!(
            "{} DO $probe$ DECLARE state text; detail text; BEGIN \
               BEGIN \
                 DELETE FROM runs WHERE run_id='a1'; \
                 RAISE EXCEPTION 'probe-not-refused'; \
               EXCEPTION WHEN SQLSTATE '55000' THEN \
                 GET STACKED DIAGNOSTICS state = RETURNED_SQLSTATE, \
                                         detail = MESSAGE_TEXT; \
                 ASSERT state = '55000', 'nonterminal delete SQLSTATE drifted'; \
                 ASSERT detail = 'run-delete-nonterminal', \
                        'nonterminal delete refusal drifted: ' || detail; \
               END; \
               ASSERT EXISTS (SELECT FROM runs WHERE run_id='a1'), \
                      'a refused delete removed the live run'; \
               ASSERT EXISTS (SELECT FROM run_queue WHERE run_id='a1'), \
                      'a refused delete cascaded the live queue row'; \
             END $probe$; ROLLBACK; SELECT 'refused';",
            app_preamble()
        ),
    );
    assert!(
        nonterminal_refused.contains("refused"),
        "a dispatched run was deletable through the table grant"
    );

    // Referential integrity still reaps the ledger row once the run is
    // terminal. The cascade runs as the REFERENCING table's owner, not as
    // `wamn_app`, so the admission ledger needs no DELETE grant of its own.
    success(
        &url,
        &format!(
            "{} UPDATE runs SET status='completed' WHERE run_id='a1'; \
             DELETE FROM runs WHERE run_id='a1'; COMMIT; \
             SELECT 'cascaded';",
            app_preamble()
        ),
    );
    let remaining = success(
        &url,
        "SELECT \
           (SELECT count(*) FROM wamn_run.invocation_admissions WHERE run_id='a1'), \
           (SELECT count(*) FROM wamn_run.run_queue WHERE run_id='a1');",
    );
    assert_eq!(
        remaining.trim(),
        "0|0",
        "ON DELETE CASCADE must reap both admission and queue authority"
    );
}
