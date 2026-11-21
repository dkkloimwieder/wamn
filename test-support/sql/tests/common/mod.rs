//! Product installs on a private server, a CloudNativePG stand-in, and the
//! refusals that the installed catalog implies.

use std::borrow::Cow;
use std::fmt::Write as _;
use std::io::Write as _;
use std::num::NonZeroU32;
use std::path::Path;
use std::process::Command;

use anyhow::{Context as _, Result, bail, ensure};
use postgres_protocol::escape::{escape_identifier, escape_literal};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Connection as _, PgConnection};
use url::Url;
use wamn_platform_install::provision_org::OrgLogin;
use wamn_provision::WorkloadRoleFamily;
use wamn_provision::migrations::MigrationDatabase;
use wamn_test_infrastructure::containers::{Postgres, start_postgres};

/// The password that the stand-in gives each declared login.
///
/// CloudNativePG gives these logins a client certificate and no password. The
/// test server accepts only SCRAM passwords, so the stand-in sets one. A grant
/// refusal does not depend on how the login authenticated.
pub const PASSWORD: &str = "grant-refusal-test";

/// The model schema that a package migration creates in the project database.
pub const MODEL_SCHEMA: &str = "acme_model";

/// One rendered chart document. The stand-in applies Clusters and Databases.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind")]
enum Resource {
    Cluster { spec: ClusterSpec },
    Database { spec: DatabaseSpec },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct ClusterSpec {
    bootstrap: Bootstrap,
    managed: Managed,
}

#[derive(Debug, Deserialize)]
struct Bootstrap {
    initdb: Initdb,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Initdb {
    database: String,
    owner: String,
    #[serde(rename = "postInitSQL", default)]
    post_init_sql: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Managed {
    roles: Vec<ManagedRole>,
}

/// A CloudNativePG managed role. An unknown field fails the parse, so the
/// stand-in never drops an attribute that the chart declares.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[expect(clippy::struct_excessive_bools, reason = "the fields mirror the CloudNativePG role schema")]
pub struct ManagedRole {
    pub name: String,
    #[serde(default)]
    pub login: bool,
    #[serde(default = "inherit_default")]
    pub inherit: bool,
    #[serde(default)]
    pub replication: bool,
    pub connection_limit: Option<i64>,
    #[serde(default)]
    pub in_roles: Vec<String>,
    #[serde(default)]
    pub disable_password: bool,
}

fn inherit_default() -> bool {
    true
}

#[derive(Debug, Deserialize)]
struct DatabaseSpec {
    name: String,
    owner: String,
    #[serde(default)]
    extensions: Vec<Extension>,
}

#[derive(Debug, Deserialize)]
struct Extension {
    name: String,
}

/// Schemas that PostgreSQL owns.
const USER_SCHEMA: &str = "n.nspname <> ALL (ARRAY['pg_catalog', 'information_schema']) \
     AND n.nspname !~ '^pg_(toast|temp)'";

/// Every action outside the effective privileges of `$1`, from the catalog of the current
/// database: column reads and writes, row deletes, truncation, sequence use, function and
/// procedure calls, object creation, and role changes.
const REFUSED_ACTIONS_SQL: &str = r"
WITH relation AS (
    SELECT c.oid, c.relkind, format('%I.%I', n.nspname, c.relname) AS name
      FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
     WHERE USER_SCHEMA AND c.relkind IN ('r', 'p', 'v', 'm', 'f', 'S')
), column_privilege AS (
    SELECT r.oid, r.name, a.attname, p.privilege
      FROM relation r
      JOIN pg_catalog.pg_attribute a ON a.attrelid = r.oid AND a.attnum > 0 AND NOT a.attisdropped
     CROSS JOIN (VALUES ('SELECT'), ('INSERT'), ('UPDATE')) AS p(privilege)
     WHERE r.relkind IN ('r', 'p') OR (r.relkind IN ('v', 'm', 'f') AND p.privilege = 'SELECT')
), routine AS (
    SELECT p.oid, p.prokind, n.nspname, p.proname,
           coalesce((SELECT string_agg(
                         CASE WHEN p.provariadic <> 0 AND a.ordinality = p.pronargs
                              THEN 'VARIADIC ' ELSE '' END
                         || '(SELECT NULL::' || pg_catalog.format_type(a.type, NULL) || ')',
                         ', ' ORDER BY a.ordinality)
                       FROM unnest(p.proargtypes::oid[]) WITH ORDINALITY AS a(type, ordinality)),
                    '') AS arguments
      FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace
     WHERE USER_SCHEMA AND p.prokind IN ('f', 'p')
       AND p.prorettype <> ALL (ARRAY['trigger'::regtype, 'event_trigger'::regtype])
       -- A pseudo-typed argument has no typed NULL to call with. Each NULL is a
       -- subquery, because the planner folds a strict call on a NULL constant
       -- without the EXECUTE check.
       AND NOT EXISTS (SELECT FROM unnest(p.proargtypes::oid[]) AS a(type)
                         JOIN pg_catalog.pg_type t ON t.oid = a.type WHERE t.typtype = 'p')
)
SELECT CASE privilege
         WHEN 'SELECT' THEN format('SELECT %I FROM %s WHERE false', attname, name)
         WHEN 'INSERT' THEN format('INSERT INTO %s (%I) VALUES (DEFAULT)', name, attname)
         ELSE format('UPDATE %s SET %I = DEFAULT WHERE false', name, attname)
       END
  FROM column_privilege WHERE NOT has_column_privilege($1::name, oid, attname, privilege)
UNION ALL
SELECT format('DELETE FROM %s WHERE false', name) FROM relation
 WHERE relkind IN ('r', 'p') AND NOT has_table_privilege($1::name, oid, 'DELETE')
UNION ALL
SELECT format('TRUNCATE %s', name) FROM relation
 WHERE relkind IN ('r', 'p') AND NOT has_table_privilege($1::name, oid, 'TRUNCATE')
UNION ALL
SELECT format('CREATE TRIGGER wamn_grant_probe BEFORE UPDATE ON %s FOR EACH ROW '
              'EXECUTE FUNCTION pg_catalog.suppress_redundant_updates_trigger()', name)
  FROM relation WHERE relkind IN ('r', 'p') AND NOT has_table_privilege($1::name, oid, 'TRIGGER')
UNION ALL
-- A reference needs a referencing table. A role that may create no temporary
-- table is refused that first, and the TEMPORARY probe below checks it. A
-- foreign key also needs a unique referenced column, so only those are probed.
SELECT format('CREATE TEMPORARY TABLE wamn_grant_probe (probe %s REFERENCES %s (%I))',
              pg_catalog.format_type(a.atttypid, a.atttypmod), r.name, a.attname)
  FROM relation r
  JOIN pg_catalog.pg_index i ON i.indrelid = r.oid AND i.indisunique AND i.indnkeyatts = 1
       AND i.indpred IS NULL AND i.indexprs IS NULL
  JOIN pg_catalog.pg_attribute a ON a.attrelid = r.oid AND a.attnum = i.indkey[0]
 WHERE r.relkind IN ('r', 'p')
   AND has_database_privilege($1::name, current_database(), 'TEMPORARY')
   AND NOT has_column_privilege($1::name, r.oid, a.attname, 'REFERENCES')
UNION ALL
SELECT format('SELECT nextval(%L::regclass)', name) FROM relation
 -- CASE orders the checks: has_sequence_privilege refuses a relation of another kind.
 WHERE CASE WHEN relkind = 'S' THEN NOT has_sequence_privilege($1::name, oid, 'USAGE')
                                     AND NOT has_sequence_privilege($1::name, oid, 'UPDATE') END
UNION ALL
SELECT format('SELECT last_value FROM %s', name) FROM relation
 WHERE CASE WHEN relkind = 'S' THEN NOT has_sequence_privilege($1::name, oid, 'SELECT') END
UNION ALL
SELECT format(CASE prokind WHEN 'p' THEN 'CALL %I.%I(%s)' ELSE 'SELECT %I.%I(%s)' END,
              nspname, proname, arguments)
  FROM routine WHERE NOT has_function_privilege($1::name, oid, 'EXECUTE')
UNION ALL
SELECT format('CREATE TABLE %I.wamn_grant_probe ()', n.nspname) FROM pg_catalog.pg_namespace n
 WHERE USER_SCHEMA AND NOT has_schema_privilege($1::name, n.oid, 'CREATE')
UNION ALL
SELECT 'CREATE SCHEMA wamn_grant_probe'
 WHERE NOT has_database_privilege($1::name, current_database(), 'CREATE')
UNION ALL
SELECT 'CREATE TEMPORARY TABLE wamn_grant_probe ()'
 WHERE NOT has_database_privilege($1::name, current_database(), 'TEMPORARY')
";

/// Role changes the session cannot make. PostgreSQL checks SET ROLE against the
/// session login, so only a login actor runs them.
const REFUSED_ROLE_SQL: &str = r"
SELECT format('SET ROLE %I', rolname) FROM pg_catalog.pg_roles
 WHERE rolname <> $1::name AND NOT pg_has_role($1::name, oid, 'SET')
";

/// Run one statement, naming it in the error.
pub async fn execute(connection: &mut PgConnection, statement: &str) -> Result<()> {
    sqlx::raw_sql(sqlx::AssertSqlSafe(statement))
        .execute(connection)
        .await
        .with_context(|| format!("run {statement}"))?;
    Ok(())
}

fn database_admin_url(postgres: &Postgres, database: &str) -> Result<Url> {
    let mut url = Url::parse(postgres.admin_url())?;
    url.path_segments_mut()
        .map_err(|()| anyhow::anyhow!("the server URL has no path"))?
        .clear()
        .push(database);
    Ok(url)
}

/// Render chart templates with `helm template` and return the documents.
fn render(chart: &str, values: Option<&Value>, templates: &[&str]) -> Result<Vec<Resource>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../deploy/charts")
        .join(chart);
    let mut command = Command::new("helm");
    command.arg("template").arg("wamn").arg(&path);
    let mut values_file = None;
    if let Some(values) = values {
        let mut file = tempfile::NamedTempFile::new()?;
        serde_json::to_writer(&mut file, values)?;
        file.flush()?;
        command.arg("--values").arg(file.path());
        values_file = Some(file);
    }
    for template in templates {
        command.arg("--show-only").arg(template);
    }
    let output = command.output().context("run helm template")?;
    drop(values_file);
    ensure!(
        output.status.success(),
        "helm template {chart} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout)?;
    let mut documents = Vec::new();
    for document in serde_yaml::Deserializer::from_str(&text) {
        if let Some(resource) = Option::<Resource>::deserialize(document)? {
            documents.push(resource);
        }
    }
    Ok(documents)
}

/// The SQL that CloudNativePG runs for one managed role.
fn managed_role_sql(role: &ManagedRole) -> Result<String> {
    let flag = |enabled: bool, on: &'static str, off: &'static str| if enabled { on } else { off };
    let mut sql = format!(
        "CREATE ROLE {} {} {} {} NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS",
        escape_identifier(&role.name),
        flag(role.login, "LOGIN", "NOLOGIN"),
        flag(role.inherit, "INHERIT", "NOINHERIT"),
        flag(role.replication, "REPLICATION", "NOREPLICATION"),
    );
    if let Some(limit) = role.connection_limit {
        write!(sql, " CONNECTION LIMIT {limit}")?;
    }
    if !role.in_roles.is_empty() {
        let groups: Vec<String> = role.in_roles.iter().map(|group| escape_identifier(group)).collect();
        write!(sql, " IN ROLE {}", groups.join(", "))?;
    }
    if role.login {
        write!(sql, " PASSWORD {}", escape_literal(PASSWORD))?;
    }
    Ok(sql)
}

/// Stand in for CloudNativePG: run each Cluster bootstrap, create its managed roles,
/// then apply each Database resource.
async fn cloudnative_pg(postgres: &Postgres, resources: &[Resource]) -> Result<()> {
    let mut admin = wamn_postgres::connect(postgres.admin_url()).await?;
    for resource in resources {
        let Resource::Cluster { spec } = resource else {
            continue;
        };
        let initdb = &spec.bootstrap.initdb;
        // initdb creates the application owner as a login, then its database.
        execute(
            &mut admin,
            &format!("CREATE ROLE {} LOGIN", escape_identifier(&initdb.owner)),
        )
        .await?;
        execute(
            &mut admin,
            &format!(
                "CREATE DATABASE {} OWNER {}",
                escape_identifier(&initdb.database),
                escape_identifier(&initdb.owner)
            ),
        )
        .await?;
        for statement in &initdb.post_init_sql {
            execute(&mut admin, statement).await?;
        }
        for role in &spec.managed.roles {
            execute(&mut admin, &managed_role_sql(role)?).await?;
        }
    }
    for resource in resources {
        let Resource::Database { spec } = resource else {
            continue;
        };
        execute(
            &mut admin,
            &format!(
                "ALTER DATABASE {} OWNER TO {}",
                escape_identifier(&spec.name),
                escape_identifier(&spec.owner)
            ),
        )
        .await?;
        let mut connection =
            wamn_postgres::connect(database_admin_url(postgres, &spec.name)?.as_str()).await?;
        for extension in &spec.extensions {
            execute(
                &mut connection,
                &format!("CREATE EXTENSION IF NOT EXISTS {}", escape_identifier(&extension.name)),
            )
            .await?;
        }
    }
    Ok(())
}

/// A project database installed by the product on its own server.
#[derive(Debug)]
pub struct Project {
    pub postgres: Postgres,
    pub database: String,
}

impl Project {
    pub fn login_url(&self, login: &str) -> Result<Url> {
        self.postgres.database_url(&self.database, login, PASSWORD)
    }

    pub async fn admin(&self) -> Result<PgConnection> {
        wamn_postgres::connect(database_admin_url(&self.postgres, &self.database)?.as_str()).await
    }
}

/// Install a project database the way an environment apply does.
pub async fn project() -> Result<Project> {
    let postgres = start_postgres().await?;
    let org: wamn_registry::OrgId = "acme".parse()?;
    let database = wamn_registry::name::project_env_database_name(
        &org,
        &"billing".parse()?,
        &"dev".parse()?,
        &"a1b2c3d4".parse()?,
    );
    let owner = WorkloadRoleFamily::Migration.login_name();
    let connection = wamn_postgres::ConnectionConfig {
        host: "127.0.0.1".into(),
        port: 5432,
        database: database.clone(),
        username: owner.parse()?,
        credentials: wamn_postgres::SecretRef {
            namespace: org.as_str().to_owned(),
            name: wamn_registry::name::project_login_certificate(&database, owner),
        },
        server_ca: wamn_postgres::SecretRef {
            namespace: org.as_str().to_owned(),
            name: "ca".into(),
        },
    };
    let values = wamn_platform_install::environment::database_values(
        &connection,
        1,
        json!({"instances": 1, "cpu": "1", "memory": "1Gi", "storage": "1Gi"}),
        &json!({"image": "postgres"}),
    )?;
    let documents = render(
        "project",
        Some(&values),
        &["templates/cluster.yaml", "templates/database.yaml"],
    )?;
    cloudnative_pg(&postgres, &documents).await?;

    let owner_url = postgres.database_url(&database, owner, PASSWORD)?;
    let mut project = wamn_postgres::connect(owner_url.as_str()).await?;
    execute(
        &mut project,
        &wamn_environment::provision_project_env::privilege_sql(&database),
    )
    .await?;
    wamn_platform_install::platform_migrations::install_project(&mut project).await?;
    wamn_environment::platform_migrations::restore_project(
        owner_url.as_str(),
        &wamn_platform_install::platform_migrations::embedded_sources(MigrationDatabase::Project),
    )
    .await?;
    let logins = wamn_environment::provision_project_env::workload::connections(&connection)?;
    wamn_environment::provision_project_env::workload::grant(&mut project, &logins).await?;
    // A package migration creates each model schema, then the environment grants its capture.
    execute(
        &mut project,
        &format!("CREATE SCHEMA {}", escape_identifier(MODEL_SCHEMA)),
    )
    .await?;
    execute(
        &mut project,
        &wamn_environment::environment::cdc_reader::cdc_schema_sql_bundle(
            &[MODEL_SCHEMA.parse()?],
            &database,
        ),
    )
    .await?;
    Ok(Project { postgres, database })
}

/// The system database installed by the product on its own server.
#[derive(Debug)]
pub struct System {
    pub postgres: Postgres,
    pub database: String,
    /// The CloudNativePG roles of the rendered system Cluster.
    pub roles: Vec<ManagedRole>,
    pub orgs: Vec<OrgLogin>,
}

impl System {
    pub fn login_url(&self, login: &str, password: &str) -> Result<Url> {
        self.postgres.database_url(&self.database, login, password)
    }

    pub async fn admin(&self) -> Result<PgConnection> {
        wamn_postgres::connect(database_admin_url(&self.postgres, &self.database)?.as_str()).await
    }

    /// The declared login that is a member of `group`.
    pub fn declared_login(&self, group: &str) -> Result<&str> {
        self.roles
            .iter()
            .find(|role| role.login && role.in_roles.iter().any(|name| name == group))
            .map(|role| role.name.as_str())
            .with_context(|| format!("the system Cluster declares no login in {group}"))
    }

    /// The org login of `group` and its password.
    pub fn org_login(&self, group: &str) -> Result<&OrgLogin> {
        self.orgs
            .iter()
            .find(|login| {
                login
                    .login
                    .as_str()
                    .strip_prefix(group)
                    .is_some_and(|rest| rest.starts_with('_'))
            })
            .with_context(|| format!("no org login in {group}"))
    }
}

/// Install the system database the way the platform SQL and org provisioning Jobs do.
pub async fn system() -> Result<System> {
    let postgres = start_postgres().await?;
    let documents = render("platform", None, &["templates/system-database.yaml"])?;
    cloudnative_pg(&postgres, &documents).await?;
    let Some(spec) = documents.iter().find_map(|resource| match resource {
        Resource::Cluster { spec } => Some(spec),
        _ => None,
    }) else {
        bail!("the platform chart renders no system Cluster");
    };
    let database = spec.bootstrap.initdb.database.clone();
    let roles = spec.managed.roles.clone();
    let migration = MigrationDatabase::System.login(&database);
    let mut admin = wamn_postgres::connect(database_admin_url(&postgres, &database)?.as_str()).await?;
    wamn_platform_install::provision_system::install_system(
        &mut admin,
        postgres
            .database_url(&database, migration.as_str(), PASSWORD)?
            .as_str(),
        postgres.admin_url(),
        &"example.invalid".parse()?,
        NonZeroU32::new(8).context("a stream limit")?,
    )
    .await?;
    // Provisioning gives the Pooler login no password, because it authenticates
    // with a client certificate. The stand-in sets one, as for the declared logins.
    execute(
        &mut admin,
        &format!(
            "ALTER ROLE {} PASSWORD {}",
            escape_identifier("wamn_pooler_auth"),
            escape_literal(PASSWORD)
        ),
    )
    .await?;
    let orgs = wamn_platform_install::provision_org::create_org_logins(
        &mut admin,
        &"acme".parse()?,
        &database,
    )
    .await?;
    Ok(System {
        postgres,
        database,
        roles,
        orgs,
    })
}

/// Log in as `login`, and check that the server sees that login.
pub async fn log_in(url: &Url) -> Result<PgConnection> {
    let mut connection = wamn_postgres::connect(url.as_str()).await?;
    let current: String = sqlx::query_scalar("SELECT current_user::text")
        .fetch_one(&mut connection)
        .await?;
    ensure!(current == url.username(), "logged in as {current}");
    Ok(connection)
}

/// The SQLSTATE of a database error.
pub fn sqlstate(error: &sqlx::Error) -> Option<Cow<'_, str>> {
    error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
}

/// Run each action as `actor` in a rolled-back transaction, and check that the server refuses
/// each one with SQLSTATE 42501. Return the number of actions.
pub async fn assert_refused(actor: &mut PgConnection, label: &str, actions: &[String]) -> Result<usize> {
    let mut failures = Vec::new();
    for action in actions {
        let mut transaction = actor.begin().await?;
        let result = sqlx::raw_sql(sqlx::AssertSqlSafe(action.as_str()))
            .execute(&mut *transaction)
            .await;
        transaction.rollback().await?;
        match result {
            Ok(_) => failures.push(format!("{action}: accepted")),
            Err(error) if sqlstate(&error).as_deref() == Some("42501") => {}
            Err(error) => failures.push(format!("{action}: {error}")),
        }
    }
    ensure!(
        failures.is_empty(),
        "{label} was not refused:\n{}",
        failures.join("\n")
    );
    Ok(actions.len())
}

/// The actions outside the effective privileges of `role` in the database of `admin`.
///
/// The set is empty for a role that owns every object, such as the system migration login.
pub async fn refused_actions(admin: &mut PgConnection, role: &str) -> Result<Vec<String>> {
    let actions: Vec<String> =
        sqlx::query_scalar(sqlx::AssertSqlSafe(REFUSED_ACTIONS_SQL.replace("USER_SCHEMA", USER_SCHEMA)))
            .bind(role)
            .fetch_all(&mut *admin)
            .await?;
    Ok(actions)
}

/// Check every refusal of a login: the catalog-derived actions, role changes,
/// self-elevation and databases it may not connect to. Return the number checked.
pub async fn assert_login_refusals(
    admin: &mut PgConnection,
    postgres: &Postgres,
    login: &str,
    password: &str,
    actor: &mut PgConnection,
    contract: &[String],
) -> Result<usize> {
    let mut actions = refused_actions(admin, login).await?;
    let roles: Vec<String> = sqlx::query_scalar(REFUSED_ROLE_SQL)
        .bind(login)
        .fetch_all(&mut *admin)
        .await?;
    actions.extend(roles);
    for attribute in ["SUPERUSER", "BYPASSRLS", "CREATEROLE", "CREATEDB", "REPLICATION"] {
        actions.push(format!("ALTER ROLE {} {attribute}", escape_identifier(login)));
    }
    actions.extend_from_slice(contract);
    let mut checked = assert_refused(actor, login, &actions).await?;

    let databases: Vec<String> = sqlx::query_scalar(
        "SELECT datname::text FROM pg_catalog.pg_database \
          WHERE datallowconn AND NOT has_database_privilege($1::name, oid, 'CONNECT')",
    )
    .bind(login)
    .fetch_all(&mut *admin)
    .await?;
    for database in databases {
        let url = postgres.database_url(&database, login, password)?;
        match wamn_postgres::connect(url.as_str()).await {
            Ok(_) => bail!("{login} connected to {database}"),
            Err(error) => {
                let code = error.downcast_ref::<sqlx::Error>().and_then(sqlstate);
                ensure!(
                    code.as_deref() == Some("42501"),
                    "{login} connecting to {database}: {error:#}"
                );
            }
        }
        checked += 1;
    }
    Ok(checked)
}

/// The role holds no elevated attribute and reaches no elevated role.
pub async fn assert_unelevated(admin: &mut PgConnection, role: &str, replication: bool) -> Result<()> {
    let (elevated, replicates, reaches): (bool, bool, bool) = sqlx::query_as(
        "SELECT rolsuper OR rolbypassrls OR rolcreatedb OR rolcreaterole, rolreplication, \
                EXISTS (SELECT FROM pg_catalog.pg_roles r \
                         WHERE r.oid <> l.oid \
                           AND (r.rolsuper OR r.rolbypassrls OR r.rolcreatedb OR r.rolcreaterole) \
                           AND pg_has_role(l.oid, r.oid, 'MEMBER')) \
           FROM pg_catalog.pg_roles l WHERE rolname = $1::name",
    )
    .bind(role)
    .fetch_one(&mut *admin)
    .await?;
    ensure!(!elevated, "{role} holds an elevated attribute");
    ensure!(replicates == replication, "{role} replication is {replicates}");
    ensure!(!reaches, "{role} is a member of an elevated role");
    Ok(())
}

/// The role can pass on no privilege, through itself, its groups or PUBLIC.
pub async fn assert_no_grant_options(admin: &mut PgConnection, role: &str) -> Result<()> {
    let grantable: Vec<String> = sqlx::query_scalar(
        "WITH privilege AS (
           SELECT c.oid::regclass::text AS object, a.* FROM pg_catalog.pg_class c CROSS JOIN LATERAL aclexplode(c.relacl) a
           UNION ALL SELECT c.attrelid::regclass::text, a.* FROM pg_catalog.pg_attribute c CROSS JOIN LATERAL aclexplode(c.attacl) a
           UNION ALL SELECT n.nspname::text, a.* FROM pg_catalog.pg_namespace n CROSS JOIN LATERAL aclexplode(n.nspacl) a
           UNION ALL SELECT p.oid::regprocedure::text, a.* FROM pg_catalog.pg_proc p CROSS JOIN LATERAL aclexplode(p.proacl) a
           UNION ALL SELECT d.datname::text, a.* FROM pg_catalog.pg_database d CROSS JOIN LATERAL aclexplode(d.datacl) a)
         SELECT DISTINCT object FROM privilege
          WHERE is_grantable AND (grantee = 0 OR pg_has_role($1::name, grantee, 'USAGE'))",
    )
    .bind(role)
    .fetch_all(&mut *admin)
    .await?;
    ensure!(grantable.is_empty(), "{role} can pass on privileges: {grantable:?}");
    Ok(())
}

/// The relations that `role` can write, other than its own transaction identity, outside `schema`.
pub async fn writes_outside(admin: &mut PgConnection, role: &str, schema: Option<&str>) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT c.oid::regclass::text FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
          WHERE {USER_SCHEMA} AND n.nspname IS DISTINCT FROM $2 AND c.relkind IN ('r', 'p', 'v', 'm', 'f')
            AND (has_table_privilege($1::name, c.oid, 'UPDATE, DELETE, TRUNCATE, REFERENCES, TRIGGER')
                 OR has_any_column_privilege($1::name, c.oid, 'UPDATE, REFERENCES')
                 OR (c.oid <> 'platform.tx_identity'::regclass
                     AND (has_table_privilege($1::name, c.oid, 'INSERT')
                          OR has_any_column_privilege($1::name, c.oid, 'INSERT'))))"
    )))
    .bind(role)
    .bind(schema)
    .fetch_all(&mut *admin)
    .await?)
}

