// The unchanged Acme overlay of a candidate release against a fresh
// installation of its base.

use std::path::Path;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use wamn_catalog::schema::ForeignKeyColumn;
use wamn_digest::bytes_sha256;
use wamn_package_control::apply_package::{self, ApplyPackageRequest};
use wamn_schema_introspection::ir::{Constraint, ForeignKeyAction, postgres_type};

use super::identity;

// This projection observes installed facts, including granted schemas. It does
// not admit authoring input or construct a complete authoring catalog identity.
async fn observe_installed_contract(
    project: &tokio_postgres::Client,
    required: &Value,
) -> anyhow::Result<Value> {
    let mut tables = Vec::new();
    for table in required["tables"]
        .as_array()
        .context("required tables must be an array")?
    {
        let schema = table["schema"]
            .as_str()
            .context("required schema must be a name")?;
        let name = table["table"]
            .as_str()
            .context("required table must be a name")?;
        let rows = project
            .query(wamn_schema_control::select_schema_columns_sql(), &[&schema])
            .await?;
        let mut columns = Vec::new();
        for row in rows.iter().filter(|row| row.get::<_, &str>(0) == name) {
            columns.push(json!({
                "name":row.get::<_, &str>(1),
                "type":postgres_type(row.get::<_, &str>(4))?.as_str(),
                "nullable":!row.get::<_, bool>(2),
            }));
        }
        ensure!(
            !columns.is_empty(),
            "unsatisfied overlay schema requirement: {schema}.{name}"
        );
        let mut constraints = Vec::new();
        for constraint in table["constraints"]
            .as_array()
            .context("required constraints must be an array")?
        {
            let constraint_name = constraint["name"]
                .as_str()
                .context("required constraint must be a name")?;
            // Ordered key pairs and pg_get_expr(false) match the existing IR
            // representation; rendered pg_get_constraintdef text does not.
            let row = project.query_opt(
                "SELECT con.contype::text, \
                    ARRAY(SELECT att.attname::text FROM unnest(con.conkey) WITH ORDINALITY key(attnum, position) \
                          JOIN pg_catalog.pg_attribute att ON att.attrelid = con.conrelid AND att.attnum = key.attnum \
                          ORDER BY key.position), \
                    referenced_namespace.nspname::text, referenced_relation.relname::text, \
                    ARRAY(SELECT att.attname::text FROM unnest(con.confkey) WITH ORDINALITY key(attnum, position) \
                          JOIN pg_catalog.pg_attribute att ON att.attrelid = con.confrelid AND att.attnum = key.attnum \
                          ORDER BY key.position), \
                    con.confupdtype::text, con.confdeltype::text, \
                    pg_catalog.pg_get_expr(con.conbin, con.conrelid, false), con.conenforced, con.convalidated, con.condeferrable, con.condeferred \
                 FROM pg_catalog.pg_constraint con \
                 JOIN pg_catalog.pg_class relation ON relation.oid = con.conrelid \
                 JOIN pg_catalog.pg_namespace namespace ON namespace.oid = relation.relnamespace \
                 LEFT JOIN pg_catalog.pg_class referenced_relation ON referenced_relation.oid = con.confrelid \
                 LEFT JOIN pg_catalog.pg_namespace referenced_namespace ON referenced_namespace.oid = referenced_relation.relnamespace \
                 WHERE namespace.nspname = $1 AND relation.relname = $2 AND con.conname = $3 AND relation.relkind = 'r'",
                &[&schema, &name, &constraint_name],
            ).await?.with_context(|| format!("unsatisfied overlay schema requirement: {schema}.{name}.{constraint_name}"))?;
            ensure!(
                row.get::<_, bool>(8)
                    && row.get::<_, bool>(9)
                    && !row.get::<_, bool>(10)
                    && !row.get::<_, bool>(11),
                "unsatisfied overlay schema requirement: {schema}.{name}.{constraint_name} must be enforced, validated, and immediate"
            );
            let local: Vec<String> = row.get(1);
            let observed = match row.get::<_, &str>(0) {
                "p" => Constraint::primary_key(constraint_name, local)?,
                "u" => Constraint::unique(constraint_name, local)?,
                "c" => Constraint::check(constraint_name, row.get::<_, String>(7))?,
                "f" => {
                    let referenced: Vec<String> = row.get(4);
                    ensure!(
                        local.len() == referenced.len(),
                        "foreign key column pairing differs"
                    );
                    Constraint::foreign_key(
                        constraint_name,
                        local
                            .into_iter()
                            .zip(referenced)
                            .map(|(local, remote)| ForeignKeyColumn::new(local, remote))
                            .collect(),
                        row.get::<_, String>(2),
                        row.get::<_, String>(3),
                        foreign_key_action(row.get(5))?,
                        foreign_key_action(row.get(6))?,
                    )?
                }
                type_ => anyhow::bail!(
                    "unsatisfied overlay schema requirement: {schema}.{name}.{constraint_name} has type {type_}"
                ),
            };
            constraints.push(serde_json::to_value(observed)?);
        }
        tables
            .push(json!({"schema":schema,"name":name,"columns":columns,"constraints":constraints}));
    }
    Ok(json!({"tables":tables}))
}

