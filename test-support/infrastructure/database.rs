//! Product installs of the system and project databases on a private server,
//! with a CloudNativePG stand-in that applies the rendered chart resources.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;
use std::process::Command;

use anyhow::{Context as _, Result, bail, ensure};
use postgres_protocol::escape::{escape_identifier, escape_literal};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgConnection;
use url::Url;
use wamn_identity_protocol::workload_role::WorkloadRoleFamily;
use wamn_platform_install::provision_org::OrgLogin;
use wamn_postgres::migration_scope::MigrationDatabase;

use crate::containers::{Postgres, start_postgres};

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
    Cluster {
        spec: ClusterSpec,
    },
    Database {
        spec: DatabaseSpec,
    },
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
#[expect(
    clippy::struct_excessive_bools,
    reason = "the fields mirror the CloudNativePG role schema"
)]
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

/// Run one statement, naming it in the error.
pub async fn execute(connection: &mut PgConnection, statement: &str) -> Result<()> {
    sqlx::raw_sql(sqlx::AssertSqlSafe(statement))
        .execute(connection)
        .await
        .with_context(|| format!("run {statement}"))?;
    Ok(())
}

/// The superuser URL of `database` on the server.
pub fn database_admin_url(postgres: &Postgres, database: &str) -> Result<Url> {
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
        let groups: Vec<String> = role
            .in_roles
            .iter()
            .map(|group| escape_identifier(group))
            .collect();
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
                &format!(
                    "CREATE EXTENSION IF NOT EXISTS {}",
                    escape_identifier(&extension.name)
                ),
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
    /// The extensions that the rendered Database resources declare.
    pub extensions: Vec<String>,
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
    let connection = wamn_postgres::database::ConnectionConfig {
        host: "127.0.0.1".into(),
        port: 5432,
        database: database.clone(),
        username: owner.parse()?,
        credentials: wamn_postgres::database::SecretRef {
            namespace: org.as_str().to_owned(),
            name: wamn_registry::name::project_login_certificate(&database, owner),
        },
        server_ca: wamn_postgres::database::SecretRef {
            namespace: org.as_str().to_owned(),
            name: "ca".into(),
        },
    };
    let values = wamn_env::platform_env::database_values(
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
        &wamn_env::project_setup::privilege_sql(&database),
    )
    .await?;
    wamn_env::platform_migrations::install_project(&mut project).await?;
    wamn_env::env_schema::restore_project(
        owner_url.as_str(),
        &wamn_postgres::migrations::embedded_sources(MigrationDatabase::Project),
    )
    .await?;
    let logins = wamn_env::provision_project_env::workload::connections(&connection)?;
    wamn_env::project_setup::grant(&mut project, &logins).await?;
    // A package migration creates each model schema, then the environment grants its capture.
    execute(
        &mut project,
        &format!("CREATE SCHEMA {}", escape_identifier(MODEL_SCHEMA)),
    )
    .await?;
    execute(
        &mut project,
        &wamn_env::capture_sql::cdc_schema_sql_bundle(&[MODEL_SCHEMA.parse()?], &database),
    )
    .await?;
    Ok(Project {
        postgres,
        database,
        extensions: declared_extensions(documents),
    })
}

/// The system database installed by the product on its own server.
#[derive(Debug)]
pub struct System {
    pub postgres: Postgres,
    pub database: String,
    /// The CloudNativePG roles of the rendered system Cluster.
    pub roles: Vec<ManagedRole>,
    pub orgs: Vec<OrgLogin>,
    /// The extensions that the rendered Database resources declare.
    pub extensions: Vec<String>,
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
        org_login(&self.orgs, group)
    }
}

/// The login of `group` among the org logins that provisioning returned.
pub fn org_login<'a>(logins: &'a [OrgLogin], group: &str) -> Result<&'a OrgLogin> {
    logins
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

/// The extension names of the Database resources in `documents`.
fn declared_extensions(documents: Vec<Resource>) -> Vec<String> {
    documents
        .into_iter()
        .filter_map(|resource| match resource {
            Resource::Database { spec } => Some(spec),
            _ => None,
        })
        .flat_map(|spec| spec.extensions.into_iter().map(|extension| extension.name))
        .collect()
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
    let mut admin =
        wamn_postgres::connect(database_admin_url(&postgres, &database)?.as_str()).await?;
    wamn_platform_install::provision_system::install_system(
        &mut admin,
        postgres
            .database_url(&database, migration.as_str(), PASSWORD)?
            .as_str(),
        postgres.admin_url(),
        &"example.invalid".parse()?,
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
        extensions: declared_extensions(documents),
    })
}
