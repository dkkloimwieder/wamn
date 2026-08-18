//! Ignored live gate for the wiring activation verb (wamn-0h0g.18.2).
//!
//! The sibling `wiring_storage.rs` pins what the DDL *says*. This proves what it
//! *does*, which no pure test can reach: that the pointer flip and the rollback
//! are one statement, that the doorbell rings on commit and only on commit, that
//! a disabled or tombstoned pointer resolves to nothing, and that the app role
//! can serve the read without any grant beyond `SELECT`.
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

use std::io::Write as _;
use std::process::{Command, Output, Stdio};

use wamn_catalog::{
    WIRING_ACTIVATION_CHANNEL, WiringActivationNotice, flip_activation,
    previous_confirmed_definition, record_activation_event, resolve_active_wiring,
};

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

fn preamble() -> String {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../..");
    let catalog = std::fs::read_to_string(format!("{root}/deploy/sql/catalog-schema.sql"))
        .expect("read catalog DDL");
    format!(
        "DO $$ BEGIN \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') THEN \
             CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
           END IF; \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_scenario_author') THEN \
             CREATE ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
           END IF; \
         END $$;\n\
         DROP SCHEMA IF EXISTS catalog CASCADE;\n\
         {catalog}\n\
         DO $$ BEGIN \
           EXECUTE format('GRANT CONNECT ON DATABASE %I TO wamn_app', current_database()); \
         END $$;\n\
         GRANT USAGE ON SCHEMA catalog TO wamn_app;\n\
         SET app.tenant = 't1';\n\
         INSERT INTO catalog.catalogs \
                (tenant_id, catalog_id, version, environment, schema_version, state) \
         VALUES ('t1','shop',1,'prod','1','applied'), ('t1','shop',2,'prod','1','draft');\n\
         INSERT INTO catalog.catalog_heads \
                (tenant_id, catalog_id, environment, applied_catalog_version) \
         VALUES ('t1','shop','prod',1);\n\
         INSERT INTO catalog.wirings (tenant_id, catalog_id, wiring_id, version, \
                gated_catalog_version, graph_json, wiring_hash, gate_report_id) \
         VALUES ('t1','shop','orders-create',1,1,'{{\"n\":1}}','{a}','gate-1'), \
                ('t1','shop','orders-create',2,1,'{{\"n\":2}}','{b}','gate-2'), \
                ('t1','shop','orders-create',3,2,'{{\"n\":3}}','{c}','gate-3');\n",
        a = hash('a'),
        b = hash('b'),
        c = hash('c'),
    )
}

/// `PREPARE` the four real builders under short names.
fn prepared() -> String {
    format!(
        "PREPARE flip (text,text,text,text,boolean) AS {flip};\n\
         PREPARE record (text,text,text,boolean,text,text,text,text,text) AS {record};\n\
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
                        NULL,NULL,'ops','{reason}');\n\
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
    let url = std::env::var("WAMN_CATALOG_PG_URL")
        .expect("set WAMN_CATALOG_PG_URL to the throwaway superuser database");

    // One psql session throughout: it LISTENs before the first flip, so every
    // doorbell the server delivers lands in this stdout in commit order.
    let mut script = preamble();
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

    // The serving role holds SELECT and nothing else: it must resolve the wiring
    // and must not be able to move the pointer.
    script.push_str(
        "BEGIN;\nSET LOCAL ROLE wamn_app;\nSET LOCAL app.tenant = 't1';\n\
         CREATE TEMP TABLE as_app AS EXECUTE resolve('shop','prod','orders-create');\n\
         SELECT 'as_app=' || coalesce((SELECT version::text FROM as_app), 'dark');\nCOMMIT;\n",
    );

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
        "the app role serves the read with only its SELECT grant"
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
            "SET app.tenant = 't1';\n{prepared}\
             EXECUTE flip('shop','prod','orders-create','{c}',true);\n",
            prepared = prepared(),
            c = hash('c'),
        ),
    );
    assert!(
        refused.contains("wiring-definition-not-current"),
        "the ungated definition must be refused by name: {refused}"
    );

    let denied = refusal(
        &url,
        "BEGIN;\nSET LOCAL ROLE wamn_app;\nSET LOCAL app.tenant = 't1';\n\
         UPDATE catalog.wiring_activation SET enabled = false;\nCOMMIT;\n",
    );
    assert!(
        denied.contains("permission denied"),
        "the flip is a management-plane write; the app role must not reach it: {denied}"
    );
}
