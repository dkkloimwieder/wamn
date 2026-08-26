//! The live proof that the Rust and SQL halves of the tenant key AGREE.
//!
//! This is the sharpest failure mode in `wamn-0h0g.22.6`: provisioning mints a
//! guest login whose name carries the scope digest, and every governed RLS
//! predicate recomputes that digest in SQL from `current_user`. If the two
//! implementations disagree by one byte, every guest read refuses — and no pure
//! test can catch it, because one of the two implementations only exists inside
//! PostgreSQL.
//!
//! Shells out to `psql` deliberately: this crate has no database client
//! dependency and the existing live gates here (`cdc.rs`) take the same shape.

use std::process::Command;

use wamn_control_provision::tenant_key::{tenant_key, tenant_key_function_sql};

/// Every test gets its OWN database and its OWN roles.
///
/// Roles are CLUSTER-WIDE and each test rebuilds its world from scratch, so
/// sharing names would make these tests destroy each other whenever the runner
/// schedules them in parallel — which the default workspace sweep does. Per-test
/// isolation removes the shared mutable state instead of relying on
/// `--test-threads=1` being remembered.
struct Scope {
    db: String,
    owner: String,
    app: String,
}

impl Scope {
    fn new(slug: &str) -> Self {
        Self {
            db: format!("wamn-db-acme--billing--{slug}"),
            owner: format!("wamn_db_owner_{slug}"),
            app: format!("wamn_app_{slug}"),
        }
    }
}

/// Tenants spanning the charset `valid_tenant` admits (ASCII alphanumeric plus
/// `-` and `_`), including the boundary lengths where a framing bug would show.
const TENANTS: &[&str] = &[
    "acme",
    "t1",
    "a",
    "tenant-with-hyphens",
    "tenant_with_underscores",
    "A1b2C3",
    "0123456789012345678901234567890123456789012345678901234567890123",
];

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

