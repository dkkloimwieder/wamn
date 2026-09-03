//! Live PostgreSQL 18 exit gate for migration-derived catalog introspection.
//!
//! `WAMN_SCHEMA_INTROSPECTION_PG_URL` must name a disposable PostgreSQL 18
//! maintenance database through a superuser connection. The admin identity is
//! used only to create, measure, and remove the fixture database and negative
//! catalog objects. The real Receiving migration and supported additive changes
//! execute through a dedicated non-superuser role that owns only `receiving`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use tokio_postgres::{Client, Config, NoTls};

use wamn_schema_introspection::ir::{ColumnDefault, ColumnGeneration, IdentityMode};
use wamn_schema_introspection::migration_policy::validate_migration_file;
use wamn_schema_introspection::postgres::{
    PostgresIntrospectionErrorKind, read_catalog, read_catalog_excluding_relations,
};

const APPLICATION_SCHEMA: &str = "receiving";
const CONTROL_SCHEMA: &str = "wamn_control";
const FIXTURE_SCHEMA: &str = "wamn_introspection_fixture";
const STATEMENT_TIMEOUT: &str = "5s";
const LOCK_TIMEOUT: &str = "2s";
const TRANSACTION_TIMEOUT: &str = "15s";

struct Fixture {
    database: String,
    role: String,
    password: String,
    migration_path: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let id = std::process::id();
        Self {
            database: format!("wamn_intro_{id}"),
            role: format!("wamn_intro_migrator_{id}"),
            password: format!("wamn_intro_password_{id}"),
            migration_path: Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../packages/receiving/migrations/0001_initial.sql"),
        }
    }
}

fn identifier(value: &str) -> String {
    assert!(
        !value.is_empty()
            && value.len() < 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
        "fixture identifier must be a short ASCII SQL identifier"
    );
    format!("\"{value}\"")
}