fn foreign_key_action(action: &str) -> anyhow::Result<ForeignKeyAction> {
    match action {
        "a" => Ok(ForeignKeyAction::NoAction),
        "r" => Ok(ForeignKeyAction::Restrict),
        "c" => Ok(ForeignKeyAction::Cascade),
        "n" => Ok(ForeignKeyAction::SetNull),
        "d" => Ok(ForeignKeyAction::SetDefault),
        other => anyhow::bail!("unsupported observed foreign key action {other}"),
    }
}

fn required_contract_observation(required: &Value, observed: &Value) -> anyhow::Result<Value> {
    let tables = required["tables"]
        .as_array()
        .context("overlay package contract must carry required tables")?;
    ensure!(
        !tables.is_empty(),
        "overlay schema observation must execute at least one requirement"
    );
    let live = observed["tables"]
        .as_array()
        .context("live catalog must carry tables")?;
    let mut field_count = 0;
    let mut constraint_count = 0;
    for table in tables {
        let schema = table["schema"]
            .as_str()
            .context("required schema must be a name")?;
        let name = table["table"]
            .as_str()
            .context("required table must be a name")?;
        let actual = live
            .iter()
            .find(|item| item["schema"] == schema && item["name"] == name)
            .with_context(|| format!("unsatisfied overlay schema requirement: {schema}.{name}"))?;
        let columns = actual["columns"]
            .as_array()
            .context("live relation must carry columns")?;
        for field in table["fields"]
            .as_array()
            .context("required relation must carry fields")?
        {
            let field_name = field["name"]
                .as_str()
                .context("required field must be a name")?;
            let column = columns
                .iter()
                .find(|item| item["name"] == field_name)
                .with_context(|| {
                    format!("unsatisfied overlay schema requirement: {schema}.{name}.{field_name}")
                })?;
            ensure!(
                column["type"] == field["type"] && column["nullable"] == field["nullable"],
                "unsatisfied overlay schema requirement: {schema}.{name}.{field_name} expected type={} nullable={}, observed type={} nullable={}",
                field["type"],
                field["nullable"],
                column["type"],
                column["nullable"]
            );
            field_count += 1;
        }
        let constraints = actual["constraints"]
            .as_array()
            .context("live relation must carry constraints")?;
        for constraint in table["constraints"]
            .as_array()
            .context("required relation must carry constraints")?
        {
            let constraint_name = constraint["name"]
                .as_str()
                .context("required constraint must be a name")?;
            let actual = constraints
                .iter()
                .find(|item| item["name"] == constraint_name)
                .with_context(|| {
                    format!(
                        "unsatisfied overlay schema requirement: {schema}.{name}.{constraint_name}"
                    )
                })?;
            let mut definition = actual
                .as_object()
                .context("live constraint must be an object")?
                .clone();
            definition.remove("name");
            ensure!(
                Value::Object(definition) == constraint["definition"],
                "unsatisfied overlay schema requirement: {schema}.{name}.{constraint_name} definition differs"
            );
            constraint_count += 1;
        }
    }
    ensure!(
        field_count > 0 && constraint_count > 0,
        "overlay schema observation must execute field and constraint requirements"
    );
    Ok(
        json!({"tables":tables.len(),"fields":field_count,"constraints":constraint_count,"result":"satisfied"}),
    )
}

