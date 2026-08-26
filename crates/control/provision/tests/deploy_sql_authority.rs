//! The hand-written DDL's tenant floor derives from `current_user`.
//!
//! `wamn-0h0g.22.6.3` swept 43 guest-reachable relations across four files off
//! the settable `app.tenant` claim and onto
//! `wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key()`,
//! each with the expression index that keeps the predicate sargable.
//!
//! # Why these tests read the files as text
//!
//! `wamn-hopk` R5 forbids tests that scan source as text, with ONE exemption:
//! identity pins — byte-equality against a named artifact. The bootstrap block
//! is generated SQL checked into four files, so byte-equality against the
//! builder is exactly that exemption, and it is the only thing that catches a
//! hand-edit of a security-critical function definition.
//!
//! The live half asserts the SERVER's answer (`pg_policy`, `pg_index`,
//! `has_table_privilege`), never the file text.
//!
//! ```bash
//! docker run -d --name wamn-floor-pg -e POSTGRES_PASSWORD=probe \
//!   -p 127.0.0.1:5434:5432 postgres:18
//! until psql postgres://postgres:probe@localhost:5434/postgres -Atqc 'select 1'; do :; done
//! WAMN_TENANT_FLOOR_PG_URL=postgres://postgres:probe@localhost:5434/postgres \
//!   cargo test -p wamn-control-provision --test deploy_sql_authority
//! docker rm -f wamn-floor-pg      # BY EXPLICIT NAME. Never prune.
//! ```

use std::process::Command;

use wamn_control_provision::CredentialGeneration;
use wamn_control_provision::tenant_key::{authority_derivations_bootstrap_sql, tenant_key};
use wamn_control_provision::workload_role::{
    WorkloadRoleFamily, WorkloadRoleScope, workload_generation_role,
};

const POSTGRES_INIT: &str = include_str!("../../../../deploy/sql/postgres-init.sql");
const CATALOG_SCHEMA: &str = include_str!("../../../../deploy/sql/catalog-schema.sql");
const RUN_STATE: &str = include_str!("../../../../deploy/sql/run-state.sql");
const RUN_QUEUE: &str = include_str!("../../../../deploy/sql/run-queue.sql");
const APP_SCHEMA: &str = include_str!("../../../../deploy/sql/app-schema.sql");

/// The retired predicate, exactly as the files spelled it.
const RETIRED: &str = "tenant_id = NULLIF(current_setting('app.tenant', true), '')";

/// The governed predicate every swept policy carries.
const GOVERNED: &str = "wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key()";

/// The two relations whose claim is HOST-INJECTED, measured from
/// `has_table_privilege` rather than assumed: the guest ACL role holds nothing
/// on either, so re-keying them would be change without a threat.
const HOST_INJECTED: [&str; 2] = ["wamn_run.operator_run_actions", "wamn_run.run_queue"];

/// Every file that carries the bootstrap, with the number of governed policies
/// it re-keyed and the number of retired predicates it deliberately keeps.
const FILES: [(&str, &str, usize, usize); 4] = [
    ("catalog-schema.sql", CATALOG_SCHEMA, 24, 0),
    ("app-schema.sql", APP_SCHEMA, 7, 0),
    // run-state.sql keeps `operator_run_actions`: one policy, two clauses.
    ("run-state.sql", RUN_STATE, 5, 2),
    ("postgres-init.sql", POSTGRES_INIT, 7, 0),
];

/// IDENTITY PIN (the R5 exemption): the checked-in bootstrap is the builder's
/// output byte for byte. An attacker who can hand-edit `tenant_key` in one of
/// these files owns every predicate in that database, and nothing else in the
/// tree would notice.
#[test]
fn every_file_carries_the_generated_bootstrap_verbatim() {
    let bootstrap = authority_derivations_bootstrap_sql();
    for (name, sql, _, _) in FILES {
        assert_eq!(
            sql.matches(&bootstrap).count(),
            1,
            "{name} must carry authority_derivations_bootstrap_sql() exactly once, \
             byte for byte"
        );
    }
}

