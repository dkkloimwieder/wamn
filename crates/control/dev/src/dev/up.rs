//! Provision a disposable development environment and write its configuration.
use super::environment::{DevEnvironmentInputs, connect, provision, write_dev_config};
use anyhow::Context as _;
use std::fs::Permissions;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;

/// Explicit inputs for a disposable development environment.
#[derive(Debug)]
pub struct DevUpRequest {
    pub system_database_url: String,
    pub root: PathBuf,
    /// The one local NATS and the development certificate.
    pub nats: wamn_broker::nats::PlatformNats,
    pub stream_replicas: usize,
    pub dup_window_secs: u64,
    pub otel_exporter_otlp_endpoint: String,
    pub route_host: String,
    pub platform_domain: String,
    pub flow_http_component: PathBuf,
    pub host_binary: PathBuf,
    /// The built platform UI directory served beside application bundles.
    pub platform_ui: PathBuf,
    pub packages: Vec<PathBuf>,
    /// The built `wamn-cdc-reader` the loop runs beside the host.
    pub cdc_reader_binary: PathBuf,
    /// The built platform materializer the loop runs beside the release.
    pub materializer_component: PathBuf,
}

/// Provision the environment and return the configuration file.
pub async fn provision_environment(mut args: DevUpRequest) -> anyhow::Result<PathBuf> {
    let document = super::session::read_environment(&args.root.join("environment.k"))?;
    // Settled before a single credential is minted: an environment is
    // expensive to stand up.
    wamn_control_provision::validate_platform_domain(&args.platform_domain)?;
    super::control::preflight(&args)?;

    std::fs::create_dir_all(&args.root)
        .with_context(|| format!("create the environment directory {}", args.root.display()))?;
    // Minted PATs and credential URLs land here, so the directory is the wall.
    std::fs::set_permissions(&args.root, Permissions::from_mode(0o700))
        .with_context(|| format!("restrict {} to its owner", args.root.display()))?;

    args.root = args.root.canonicalize()?;

    let mut package_sources = Vec::with_capacity(args.packages.len());
    for package in &args.packages {
        package_sources.push(
            package
                .canonicalize()
                .with_context(|| format!("resolve package source {}", package.display()))?,
        );
    }
    let local_artifacts = super::config::LocalArtifacts {
        directory: args.root.canonicalize()?.join("local-artifacts"),
        flow_http_component: args.flow_http_component.canonicalize().with_context(|| {
            format!(
                "resolve local flow-http component {}",
                args.flow_http_component.display()
            )
        })?,
        materializer_component: args
            .materializer_component
            .canonicalize()
            .with_context(|| {
                format!(
                    "resolve local materializer component {}",
                    args.materializer_component.display()
                )
            })?,
    };
    let mut inputs = DevEnvironmentInputs {
        local_artifacts,
        host_binary: args.host_binary.clone(),
        nats: args.nats.clone(),
        stream_replicas: args.stream_replicas,
        dup_window_secs: args.dup_window_secs,
        otel_exporter_otlp_endpoint: args.otel_exporter_otlp_endpoint.clone(),
        route_host: args.route_host.clone(),
        platform_domain: args.platform_domain.clone(),
        package_sources,
        cdc_reader: None,
    };

    let mut admin = connect(&args.system_database_url).await?;
    let environment = provision(
        &args.system_database_url,
        &mut admin,
        &args.root,
        &inputs.platform_domain,
        &document,
    )
    .await?;
    inputs.cdc_reader = Some(
        enable_cdc(
            &args,
            &inputs,
            &mut admin,
            &environment.route.database_url,
            &environment.identity,
        )
        .await?,
    );
    let config = write_dev_config(
        &args.root,
        &args.system_database_url,
        &environment.route,
        &environment.credentials,
        &inputs,
        &environment.identity,
    )?;

    let web = super::control::start(&args, &environment).await;
    if let Err(error) = web {
        environment.issuer.stop().await?;
        return Err(error);
    }
    environment.issuer.retain(&args.root.canonicalize()?)?;
    Ok(config)
}

/// Write private local process input without creating a credential backend.
pub(super) fn write_private(path: &std::path::Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(Permissions::from_mode(0o600))?;
    file.write_all(bytes)?;
    Ok(())
}

async fn enable_cdc(
    args: &DevUpRequest,
    inputs: &DevEnvironmentInputs,
    admin: &mut sqlx::PgConnection,
    target_url: &str,
    identity: &super::activation::DevActivationIdentity,
) -> anyhow::Result<super::config::CdcReader> {
    let triple = wamn_registry::Triple::new(
        &identity.org,
        identity.environment.clone(),
        &identity.project,
    );
    let manifests = inputs
        .package_sources
        .iter()
        .map(|root| super::config::read_package_manifest(root))
        .collect::<Result<Vec<_>, _>>()?;
    let schemas = manifests
        .iter()
        .flat_map(|manifest| manifest.models.values().map(|model| model.schema.clone()))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let instance = wamn_environment_control::provision_project_env::read_project_env_instance(
        &args.system_database_url,
        &triple,
    )
    .await?;
    let name = wamn_control_provision::cdc_object_name(
        &identity.org,
        &identity.project,
        &identity.environment,
        &instance,
    );
    let role = name.clone();
    let cdc_url = super::credentials::login(target_url, &role, None, "").await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "ALTER ROLE {} REPLICATION",
        postgres_protocol::escape::escape_identifier(&role)
    )))
    .execute(&mut *admin)
    .await?;
    let consumers = wamn_broker::events::entity_consumers(
        &triple,
        manifests
            .iter()
            .flat_map(wamn_compiler::manifest::PackageManifest::entity_pairs),
    );
    let js = async_nats::jetstream::new(inputs.nats.connect().await?);
    wamn_broker::streams::provision(
        &js,
        &triple,
        inputs.stream_replicas,
        std::time::Duration::from_secs(inputs.dup_window_secs),
        &consumers,
    )
    .await?;
    let (_, database) = wamn_postgres::identity(target_url)?;
    let mut sql = wamn_environment_control::enable_cdc_project_env::cdc_sql_bundle(
        &schemas, &name, &database,
    );
    for model in manifests
        .iter()
        .flat_map(|manifest| manifest.models.values())
    {
        sql.push_str(
            &wamn_environment_control::replica_identity::alter_replica_identity_sql(
                &model.schema,
                &model.table,
                wamn_environment_control::replica_identity::ReplicaIdentity::Full,
            ),
        );
        sql.push(';');
    }
    sql.push_str(&wamn_control_provision::sql::create_publication_schemas_sql(&name, &schemas));
    let cdc_sql_file = args.root.join("cdc.sql");
    write_private(&cdc_sql_file, sql.as_bytes())?;
    wamn_environment_control::enable_cdc_project_env::record_event_reader(
        &args.system_database_url,
        &triple,
        &schemas,
        &name,
        None,
    )
    .await?;
    let coordinate = super::environment::JourneyTarget {
        org: identity.org.clone(),
        project: identity.project.clone(),
        environment: identity.environment.clone(),
    };
    let registry = super::credentials::workload(
        &args.system_database_url,
        &coordinate,
        wamn_control_provision::WorkloadRoleFamily::RegistryReader,
    )
    .await?;
    Ok(super::config::CdcReader {
        binary: args.cdc_reader_binary.canonicalize()?,
        system_database_url: registry,
        cdc_url,
        cdc_sql_file,
    })
}
