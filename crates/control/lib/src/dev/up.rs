//! Provision a disposable development environment and write its configuration.
use super::environment::{DevEnvironmentInputs, connect, provision, write_dev_config};
use anyhow::Context as _;
use std::fs::{OpenOptions, Permissions};
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;

/// Explicit inputs for a disposable development environment.
#[derive(Debug)]
pub struct DevUpRequest {
    pub system_database_url: String,
    pub root: PathBuf,
    pub nats_url: String,
    pub event_nats_url: String,
    pub event_nats_username: String,
    pub event_nats_password_file: PathBuf,
    pub event_provisioning_username: String,
    pub event_provisioning_password_file: PathBuf,
    pub stream_replicas: usize,
    pub dup_window_secs: u64,
    pub otel_exporter_otlp_endpoint: String,
    pub route_host: String,
    pub platform_domain: String,
    pub flow_http_component: PathBuf,
    pub local_bindings: Option<PathBuf>,
    pub host_binary: PathBuf,
    /// The built platform UI directory served beside application bundles.
    pub platform_ui: PathBuf,
    pub packages: Vec<PathBuf>,
    /// The built `wamn-cdc-reader` the loop runs beside the host.
    pub cdc_reader_binary: PathBuf,
    /// The event-broker credential the reader publishes change events with.
    pub event_publisher_username: String,
    pub event_publisher_password_file: PathBuf,
    /// The built platform materializer the loop runs beside the release.
    pub materializer_component: PathBuf,
    /// The event-broker credential the materializer consumes with.
    pub event_materializer_username: String,
    pub event_materializer_password_file: PathBuf,
}

/// Provision the environment and return the configuration file.
pub async fn provision_environment(mut args: DevUpRequest) -> anyhow::Result<PathBuf> {
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
        bindings: args
            .local_bindings
            .as_ref()
            .map(|path| path.canonicalize())
            .transpose()?,
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
        nats_url: args.nats_url.clone(),
        event_nats_url: args.event_nats_url.clone(),
        event_nats_username: args.event_nats_username.clone(),
        event_nats_password_file: args.event_nats_password_file.clone(),
        stream_replicas: args.stream_replicas,
        dup_window_secs: args.dup_window_secs,
        otel_exporter_otlp_endpoint: args.otel_exporter_otlp_endpoint.clone(),
        route_host: args.route_host.clone(),
        platform_domain: args.platform_domain.clone(),
        package_sources,
        cdc_reader: None,
        event_materializer_username: args.event_materializer_username.clone(),
        event_materializer_password_file: args.event_materializer_password_file.clone(),
    };

    let (admin, admin_task) = connect(&args.system_database_url).await?;
    let environment = provision(
        &args.system_database_url,
        admin.as_ref(),
        &args.root,
        &inputs.platform_domain,
        &inputs.package_sources,
    )
    .await?;
    inputs.cdc_reader = Some(
        enable_cdc(
            &args,
            &inputs,
            admin.as_ref(),
            &environment.route.database_url,
            &environment.identity,
        )
        .await?,
    );
    let config = write_dev_config(
        &args.root,
        &args.system_database_url,
        &environment.template,
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
    admin_task.abort();
    Ok(config)
}

/// Write private runtime credentials into the environment directory.
pub(super) fn write_private(target: &std::path::Path, bytes: &[u8]) -> anyhow::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(target)
        .with_context(|| format!("create {}", target.display()))?;
    // A file left by an earlier run keeps its old mode, so set it again.
    file.set_permissions(Permissions::from_mode(0o600))?;
    file.write_all(bytes)
        .with_context(|| format!("write {}", target.display()))?;
    Ok(())
}