async fn connect(config: Config) -> Client {
    let (client, connection) = config.connect(NoTls).await.expect("connect to PostgreSQL");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

async fn configure_admin(client: &Client) {
    client
        .batch_execute(&format!(
            "SET statement_timeout = '{STATEMENT_TIMEOUT}'; \
             SET lock_timeout = '{LOCK_TIMEOUT}'; \
             SET transaction_timeout = '{TRANSACTION_TIMEOUT}'"
        ))
        .await
        .expect("bound admin fixture timeouts");
}

async fn assert_postgres_18(client: &Client) {
    let version = client
        .query_one("SELECT current_setting('server_version_num')", &[])
        .await
        .expect("read PostgreSQL server version")
        .get::<_, String>(0)
        .parse::<u32>()
        .expect("server_version_num is numeric");
    assert!(
        (180_000..190_000).contains(&version),
        "live gate requires PostgreSQL 18, found server_version_num={version}"
    );
}

async fn remove_fixture(admin: &Client, fixture: &Fixture) {
    admin
        .batch_execute(&format!(
            "DROP DATABASE IF EXISTS {} WITH (FORCE)",
            identifier(&fixture.database)
        ))
        .await
        .expect("drop fixture database");
    admin
        .batch_execute(&format!(
            "DROP ROLE IF EXISTS {}",
            identifier(&fixture.role)
        ))
        .await
        .expect("drop fixture migration role");
}

async fn create_fixture(admin: &Client, fixture: &Fixture, admin_config: &Config) {
    remove_fixture(admin, fixture).await;
    admin
        .batch_execute(&format!(
            "CREATE ROLE {} LOGIN PASSWORD '{}' NOSUPERUSER NOCREATEDB NOCREATEROLE \
             NOINHERIT NOREPLICATION NOBYPASSRLS",
            identifier(&fixture.role),
            fixture.password
        ))
        .await
        .expect("create least-privilege migration role");
    admin
        .batch_execute(&format!(
            "CREATE DATABASE {}",
            identifier(&fixture.database)
        ))
        .await
        .expect("create fixture database");
    admin
        .batch_execute(&format!(
            "REVOKE ALL ON DATABASE {} FROM PUBLIC; \
             GRANT CONNECT ON DATABASE {} TO {}",
            identifier(&fixture.database),
            identifier(&fixture.database),
            identifier(&fixture.role)
        ))
        .await
        .expect("confine fixture database privileges");

    let mut target_config = admin_config.clone();
    target_config.dbname(&fixture.database);
    let target_admin = connect(target_config).await;
    configure_admin(&target_admin).await;
    target_admin
        .batch_execute(&format!(
            "CREATE SCHEMA {APPLICATION_SCHEMA} AUTHORIZATION {}; \
             CREATE SCHEMA {CONTROL_SCHEMA}; \
             CREATE TABLE {CONTROL_SCHEMA}.control_only (id bigint); \
             CREATE SCHEMA {FIXTURE_SCHEMA}",
            identifier(&fixture.role)
        ))
        .await
        .expect("create owned application schema and unconfigured fixture schemas");
}

fn target_config(admin_config: &Config, fixture: &Fixture, as_migration_role: bool) -> Config {
    let mut config = admin_config.clone();
    config.dbname(&fixture.database);
    if as_migration_role {
        config.user(&fixture.role);
        config.password(fixture.password.as_bytes());
    }
    config
}

async fn execute_migration_transaction(client: &Client, sql: &str) {
    client
        .batch_execute(&format!(
            "BEGIN; \
             SET LOCAL statement_timeout = '{STATEMENT_TIMEOUT}'; \
             SET LOCAL lock_timeout = '{LOCK_TIMEOUT}'; \
             SET LOCAL transaction_timeout = '{TRANSACTION_TIMEOUT}'"
        ))
        .await
        .expect("begin bounded migration transaction");

    let settings = client
        .query_one(
            "SELECT current_setting('statement_timeout'), \
                    current_setting('lock_timeout'), \
                    current_setting('transaction_timeout')",
            &[],
        )
        .await
        .expect("read migration transaction timeouts");
    assert_eq!(settings.get::<_, String>(0), STATEMENT_TIMEOUT);
    assert_eq!(settings.get::<_, String>(1), LOCK_TIMEOUT);
    assert_eq!(settings.get::<_, String>(2), TRANSACTION_TIMEOUT);

    if let Err(error) = client.batch_execute(sql).await {
        let _ = client.batch_execute("ROLLBACK").await;
        panic!("apply SQL as migration role: {error}");
    }
    client
        .batch_execute("COMMIT")
        .await
        .expect("commit migration transaction");
}

async fn assert_role_boundary(admin: &Client, target_admin: &Client, fixture: &Fixture) {
    let role = admin
        .query_one(
            "SELECT rolsuper, rolcreatedb, rolcreaterole, rolinherit, rolreplication, rolbypassrls \
               FROM pg_catalog.pg_roles WHERE rolname = $1",
            &[&fixture.role],
        )
        .await
        .expect("read migration role attributes");
    for column in 0..6 {
        assert!(
            !role.get::<_, bool>(column),
            "migration role attribute {column} is false"
        );
    }

    let owned_schemas = target_admin
        .query(
            "SELECT namespace.nspname::text \
               FROM pg_catalog.pg_namespace AS namespace \
               JOIN pg_catalog.pg_roles AS owner ON owner.oid = namespace.nspowner \
              WHERE owner.rolname = $1 ORDER BY namespace.nspname",
            &[&fixture.role],
        )
        .await
        .expect("read schemas owned by migration role")
        .into_iter()
        .map(|row| row.get::<_, String>(0))
        .collect::<Vec<_>>();
    assert_eq!(owned_schemas, [APPLICATION_SCHEMA]);

    let database_privileges = target_admin
        .query_one(
            "SELECT has_database_privilege($1, current_database(), 'CONNECT'), \
                    has_database_privilege($1, current_database(), 'CREATE'), \
                    has_database_privilege($1, current_database(), 'TEMPORARY')",
            &[&fixture.role],
        )
        .await
        .expect("read migration role database privileges");
    assert!(database_privileges.get::<_, bool>(0));
    assert!(!database_privileges.get::<_, bool>(1));
    assert!(!database_privileges.get::<_, bool>(2));
}

async fn server_table_names(client: &Client) -> Vec<(String, String)> {
    client
        .query(
            "SELECT namespace.nspname::text, relation.relname::text \
               FROM pg_catalog.pg_class AS relation \
               JOIN pg_catalog.pg_namespace AS namespace \
                 ON namespace.oid = relation.relnamespace \
              WHERE namespace.nspname = ANY($1::text[]) AND relation.relkind = 'r' \
              ORDER BY namespace.nspname, relation.relname",
            &[&vec![APPLICATION_SCHEMA.to_owned()]],
        )
        .await
        .expect("read server table names")
        .into_iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect()
}

async fn assert_base_ir(client: &Client, admin: &Client) {
    let first = read_catalog(client, &[APPLICATION_SCHEMA])
        .await
        .expect("introspect real Receiving migration");
    let second = read_catalog(client, &[APPLICATION_SCHEMA])
        .await
        .expect("repeat Receiving introspection");
    assert_eq!(first.canonical_json_bytes(), second.canonical_json_bytes());

    let server_tables = server_table_names(client).await;
    let ir_tables = first
        .tables()
        .iter()
        .map(|table| (table.schema().to_owned(), table.name().to_owned()))
        .collect::<Vec<_>>();
    assert_eq!(
        ir_tables, server_tables,
        "IR preserves every configured table"
    );
    assert_eq!(
        server_tables.len(),
        7,
        "the real migration created seven tables"
    );
    assert!(
        first
            .tables()
            .iter()
            .all(|table| table.schema() == APPLICATION_SCHEMA),
        "unconfigured control schemas never enter IR"
    );

    let control_exists = admin
        .query_one(
            "SELECT to_regclass('wamn_control.control_only') IS NOT NULL",
            &[],
        )
        .await
        .expect("read control-schema server answer")
        .get::<_, bool>(0);
    assert!(
        control_exists,
        "control object exists on the server but not in IR"
    );

    let constraint_indexes = client
        .query_one(
            "SELECT count(*) \
               FROM pg_catalog.pg_constraint AS constraint_row \
               JOIN pg_catalog.pg_class AS relation ON relation.oid = constraint_row.conrelid \
               JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
              WHERE namespace.nspname = $1 AND constraint_row.conindid <> 0",
            &[&APPLICATION_SCHEMA],
        )
        .await
        .expect("count constraint-backed indexes")
        .get::<_, i64>(0);
    assert!(
        constraint_indexes > 0,
        "server reports constraint-backed indexes"
    );
    assert!(
        first
            .tables()
            .iter()
            .all(|table| table.indexes().is_empty()),
        "constraint-backed indexes normalize into constraints, not ordinary indexes"
    );
}

async fn assert_control_owned_relations_are_outside_package_ir(client: &Client) {
    let before = read_catalog(client, &[APPLICATION_SCHEMA])
        .await
        .expect("read application catalog before host maps");
    client
        .batch_execute(
            "CREATE TABLE receiving.wamn_entities ( \
               relation_oid oid PRIMARY KEY, \
               package_id text NOT NULL, \
               entity_id text NOT NULL, \
               table_name text NOT NULL); \
             CREATE TABLE receiving.wamn_cdc_exclusions ( \
               relation_oid oid PRIMARY KEY, \
               package_id text NOT NULL, \
               relation_id text NOT NULL, \
               table_name text NOT NULL)",
        )
        .await
        .expect("create the exact host-owned relation maps");

    let unscoped = read_catalog(client, &[APPLICATION_SCHEMA])
        .await
        .expect_err("raw catalog reader admitted a host-owned oid column");
    assert_eq!(
        unscoped.kind(),
        PostgresIntrospectionErrorKind::UnsupportedColumnType
    );

    let excluded = [
        (APPLICATION_SCHEMA, "wamn_entities"),
        (APPLICATION_SCHEMA, "wamn_cdc_exclusions"),
    ];
    let scoped = read_catalog_excluding_relations(client, &[APPLICATION_SCHEMA], &excluded)
        .await
        .expect("package reader excludes exact host-owned relations");
    let repeated = read_catalog_excluding_relations(client, &[APPLICATION_SCHEMA], &excluded)
        .await
        .expect("repeat exact host-owned relation exclusion");
    assert_eq!(scoped.canonical_json_bytes(), before.canonical_json_bytes());
    assert_eq!(
        scoped.canonical_json_bytes(),
        repeated.canonical_json_bytes()
    );

    client
        .batch_execute(
            "DROP TABLE receiving.wamn_cdc_exclusions; \
             DROP TABLE receiving.wamn_entities",
        )
        .await
        .expect("remove host-owned relation maps");
}

async fn add_supported_columns(client: &Client) {
    execute_migration_transaction(
        client,
        r"
ALTER TABLE receiving.purchase_order
    ADD COLUMN additive_boolean boolean,
    ADD COLUMN additive_int32 int4,
    ADD COLUMN additive_int64 int8,
    ADD COLUMN additive_float64 float8,
    ADD COLUMN additive_text text,
    ADD COLUMN additive_bytes bytea,
    ADD COLUMN additive_numeric numeric,
    ADD COLUMN additive_timestamptz timestamptz,
    ADD COLUMN additive_json jsonb,
    ADD COLUMN additive_uuid uuid,
    ADD COLUMN additive_identity bigint GENERATED ALWAYS AS IDENTITY,
    ADD COLUMN additive_status_key text GENERATED ALWAYS AS (lower(status)) STORED,
    ADD COLUMN acme_inspection_required boolean NOT NULL DEFAULT false,
    ADD COLUMN acme_quality_status text NOT NULL DEFAULT 'not_required',
    ADD CONSTRAINT purchase_order_acme_quality_status_check
        CHECK (acme_quality_status IN ('not_required', 'pending', 'approved'));
CREATE TABLE receiving.quality_inspection (
    receipt_id uuid
        CONSTRAINT quality_inspection_receipt_id_pkey PRIMARY KEY
        CONSTRAINT quality_inspection_receipt_id_fkey
        REFERENCES receiving.receipt (id),
    status text NOT NULL DEFAULT 'pending',
    row_version int8 NOT NULL DEFAULT 1,
    CONSTRAINT quality_inspection_status_check
        CHECK (status IN ('pending', 'approved'))
)
",
    )
    .await;
}

async fn assert_additive_columns(client: &Client) {
    for (index, quality_status) in ["not_required", "pending", "approved"]
        .into_iter()
        .enumerate()
    {
        let purchase_order_number = format!("quality-status-{index}");
        client
            .execute(
                "INSERT INTO receiving.purchase_order \
                    (purchase_order_number, supplier_id, acme_quality_status) \
                 VALUES ($1, gen_random_uuid(), $2)",
                &[&purchase_order_number, &quality_status],
            )
            .await
            .unwrap_or_else(|error| {
                panic!("quality status {quality_status:?} was refused: {error}")
            });
    }
    let invalid_status = client
        .execute(
            "INSERT INTO receiving.purchase_order \
                (purchase_order_number, supplier_id, acme_quality_status) \
             VALUES ('quality-status-invalid', gen_random_uuid(), 'unknown')",
            &[],
        )
        .await
        .expect_err("an undeclared quality status was admitted");
    assert_eq!(
        invalid_status
            .as_db_error()
            .map(|error| error.code().code()),
        Some("23514")
    );
    assert_eq!(
        invalid_status
            .as_db_error()
            .and_then(|error| error.constraint()),
        Some("purchase_order_acme_quality_status_check")
    );

    let server_columns = client
        .query(
            "SELECT attribute.attname::text, \
                    pg_catalog.format_type(attribute.atttypid, attribute.atttypmod), \
                    attribute.attidentity::text, attribute.attgenerated::text \
               FROM pg_catalog.pg_attribute AS attribute \
               JOIN pg_catalog.pg_class AS relation ON relation.oid = attribute.attrelid \
               JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
              WHERE namespace.nspname = 'receiving' \
                AND relation.relname = 'purchase_order' \
                AND attribute.attname LIKE 'additive_%' \
                AND attribute.attnum > 0 AND NOT attribute.attisdropped \
              ORDER BY attribute.attname",
            &[],
        )
        .await
        .expect("read additive columns from pg_catalog")
        .into_iter()
        .map(|row| {
            (
                row.get::<_, String>(0),
                (
                    row.get::<_, String>(1),
                    row.get::<_, String>(2),
                    row.get::<_, String>(3),
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        server_columns.len(),
        12,
        "server reports every additive column"
    );
    let expected_server_types = [
        ("additive_boolean", "boolean"),
        ("additive_bytes", "bytea"),
        ("additive_float64", "double precision"),
        ("additive_identity", "bigint"),
        ("additive_int32", "integer"),
        ("additive_int64", "bigint"),
        ("additive_json", "jsonb"),
        ("additive_numeric", "numeric"),
        ("additive_status_key", "text"),
        ("additive_text", "text"),
        ("additive_timestamptz", "timestamp with time zone"),
        ("additive_uuid", "uuid"),
    ];
    for (name, expected_type) in expected_server_types {
        assert_eq!(
            server_columns[name].0, expected_type,
            "pg_catalog reports the expected PostgreSQL type for {name}"
        );
    }
    assert_eq!(server_columns["additive_identity"].1, "a");
    assert_eq!(server_columns["additive_status_key"].2, "s");

    let ir = read_catalog(client, &[APPLICATION_SCHEMA])
        .await
        .expect("introspect supported additive columns");
    let repeated = read_catalog(client, &[APPLICATION_SCHEMA])
        .await
        .expect("repeat additive introspection");
    assert_eq!(ir.canonical_json_bytes(), repeated.canonical_json_bytes());
    let table = ir
        .tables()
        .iter()
        .find(|table| table.name() == "purchase_order")
        .expect("purchase_order remains in IR");
    let inspection_required = table
        .columns()
        .iter()
        .find(|column| column.name() == "acme_inspection_required")
        .expect("client inspection field remains in IR");
    assert_eq!(
        inspection_required.default(),
        Some(ColumnDefault::BooleanFalse)
    );
    let quality_status = table
        .columns()
        .iter()
        .find(|column| column.name() == "acme_quality_status")
        .expect("client quality field remains in IR");
    assert_eq!(
        quality_status.default(),
        Some(ColumnDefault::TextNotRequired)
    );
    assert!(!quality_status.nullable());
    assert!(
        table
            .constraints()
            .iter()
            .any(|constraint| { constraint.name() == "purchase_order_acme_quality_status_check" }),
        "client quality check remains in IR"
    );
    let inspection = ir
        .tables()
        .iter()
        .find(|table| table.name() == "quality_inspection")
        .expect("quality inspection remains in IR");
    let inspection_status = inspection
        .columns()
        .iter()
        .find(|column| column.name() == "status")
        .expect("quality inspection status remains in IR");
    assert_eq!(
        inspection_status.default(),
        Some(ColumnDefault::TextPending)
    );
    let ir_columns = table
        .columns()
        .iter()
        .filter(|column| column.name().starts_with("additive_"))
        .map(|column| (column.name(), column))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(ir_columns.len(), server_columns.len());

    let expected_types = [
        ("additive_boolean", "boolean"),
        ("additive_bytes", "bytes"),
        ("additive_float64", "float64"),
        ("additive_int32", "int32"),
        ("additive_int64", "int64"),
        ("additive_json", "json"),
        ("additive_numeric", "numeric"),
        ("additive_text", "text"),
        ("additive_timestamptz", "timestamptz"),
        ("additive_uuid", "uuid"),
    ];
    for (name, expected_type) in expected_types {
        assert_eq!(
            ir_columns[name].column_type().as_str(),
            expected_type,
            "reader preserves {name} from the server answer"
        );
    }
    assert!(matches!(
        ir_columns["additive_identity"].generation(),
        Some(ColumnGeneration::Identity {
            mode: IdentityMode::Always
        })
    ));
    assert!(matches!(
        ir_columns["additive_status_key"].generation(),
        Some(ColumnGeneration::Stored { expression }) if expression.as_ref() == "lower(status)"
    ));

    let identity_sequence = client
        .query_one(
            "SELECT count(*) = 1 \
               FROM pg_catalog.pg_class AS sequence_relation \
               JOIN pg_catalog.pg_depend AS dependency \
                 ON dependency.classid = 'pg_catalog.pg_class'::pg_catalog.regclass \
                AND dependency.objid = sequence_relation.oid \
                AND dependency.deptype = 'i' \
               JOIN pg_catalog.pg_class AS table_relation \
                 ON table_relation.oid = dependency.refobjid \
               JOIN pg_catalog.pg_attribute AS attribute \
                 ON attribute.attrelid = table_relation.oid \
                AND attribute.attnum = dependency.refobjsubid \
              WHERE sequence_relation.relkind = 'S' \
                AND table_relation.relname = 'purchase_order' \
                AND attribute.attname = 'additive_identity'",
            &[],
        )
        .await
        .expect("read identity sequence dependency from pg_catalog")
        .get::<_, bool>(0);
    assert!(
        identity_sequence,
        "server identity sequence normalized into its column"
    );
}

async fn refusal_case(
    admin: &Client,
    reader: &Client,
    create_sql: &str,
    server_probe_sql: &str,
    cleanup_sql: &str,
    expected: PostgresIntrospectionErrorKind,
) -> wamn_schema_introspection::postgres::PostgresIntrospectionError {
    admin
        .batch_execute(create_sql)
        .await
        .expect("create legitimate catalog refusal input");
    let server_answer = admin.query_one(server_probe_sql, &[]).await;
    let reader_answer = read_catalog(reader, &[APPLICATION_SCHEMA]).await;
    let cleanup = admin.batch_execute(cleanup_sql).await;

    assert!(
        server_answer
            .expect("read refused object from server catalog")
            .get::<_, bool>(0),
        "the refused object is present in the server answer"
    );
    let error = reader_answer.expect_err("catalog reader must refuse the server object");
    assert_eq!(error.kind(), expected, "typed refusal: {error}");
    cleanup.expect("remove refused catalog object");
    error
}

async fn assert_refusal_matrix(admin: &Client, reader: &Client) {
    refusal_case(
        admin,
        reader,
        "CREATE UNLOGGED TABLE receiving.refused_unlogged (id bigint)",
        "SELECT c.relpersistence='u' FROM pg_catalog.pg_class c \
          JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace \
          WHERE n.nspname='receiving' AND c.relname='refused_unlogged' \
          AND c.relkind='r'",
        "DROP TABLE receiving.refused_unlogged",
        PostgresIntrospectionErrorKind::UnsupportedTable,
    )
    .await;
    refusal_case(
        admin,
        reader,
        "CREATE VIEW receiving.refused_view AS SELECT 1::bigint AS id",
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n \
          ON n.oid=c.relnamespace WHERE n.nspname='receiving' AND c.relname='refused_view' \
          AND c.relkind='v')",
        "DROP VIEW receiving.refused_view",
        PostgresIntrospectionErrorKind::UnsupportedView,
    )
    .await;
    refusal_case(
        admin,
        reader,
        "CREATE MATERIALIZED VIEW receiving.refused_materialized AS SELECT 1::bigint AS id",
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n \
          ON n.oid=c.relnamespace WHERE n.nspname='receiving' \
          AND c.relname='refused_materialized' AND c.relkind='m')",
        "DROP MATERIALIZED VIEW receiving.refused_materialized",
        PostgresIntrospectionErrorKind::UnsupportedMaterializedView,
    )
    .await;
    refusal_case(
        admin,
        reader,
        "CREATE FOREIGN DATA WRAPPER wamn_refused_fdw NO HANDLER; \
         CREATE SERVER wamn_refused_server FOREIGN DATA WRAPPER wamn_refused_fdw; \
         CREATE FOREIGN TABLE receiving.refused_foreign (id bigint) SERVER wamn_refused_server",
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n \
          ON n.oid=c.relnamespace WHERE n.nspname='receiving' AND c.relname='refused_foreign' \
          AND c.relkind='f')",
        "DROP FOREIGN TABLE receiving.refused_foreign; \
         DROP SERVER wamn_refused_server; DROP FOREIGN DATA WRAPPER wamn_refused_fdw",
        PostgresIntrospectionErrorKind::UnsupportedForeignTable,
    )
    .await;
    refusal_case(
        admin,
        reader,
        "CREATE FUNCTION receiving.refused_function() RETURNS bigint LANGUAGE SQL AS 'SELECT 1'",
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n \
          ON n.oid=p.pronamespace WHERE n.nspname='receiving' \
          AND p.proname='refused_function' AND p.prokind='f')",
        "DROP FUNCTION receiving.refused_function()",
        PostgresIntrospectionErrorKind::UnsupportedRoutine,
    )
    .await;
    refusal_case(
        admin,
        reader,
        "CREATE PROCEDURE receiving.refused_procedure() LANGUAGE SQL AS 'SELECT 1'",
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n \
          ON n.oid=p.pronamespace WHERE n.nspname='receiving' \
          AND p.proname='refused_procedure' AND p.prokind='p')",
        "DROP PROCEDURE receiving.refused_procedure()",
        PostgresIntrospectionErrorKind::UnsupportedRoutine,
    )
    .await;
    refusal_case(
        admin,
        reader,
        "CREATE FUNCTION wamn_introspection_fixture.trigger_function() RETURNS trigger \
           LANGUAGE plpgsql AS 'BEGIN RETURN NEW; END'; \
         CREATE TRIGGER refused_trigger BEFORE INSERT ON receiving.item \
           FOR EACH ROW EXECUTE FUNCTION wamn_introspection_fixture.trigger_function()",
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_trigger t JOIN pg_catalog.pg_class c \
          ON c.oid=t.tgrelid JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace \
          WHERE n.nspname='receiving' AND t.tgname='refused_trigger' AND NOT t.tgisinternal)",
        "DROP TRIGGER refused_trigger ON receiving.item; \
         DROP FUNCTION wamn_introspection_fixture.trigger_function()",
        PostgresIntrospectionErrorKind::UnsupportedTrigger,
    )
    .await;
    refusal_case(
        admin,
        reader,
        "CREATE RULE refused_rule AS ON UPDATE TO receiving.item DO NOTHING",
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_rewrite r JOIN pg_catalog.pg_class c \
          ON c.oid=r.ev_class JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace \
          WHERE n.nspname='receiving' AND r.rulename='refused_rule')",
        "DROP RULE refused_rule ON receiving.item",
        PostgresIntrospectionErrorKind::UnsupportedRule,
    )
    .await;
    refusal_case(
        admin,
        reader,
        "CREATE POLICY refused_policy ON receiving.item USING (true)",
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_policy p JOIN pg_catalog.pg_class c \
          ON c.oid=p.polrelid JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace \
          WHERE n.nspname='receiving' AND p.polname='refused_policy')",
        "DROP POLICY refused_policy ON receiving.item",
        PostgresIntrospectionErrorKind::UnsupportedPolicy,
    )
    .await;
    refusal_case(
        admin,
        reader,
        "CREATE DOMAIN receiving.refused_domain AS text",
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_type t JOIN pg_catalog.pg_namespace n \
          ON n.oid=t.typnamespace WHERE n.nspname='receiving' \
          AND t.typname='refused_domain' AND t.typtype='d')",
        "DROP DOMAIN receiving.refused_domain",
        PostgresIntrospectionErrorKind::UnsupportedCustomType,
    )
    .await;
    refusal_case(
        admin,
        reader,
        "CREATE TYPE receiving.refused_enum AS ENUM ('one', 'two')",
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_type t JOIN pg_catalog.pg_namespace n \
          ON n.oid=t.typnamespace WHERE n.nspname='receiving' \
          AND t.typname='refused_enum' AND t.typtype='e')",
        "DROP TYPE receiving.refused_enum",
        PostgresIntrospectionErrorKind::UnsupportedCustomType,
    )
    .await;
    refusal_case(
        admin,
        reader,
        "CREATE TABLE receiving.refused_acl (id bigint); \
         GRANT SELECT ON receiving.refused_acl TO PUBLIC",
        "SELECT c.relacl IS NOT NULL FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n \
          ON n.oid=c.relnamespace WHERE n.nspname='receiving' AND c.relname='refused_acl'",
        "DROP TABLE receiving.refused_acl",
        PostgresIntrospectionErrorKind::UnsupportedAcl,
    )
    .await;
    refusal_case(
        admin,
        reader,
        "ALTER TABLE receiving.purchase_order ADD COLUMN refused_identity bigint \
           GENERATED ALWAYS AS IDENTITY (START WITH 2)",
        "SELECT sequence_data.seqstart=2 FROM pg_catalog.pg_sequence AS sequence_data \
          WHERE sequence_data.seqrelid = \
            pg_catalog.pg_get_serial_sequence( \
              'receiving.purchase_order', 'refused_identity')::pg_catalog.regclass",
        "ALTER TABLE receiving.purchase_order DROP COLUMN refused_identity",
        PostgresIntrospectionErrorKind::UnsupportedIdentity,
    )
    .await;
    refusal_case(
        admin,
        reader,
        "ALTER TABLE receiving.item ADD COLUMN refused_collation text COLLATE \"C\"",
        "SELECT a.attcollation <> t.typcollation \
          FROM pg_catalog.pg_attribute a JOIN pg_catalog.pg_class c ON c.oid=a.attrelid \
          JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace \
          JOIN pg_catalog.pg_type t ON t.oid=a.atttypid WHERE n.nspname='receiving' \
          AND c.relname='item' AND a.attname='refused_collation'",
        "ALTER TABLE receiving.item DROP COLUMN refused_collation",
        PostgresIntrospectionErrorKind::UnsupportedColumnCollation,
    )
    .await;
    refusal_case(
        admin,
        reader,
        "ALTER TABLE receiving.item ADD COLUMN refused_date date",
        "SELECT pg_catalog.format_type(a.atttypid,a.atttypmod)='date' \
          FROM pg_catalog.pg_attribute a JOIN pg_catalog.pg_class c ON c.oid=a.attrelid \
          JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='receiving' \
          AND c.relname='item' AND a.attname='refused_date'",
        "ALTER TABLE receiving.item DROP COLUMN refused_date",
        PostgresIntrospectionErrorKind::UnsupportedColumnType,
    )
    .await;
    refusal_case(
        admin,
        reader,
        "ALTER TABLE receiving.purchase_order ALTER COLUMN status SET DEFAULT 'closed'",
        "SELECT pg_catalog.pg_get_expr(d.adbin,d.adrelid,false)=$$'closed'::text$$ \
          FROM pg_catalog.pg_attrdef d JOIN pg_catalog.pg_class c ON c.oid=d.adrelid \
          JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace \
          JOIN pg_catalog.pg_attribute a ON a.attrelid=d.adrelid AND a.attnum=d.adnum \
          WHERE n.nspname='receiving' AND c.relname='purchase_order' AND a.attname='status'",
        "ALTER TABLE receiving.purchase_order ALTER COLUMN status SET DEFAULT 'open'",
        PostgresIntrospectionErrorKind::UnsupportedColumnDefault,
    )
    .await;
    refusal_case(
        admin,
        reader,
        "ALTER TABLE receiving.purchase_order ADD COLUMN refused_virtual text \
           GENERATED ALWAYS AS (lower(status)) VIRTUAL",
        "SELECT a.attgenerated='v' FROM pg_catalog.pg_attribute a \
          JOIN pg_catalog.pg_class c ON c.oid=a.attrelid JOIN pg_catalog.pg_namespace n \
          ON n.oid=c.relnamespace WHERE n.nspname='receiving' \
          AND c.relname='purchase_order' AND a.attname='refused_virtual'",
        "ALTER TABLE receiving.purchase_order DROP COLUMN refused_virtual",
        PostgresIntrospectionErrorKind::UnsupportedGeneratedColumn,
    )
    .await;
    refusal_case(
        admin,
        reader,
        "CREATE INDEX purchase_order_status_expression_idx \
           ON receiving.purchase_order (lower(status))",
        "SELECT i.indexprs IS NOT NULL FROM pg_catalog.pg_index i JOIN pg_catalog.pg_class c \
          ON c.oid=i.indexrelid WHERE c.relname='purchase_order_status_expression_idx'",
        "DROP INDEX receiving.purchase_order_status_expression_idx",
        PostgresIntrospectionErrorKind::UnsupportedIndex,
    )
    .await;
    let wrong_index_name = refusal_case(
        admin,
        reader,
        "CREATE INDEX refused_name ON receiving.purchase_order (status)",
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n \
          ON n.oid=c.relnamespace WHERE n.nspname='receiving' AND c.relname='refused_name')",
        "DROP INDEX receiving.refused_name",
        PostgresIntrospectionErrorKind::UnsupportedIndex,
    )
    .await;
    assert_eq!(
        wrong_index_name.detail(),
        "name must use the authored convention `purchase_order_status_idx`"
    );
    let repeated_index_column = refusal_case(
        admin,
        reader,
        "CREATE INDEX purchase_order_status_status_idx \
           ON receiving.purchase_order (status, status)",
        "SELECT i.indnkeyatts=2 AND i.indkey[0]=i.indkey[1] \
          FROM pg_catalog.pg_index i JOIN pg_catalog.pg_class c \
          ON c.oid=i.indexrelid WHERE c.relname='purchase_order_status_status_idx'",
        "DROP INDEX receiving.purchase_order_status_status_idx",
        PostgresIntrospectionErrorKind::UnsupportedIndex,
    )
    .await;
    assert!(
        repeated_index_column
            .detail()
            .contains("distinct named columns")
    );
    let wrong_constraint_name = refusal_case(
        admin,
        reader,
        "ALTER TABLE receiving.item ADD CONSTRAINT refused_name CHECK (item_number <> '')",
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint WHERE conname='refused_name')",
        "ALTER TABLE receiving.item DROP CONSTRAINT refused_name",
        PostgresIntrospectionErrorKind::UnsupportedConstraint,
    )
    .await;
    assert_eq!(
        wrong_constraint_name.detail(),
        "name must use the authored convention `item_item_number_check`"
    );
    refusal_case(
        admin,
        reader,
        "CREATE SEQUENCE receiving.refused_sequence",
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n \
          ON n.oid=c.relnamespace WHERE n.nspname='receiving' \
          AND c.relname='refused_sequence' AND c.relkind='S')",
        "DROP SEQUENCE receiving.refused_sequence",
        PostgresIntrospectionErrorKind::UnsupportedSequence,
    )
    .await;
}

