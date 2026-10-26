//! Shared setup calls for the application test environments.

use std::path::Path;

use anyhow::{Context as _, ensure};
use tokio_postgres::Client;
use wamn_control::component_admission::{self, AdmitComponentRequest};
use wamn_control::enable_cdc_project_env::{self, EnableCdcProjectEnvRequest};
use wamn_control::provision_project_env::{
    self, ProvisionProjectEnvRequest, ProvisionedRoute, read_json, secret_value,
};
use wamn_control::reconcile_package_data_access::{self, ReconcilePackageDataAccessRequest};
use wamn_control_provision::{project_env_database_name, sql, validate_instance_suffix};

/// Provision the declared project and read its emitted connection and credentials.
///
/// The caller stops its separate PAT service before applying the database setup.
pub async fn provision_project(
    args: ProvisionProjectEnvRequest,
    admin_url: &str,
) -> anyhow::Result<ProvisionedRoute> {
    let database_path = args
        .emit_database
        .clone()
        .context("set the Database output path")?;
    let route_secret = args
        .emit_operator_pat_secret
        .clone()
        .context("set the operator Secret output path")?;
    let management_secret = args.emit_management_author_pat_secret.clone();
    let database_prefix = project_env_database_name(&args.org, &args.project, &args.env, "");
    provision_project_env::provision_project_env(&args)
        .await
        .context("provision the project environment")?;
    let database = read_json(&database_path)?["spec"]["name"]
        .as_str()
        .context("Database output carries spec.name")?
        .to_owned();
    let instance = database
        .strip_prefix(&database_prefix)
        .context("the emitted Database belongs to another project environment")?;
    validate_instance_suffix(instance)
        .context("the emitted Database has an invalid instance suffix")?;

    let mut database_url =
        url::Url::parse(admin_url).context("parse the disposable cluster URL")?;
    database_url.set_path(&format!("/{database}"));
    database_url.set_query(None);
    database_url.set_fragment(None);
    Ok(ProvisionedRoute {
        database_url: database_url.into(),
        token: secret_value(&route_secret, "token")?,
        token_prefix: secret_annotation(&route_secret, "wamn.io/pat-prefix")?,
        principal_subject: secret_annotation(&route_secret, "wamn.io/principal-subject")?,
        management_token: management_secret
            .as_deref()
            .map(|path| secret_value(path, "token"))
            .transpose()?,
        management_principal_subject: management_secret
            .as_deref()
            .map(|path| secret_annotation(path, "wamn.io/principal-subject"))
            .transpose()?,
    })
}

/// Apply the emitted project setup after the PAT service has stopped.
/// `database_url` names the emitted database on the supplied admin cluster.
pub async fn apply_project_database(
    admin: &Client,
    database_url: &str,
    privilege_path: &Path,
) -> anyhow::Result<()> {
    let configuration: tokio_postgres::Config = database_url
        .parse()
        .context("parse the emitted project database URL")?;
    let database = configuration
        .get_dbname()
        .context("the emitted project URL names its database")?;
    // Commit the NOLOGIN posture before the existing bounded session drain.
    admin
        .batch_execute(&provision_project_env::role_posture_sql())
        .await
        .context("apply the project role posture")?;
    admin
        .batch_execute(&sql::drain_app_role_sessions_sql())
        .await
        .context("drain sessions of the retired shared app login")?;
    admin
        .batch_execute(&sql::create_database_named_sql(database))
        .await
        .context("create the emitted project database")?;
    // The Database CR names wamn_db_owner in spec.owner; package DDL runs as
    // that role and needs the owner's CREATE on the database.
    admin
        .batch_execute(&sql::set_database_owner_sql(database))
        .await
        .context("give the emitted project database to wamn_db_owner")?;
    admin
        .batch_execute(
            &std::fs::read_to_string(privilege_path).context("read emitted privilege SQL")?,
        )
        .await
        .context("apply emitted project privileges")?;

    Ok(())
}

fn secret_annotation(path: &Path, name: &str) -> anyhow::Result<String> {
    read_json(path)?["metadata"]["annotations"][name]
        .as_str()
        .map(str::to_owned)
        .with_context(|| format!("{} carries annotation {name}", path.display()))
}

/// Apply the installed package grant union and require replay to make no change.
pub async fn reconcile_package_data_access(
    args: ReconcilePackageDataAccessRequest,
) -> anyhow::Result<()> {
    let again = ReconcilePackageDataAccessRequest {
        packages: args.packages.clone(),
        database_url: args.database_url.clone(),
        org: args.org.clone(),
        project: args.project.clone(),
        environment: args.environment.clone(),
    };
    reconcile_package_data_access::reconcile_package_data_access(args)
        .await
        .context("converge the fresh installed-set data-access union")?;
    let again = reconcile_package_data_access::reconcile_package_data_access(again)
        .await
        .context("replay the installed-set data-access union")?;
    ensure!(
        again.is_noop(),
        "installed-set data-access reconciliation did not converge"
    );
    Ok(())
}

/// Admit one component from its exact bytes and declaration. Return its
/// admitted digest for the application's connection binding. push-package
/// pushes the component with the package artifact.
pub fn admit_component_digest(admit: AdmitComponentRequest) -> anyhow::Result<String> {
    Ok(component_admission::admit_component(admit)?
        .component_digest()
        .to_owned())
}

/// Configure the declared CDC reader and apply the existing SQL statements.
/// Apply the slot and schema bundle before the publication, as env apply does.
pub async fn configure_cdc(
    args: EnableCdcProjectEnvRequest,
    cluster: &Client,
    project: &Client,
) -> anyhow::Result<()> {
    let role_path = args
        .emit_role_sql
        .clone()
        .context("set the CDC role SQL output path")?;
    let secret_path = args
        .emit_secret
        .clone()
        .context("set the CDC Secret output path")?;
    let schemas = args.schemas.clone();
    let outcome = enable_cdc_project_env::enable_cdc_project_env(&args)
        .await
        .context("configure the declared CDC reader")?;
    let configuration: tokio_postgres::Config = secret_value(&secret_path, "url")?
        .parse()
        .context("parse the emitted CDC database URL")?;
    let role = configuration
        .get_user()
        .context("the CDC URL names its replication role")?;
    cluster
        .batch_execute(&std::fs::read_to_string(&role_path).context("read emitted CDC role SQL")?)
        .await
        .context("apply the emitted CDC role")?;
    project
        .batch_execute(&outcome.cdc_sql)
        .await
        .context("apply the declared CDC SQL")?;
    project
        .batch_execute(&sql::create_publication_schemas_sql(role, &schemas))
        .await
        .context("create the declared CDC publication")?;
    enable_cdc_project_env::record_event_reader(
        args.system_database_url
            .as_deref()
            .context("set the system database URL")?,
        &outcome.triple,
        &schemas,
        &outcome.cdc_name,
        args.secret_namespace.as_deref(),
    )
    .await
    .context("record the CDC reader")
}
