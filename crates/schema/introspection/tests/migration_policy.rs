use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use wamn_schema_introspection::migration_policy::{
    MigrationPolicyError, MigrationPolicyErrorKind, validate_migration_file,
};

static ARTIFACT_ID: AtomicU64 = AtomicU64::new(0);

struct TempArtifact {
    path: PathBuf,
}

impl TempArtifact {
    fn write(extension: &str, sql: &str) -> Self {
        let id = ARTIFACT_ID.fetch_add(1, Ordering::Relaxed);
        let directory =
            std::env::temp_dir().join(format!("wamn-migration-policy-{}-{id}", std::process::id()));
        fs::create_dir(&directory).expect("create isolated migration-policy test directory");
        let path = directory.join(format!("migration.{extension}"));
        fs::write(&path, sql).expect("write migration-policy test artifact");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempArtifact {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        if let Some(directory) = self.path.parent() {
            let _ = fs::remove_dir(directory);
        }
    }
}

fn refusal(sql: &str) -> MigrationPolicyError {
    let artifact = TempArtifact::write("sql", sql);
    validate_migration_file(artifact.path(), "receiving")
        .expect_err("migration policy must refuse the test artifact")
}

#[test]
fn accepts_the_package_owned_receiving_migration() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../packages/receiving/migrations/0001_initial.sql");
    validate_migration_file(&path, "receiving").expect("Receiving migration must be admitted");
}

#[test]
fn reads_sql_artifacts_and_refuses_rust_sources() {
    let artifact = TempArtifact::write(
        "rs",
        "const SQL: &str = \"CREATE TABLE receiving.not_an_artifact (id uuid)\";",
    );
    let error = validate_migration_file(artifact.path(), "receiving")
        .expect_err("Rust source must never be the migration-policy subject");

    assert_eq!(error.kind(), MigrationPolicyErrorKind::NotSqlArtifact);
    assert_eq!(error.path(), artifact.path());
    assert_eq!(error.statement_index(), None);
}

#[test]
fn comments_and_quoted_regions_cannot_invent_statements() {
    let artifact = TempArtifact::write(
        "sql",
        r#"
-- SET ROLE attacker;
/* outer CREATE EXTENSION bogus; /* nested GRANT ALL TO attacker; */ still inert */
CREATE TABLE receiving.lexical_probe (
    "SET ROLE; quoted identifier" text,
    standard_text text DEFAULT 'SET ROLE; it''s inert',
    escaped_text text DEFAULT E'quote: \'; CREATE ROLE hidden',
    dollar_text text DEFAULT $body$; DROP DATABASE hidden; $body$
);
"#,
    );

    validate_migration_file(artifact.path(), "receiving")
        .expect("comments and quoted contents must remain lexically opaque");
}

#[test]
fn a_real_statement_after_quoted_content_is_still_refused() {
    let error =
        refusal("CREATE TABLE receiving.safe (note text DEFAULT 'SET ROLE;'); SET ROLE attacker;");

    assert_eq!(error.kind(), MigrationPolicyErrorKind::SetRole);
    assert_eq!(error.statement_index(), Some(2));
}

#[test]
fn refuses_session_authorization_switches() {
    for sql in [
        "SET ROLE attacker;",
        "SET LOCAL ROLE attacker;",
        "SET SESSION AUTHORIZATION attacker;",
        "RESET ROLE;",
    ] {
        let error = refusal(sql);
        assert_eq!(error.kind(), MigrationPolicyErrorKind::SetRole, "{sql}");
        assert!(error.to_string().contains("statement 1"), "{sql}");
    }
}

#[test]
fn refuses_role_operations() {
    for sql in [
        "CREATE ROLE attacker;",
        "ALTER USER attacker SUPERUSER;",
        "DROP GROUP attacker;",
        "REASSIGN OWNED BY attacker TO postgres;",
        "DROP OWNED BY attacker;",
    ] {
        let error = refusal(sql);
        assert_eq!(
            error.kind(),
            MigrationPolicyErrorKind::RoleOperation,
            "{sql}"
        );
    }
}

#[test]
fn refuses_grant_operations() {
    for sql in [
        "GRANT SELECT ON receiving.item TO attacker;",
        "REVOKE ALL ON receiving.item FROM attacker;",
        "ALTER DEFAULT PRIVILEGES GRANT SELECT ON TABLES TO attacker;",
    ] {
        let error = refusal(sql);
        assert_eq!(
            error.kind(),
            MigrationPolicyErrorKind::GrantOperation,
            "{sql}"
        );
    }
}

#[test]
fn refuses_nontransactional_operations() {
    for sql in [
        "CREATE INDEX CONCURRENTLY item_number_idx ON receiving.item (item_number);",
        "DROP INDEX CONCURRENTLY receiving.item_number_idx;",
        "CREATE DATABASE other_database;",
        "VACUUM receiving.item;",
    ] {
        let error = refusal(sql);
        assert_eq!(
            error.kind(),
            MigrationPolicyErrorKind::NontransactionalOperation,
            "{sql}"
        );
    }
}

#[test]
fn refuses_cross_schema_mutation() {
    let error = refusal("CREATE TABLE public.escape (id uuid);");

    assert_eq!(error.kind(), MigrationPolicyErrorKind::CrossSchemaMutation);
    assert!(error.to_string().contains("public"));
    assert!(error.to_string().contains("receiving"));
}

