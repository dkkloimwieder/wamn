//! Ignored live gates for the wiring relations (wamn-0h0g.18.2, .18.5).
//!
//! The sibling `wiring_storage.rs` pins what the DDL *says*. These prove what it
//! *does*, which no pure test can reach: that the pointer flip and the rollback
//! are one statement, that the doorbell rings on commit and only on commit, that
//! a disabled or tombstoned pointer resolves to nothing, that an App generation
//! can serve the read through the stable role's `SELECT`, and that a document
//! declaring an entry and terminals reaches a CONVERGED database and comes back
//! out of `graph_json` with the same derived identity.
//!
//! Every statement under test is the real builder from `wamn_catalog`, executed
//! through `PREPARE`/`EXECUTE` so the gate cannot pass against text that only
//! resembles what production sends.
//!
//! Run it against a THROWAWAY superuser database — it creates cluster-wide roles
//! and rewrites the `catalog` schema:
//!
//! ```text
//! docker run -d --name pg -e POSTGRES_PASSWORD=pw -p 5433:5432 postgres:18
//! WAMN_CATALOG_PG_URL=postgresql://postgres:pw@127.0.0.1:5433/postgres \
//!   cargo test -p wamn-catalog --test wiring_activation_live -- --ignored
//! ```

use std::collections::BTreeMap;
use std::io::Write as _;
use std::process::{Command, Output, Stdio};
use std::sync::{Mutex, MutexGuard};

use wamn_catalog::{
    WIRING_ACTIVATION_CHANNEL, WiringActivationNotice, WiringDocument, WiringEdge, WiringNode,
    WiringTerminal, flip_activation, previous_confirmed_definition, record_activation_event,
    resolve_active_wiring,
};
use wamn_control_provision::{
    CredentialGeneration, WorkloadRoleFamily, WorkloadRoleScope, sql, workload_generation_role,
};
use wamn_event_wire::Op;

const TENANT: &str = "t1";
const APP_GENERATION_PASSWORD: &str = "test-owned-app-generation-password";
const APP_GENERATION_VALID_UNTIL: &str = "2099-01-01T00:00:00Z";

/// Every gate here rewrites the ONE `catalog` schema of the one database
/// `WAMN_CATALOG_PG_URL` names, so they take turns. Without this the default
/// harness runs them concurrently and each sees the other's `DROP SCHEMA
/// catalog CASCADE` land mid-install — a red that says nothing about the DDL.
static DATABASE: Mutex<()> = Mutex::new(());

/// Claim the database for the duration of one gate. A poisoned lock is a gate
/// that already failed and reported why; the next one still gets a clean schema
/// from its own preamble.
fn exclusive() -> MutexGuard<'static, ()> {
    DATABASE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn psql(url: &str, script: &str) -> Output {
    let mut child = Command::new("psql")
        .args(["-X", "-Atq", url])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run psql");
    child
        .stdin
        .take()
        .expect("psql stdin")
        .write_all(script.as_bytes())
        .expect("write psql script");
    child.wait_with_output().expect("wait for psql")
}

fn success(url: &str, script: &str) -> String {
    let output = psql(url, script);
    let stdout = String::from_utf8(output.stdout).expect("psql stdout is utf-8");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success() && !stderr.contains("ERROR:"),
        "psql failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    stdout
}

fn current_database(url: &str) -> String {
    let database = success(url, "SELECT current_database();\n");
    let database = database.trim();
    assert!(!database.is_empty(), "current_database() returned no name");
    database.to_owned()
}

fn app_generation(database: &str) -> String {
    workload_generation_role(
        WorkloadRoleFamily::App,
        WorkloadRoleScope::Tenant {
            tenant: TENANT,
            database,
        },
        CredentialGeneration::A,
    )
    .expect("App accepts tenant scope")
}

fn refusal(url: &str, script: &str) -> String {
    let output = psql(url, script);
    let stderr = String::from_utf8(output.stderr).expect("psql stderr is utf-8");
    assert!(
        stderr.contains("ERROR:"),
        "expected a refusal\nstdout:\n{}\nstderr:\n{stderr}",
        String::from_utf8_lossy(&output.stdout)
    );
    stderr
}