/// Enable CDC for the environment with the cluster's verb and record what the
/// loop's reader needs. The verb provisions the event streams with the
/// materializer consumers of the packages, and records the registration. The
/// role SQL applies here. The CDC SQL holds the slot, so the loop applies it to
/// each target it creates.
async fn enable_cdc(
    args: &DevUpRequest,
    inputs: &DevEnvironmentInputs,
    admin: &tokio_postgres::Client,
    target_url: &str,
    identity: &super::activation::DevActivationIdentity,
) -> anyhow::Result<super::config::CdcReader> {
    let scope = wamn_control_registry::Triple::new(
        &identity.org,
        &identity.project,
        identity.environment.clone(),
    );
    let manifests = inputs
        .package_sources
        .iter()
        .map(|root| super::config::read_package_manifest(root))
        .collect::<Result<Vec<_>, _>>()?;
    let consumers = wamn_control_provision::events::entity_consumers(&scope, &manifests);
    let schemas = manifests
        .iter()
        .flat_map(|manifest| manifest.models.values().map(|model| model.schema.clone()))
        .chain(std::iter::once(identity.schema.clone()))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let consumer_config = consumers
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()?;
    let target = url::Url::parse(target_url).context("parse the target database URL")?;
    let mut password = [0u8; 32];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut password)
        .map_err(|_| anyhow::anyhow!("generate the replication password"))?;
    let cdc_sql_file = args.root.join("cdc.sql");
    let outcome = crate::enable_cdc_project_env::enable_cdc_project_env(
        &crate::enable_cdc_project_env::EnableCdcProjectEnvRequest {
            org: identity.org.clone(),
            project: identity.project.clone(),
            env: identity.environment.clone(),
            schemas: schemas.clone(),
            system_database_url: Some(args.system_database_url.clone()),
            cluster: None,
            replication_password: hex::encode(password),
            db_host: target.host_str().map(str::to_owned),
            db_port: target.port_or_known_default().unwrap_or(5432),
            namespace: "wamn-system".to_owned(),
            cluster_namespace: "wamn-system".to_owned(),
            secret_namespace: None,
            stream: None,
            nats_url: inputs.event_nats_url.clone(),
            nats_username: args.event_provisioning_username.clone(),
            nats_password_file: args.event_provisioning_password_file.clone(),
            stream_replicas: inputs.stream_replicas,
            dup_window_secs: inputs.dup_window_secs,
            consumer_config,
            // The role SQL carries the password, so it stays in memory.
            emit_role_sql: None,
            emit_cdc_sql: None,
            emit_publication: None,
            emit_secret: None,
        },
    )
    .await?;
    // No CNPG operator runs here, so the superuser that local dev holds creates
    // the publication with the statement the fixtures use. It follows the
    // schema guard of the bundle. The bundle creates the slot before any write.
    let full = manifests
        .iter()
        .flat_map(|manifest| manifest.models.values())
        .map(|model| {
            format!(
                "{};",
                wamn_schema_control::alter_replica_identity_sql(
                    &model.schema,
                    &model.table,
                    wamn_schema_control::ReplicaIdentity::Full
                )
            )
        })
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(
        &cdc_sql_file,
        format!(
            "{}{full}\n{}\n",
            outcome.cdc_sql,
            wamn_control_provision::sql::create_publication_schemas_sql(
                &outcome.cdc_name,
                &schemas
            )
        ),
    )
    .with_context(|| format!("write {}", cdc_sql_file.display()))?;
    admin
        .batch_execute(&outcome.role_sql)
        .await
        .context("apply the replication role SQL")?;
    crate::enable_cdc_project_env::record_event_reader(
        &args.system_database_url,
        &outcome.triple,
        &schemas,
        &outcome.cdc_name,
        None,
    )
    .await?;
    let cdc_url = outcome.secret["stringData"]["url"]
        .as_str()
        .context("the CDC Secret carries its url")?
        .to_owned();
    let reader_secret = args.root.join("registry-reader.json");
    crate::provision_project_env::run_workload_action(&super::environment::generation_args(
        &super::environment::JourneyScope::receiving(),
        wamn_control_provision::WorkloadRoleFamily::RegistryReader,
        &args.system_database_url,
        None,
        &reader_secret,
    ))
    .await
    .context("prepare the registry-reader generation")?;
    Ok(super::config::CdcReader {
        binary: args
            .cdc_reader_binary
            .canonicalize()
            .context("resolve the CDC reader binary")?,
        system_database_url: crate::provision_project_env::secret_value(&reader_secret, "url")?,
        cdc_url,
        nats_username: args.event_publisher_username.clone(),
        nats_password_file: args
            .event_publisher_password_file
            .canonicalize()
            .context("resolve the event publisher password file")?,
        cdc_sql_file,
    })
}