/// The schema the package at `root` needs, as its build wrote it.
fn required_schema_contract(root: &Path) -> anyhow::Result<Value> {
    wamn_schema_generator::build_record::require_current(root)?;
    let relative = Path::new(wamn_schema_generator::REQUIRED_SCHEMA_CONTRACT_PATH)
        .strip_prefix("generated")?;
    let path = wamn_schema_generator::output_root(root).join(relative);
    Ok(serde_json::from_slice(
        &std::fs::read(&path).with_context(|| format!("read {}", path.display()))?,
    )?)
}

/// A PostgreSQL 18 server of its own, prepared by deploy/sql/postgres-init.sql,
/// which creates shared roles and the database wamn, applied as the superuser
/// postgres.
async fn production_server() -> anyhow::Result<wamn_test_infrastructure::postgres::OwnedPostgres> {
    let postgres = wamn_test_infrastructure::postgres::start(&[])?;
    let config: tokio_postgres::Config = postgres.database("postgres")?.url().parse()?;
    let password = config
        .get_password()
        .context("the test server URL carries the superuser password")?;
    let init = tokio::process::Command::new("/usr/lib/postgresql/18/bin/psql")
        .env_clear()
        .env("PGHOST", "127.0.0.1")
        .env("PGPORT", config.get_ports()[0].to_string())
        .env("PGUSER", "postgres")
        .env("PGPASSWORD", std::str::from_utf8(password)?)
        .env("PGDATABASE", "postgres")
        .args(["-X", "-q", "-v", "ON_ERROR_STOP=1", "-f"])
        .arg(super::repository_root()?.join("deploy/sql/postgres-init.sql"))
        .output()
        .await
        .context("run psql for deploy/sql/postgres-init.sql")?;
    ensure!(
        init.status.success(),
        "deploy/sql/postgres-init.sql failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    Ok(postgres)
}

/// Install `packages`, bases first, into the new database `name` of `server`
/// through the production boundary, and connect to it.
async fn install_fresh(
    server: &mut wamn_test_infrastructure::postgres::OwnedPostgres,
    name: &str,
    packages: &[std::path::PathBuf],
) -> anyhow::Result<(
    String,
    std::sync::Arc<tokio_postgres::Client>,
    tokio::task::JoinHandle<()>,
)> {
    let url = server.create_database(name)?.url().to_owned();
    let (project, connection_task) = super::connect(&url).await?;
    let fresh = project.query_one("SELECT current_setting('server_version_num')::integer, current_database()::text, to_regnamespace('receiving') IS NULL AND to_regnamespace('catalog') IS NULL", &[]).await?;
    ensure!(
        (180_000..190_000).contains(&fresh.get::<_, i32>(0)) && fresh.get::<_, bool>(2),
        "observer test requires fresh PostgreSQL 18"
    );
    project
        .batch_execute(wamn_control_provision::sql::ensure_db_owner_role_sql())
        .await?;
    project
        .batch_execute(&wamn_control_provision::sql::set_database_owner_sql(
            fresh.get(1),
        ))
        .await?;
    super::install_journey_platform_base_grants(&project, super::PLATFORM_DOMAIN).await?;
    let roots = packages
        .iter()
        .map(|package| {
            Ok(wamn_package_control::apply_package::PackageRoot {
                path: package.clone(),
                artifact_digest: Some(
                    wamn_test_infrastructure::fixture_digest::fixture_artifact_digest(package)?,
                ),
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    for package in &roots {
        apply_package::apply_package(ApplyPackageRequest {
            component_source: wamn_package_control::apply_package::component_source_from_env(None)
                .unwrap(),
            changes: wamn_broker::Publisher::default(),
            package: package.clone(),
            database_url: url.clone(),
            org: identity().org.clone(),
            project: identity().project.clone(),
            environment: identity().environment.clone(),
        })
        .await?;
    }
    wamn_environment_control::reconcile_package_data_access::reconcile_package_data_access(
        wamn_environment_control::reconcile_package_data_access::ReconcilePackageDataAccessRequest {
            packages: roots,
            database_url: url.clone(),
            org: identity().org.clone(),
            project: identity().project.clone(),
            environment: identity().environment.clone(),
        },
    )
    .await?;
    Ok((url, project, connection_task))
}