/// Every doorbell payload psql reported, in the order the server delivered them.
fn notices(stdout: &str) -> Vec<WiringActivationNotice> {
    stdout
        .lines()
        .filter(|line| {
            line.contains(&format!(
                "Asynchronous notification \"{WIRING_ACTIVATION_CHANNEL}\""
            ))
        })
        .map(|line| {
            let payload = line
                .split_once("with payload \"")
                .expect("psql reports the payload")
                .1
                .rsplit_once("\" received from")
                .expect("psql terminates the payload")
                .0;
            serde_json::from_str::<WiringActivationNotice>(payload)
                .unwrap_or_else(|error| panic!("the doorbell payload {payload:?} parses: {error}"))
        })
        .collect()
}

fn hash(letter: char) -> String {
    format!("sha256:{}", String::from(letter).repeat(64))
}

/// `(hash, enabled)` of one delivered doorbell, for comparing whole sequences.
fn rung(notice: &WiringActivationNotice) -> (String, bool) {
    assert_eq!(notice.tenant_id, "t1");
    assert_eq!(notice.catalog_id, "shop");
    assert_eq!(notice.environment, "prod");
    assert_eq!(notice.wiring_id, "orders-create");
    (notice.confirmed_definition_hash.clone(), notice.enabled)
}

fn preamble(database: &str, app_generation: &str) -> String {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../..");
    let catalog = std::fs::read_to_string(format!("{root}/deploy/sql/catalog-schema.sql"))
        .expect("read catalog DDL");
    let prepare_app = sql::prepare_workload_generation_sql(
        WorkloadRoleFamily::App,
        database,
        app_generation,
        APP_GENERATION_PASSWORD,
        APP_GENERATION_VALID_UNTIL,
    );
    format!(
        "{prepare_app}\n\
         DO $$ BEGIN \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_scenario_author') THEN \
             CREATE ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
           END IF; \
         END $$;\n\
         DROP SCHEMA IF EXISTS catalog CASCADE;\n\
         {catalog}\n\
         GRANT USAGE ON SCHEMA catalog TO wamn_app;\n\
         SET app.tenant = '{TENANT}';\n\
         INSERT INTO catalog.catalogs \
                (tenant_id, catalog_id, version, environment, schema_version, state) \
         VALUES ('t1','shop',1,'prod','1','applied'), ('t1','shop',2,'prod','1','draft');\n\
         INSERT INTO catalog.catalog_heads \
                (tenant_id, catalog_id, environment, applied_catalog_version) \
         VALUES ('t1','shop','prod',1);\n\
         INSERT INTO catalog.wirings (tenant_id, catalog_id, wiring_id, version, \
                gated_catalog_version, graph_json, wiring_hash) \
         VALUES ('t1','shop','orders-create',1,1,'{{\"n\":1}}','{a}'), \
                ('t1','shop','orders-create',2,1,'{{\"n\":2}}','{b}'), \
                ('t1','shop','orders-create',3,2,'{{\"n\":3}}','{c}');\n",
        a = hash('a'),
        b = hash('b'),
        c = hash('c'),
    )
}

/// `PREPARE` the four real builders under short names.
fn prepared() -> String {
    format!(
        "PREPARE flip (text,text,text,text,boolean) AS {flip};\n\
         PREPARE record (text,text,text,boolean,text,text,text,text) AS {record};\n\
         PREPARE prior (text,text,text,text) AS {prior};\n\
         PREPARE resolve (text,text,text) AS {resolve};\n",
        flip = flip_activation(),
        record = record_activation_event(),
        prior = previous_confirmed_definition(),
        resolve = resolve_active_wiring(),
    )
}

/// One activation: the flip and its provenance row, in one transaction.
fn activate(definition: &str, enabled: bool, reason: &str) -> String {
    format!(
        "BEGIN;\n\
         EXECUTE flip('shop','prod','orders-create','{definition}',{enabled});\n\
         EXECUTE record('shop','prod','orders-create',{enabled},'{definition}',\
                        NULL,'ops','{reason}');\n\
         COMMIT;\n"
    )
}

