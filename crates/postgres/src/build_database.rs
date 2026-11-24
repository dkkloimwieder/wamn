//! PostgreSQL adapter for package verification.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context as _, Result, bail, ensure};
use postgres_protocol::escape::{escape_identifier, escape_literal};
use serde::{Deserialize, Serialize};
use sqlx::{
    AssertSqlSafe, Column as _, Connection as _, Either, Executor as _, Row as _, SqlSafeStr as _,
    TypeInfo as _,
    migrate::Migrator,
    postgres::{PgConnectOptions, PgSslMode},
};
use wamn_codegen::AuthoredSql;
use wamn_codegen::database::{
    BuildDatabase, DescribedColumn, DescribedStatement, HistoryTable, Migration, RelationColumn,
    VerificationDatabase,
};
use wamn_component_policy::PackageManifest;
use wamn_package::manifest::CONTROL_OWNED_RELATION_TABLES;
use wamn_package::manifest::source_name::SourceName;
use wamn_schema::ir::CatalogIr;

use crate::catalog::read_catalog_excluding_relations;

const RECORD_HISTORY_SQL: &str = include_str!("../../../deploy/sql/record-history.sql");
const RECORD_HISTORY_APP_GRANTS_SQL: &str =
    include_str!("../../../deploy/sql/record-history-app-grants.sql");

/// Verification against PostgreSQL using the project's TLS connection rules.
#[derive(Debug)]
pub struct PostgresBuildDatabase;

#[async_trait::async_trait]
impl BuildDatabase for PostgresBuildDatabase {
    async fn migration_directory(&self, path: &Path) -> Result<Vec<Migration>> {
        Ok(migration_records(Migrator::new(path).await?))
    }

    async fn package_migrations(&self, root: &Path) -> Result<Vec<Migration>> {
        Ok(migration_records(migrator(root).await?))
    }

    async fn server_version(&self, url: &str) -> Result<u32> {
        let mut connection = connect(url).await?;
        let version: i32 =
            sqlx::query_scalar("SELECT current_setting('server_version_num')::integer")
                .fetch_one(&mut connection)
                .await?;
        Ok(u32::try_from(version)?)
    }

    async fn create_verification_database(&self, server_url: &str) -> Result<VerificationDatabase> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .context("read the clock")?
            .as_nanos();
        let name = format!(
            "wamn_build_{}_{nanos}_{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let url = database_configuration(server_url, &name)?;
        execute_on_server(
            server_url,
            &format!("CREATE DATABASE {}", escape_identifier(&name)),
        )
        .await
        .context("create the verification database")?;
        Ok(VerificationDatabase { name, url })
    }

