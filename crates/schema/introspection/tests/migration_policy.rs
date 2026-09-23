use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use wamn_schema_introspection::migration_policy::{
    DefinitionAction, DefinitionKind, MigrationPolicyError, MigrationPolicyErrorKind,
    inspect_migration_definition_mutations, validate_migration_file,
    validate_migration_file_for_schemas,
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
    validate_migration_file(artifact.path(), "inventory")
        .expect_err("migration policy must refuse the test artifact")
}

#[test]
fn accepts_the_package_owned_fixture_migration() {
    let path = wamn_fixture_package::migrations_dir().join("0001_initial.sql");
    validate_migration_file(&path, wamn_fixture_package::SCHEMA)
        .expect("the fixture migration must be admitted");
}

#[test]
fn admits_every_manifest_schema_and_refuses_an_out_of_set_target() {
    let admitted = TempArtifact::write(
        "sql",
        "CREATE TABLE warehouse.one (id uuid); CREATE TABLE inventory.two (id uuid);",
    );
    validate_migration_file_for_schemas(admitted.path(), &["inventory", "warehouse"])
        .expect("both manifest-declared schemas are admitted");

    let refused = TempArtifact::write(
        "sql",
        "CREATE TABLE warehouse.one (id uuid); CREATE TABLE control.hidden (id uuid);",
    );
    let error = validate_migration_file_for_schemas(refused.path(), &["inventory", "warehouse"])
        .expect_err("a target outside the manifest schema set must refuse");
    assert_eq!(error.kind(), MigrationPolicyErrorKind::CrossSchemaMutation);
    assert_eq!(error.statement_index(), Some(2));
    assert!(error.to_string().contains("control"));
}

#[test]
fn admits_the_demanded_overlay_additions() {
    let artifact = TempArtifact::write(
        "sql",
        r"
ALTER TABLE inventory.widget_maker
    ADD COLUMN overlay_inspection_required boolean NOT NULL DEFAULT false;
ALTER TABLE inventory.widget_maker
    ADD COLUMN overlay_quality_status text NOT NULL DEFAULT 'not_required';
ALTER TABLE inventory.widget_maker
    ADD CONSTRAINT widget_maker_overlay_quality_status_check
    CHECK (overlay_quality_status IN ('not_required', 'pending', 'approved', 'rejected'));
",
    );

    validate_migration_file(artifact.path(), "inventory")
        .expect("the two client fields and named check are the admitted overlay DDL");
}

#[test]
fn admits_a_nullable_column_of_every_modeled_type() {
    for column_type in [
        "boolean",
        "integer",
        "bigint",
        "double precision",
        "text",
        "bytea",
        "numeric",
        "timestamp with time zone",
        "jsonb",
        "uuid",
    ] {
        let sql =
            format!("ALTER TABLE inventory.widget ADD COLUMN overlay_description {column_type};");
        let artifact = TempArtifact::write("sql", &sql);
        validate_migration_file(artifact.path(), "inventory")
            .unwrap_or_else(|error| panic!("{sql} must be admitted: {error}"));
    }

    let uppercase = TempArtifact::write(
        "sql",
        "ALTER TABLE inventory.widget ADD COLUMN overlay_moved_at TIMESTAMP WITH TIME ZONE;",
    );
    validate_migration_file(uppercase.path(), "inventory")
        .expect("the type spelling is admitted without regard to case");
}