/// The env-hot read, reported as `<label>=<version>` or `<label>=dark`.
fn read(label: &str) -> String {
    format!(
        "CREATE TEMP TABLE {label} AS EXECUTE resolve('shop','prod','orders-create');\n\
         SELECT '{label}=' || coalesce((SELECT version::text FROM {label}), 'dark');\n"
    )
}

#[test]
#[ignore = "requires WAMN_CATALOG_PG_URL and a throwaway PostgreSQL database"]
fn wiring_activation_live() {
    let _database = exclusive();
    let url = std::env::var("WAMN_CATALOG_PG_URL")
        .expect("set WAMN_CATALOG_PG_URL to the throwaway superuser database");
    let database = current_database(&url);
    let app_generation = app_generation(&database);

    // One psql session throughout: it LISTENs before the first flip, so every
    // doorbell the server delivers lands in this stdout in commit order.
    let mut script = preamble(&database, &app_generation);
    script.push_str(&format!("LISTEN {WIRING_ACTIVATION_CHANNEL};\n"));
    script.push_str(&prepared());

    // The first activation is an INSERT; every later flip is an UPDATE of the
    // same key, because the pointer's PK is (tenant, catalog, env, wiring).
    script.push_str(&activate(&hash('a'), true, "activate v1"));
    script.push_str(&read("lit_v1"));
    script.push_str(&activate(&hash('b'), true, "activate v2"));
    script.push_str(&read("lit_v2"));

    // Rollback is discovered, then performed by the SAME `flip` statement — the
    // one this script prepared once, from the one builder in the crate.
    script.push_str(&format!(
        "CREATE TEMP TABLE prior_probe AS EXECUTE prior('shop','prod','orders-create','{b}');\n\
         SELECT 'prior=' || coalesce((SELECT confirmed_definition_hash FROM prior_probe), 'none');\n",
        b = hash('b'),
    ));
    script.push_str(&activate(&hash('a'), true, "rollback to v1"));
    script.push_str(&read("rolled_back"));

    // A flip that does not commit rings nothing and moves nothing.
    script.push_str(&format!(
        "BEGIN;\nEXECUTE flip('shop','prod','orders-create','{b}',true);\nROLLBACK;\n",
        b = hash('b'),
    ));
    script.push_str(&read("after_abort"));

    // Taking a wiring dark is the same flip with `enabled = false`; the
    // activation guard returns early for it and so can never refuse it.
    script.push_str(&activate(&hash('a'), false, "take dark"));
    script.push_str(&read("dark"));
    script.push_str(&activate(&hash('a'), true, "relight"));

    // A tombstone retires the id even though the pointer row survives enabled.
    script.push_str(
        "INSERT INTO catalog.wiring_tombstones \
                (tenant_id, catalog_id, environment, wiring_id, removed_in_catalog_version) \
         VALUES ('t1','shop','prod','orders-create',1);\n",
    );
    script.push_str(&read("tombstoned"));
    script.push_str("DELETE FROM catalog.wiring_tombstones;\n");

    // The serving generation inherits SELECT and nothing else from the stable
    // ACL role. Its `current_user` supplies the tenant authority; `app.tenant`
    // remains only the resolve builder's matching data-value input.
    script.push_str(&format!(
        "BEGIN;\nSET LOCAL ROLE {app_generation};\nSET LOCAL app.tenant = '{TENANT}';\n\
         SELECT 'as_app_user=' || current_user;\n\
         CREATE TEMP TABLE as_app AS EXECUTE resolve('shop','prod','orders-create');\n\
         SELECT 'as_app=' || coalesce((SELECT version::text FROM as_app), 'dark');\nCOMMIT;\n"
    ));

    let stdout = success(&url, &script);
    let reported = |label: &str| {
        stdout
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{label}=")))
            .unwrap_or_else(|| panic!("{label} was not reported\n{stdout}"))
    };

    assert_eq!(reported("lit_v1"), "1", "the first activation serves v1");
    assert_eq!(reported("lit_v2"), "2", "the second flip serves v2");
    assert_eq!(
        reported("prior"),
        hash('a'),
        "the rollback target is the last hash this pointer served that is not the current one"
    );
    assert_eq!(
        reported("rolled_back"),
        "1",
        "rollback is the same flip: one statement put v1 back"
    );
    assert_eq!(
        reported("after_abort"),
        "1",
        "a flip that did not commit moved nothing"
    );
    assert_eq!(
        reported("dark"),
        "dark",
        "a disabled pointer resolves to nothing"
    );
    assert_eq!(
        reported("tombstoned"),
        "dark",
        "a retired wiring id stops resolving even with an enabled pointer"
    );
    assert_eq!(
        reported("as_app"),
        "1",
        "the App generation serves the read through the stable role's SELECT grant"
    );
    assert_eq!(
        reported("as_app_user"),
        app_generation,
        "the tenant authority must be the prepared App generation"
    );

    // The doorbell rings on the INSERT and on every UPDATE, carries the flip's
    // own hash and enabled flag, and stays silent for the aborted transaction.
    let rings: Vec<(String, bool)> = notices(&stdout).iter().map(rung).collect();
    assert_eq!(
        rings,
        vec![
            (hash('a'), true),
            (hash('b'), true),
            (hash('a'), true),
            (hash('a'), false),
            (hash('a'), true),
        ],
        "five committed flips, five doorbells, and nothing for the rolled-back one\n{stdout}"
    );

    // A definition gated against a catalog version this environment has not
    // applied cannot be activated, however the caller spells the request.
    let refused = refusal(
        &url,
        &format!(
            "\\set VERBOSITY verbose\n\
             SET app.tenant = 't1';\n{prepared}\
             EXECUTE flip('shop','prod','orders-create','{c}',true);\n",
            prepared = prepared(),
            c = hash('c'),
        ),
    );
    let refused_sqlstate = refused.lines().find_map(|line| {
        line.strip_prefix("ERROR:  ")
            .and_then(|detail| detail.split_once(':'))
            .map(|(sqlstate, _message)| sqlstate)
    });
    assert_eq!(
        refused_sqlstate,
        Some("23503"),
        "the ungated definition must use the foreign-key refusal class: {refused}"
    );
    assert!(
        refused.contains("wiring-definition-not-current"),
        "the ungated definition must be refused by name: {refused}"
    );

    let denied = refusal(
        &url,
        &format!(
            "BEGIN;\nSET LOCAL ROLE {app_generation};\nSET LOCAL app.tenant = '{TENANT}';\n\
             UPDATE catalog.wiring_activation SET enabled = false;\nCOMMIT;\n"
        ),
    );
    assert!(
        denied.contains("permission denied"),
        "the flip is a management-plane write; the app role must not reach it: {denied}"
    );
}