    async fn drop_verification_database(&self, server_url: &str, name: &str) -> Result<()> {
        execute_on_server(
            server_url,
            &format!(
                "DROP DATABASE IF EXISTS {} WITH (FORCE)",
                escape_identifier(name)
            ),
        )
        .await
        .with_context(|| format!("drop the verification database {name}"))
    }
    async fn migrate(
        &self,
        database_url: &str,
        schemas: &[String],
        directories: &[&Path],
        history: &[HistoryTable<'_>],
    ) -> Result<()> {
        let client = pool(database_url, 4)
            .await
            .context("connect to the verification database")?;
        let result = async {
            if !schemas.is_empty() {
                let quoted = schemas
                    .iter()
                    .map(|schema| escape_identifier(schema))
                    .collect::<Vec<_>>();
                let mut sql = String::new();
                for schema in &quoted {
                    writeln!(sql, "CREATE SCHEMA {schema};")
                        .expect("writing to a String cannot fail");
                }
                write!(
                    sql,
                    "ALTER DATABASE {} SET search_path TO {}, public",
                    escape_identifier(&identity(database_url)?.1),
                    quoted.join(", ")
                )
                .expect("writing to a String cannot fail");
                sqlx::raw_sql(AssertSqlSafe(sql).into_sql_str())
                    .execute(&client)
                    .await
                    .context("create the package schemas")?;
            }
            // Public platform identity keys support application foreign keys.
            // These verification relations contain no system database or registry data.
            sqlx::raw_sql(
                "CREATE SCHEMA IF NOT EXISTS platform;
             CREATE TABLE IF NOT EXISTS platform.external_system (id uuid PRIMARY KEY);
             CREATE TABLE IF NOT EXISTS platform.file (id uuid PRIMARY KEY)",
            )
            .execute(&client)
            .await
            .context("create platform references for package verification")?;
            // Platform helpers precede generated SQL in every verification database.
            create_history_tables(&client, history).await?;
            for (index, directory) in directories.iter().enumerate() {
                let mut migrator = migrator(directory).await?;
                migrator.dangerous_set_table_name(format!("_sqlx_verification_{index}"));
                let mut connection = connect(database_url).await?;
                register_verification_principal(&mut connection).await?;
                migrator
                    .run_direct(None, &mut connection, false)
                    .await
                    .context("migrate the verification database")?;
            }
            create_history_tables(&client, history).await
        }
        .await;
        client.close().await;
        result
    }

    async fn describe_statements(
        &self,
        database_url: &str,
        corpus: &[AuthoredSql<'_>],
    ) -> Result<BTreeMap<String, DescribedStatement>> {
        let mut connection = connect(database_url)
            .await
            .context("connect to the verification database to describe statements")?;
        let views: Vec<sqlx::postgres::types::Oid> = sqlx::query_scalar(
            AssertSqlSafe(
                "SELECT c.oid FROM pg_catalog.pg_class c \
            JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
            JOIN pg_catalog.pg_views v ON v.schemaname = n.nspname AND v.viewname = c.relname"
                    .to_owned(),
            )
            .into_sql_str(),
        )
        .fetch_all(&mut connection)
        .await
        .context("read view identities for statement descriptions")?;
        let mut described = BTreeMap::new();
        for source in corpus {
            let (path, bytes) = (source.path(), source.bytes());
            let sql = std::str::from_utf8(bytes)
                .with_context(|| format!("statement {path} is not UTF-8"))?;
            let describe = (&mut connection)
                .describe(AssertSqlSafe(sql.to_owned()).into_sql_str())
                .await
                .with_context(|| format!("describe statement {path}"))?;
            let parameters = match describe.parameters {
                Some(Either::Left(types)) => types.iter().map(|ty| ty.name().to_owned()).collect(),
                Some(Either::Right(_)) | None => {
                    bail!("describe of statement {path} gave no parameter types")
                }
            };
            let columns = describe
                .columns
                .iter()
                .zip(describe.nullable.iter())
                .map(|(column, nullable)| DescribedColumn {
                    name: column.name().to_owned(),
                    type_name: column.type_info().name().to_owned(),
                    nullable: *nullable,
                    view_column: column.relation_id().is_some_and(|id| views.contains(&id)),
                })
                .collect();
            described.insert(
                path.to_owned(),
                DescribedStatement {
                    parameters,
                    columns,
                },
            );
        }
        connection
            .close()
            .await
            .context("close the describe connection")?;
        Ok(described)
    }

    async fn read_catalog(
        &self,
        database_url: &str,
        schemas: &[&str],
        selected: &[String],
    ) -> Result<CatalogIr> {
        let client = pool(database_url, 4)
            .await
            .context("connect to the migrated database")?;
        let unselected = sqlx::query(
            "SELECT n.nspname::text, c.relname::text FROM pg_catalog.pg_class c
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
         WHERE n.nspname = ANY($1::text[]) AND c.relname <> ALL($2::text[])
         AND c.relkind IN ('r', 'v')",
        )
        .bind(schemas)
        .bind(selected)
        .fetch_all(&client)
        .await
        .context("select the package's catalog relations")?;
        let unselected = unselected
            .iter()
            .map(|row| Ok((row.try_get::<String, _>(0)?, row.try_get::<String, _>(1)?)))
            .collect::<Result<Vec<_>, sqlx::Error>>()?;
        let excluded_relations = schemas
            .iter()
            .flat_map(|schema| {
                CONTROL_OWNED_RELATION_TABLES
                    .iter()
                    .map(move |table| (*schema, *table))
            })
            .chain(
                unselected
                    .iter()
                    .map(|(schema, table)| (schema.as_str(), table.as_str())),
            )
            .collect::<Vec<_>>();
        let result = read_catalog_excluding_relations(&client, schemas, &excluded_relations)
            .await
            .context("introspect package schemas");
        client.close().await;
        result
    }

    async fn ordered_columns(
        &self,
        database_url: &str,
        manifest: &PackageManifest,
    ) -> Result<BTreeMap<SourceName, Vec<RelationColumn>>> {
        let client = pool(database_url, 4).await?;
        let result = async {
            let mut columns = BTreeMap::new();
            for (id, model) in &manifest.models {
                let rows = sqlx::query(SELECT_RELATION_COLUMNS_SQL)
                    .bind(model.schema.as_str())
                    .bind(model.table.as_str())
                    .fetch_all(&client)
                    .await?;
                columns.insert(
                    id.clone(),
                    rows.iter()
                        .map(|row| {
                            Ok(RelationColumn {
                                name: row.try_get(0)?,
                                primary_key: row.try_get(1)?,
                                timestamptz: row.try_get(2)?,
                            })
                        })
                        .collect::<Result<Vec<_>, sqlx::Error>>()?,
                );
            }
            Ok(columns)
        }
        .await;
        client.close().await;
        result
    }
}

/// Resolve both directories through SQLx, including reversible files and ignored entries.
pub async fn migrator(root: &Path) -> anyhow::Result<Migrator> {
    let mut migrations = Vec::new();
    for relative in ["migrations", "history"] {
        let directory = root.join(relative);
        if directory.is_dir() {
            migrations.extend(Migrator::new(directory).await?.migrations.into_owned());
        }
    }
    let mut migrator = Migrator::with_migrations(migrations);
    let mut versions = BTreeSet::new();
    for migration in migrator
        .iter()
        .filter(|migration| migration.migration_type.is_up_migration())
    {
        ensure!(
            versions.insert(migration.version),
            "duplicate migration version {} in {}",
            migration.version,
            root.display()
        );
    }
    migrator.set_locking(false);
    Ok(migrator)
}

fn migration_records(migrator: Migrator) -> Vec<Migration> {
    migrator
        .migrations
        .into_owned()
        .into_iter()
        .map(|migration| Migration {
            version: migration.version,
            up: migration.migration_type.is_up_migration(),
            sql: migration.sql.as_str().to_owned(),
        })
        .collect()
}

async fn create_history_tables(client: &sqlx::PgPool, history: &[HistoryTable<'_>]) -> Result<()> {
    sqlx::raw_sql(
        AssertSqlSafe(format!(
            "DO $application_role$ BEGIN \
               IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = {literal}) THEN \
                 CREATE ROLE {role} NOLOGIN; \
               END IF; \
             EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; \
             END $application_role$",
            literal = escape_literal(wamn_role::APP_ROLE),
            role = escape_identifier(wamn_role::APP_ROLE),
        ))
        .into_sql_str(),
    )
    .execute(client)
    .await
    .context("create the application role")?;
    sqlx::raw_sql(RECORD_HISTORY_SQL)
        .execute(client)
        .await
        .context("apply record-history.sql")?;
    sqlx::raw_sql(RECORD_HISTORY_APP_GRANTS_SQL)
        .execute(client)
        .await
        .context("apply record-history-app-grants.sql")?;
    for model in history {
        let exists: bool = sqlx::query_scalar(
            "SELECT to_regclass(format('%I.%I', $1::text, $2::text)) IS NOT NULL",
        )
        .bind(model.schema)
        .bind(model.table)
        .fetch_one(client)
        .await?;
        if !exists {
            continue;
        }
        sqlx::raw_sql(AssertSqlSafe(model.sql.as_str()).into_sql_str())
            .execute(client)
            .await
            .with_context(|| format!("create history table of {}.{}", model.schema, model.table))?;
    }
    Ok(())
}

async fn register_verification_principal(connection: &mut sqlx::PgConnection) -> Result<()> {
    let component = wamn_identity_protocol::PlatformComponent::ApplyPackage;
    sqlx::query("INSERT INTO platform.login_principal(login,principal_id) VALUES (current_user,$1) ON CONFLICT(login) DO UPDATE SET principal_id=EXCLUDED.principal_id")
        .bind(sqlx::types::Uuid::from(component.principal_id()))
        .execute(connection)
        .await?;
    Ok(())
}

/// Each column of one relation in attribute order: its name, whether the
/// primary key holds it, and whether its type is `timestamptz`.
const SELECT_RELATION_COLUMNS_SQL: &str = "\
SELECT a.attname::text, \
       COALESCE(a.attnum = ANY (i.indkey), false), \
       a.atttypid = 'pg_catalog.timestamptz'::pg_catalog.regtype \
  FROM pg_catalog.pg_attribute AS a \
  LEFT JOIN pg_catalog.pg_index AS i ON i.indrelid = a.attrelid AND i.indisprimary \
 WHERE a.attrelid = pg_catalog.to_regclass(pg_catalog.format('%I.%I', $1::text, $2::text)) \
   AND a.attnum > 0 AND NOT a.attisdropped \
 ORDER BY a.attnum";

async fn execute_on_server(server_url: &str, sql: &str) -> Result<()> {
    let client = pool(server_url, 4)
        .await
        .context("connect to the PostgreSQL server")?;
    let result = sqlx::raw_sql(AssertSqlSafe((sql).to_owned()).into_sql_str())
        .execute(&client)
        .await;
    client.close().await;
    result.map(|_| ()).with_context(|| format!("execute {sql}"))
}

/// Root of the database credential mounts in deployed processes.
pub const DATABASE_DIR: &str = "/etc/wamn/db";

/// A named credential source in the organization's namespace.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SecretRef {
    pub namespace: String,
    pub name: String,
}

/// Public connection data. Login and CA bytes come from mounted files.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionConfig {
    pub host: String,
    pub port: u16,
    pub database: String,
    pub username: wamn_role::LoginName,
    pub credentials: SecretRef,
    pub server_ca: SecretRef,
}

impl ConnectionConfig {
    /// Open options using the named credential directory below the supplied mount root.
    pub async fn mounted_options(&self, root: &Path) -> anyhow::Result<PgConnectOptions> {
        let directory = credential_directory(root, &self.credentials.name)?;
        let options = PgConnectOptions::new_without_pgpass()
            .host(&self.host)
            .port(self.port)
            .database(&self.database)
            .username(self.username.as_str())
            .ssl_root_cert(directory.join("ca.crt"))
            .ssl_mode(PgSslMode::VerifyFull);

        let cert = directory.join("tls.crt");
        let key = directory.join("tls.key");
        let has_cert = credential_exists(&cert).await?;
        let has_key = credential_exists(&key).await?;
        if !has_cert && !has_key {
            let password = read_password(&directory, &self.username).await?;
            return Ok(options.password(&password));
        }

        if !has_cert || !has_key || credential_exists(&directory.join("password")).await? {
            return Err(CertificateLayoutError { directory }.into());
        }
        // SQLx reads the files on each new connection, including after certificate renewal.
        Ok(options.ssl_client_cert(cert).ssl_client_key(key))
    }