#[test]
fn refuses_unnamed_supported_constraints_with_table_and_kind() {
    for (sql, constraint_kind) in [
        (
            "CREATE TABLE receiving.unnamed_primary (id uuid PRIMARY KEY);",
            "primary key",
        ),
        (
            "CREATE TABLE receiving.unnamed_unique (value text UNIQUE);",
            "unique",
        ),
        (
            "CREATE TABLE receiving.unnamed_foreign (item_id uuid REFERENCES receiving.item (id));",
            "foreign key",
        ),
        (
            "CREATE TABLE receiving.unnamed_check (quantity numeric CHECK (quantity > 0));",
            "check",
        ),
    ] {
        let error = refusal(sql);
        assert_eq!(
            error.kind(),
            MigrationPolicyErrorKind::UnnamedConstraint,
            "{sql}"
        );
        let display = error.to_string();
        assert!(display.contains("unnamed_"), "{display}");
        assert!(display.contains(constraint_kind), "{display}");
    }
}

#[test]
fn refuses_constraint_names_at_postgresqls_truncation_boundary() {
    let admitted_name = "a".repeat(63);
    let admitted = TempArtifact::write(
        "sql",
        &format!(
            "CREATE TABLE receiving.name_at_limit (id uuid CONSTRAINT {admitted_name} PRIMARY KEY);"
        ),
    );
    validate_migration_file(admitted.path(), "receiving")
        .expect("a 63-byte constraint name must remain byte-exact in PostgreSQL");

    let refused_name = "a".repeat(64);
    let error = refusal(&format!(
        "CREATE TABLE receiving.name_over_limit (id uuid CONSTRAINT {refused_name} PRIMARY KEY);"
    ));
    assert_eq!(
        error.kind(),
        MigrationPolicyErrorKind::ConstraintNameTooLong
    );
    let display = error.to_string();
    assert!(display.contains("name_over_limit"), "{display}");
    assert!(display.contains("64 bytes"), "{display}");
}

#[test]
fn refuses_every_documented_ruled_object_class() {
    for sql in [
        "CREATE EXTENSION pgcrypto;",
        "CREATE FUNCTION receiving.f() RETURNS trigger LANGUAGE plpgsql AS $$BEGIN RETURN NEW; END$$;",
        "CREATE PROCEDURE receiving.p() LANGUAGE SQL AS $$SELECT 1$$;",
        "CREATE TRIGGER t BEFORE INSERT ON receiving.item EXECUTE FUNCTION receiving.f();",
        "CREATE RULE r AS ON INSERT TO receiving.item DO NOTHING;",
        "CREATE EVENT TRIGGER e ON ddl_command_start EXECUTE FUNCTION receiving.f();",
        "CREATE FOREIGN TABLE receiving.remote (id uuid) SERVER remote;",
        "CREATE VIEW receiving.v AS SELECT 1;",
        "CREATE MATERIALIZED VIEW receiving.mv AS SELECT 1;",
        "CREATE POLICY p ON receiving.item USING (true);",
        "CREATE LANGUAGE plpython3u;",
        "CREATE TYPE receiving.state AS ENUM ('open');",
        "CREATE DOMAIN receiving.identifier AS text;",
        "DO $$BEGIN NULL; END$$;",
        "ALTER TABLE receiving.item ENABLE ROW LEVEL SECURITY;",
    ] {
        let error = refusal(sql);
        assert_eq!(
            error.kind(),
            MigrationPolicyErrorKind::RuledOperation,
            "{sql}"
        );
    }
}

#[test]
fn refuses_every_statement_outside_the_narrow_create_table_grammar() {
    for sql in [
        "CREATE SCHEMA receiving;",
        "ALTER TABLE receiving.item ADD COLUMN note text;",
        "INSERT INTO receiving.item (item_number) VALUES ('x');",
        "CREATE TEMP TABLE receiving.temporary_item (id uuid);",
        "CREATE TABLE receiving.copy (LIKE receiving.item INCLUDING ALL);",
        "CREATE TABLE receiving.stored (id uuid) TABLESPACE fast;",
    ] {
        let error = refusal(sql);
        assert_eq!(
            error.kind(),
            MigrationPolicyErrorKind::UnsupportedStatement,
            "{sql}"
        );
    }
}

#[test]
fn refuses_unqualified_or_quoted_mutation_targets() {
    for sql in [
        "CREATE TABLE item (id uuid);",
        "CREATE TABLE \"receiving\".item (id uuid);",
    ] {
        let error = refusal(sql);
        assert_eq!(
            error.kind(),
            MigrationPolicyErrorKind::CrossSchemaMutation,
            "{sql}"
        );
    }
}

#[test]
fn refuses_unterminated_lexical_regions() {
    for sql in [
        "CREATE TABLE receiving.item (note text DEFAULT 'open);",
        "CREATE TABLE receiving.item (\"note text);",
        "CREATE TABLE receiving.item (note text DEFAULT $tag$open);",
        "CREATE TABLE receiving.item (id uuid); /* open",
    ] {
        let error = refusal(sql);
        assert_eq!(error.kind(), MigrationPolicyErrorKind::InvalidSql, "{sql}");
    }
}