/// The entry and the terminal reach an EXISTING database and survive the column.
///
/// wamn-0h0g.18.5 adds no relation and no column — the document rides
/// `catalog.wirings.graph_json` whole — so the claim to prove is not that a new
/// object installs, but that a database converged by the SAME slice
/// `ensure_catalog_storage` executes accepts the new document and gives it back
/// unchanged. `graph_json` is `jsonb`, which reorders keys and drops duplicates,
/// so "unchanged" is checked the only way that matters here: the re-parsed
/// document derives the identical `wiring_hash`.
///
/// The database is put into the pre-migration state on purpose — the four wiring
/// relations dropped — so the converge slice is genuinely exercised rather than
/// skipped over an install that already had them.
#[test]
#[ignore = "requires WAMN_CATALOG_PG_URL and a throwaway PostgreSQL database"]
fn the_terminal_document_reaches_a_converged_database_and_survives_the_column() {
    let _database = exclusive();
    let url = std::env::var("WAMN_CATALOG_PG_URL")
        .expect("set WAMN_CATALOG_PG_URL to the throwaway superuser database");
    let database = current_database(&url);
    let app_generation = app_generation(&database);

    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../..");
    let schema = std::fs::read_to_string(format!("{root}/deploy/sql/catalog-schema.sql"))
        .expect("read catalog DDL");
    let begin = schema
        .find("-- BEGIN WIRING STORAGE MIGRATION")
        .expect("the converge slice is delimited");
    let end = schema
        .find("-- END WIRING STORAGE MIGRATION")
        .expect("the converge slice is terminated");
    let converge_slice = &schema[begin..end];

    let node = |component: &str, operation: &str, terminal| WiringNode {
        component: component.to_owned(),
        interface_version: "0.1.0".to_owned(),
        operation: operation.to_owned(),
        params: BTreeMap::new(),
        terminal,
    };
    let document = WiringDocument::new(
        "orders-create",
        1,
        "in",
        BTreeMap::from([
            ("in".to_owned(), node("http-entry", "handle", None)),
            ("write".to_owned(), node("entity", "create", None)),
            (
                "out".to_owned(),
                node("respond", "emit", Some(WiringTerminal::Respond)),
            ),
            (
                "publish".to_owned(),
                node(
                    "bus",
                    "emit",
                    Some(WiringTerminal::emit("orders", Op::Insert)),
                ),
            ),
        ]),
        vec![
            WiringEdge {
                from: "in".to_owned(),
                from_port: "main".to_owned(),
                to: "write".to_owned(),
                to_port: None,
            },
            WiringEdge {
                from: "write".to_owned(),
                from_port: "main".to_owned(),
                to: "out".to_owned(),
                to_port: None,
            },
            WiringEdge {
                from: "write".to_owned(),
                from_port: "main".to_owned(),
                to: "publish".to_owned(),
                to_port: None,
            },
        ],
        Vec::new(),
    )
    .expect("a wiring declaring an entry and two terminals is a valid document");
    let wire = serde_json::to_string(&document).expect("the document serializes");

    // The probe is the one `ensure_catalog_storage` reads, spelled exactly as it
    // spells it, so a database it would converge is the database this reports on.
    let probe = |label: &str| {
        format!(
            "SELECT '{label}=' || (to_regclass('catalog.wirings') IS NOT NULL)::text || \
                    (to_regclass('catalog.wiring_tombstones') IS NOT NULL)::text || \
                    (to_regclass('catalog.wiring_activation') IS NOT NULL)::text || \
                    (to_regclass('catalog.wiring_activation_events') IS NOT NULL)::text;\n"
        )
    };

    let mut script = preamble(&database, &app_generation);
    script.push_str(
        "DROP TABLE catalog.wiring_activation_events, catalog.wiring_activation, \
                    catalog.wiring_tombstones, catalog.wirings CASCADE;\n\
         DROP FUNCTION catalog.validate_wiring_activation();\n\
         DROP FUNCTION catalog.notify_wiring_activation();\n",
    );
    script.push_str(&probe("before"));
    script.push_str(converge_slice);
    script.push_str(&probe("after"));
    script.push_str(&format!(
        "SET app.tenant = 't1';\n\
         INSERT INTO catalog.wirings (tenant_id, catalog_id, wiring_id, version, \
                gated_catalog_version, graph_json, wiring_hash) \
         VALUES ('t1','shop','orders-create',1,1,$doc${wire}$doc$,'{digest}');\n\
         SELECT 'stored=' || graph_json::text FROM catalog.wirings \
          WHERE wiring_id = 'orders-create' AND version = 1;\n",
        digest = document.wiring_hash(),
    ));

    let stdout = success(&url, &script);
    let reported = |label: &str| {
        stdout
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{label}=")))
            .unwrap_or_else(|| panic!("{label} was not reported\n{stdout}"))
    };

    assert_eq!(
        reported("before"),
        "falsefalsefalsefalse",
        "the database must start in the pre-migration state the converge path exists for"
    );
    assert_eq!(
        reported("after"),
        "truetruetruetrue",
        "the converge slice must install all four relations, not a subset"
    );

    let stored = serde_json::from_str::<serde_json::Value>(reported("stored"))
        .expect("the column gives back JSON");
    let read_back = WiringDocument::parse(&stored).expect("the stored document parses");
    assert_eq!(read_back.entry, "in", "the entry survived the jsonb column");
    assert_eq!(
        read_back.nodes["out"].terminal,
        Some(WiringTerminal::Respond)
    );
    assert_eq!(
        read_back.nodes["publish"].terminal,
        Some(WiringTerminal::emit("orders", Op::Insert))
    );
    assert_eq!(read_back.nodes["write"].terminal, None);
    assert_eq!(
        read_back.wiring_hash(),
        document.wiring_hash(),
        "jsonb reorders keys, so identity must be derived from the parsed document \
         and not from the bytes the column happened to return"
    );
}