/// Both directions. Presence of the governed predicate is not enough: a policy
/// that KEPT the retired one would still hand every tenant's rows to whoever
/// sets the claim.
#[test]
fn the_swept_files_carry_the_governed_predicate_and_only_the_ruled_exceptions() {
    for (name, sql, governed, retired) in FILES {
        // Each policy spells the predicate twice (USING + WITH CHECK) unless it
        // is read-only, so this counts clauses, not policies — the count is
        // pinned per file rather than derived, so a lost clause shows up.
        assert!(
            sql.matches(GOVERNED).count() >= governed,
            "{name} must carry at least {governed} governed predicate clauses"
        );
        assert_eq!(
            sql.matches(RETIRED).count(),
            retired,
            "{name} carries an unexpected number of retired predicates"
        );
        assert_eq!(
            sql.matches("((wamn_authority.tenant_key(tenant_id)))")
                .count(),
            governed,
            "{name} must carry one tenant-key expression index per re-keyed relation"
        );
    }
    // The file that was left alone entirely, and the determination in writing.
    assert_eq!(
        RUN_QUEUE.matches(RETIRED).count(),
        2,
        "run-queue.sql keeps its host-injected claim"
    );
    assert_eq!(
        RUN_QUEUE.matches(GOVERNED).count(),
        0,
        "run-queue.sql is not guest-reachable and must not be re-keyed"
    );
}