    /// Encode configuration without including credential or CA bytes.
    pub fn to_json(&self) -> anyhow::Result<String> {
        Ok(serde_json::to_string(self)?)
    }
}

/// Read a mounted component configuration or borrow an explicit command input.
pub fn configuration(input: &str) -> anyhow::Result<Cow<'_, str>> {
    if input.starts_with('/') {
        Ok(Cow::Owned(std::fs::read_to_string(input)?))
    } else {
        Ok(Cow::Borrowed(input))
    }
}

/// Resolve public identity without loading credentials or opening a session.
pub fn identity(input: &str) -> anyhow::Result<(wamn_role::LoginName, String)> {
    let input = configuration(input)?;
    if input.trim_start().starts_with('{') {
        let config: ConnectionConfig = serde_json::from_str(&input)?;
        Ok((config.username, config.database))
    } else {
        let options: PgConnectOptions = input.parse()?;
        Ok((
            options.get_username().parse()?,
            options.get_database().context("database is absent")?.into(),
        ))
    }
}

/// Resolve one named login directory below the supplied mount root.
pub fn credential_directory(root: &Path, name: &str) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(
        !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
        "invalid database credential name"
    );
    Ok(root.join(name))
}

/// Read a mounted password after checking the login name.
pub async fn read_password(
    directory: &Path,
    expected: &wamn_role::LoginName,
) -> anyhow::Result<String> {
    let username: wamn_role::LoginName = read_text(&directory.join("username")).await?.parse()?;
    if &username != expected {
        return Err(CredentialMismatch {
            directory: directory.to_owned(),
        }
        .into());
    }
    Ok(read_text(&directory.join("password")).await?)
}