fn psql_file(url: &str, script: &str) {
    let out = Command::new("psql")
        .arg(url)
        .args(["-v", "ON_ERROR_STOP=1", "-c", script])
        .output()
        .expect("psql runs");
    assert!(
        out.status.success(),
        "psql failed:\n--- stderr ---\n{}\n--- script ---\n{script}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Rebuild roles and database from scratch.
///
/// HERMETIC ON PURPOSE: a leftover healthy role satisfies `IF NOT EXISTS` and
/// would mask a mutated builder, so every run exercises the CURRENT builder.
fn reset(admin_url: &str, scope: &Scope) -> String {
    let (db, owner, app) = (&scope.db, &scope.owner, &scope.app);
    // CREATE/DROP DATABASE MUST BE THEIR OWN AUTOCOMMIT STATEMENTS. `psql -c`
    // wraps a multi-statement string in ONE transaction, and DROP DATABASE
    // cannot run inside one — so each statement gets its own invocation.
    psql_file(admin_url, &format!("DROP DATABASE IF EXISTS \"{db}\""));
    // DROP OWNED BY before DROP ROLE, inside an existence check: a leftover
    // healthy role satisfies IF NOT EXISTS and would mask a mutated builder.
    psql_file(
        admin_url,
        &format!(
            "DO $$ BEGIN \
               IF EXISTS (SELECT FROM pg_roles WHERE rolname = '{app}') THEN \
                 DROP OWNED BY {app}; DROP ROLE {app}; END IF; \
               IF EXISTS (SELECT FROM pg_roles WHERE rolname = '{owner}') THEN \
                 DROP OWNED BY {owner}; DROP ROLE {owner}; END IF; \
             END $$"
        ),
    );
    psql_file(admin_url, &format!("CREATE ROLE {owner} NOLOGIN"));
    psql_file(admin_url, &format!("CREATE ROLE {app} NOLOGIN"));
    psql_file(
        admin_url,
        &format!("CREATE DATABASE \"{db}\" OWNER {owner}"),
    );

    let base = admin_url.rsplit_once('/').expect("url names a database").0;
    format!("{base}/{db}")
}

/// The DDL under test, rendered for one isolated scope.
fn ddl(scope: &Scope) -> String {
    tenant_key_function_sql(&scope.db)
        .replace("\"wamn_db_owner\"", &format!("\"{}\"", scope.owner))
        .replace("\"wamn_app\"", &format!("\"{}\"", scope.app))
}

#[test]
fn the_sql_tenant_key_equals_the_rust_tenant_key() {
    let Ok(admin) = std::env::var("WAMN_TENANT_KEY_PG_URL") else {
        eprintln!(
            "skipping the_sql_tenant_key_equals_the_rust_tenant_key \
             (set WAMN_TENANT_KEY_PG_URL to run)"
        );
        return;
    };
    let scope = Scope::new("agree");
    let db_url = reset(&admin, &scope);
    psql_file(&db_url, &ddl(&scope));

    for tenant in TENANTS {
        let observed = psql(
            &db_url,
            None,
            &format!("SELECT wamn_authority.tenant_key('{tenant}')"),
        );
        let expected = tenant_key(tenant, &scope.db);
        assert_eq!(
            observed, expected,
            "tenant {tenant:?}: the SQL derivation and the Rust derivation \
             DISAGREE. Provisioning would mint a login named for one key while \
             every governed predicate computed the other, so every guest read \
             would refuse."
        );
        assert_eq!(
            expected.len(),
            40,
            "the digest is the 40-hex scope convention"
        );
    }
}

#[test]
fn the_function_carries_the_flags_the_expression_index_requires() {
    let Ok(admin) = std::env::var("WAMN_TENANT_KEY_PG_URL") else {
        eprintln!(
            "skipping the_function_carries_the_flags_the_expression_index_requires \
             (set WAMN_TENANT_KEY_PG_URL to run)"
        );
        return;
    };
    let scope = Scope::new("flags");
    let db_url = reset(&admin, &scope);
    psql_file(&db_url, &ddl(&scope));

    // Read from pg_proc, NOT from the DDL text. A function that silently lost
    // IMMUTABLE still creates fine and breaks every expression index built on
    // it, so the catalog is the only answer worth trusting.
    let flags = psql(
        &db_url,
        None,
        // Explicit casts: `\"char\" || \"char\"` is an ambiguous operator.
        "SELECT p.provolatile::text || p.proparallel::text \
           FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
          WHERE n.nspname = 'wamn_authority' AND p.proname = 'tenant_key'",
    );
    assert_eq!(
        flags, "is",
        "provolatile must be 'i' (IMMUTABLE) and proparallel 's' (SAFE); \
         without IMMUTABLE the expression index is not even creatable"
    );

    // The expression index the ruling requires must actually be creatable.
    psql_file(
        &db_url,
        "CREATE TABLE probe (tenant_id text NOT NULL); \
         CREATE INDEX IF NOT EXISTS probe_tenant_key \
           ON probe ((wamn_authority.tenant_key(tenant_id)))",
    );
    let indexed = psql(
        &db_url,
        None,
        "SELECT pg_get_indexdef(indexrelid) FROM pg_index i \
           JOIN pg_class c ON c.oid = i.indexrelid \
          WHERE c.relname = 'probe_tenant_key'",
    );
    assert!(
        indexed.contains("tenant_key"),
        "the expression index must record the derivation, got {indexed:?}"
    );
}

#[test]
fn the_guest_may_execute_the_derivation_and_may_not_replace_it() {
    let Ok(admin) = std::env::var("WAMN_TENANT_KEY_PG_URL") else {
        eprintln!(
            "skipping the_guest_may_execute_the_derivation_and_may_not_replace_it \
             (set WAMN_TENANT_KEY_PG_URL to run)"
        );
        return;
    };
    let scope = Scope::new("grants");
    let db_url = reset(&admin, &scope);
    psql_file(&db_url, &ddl(&scope));

    let execute = psql(
        &db_url,
        None,
        &format!(
            "SELECT has_function_privilege('{app}', \
             'wamn_authority.tenant_key(text)', 'EXECUTE')",
            app = scope.app
        ),
    );
    assert_eq!(
        execute, "t",
        "the guest family must be able to CALL the derivation"
    );

    // Ownership is what permits CREATE OR REPLACE. An attacker who can redefine
    // this function owns EVERY governed predicate at once, so the guest must not
    // own it.
    let owner = psql(
        &db_url,
        None,
        "SELECT pg_get_userbyid(p.proowner) \
           FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
          WHERE n.nspname = 'wamn_authority' AND p.proname = 'tenant_key'",
    );
    assert_eq!(
        owner, scope.owner,
        "the platform role owns the derivation, not the guest"
    );
    assert_ne!(
        owner, scope.app,
        "the guest must not be able to redefine the derivation"
    );

    let public_execute = psql(
        &db_url,
        None,
        "SELECT has_function_privilege('public', \
         'wamn_authority.tenant_key(text)', 'EXECUTE')",
    );
    assert_eq!(
        public_execute, "f",
        "PUBLIC must hold nothing on the derivation"
    );
}

/// R55: the bar is POST-STATE, not exit status. A second apply must leave the
/// definition byte-identical, which is what makes `catalog-schema.sql` and the
/// `wamn-0h0g.11.49` converge path provably the same object.
#[test]
fn a_second_apply_leaves_the_definition_identical() {
    let Ok(admin) = std::env::var("WAMN_TENANT_KEY_PG_URL") else {
        eprintln!(
            "skipping a_second_apply_leaves_the_definition_identical \
             (set WAMN_TENANT_KEY_PG_URL to run)"
        );
        return;
    };
    let scope = Scope::new("converge");
    let db_url = reset(&admin, &scope);
    let definition = |url: &str| {
        psql(
            url,
            None,
            "SELECT md5(pg_get_functiondef(p.oid)) \
               FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
              WHERE n.nspname = 'wamn_authority' AND p.proname = 'tenant_key'",
        )
    };

    psql_file(&db_url, &ddl(&scope));
    let first = definition(&db_url);
    psql_file(&db_url, &ddl(&scope));
    let second = definition(&db_url);

    assert_eq!(
        first, second,
        "a second apply changed the installed definition, so the two appliers \
         (catalog-schema.sql for fresh installs, wamn-0h0g.11.49's path for \
         existing databases) are not provably the same object"
    );
    assert!(!first.is_empty(), "the function must exist after applying");
}