fn psql(url: &str, database: Option<&str>, script: &str) -> String {
    let mut command = Command::new("psql");
    command
        .arg(url)
        .args(["-v", "ON_ERROR_STOP=1", "-Atqc", script]);
    if let Some(database) = database {
        command.args(["-d", database]);
    }
    let out = command.output().expect("psql runs");
    assert!(
        out.status.success(),
        "psql failed:\n--- stderr ---\n{}\n--- script ---\n{script}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Apply one file the way its own header says it must be applied.
///
/// `postgres-init.sql` carries `CREATE DATABASE` and `\connect`, so it cannot
/// run inside a transaction; `catalog-schema.sql` owns its own `BEGIN`. Neither
/// tolerates `psql -1`.
fn apply(url: &str, sql: &str) {
    let out = Command::new("psql")
        .arg(url)
        .args(["-v", "ON_ERROR_STOP=1", "-q", "-f", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map(|mut child| {
            use std::io::Write;
            child
                .stdin
                .take()
                .expect("stdin")
                .write_all(sql.as_bytes())
                .expect("write script");
            child.wait_with_output().expect("psql completes")
        })
        .expect("spawn psql (is it installed?)");
    assert!(
        out.status.success(),
        "apply failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Drop everything `postgres-init.sql` creates, so it can create it again.
///
/// `DROP DATABASE` cannot run inside a transaction and `psql -c` wraps a
/// multi-statement string in one, so each statement gets its own invocation.
/// `DROP OWNED BY` before `DROP ROLE`, inside an existence check: a leftover
/// healthy role satisfies an `IF NOT EXISTS` guard elsewhere and would mask a
/// mutated builder.
fn reset(admin_url: &str) {
    apply(admin_url, "DROP DATABASE IF EXISTS \"wamn\";\n");
    for role in ["wamn_app", "wamn_scenario_author", "wamn_effect_writer"] {
        apply(
            admin_url,
            &format!(
                "DO $$ BEGIN \
                   IF EXISTS (SELECT FROM pg_roles WHERE rolname = '{role}') THEN \
                     DROP OWNED BY {role}; DROP ROLE {role}; END IF; \
                 END $$;\n"
            ),
        );
    }
}

/// THE FLOOR, AS THE SERVER SEES IT.
///
/// Applies the real files to a fresh cluster and then asks `pg_policy`,
/// `pg_index` and `has_table_privilege` — never the file text — three things:
/// no guest-reachable relation is still on a settable claim, every re-keyed
/// relation carries its expression index, and a minted guest reads its own
/// tenant and only its own.
#[test]
fn the_swept_floor_admits_only_the_connected_guest_on_postgres() {
    let Ok(admin) = std::env::var("WAMN_TENANT_FLOOR_PG_URL") else {
        eprintln!(
            "skipping the_swept_floor_admits_only_the_connected_guest_on_postgres \
             (set WAMN_TENANT_FLOOR_PG_URL to run)"
        );
        return;
    };

    // HERMETIC, and it has to be: `postgres-init.sql` carries a bare
    // `CREATE DATABASE wamn` and bare `CREATE ROLE`s, so a second run against a
    // surviving cluster fails on the first statement — and a run that found the
    // database already populated would be asserting against LAST run's schema,
    // not this build's. Roles are CLUSTER-wide, so this gate OWNS its server:
    // point it only at a disposable one.
    reset(&admin);
    apply(&admin, POSTGRES_INIT);
    let base = admin.rsplit_once('/').expect("url names a database").0;
    let db_url = format!("{base}/wamn");
    for sql in [CATALOG_SCHEMA, RUN_STATE, RUN_QUEUE, APP_SCHEMA] {
        apply(&db_url, sql);
    }

    // 1. NOTHING guest-reachable is still on a settable claim.
    let settable = psql(
        &db_url,
        None,
        "SELECT coalesce(string_agg(rel, ' ' ORDER BY rel), '<none>') FROM (\
           SELECT n.nspname||'.'||c.relname AS rel \
             FROM pg_policy p JOIN pg_class c ON c.oid = p.polrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
            WHERE pg_get_expr(p.polqual, p.polrelid) LIKE '%app.tenant%' \
              AND has_table_privilege('wamn_app', c.oid, 'SELECT')) t",
    );
    assert_eq!(
        settable, "<none>",
        "a guest-reachable relation still keys on a claim the session can set"
    );

    // …and the two that keep the claim are exactly the ruled pair, still
    // holding no guest privilege. Asserted so the sweep cannot pass by having
    // granted the guest access to them instead.
    let kept = psql(
        &db_url,
        None,
        "SELECT string_agg(rel, ' ' ORDER BY rel) FROM (\
           SELECT n.nspname||'.'||c.relname AS rel \
             FROM pg_policy p JOIN pg_class c ON c.oid = p.polrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
            WHERE pg_get_expr(p.polqual, p.polrelid) LIKE '%app.tenant%') t",
    );
    assert_eq!(
        kept,
        HOST_INJECTED.join(" "),
        "the ruled exception set moved"
    );

    // 2. EVERY re-keyed relation carries its expression index. Without it the
    //    predicate sequential-scans, which is the cliff option (c) had to
    //    answer for. wamn-0h0g.22.15 makes this a standing denial-gate arm.
    let uncovered = psql(
        &db_url,
        None,
        "SELECT coalesce(string_agg(rel, ' ' ORDER BY rel), '<none>') FROM (\
           SELECT n.nspname||'.'||c.relname AS rel, p.polrelid AS oid \
             FROM pg_policy p JOIN pg_class c ON c.oid = p.polrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
            WHERE pg_get_expr(p.polqual, p.polrelid) LIKE '%current_tenant_key%') k \
          WHERE NOT EXISTS (SELECT 1 FROM pg_index i \
                             WHERE i.indrelid = k.oid \
                               AND pg_get_indexdef(i.indexrelid) LIKE '%tenant_key%')",
    );
    assert_eq!(
        uncovered, "<none>",
        "a re-keyed relation has no tenant-key expression index"
    );
    let governed = psql(
        &db_url,
        None,
        "SELECT count(*)::text FROM pg_policy p \
          WHERE pg_get_expr(p.polqual, p.polrelid) LIKE '%current_tenant_key%'",
    );
    assert_eq!(
        governed, "43",
        "the sweep must cover exactly the 43 guest-reachable relations"
    );

    // 3. A MINTED GUEST READS ITS OWN TENANT AND ONLY ITS OWN. The role name is
    //    composed by the mint, not by hand, so this also proves the digest the
    //    provisioner would issue matches the key the predicate computes.
    let guest = workload_generation_role(
        WorkloadRoleFamily::App,
        WorkloadRoleScope::Tenant {
            tenant: "t1",
            database: "wamn",
        },
        CredentialGeneration::A,
    )
    .expect("App takes a tenant scope");
    assert_eq!(
        &guest[guest.len() - 42..guest.len() - 2],
        tenant_key("t1", "wamn"),
        "the minted role must carry the tenant key the predicate computes"
    );
    apply(
        &admin,
        &format!(
            "DO $$ BEGIN \
               IF EXISTS (SELECT FROM pg_roles WHERE rolname = '{guest}') THEN \
                 DROP OWNED BY \"{guest}\"; DROP ROLE \"{guest}\"; END IF; \
             END $$;\n\
             CREATE ROLE \"{guest}\" NOLOGIN;\n\
             GRANT wamn_app TO \"{guest}\";\n"
        ),
    );
    apply(
        &db_url,
        &format!(
            "INSERT INTO catalog.catalogs (tenant_id, catalog_id, version, schema_version, state) \
               VALUES ('t1', 'c1', 1, 1, 'draft'), ('t2', 'c2', 1, 1, 'draft');\n\
             BEGIN;\n\
             SET LOCAL ROLE \"{guest}\";\n\
             SET LOCAL app.tenant = 't2';\n\
             DO $$ BEGIN\n\
                 ASSERT (SELECT count(*) FROM catalog.catalogs) = 1, \
                        'the guest sees exactly its own tenant';\n\
                 ASSERT (SELECT count(*) FROM catalog.catalogs WHERE tenant_id = 't2') = 0, \
                        'CROSS-TENANT READ';\n\
             END $$;\n\
             COMMIT;\n"
        ),
    );
    apply(&admin, &format!("DROP ROLE \"{guest}\";\n"));
}
