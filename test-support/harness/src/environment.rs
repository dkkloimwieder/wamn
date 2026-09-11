//! Shared setup calls for the application test environments.

use std::path::Path;

use anyhow::{Context as _, ensure};
use tokio_postgres::Client;
use wamn_control_provision::{project_env_database_name, sql, validate_instance_suffix};
use wamn_ctl::dev::environment::{ProvisionedRoute, read_json, secret_value};
use wamn_ctl::enable_cdc_project_env::{self, EnableCdcProjectEnvArgs};
use wamn_ctl::provision_project_env::{self, ProvisionProjectEnvArgs};
use wamn_ctl::push_component::{
    self, AdmitComponentArgs, PublishAdmittedComponentArgs, PushComponentArgs,
};
use wamn_ctl::reconcile_package_data_access::{self, ReconcilePackageDataAccessArgs};

/// Provision the declared project and apply its emitted database setup.
///
/// The caller owns the separate PAT service and supplies its existing arguments.
/// `admin` and `admin_url` address the same disposable PostgreSQL cluster.
pub async fn provision_project(
    args: ProvisionProjectEnvArgs,
    admin: &Client,
    admin_url: &str,
) -> anyhow::Result<ProvisionedRoute> {
    let database_path = args
        .emit_database
        .clone()
        .context("set the Database output path")?;
    let privilege_path = args
        .emit_privilege_sql
        .clone()
        .context("set the privilege SQL output path")?;
    let route_secret = args
        .emit_route_caller_pat_secret
        .clone()
        .context("set the route caller Secret output path")?;
    let management_secret = args.emit_management_author_pat_secret.clone();
    let app_password = args
        .app_password
        .clone()
        .context("supply the provisioning password argument")?;
    let database_prefix = project_env_database_name(
        args.org.as_deref().context("supply the organization")?,
        args.project.as_deref().context("supply the project")?,
        args.env.as_deref().context("supply the environment")?,
        "",
    );
    provision_project_env::run(args)
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

    // Commit the NOLOGIN posture before the existing bounded session drain.
    admin
        .batch_execute(&provision_project_env::role_posture_sql(&app_password))
        .await
        .context("apply the project role posture")?;
    admin
        .batch_execute(&sql::drain_app_role_sessions_sql())
        .await
        .context("drain sessions of the retired shared app login")?;
    admin
        .batch_execute(&sql::create_database_named_sql(&database))
        .await
        .context("create the emitted project database")?;
    admin
        .batch_execute(
            &std::fs::read_to_string(&privilege_path).context("read emitted privilege SQL")?,
        )
        .await
        .context("apply emitted project privileges")?;

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

fn secret_annotation(path: &Path, name: &str) -> anyhow::Result<String> {
    read_json(path)?["metadata"]["annotations"][name]
        .as_str()
        .map(str::to_owned)
        .with_context(|| format!("{} carries annotation {name}", path.display()))
}

/// Apply the installed package grant union and require replay to make no change.
pub async fn reconcile_package_data_access(
    args: ReconcilePackageDataAccessArgs,
) -> anyhow::Result<()> {
    let again = ReconcilePackageDataAccessArgs {
        packages: args.packages.clone(),
        database_url: args.database_url.clone(),
        tenant: args.tenant.clone(),
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

/// Publish one component through the existing admission and projection calls.
/// Return its admitted digest directly for the application's connection binding.
pub async fn push_component(args: PushComponentArgs) -> anyhow::Result<String> {
    let PushComponentArgs {
        package,
        component_bytes,
        declaration,
        artifact_base,
        registry_auth_file,
        insecure_registry,
        admitted_platform_packages,
        project_database_url,
        control_database_url,
    } = args;
    let admission = push_component::admit_component(AdmitComponentArgs {
        package,
        component_bytes,
        declaration,
        admitted_platform_packages,
    })?;
    push_component::project_admitted_component_for_verification(&admission, &project_database_url)
        .await?;
    push_component::publish_admitted_component(
        &admission,
        PublishAdmittedComponentArgs {
            artifact_base,
            registry_auth_file,
            insecure_registry,
            project_database_url,
            control_database_url,
        },
    )
    .await?;
    Ok(admission.component_digest().to_owned())
}

/// Configure the declared CDC reader and apply the existing SQL statements.
/// The publication commits before slot creation, as in the psql file caller.
pub async fn configure_cdc(
    args: EnableCdcProjectEnvArgs,
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
    let schema = args.schema.clone();
    enable_cdc_project_env::run(args)
        .await
        .context("configure the declared CDC reader")?;
    let configuration: tokio_postgres::Config = secret_value(&secret_path, "url")?
        .parse()
        .context("parse the emitted CDC database URL")?;
    let role = configuration
        .get_user()
        .context("the CDC URL names its replication role")?;
    let database = configuration
        .get_dbname()
        .context("the CDC URL names its project database")?;
    cluster
        .batch_execute(&std::fs::read_to_string(&role_path).context("read emitted CDC role SQL")?)
        .await
        .context("apply the emitted CDC role")?;
    project
        .batch_execute(&sql::ensure_schema_sql(&schema))
        .await
        .context("ensure the CDC schema")?;
    project
        .batch_execute(&sql::create_publication_sql(role, &schema))
        .await
        .context("create the declared CDC publication")?;
    project
        .batch_execute(&sql::create_failover_slot_sql(role))
        .await
        .context("create the declared CDC failover slot")?;
    project
        .batch_execute(&sql::ensure_entity_map_sql(&schema))
        .await
        .context("ensure the CDC entity map")?;
    project
        .batch_execute(&sql::ensure_cdc_exclusion_map_sql(&schema))
        .await
        .context("ensure the CDC exclusion map")?;
    project
        .batch_execute(&sql::grant_replication_access_sql(database, role, &schema))
        .await
        .context("apply the declared CDC grants")?;
    Ok(())
}