async fn credential_exists(path: &Path) -> Result<bool, CredentialReadError> {
    tokio::fs::try_exists(path)
        .await
        .map_err(|source| CredentialReadError {
            path: path.to_owned(),
            source,
        })
}

async fn read_text(path: &Path) -> Result<String, CredentialReadError> {
    tokio::fs::read_to_string(path)
        .await
        .map_err(|source| CredentialReadError {
            path: path.to_owned(),
            source,
        })
}

#[derive(Debug)]
struct CredentialReadError {
    path: PathBuf,
    source: std::io::Error,
}

impl std::fmt::Display for CredentialReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "read database credential {}", self.path.display())
    }
}

impl std::error::Error for CredentialReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

#[derive(Debug)]
struct CredentialMismatch {
    directory: PathBuf,
}

impl std::fmt::Display for CredentialMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "database credential {} names another login",
            self.directory.display()
        )
    }
}

impl std::error::Error for CredentialMismatch {}

#[derive(Debug)]
struct CertificateLayoutError {
    directory: PathBuf,
}

impl std::fmt::Display for CertificateLayoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "database credential {} requires tls.crt and tls.key together without a password file",
            self.directory.display()
        )
    }
}

impl std::error::Error for CertificateLayoutError {}

/// Resolve verified options from mounted files or explicit operator input.
pub async fn connect_options<'a>(
    input: impl Into<ConnectionInput<'a>>,
) -> anyhow::Result<PgConnectOptions> {
    let input = match input.into() {
        ConnectionInput::Options(options) => {
            let username: wamn_role::LoginName = options.get_username().parse()?;
            return Ok((*options)
                .username(username.as_str())
                .ssl_mode(PgSslMode::VerifyFull));
        }
        ConnectionInput::Configuration(input) => input,
    };
    let input = configuration(input)?;
    let options = if input.trim_start().starts_with('{') {
        let config: ConnectionConfig = serde_json::from_str(&input)?;
        config.mounted_options(Path::new(DATABASE_DIR)).await?
    } else {
        let explicit = url::Url::parse(&input)?;
        // SQLx supplies a default login when the URL omits it.
        anyhow::ensure!(
            !explicit.username().is_empty() && !explicit.path().trim_matches('/').is_empty(),
            "explicit database login and database are required"
        );
        let options: PgConnectOptions = input.parse()?;
        let username: wamn_role::LoginName = options.get_username().parse()?;
        options.username(username.as_str())
    };
    Ok(options.ssl_mode(PgSslMode::VerifyFull))
}