/// The logins mapped to platform principals that `role` reaches by membership.
pub async fn mapped_logins_reached(admin: &mut PgConnection, role: &str) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT target.rolname::text FROM pg_catalog.pg_roles target
           JOIN platform.login_principal mapped ON mapped.login = target.rolname
          WHERE target.rolname <> $1::name AND pg_has_role($1::name, target.oid, 'MEMBER')",
    )
    .bind(role)
    .fetch_all(&mut *admin)
    .await?)
}

/// Every login mapped to a principal is restricted and maps to a platform component.
pub async fn assert_mapped_logins_are_platform_components(admin: &mut PgConnection) -> Result<()> {
    let rows: Vec<(String, wamn_identity_protocol::PrincipalId, bool)> = sqlx::query_as(
        "SELECT p.login::text, p.principal_id,
                NOT (r.rolsuper OR r.rolbypassrls OR r.rolcreatedb OR r.rolcreaterole OR r.rolreplication)
           FROM platform.login_principal p JOIN pg_catalog.pg_roles r ON r.rolname = p.login",
    )
    .fetch_all(&mut *admin)
    .await?;
    ensure!(!rows.is_empty(), "the install maps no login to a principal");
    for (login, principal, restricted) in rows {
        ensure!(restricted, "mapped login {login} can bypass database privileges");
        ensure!(
            wamn_identity_protocol::PlatformComponent::ALL
                .into_iter()
                .any(|component| component.principal_id() == principal),
            "{login} maps to an unknown principal {principal}"
        );
    }
    Ok(())
}

/// A role with only PUBLIC access executes no security definer.
pub async fn assert_public_executes_no_definer(admin: &mut PgConnection) -> Result<usize> {
    const PUBLIC_ONLY: &str = "wamn_public_probe";
    execute(admin, &format!("CREATE ROLE {PUBLIC_ONLY} NOLOGIN")).await?;
    let executable: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT p.oid::regprocedure::text FROM pg_catalog.pg_proc p
           JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace
          WHERE {USER_SCHEMA} AND p.prosecdef AND has_function_privilege($1::name, p.oid, 'EXECUTE')"
    )))
    .bind(PUBLIC_ONLY)
    .fetch_all(&mut *admin)
    .await?;
    ensure!(executable.is_empty(), "PUBLIC executes security definers: {executable:?}");
    let actions = refused_actions(admin, PUBLIC_ONLY).await?;
    execute(admin, &format!("SET ROLE {PUBLIC_ONLY}")).await?;
    let checked = assert_refused(admin, PUBLIC_ONLY, &actions).await;
    execute(admin, "RESET ROLE").await?;
    checked
}