async fn run_gate(admin_config: Config, fixture: Fixture) {
    validate_migration_file(&fixture.migration_path, APPLICATION_SCHEMA)
        .expect("pre-apply policy admits the real Receiving migration");
    let migration_sql = std::fs::read_to_string(&fixture.migration_path)
        .expect("read the real Receiving migration after policy validation");

    let migration = connect(target_config(&admin_config, &fixture, true)).await;
    let target_admin = connect(target_config(&admin_config, &fixture, false)).await;
    configure_admin(&target_admin).await;
    assert_postgres_18(&migration).await;
    let identity = migration
        .query_one(
            "SELECT current_user::text, rolsuper \
               FROM pg_catalog.pg_roles WHERE rolname = current_user",
            &[],
        )
        .await
        .expect("read executing migration identity");
    assert_eq!(identity.get::<_, String>(0), fixture.role);
    assert!(!identity.get::<_, bool>(1));

    execute_migration_transaction(&migration, &migration_sql).await;
    assert_role_boundary(
        &connect(admin_config.clone()).await,
        &target_admin,
        &fixture,
    )
    .await;
    assert_base_ir(&migration, &target_admin).await;
    assert_control_owned_relations_are_outside_package_ir(&migration).await;
    add_supported_columns(&migration).await;
    assert_additive_columns(&migration).await;
    assert_refusal_matrix(&target_admin, &migration).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires WAMN_SCHEMA_INTROSPECTION_PG_URL and disposable PostgreSQL 18"]
async fn receiving_migration_round_trips_and_refuses_unsupported_server_objects() {
    let url = std::env::var("WAMN_SCHEMA_INTROSPECTION_PG_URL")
        .expect("WAMN_SCHEMA_INTROSPECTION_PG_URL must name a disposable PostgreSQL 18 server");
    let admin_config = url.parse::<Config>().expect("parse PostgreSQL admin URL");
    let admin = connect(admin_config.clone()).await;
    configure_admin(&admin).await;
    assert_postgres_18(&admin).await;
    let fixture = Fixture::new();
    create_fixture(&admin, &fixture, &admin_config).await;

    let database = fixture.database.clone();
    let role = fixture.role.clone();
    let outcome = tokio::spawn(run_gate(admin_config, fixture)).await;
    let teardown_fixture = Fixture {
        database,
        role,
        password: String::new(),
        migration_path: PathBuf::new(),
    };
    remove_fixture(&admin, &teardown_fixture).await;

    let gone = admin
        .query_one(
            "SELECT NOT EXISTS (SELECT 1 FROM pg_catalog.pg_database WHERE datname=$1), \
                    NOT EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname=$2)",
            &[&teardown_fixture.database, &teardown_fixture.role],
        )
        .await
        .expect("record clean fixture teardown");
    assert!(gone.get::<_, bool>(0), "fixture database was removed");
    assert!(gone.get::<_, bool>(1), "fixture role was removed");
    outcome.expect("live gate scenario completed without panic");
}