/// Open a dedicated session with the same verified options as pooled sessions.
pub async fn connect<'a>(
    input: impl Into<ConnectionInput<'a>>,
) -> anyhow::Result<sqlx::PgConnection> {
    Ok(sqlx::PgConnection::connect_with(&connect_options(input).await?).await?)
}

/// Construct one initially idle pool for a role and database.
pub async fn pool(input: &str, max_connections: u32) -> anyhow::Result<sqlx::PgPool> {
    Ok(sqlx::postgres::PgPoolOptions::new()
        .max_connections(max_connections)
        .connect_lazy_with(connect_options(input).await?))
}

/// Explicit connection input for component configuration or database administration.
pub enum ConnectionInput<'a> {
    Configuration(&'a str),
    Options(Box<PgConnectOptions>),
}
impl<'a> From<&'a str> for ConnectionInput<'a> {
    fn from(value: &'a str) -> Self {
        Self::Configuration(value)
    }
}

/// Select another database while retaining the explicit login and TLS references.
pub fn database_configuration(input: &str, database: &str) -> anyhow::Result<String> {
    let input = configuration(input)?;
    if input.trim_start().starts_with('{') {
        let mut config: ConnectionConfig = serde_json::from_str(&input)?;
        config.database = database.into();
        config.to_json()
    } else {
        let mut coordinate = url::Url::parse(&input)?;
        coordinate.set_path(database);
        Ok(coordinate.into())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[tokio::test]
    #[ignore = "requires: docker"]
    async fn verification_migration_accepts_a_platform_file_reference() {
        let server = wamn_test_infrastructure::containers::start_postgres()
            .await
            .unwrap();
        let database = server.create_database("tests").await.unwrap();
        let root = std::env::temp_dir().join(format!("wamn-file-reference-{}", std::process::id()));
        fs::create_dir_all(root.join("migrations")).unwrap();
        fs::write(root.join("migrations/0001_initial.sql"),
            "CREATE TABLE files_fixture.attachment (id uuid CONSTRAINT attachment_id_pkey PRIMARY KEY, file_id uuid CONSTRAINT attachment_file_id_fkey REFERENCES platform.file(id), created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP)").unwrap();
        let result = PostgresBuildDatabase
            .migrate(database.as_str(), &["files_fixture".into()], &[&root], &[])
            .await;
        fs::remove_dir_all(root).unwrap();
        result.expect("the verification database permits a platform.file foreign key");
        let client = pool(database.as_str(), 4).await.unwrap();
        let error = sqlx::raw_sql(AssertSqlSafe(("INSERT INTO files_fixture.attachment(id, file_id) VALUES (gen_random_uuid(), gen_random_uuid())").to_owned()).into_sql_str()).execute(&client).await.unwrap_err();
        assert_eq!(
            error
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .as_deref(),
            Some("23503")
        );
        client.close().await;
    }
}