#[test]
fn a_nullable_column_admits_no_further_clause_and_no_unmodeled_type() {
    for sql in [
        "ALTER TABLE inventory.widget ADD COLUMN overlay_description varchar;",
        "ALTER TABLE inventory.widget ADD COLUMN overlay_description text[];",
        "ALTER TABLE inventory.widget ADD COLUMN overlay_description numeric(10, 2);",
        "ALTER TABLE inventory.widget ADD COLUMN overlay_description text COLLATE \"C\";",
        "ALTER TABLE inventory.widget ADD COLUMN overlay_description text DEFAULT 'open';",
        "ALTER TABLE inventory.widget ADD COLUMN overlay_description text NOT NULL;",
        "ALTER TABLE inventory.widget ADD COLUMN overlay_moved_at timestamp with time zone DEFAULT now();",
        "ALTER TABLE inventory.widget ADD COLUMN overlay_id uuid REFERENCES inventory.area (id);",
        "ALTER TABLE inventory.widget ADD COLUMN overlay_id uuid PRIMARY KEY;",
        "ALTER TABLE inventory.widget ADD COLUMN overlay_description text, ADD COLUMN overlay_note text;",
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
fn reports_definition_mutations_from_the_artifact_loader() {
    let artifact = TempArtifact::write(
        "sql",
        r"
CREATE TABLE inventory.quality_inspection (id uuid);
ALTER TABLE inventory.widget_maker ADD COLUMN overlay_flag boolean NOT NULL DEFAULT false;
ALTER TABLE inventory.widget_maker ADD CONSTRAINT widget_maker_overlay_flag_check CHECK (overlay_flag);
ALTER TABLE inventory.widget_maker ALTER COLUMN status SET DEFAULT 'complete';
ALTER TABLE inventory.widget_maker DROP CONSTRAINT widget_maker_name_check;
DROP TABLE inventory.widget_maker;
",
    );
    let bytes = fs::read(artifact.path()).expect("read mutation fixture bytes");
    let mutations = inspect_migration_definition_mutations(artifact.path(), &bytes, &["inventory"])
        .expect("the loader reports definition targets independently of admission");

    let observed = mutations
        .iter()
        .map(|mutation| {
            (
                mutation.action(),
                mutation.kind(),
                mutation.schema(),
                mutation.relation(),
                mutation.definition(),
                mutation.statement_index(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        observed,
        vec![
            (
                DefinitionAction::Create,
                DefinitionKind::Relation,
                "inventory",
                "quality_inspection",
                "quality_inspection",
                1,
            ),
            (
                DefinitionAction::Add,
                DefinitionKind::Field,
                "inventory",
                "widget_maker",
                "overlay_flag",
                2,
            ),
            (
                DefinitionAction::Add,
                DefinitionKind::Constraint,
                "inventory",
                "widget_maker",
                "widget_maker_overlay_flag_check",
                3,
            ),
            (
                DefinitionAction::Alter,
                DefinitionKind::Field,
                "inventory",
                "widget_maker",
                "status",
                4,
            ),
            (
                DefinitionAction::Drop,
                DefinitionKind::Constraint,
                "inventory",
                "widget_maker",
                "widget_maker_name_check",
                5,
            ),
            (
                DefinitionAction::Drop,
                DefinitionKind::Relation,
                "inventory",
                "widget_maker",
                "widget_maker",
                6,
            ),
        ]
    );
}

#[test]
fn overlay_additions_require_an_unquoted_qualified_target() {
    for sql in [
        "ALTER TABLE widget_maker ADD COLUMN overlay_flag boolean NOT NULL DEFAULT false;",
        "ALTER TABLE public.widget_maker ADD COLUMN overlay_flag boolean NOT NULL DEFAULT false;",
        "ALTER TABLE \"inventory\".widget_maker ADD COLUMN overlay_flag boolean NOT NULL DEFAULT false;",
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
fn overlay_additions_refuse_broader_alter_table_authority() {
    for sql in [
        "ALTER TABLE inventory.widget_maker DROP COLUMN status;",
        "ALTER TABLE inventory.widget_maker ALTER COLUMN status SET DEFAULT 'closed';",
        "ALTER TABLE inventory.widget_maker ADD COLUMN IF NOT EXISTS overlay_flag boolean NOT NULL DEFAULT false;",
        "ALTER TABLE inventory.widget_maker ADD COLUMN overlay_count bigint NOT NULL DEFAULT 0;",
        "ALTER TABLE inventory.widget_maker ADD COLUMN overlay_status text NOT NULL DEFAULT 'open';",
        "ALTER TABLE inventory.widget_maker ADD CONSTRAINT overlay_unique UNIQUE (name);",
        "ALTER TABLE inventory.widget_maker ADD CONSTRAINT overlay_check CHECK (true) NOT VALID;",
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
fn overlay_constraints_require_one_explicit_short_name() {
    let unnamed = refusal(
        "ALTER TABLE inventory.widget_maker ADD CHECK (overlay_quality_status <> 'unknown');",
    );
    assert_eq!(unnamed.kind(), MigrationPolicyErrorKind::UnnamedConstraint);

    let quoted = refusal(
        "ALTER TABLE inventory.widget_maker ADD CONSTRAINT \"overlay_check\" CHECK (true);",
    );
    assert_eq!(quoted.kind(), MigrationPolicyErrorKind::UnnamedConstraint);

    let overlength = "a".repeat(64);
    let error = refusal(&format!(
        "ALTER TABLE inventory.widget_maker ADD CONSTRAINT {overlength} CHECK (true);"
    ));
    assert_eq!(
        error.kind(),
        MigrationPolicyErrorKind::ConstraintNameTooLong
    );
}

#[test]
fn reads_sql_artifacts_and_refuses_rust_sources() {
    let artifact = TempArtifact::write(
        "rs",
        "const SQL: &str = \"CREATE TABLE inventory.not_an_artifact (id uuid)\";",
    );
    let error = validate_migration_file(artifact.path(), "inventory")
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
CREATE TABLE inventory.lexical_probe (
    "SET ROLE; quoted identifier" text,
    standard_text text DEFAULT 'SET ROLE; it''s inert',
    escaped_text text DEFAULT E'quote: \'; CREATE ROLE hidden',
    dollar_text text DEFAULT $body$; DROP DATABASE hidden; $body$
);
"#,
    );

    validate_migration_file(artifact.path(), "inventory")
        .expect("comments and quoted contents must remain lexically opaque");
}

#[test]
fn a_real_statement_after_quoted_content_is_still_refused() {
    let error =
        refusal("CREATE TABLE inventory.safe (note text DEFAULT 'SET ROLE;'); SET ROLE attacker;");

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
        "GRANT SELECT ON inventory.item TO attacker;",
        "REVOKE ALL ON inventory.item FROM attacker;",
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
        "CREATE INDEX CONCURRENTLY item_number_idx ON inventory.item (item_number);",
        "DROP INDEX CONCURRENTLY inventory.item_number_idx;",
        "CREATE DATABASE other_database;",
        "VACUUM inventory.item;",
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
    assert!(error.to_string().contains("inventory"));
}

#[test]
fn refuses_unnamed_supported_constraints_with_table_and_kind() {
    for (sql, constraint_kind) in [
        (
            "CREATE TABLE inventory.unnamed_primary (id uuid PRIMARY KEY);",
            "primary key",
        ),
        (
            "CREATE TABLE inventory.unnamed_unique (value text UNIQUE);",
            "unique",
        ),
        (
            "CREATE TABLE inventory.unnamed_foreign (item_id uuid REFERENCES inventory.item (id));",
            "foreign key",
        ),
        (
            "CREATE TABLE inventory.unnamed_check (quantity numeric CHECK (quantity > 0));",
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
            "CREATE TABLE inventory.name_at_limit (id uuid CONSTRAINT {admitted_name} PRIMARY KEY);"
        ),
    );
    validate_migration_file(admitted.path(), "inventory")
        .expect("a 63-byte constraint name must remain byte-exact in PostgreSQL");

    let refused_name = "a".repeat(64);
    let error = refusal(&format!(
        "CREATE TABLE inventory.name_over_limit (id uuid CONSTRAINT {refused_name} PRIMARY KEY);"
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
fn quoted_constraint_names_remain_fail_loud_at_the_truncation_boundary() {
    let under_limit = "a".repeat(63);
    let error = refusal(&format!(
        "CREATE TABLE inventory.quoted_name (id uuid CONSTRAINT \"{under_limit}\" PRIMARY KEY);"
    ));
    assert_eq!(error.kind(), MigrationPolicyErrorKind::UnnamedConstraint);

    let at_limit = "a".repeat(64);
    let error = refusal(&format!(
        "CREATE TABLE inventory.quoted_name (id uuid CONSTRAINT \"{at_limit}\" PRIMARY KEY);"
    ));
    assert_eq!(
        error.kind(),
        MigrationPolicyErrorKind::ConstraintNameTooLong
    );
    let display = error.to_string();
    assert!(display.contains(&at_limit), "{display}");
    assert!(display.contains("64 bytes"), "{display}");
}

#[test]
fn refuses_every_documented_ruled_object_class() {
    for sql in [
        "CREATE EXTENSION pgcrypto;",
        "CREATE FUNCTION inventory.f() RETURNS trigger LANGUAGE plpgsql AS $$BEGIN RETURN NEW; END$$;",
        "CREATE PROCEDURE inventory.p() LANGUAGE SQL AS $$SELECT 1$$;",
        "CREATE TRIGGER t BEFORE INSERT ON inventory.item EXECUTE FUNCTION inventory.f();",
        "CREATE RULE r AS ON INSERT TO inventory.item DO NOTHING;",
        "CREATE EVENT TRIGGER e ON ddl_command_start EXECUTE FUNCTION inventory.f();",
        "CREATE FOREIGN TABLE inventory.remote (id uuid) SERVER remote;",
        "CREATE VIEW inventory.v AS SELECT 1;",
        "CREATE MATERIALIZED VIEW inventory.mv AS SELECT 1;",
        "CREATE POLICY p ON inventory.item USING (true);",
        "CREATE LANGUAGE plpython3u;",
        "CREATE TYPE inventory.state AS ENUM ('open');",
        "CREATE DOMAIN inventory.identifier AS text;",
        "DO $$BEGIN NULL; END$$;",
        "ALTER TABLE inventory.item ENABLE ROW LEVEL SECURITY;",
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
fn refuses_every_statement_outside_the_demanded_migration_grammar() {
    for sql in [
        "CREATE SCHEMA inventory;",
        "ALTER TABLE inventory.item DROP COLUMN note;",
        "INSERT INTO inventory.item (item_number) VALUES ('x');",
        "CREATE TEMP TABLE inventory.temporary_item (id uuid);",
        "CREATE TABLE inventory.copy (LIKE inventory.item INCLUDING ALL);",
        "CREATE TABLE inventory.stored (id uuid) TABLESPACE fast;",
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
        "CREATE TABLE \"inventory\".widget (id uuid);",
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
        "CREATE TABLE inventory.item (note text DEFAULT 'open);",
        "CREATE TABLE inventory.item (\"note text);",
        "CREATE TABLE inventory.item (note text DEFAULT $tag$open);",
        "CREATE TABLE inventory.item (id uuid); /* open",
    ] {
        let error = refusal(sql);
        assert_eq!(error.kind(), MigrationPolicyErrorKind::InvalidSql, "{sql}");
    }
}
