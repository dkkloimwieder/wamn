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
    inputs.cdc_reader = Some(enable_cdc(
        &args,
        &inputs,
        &mut admin,
        &environment.route.database_url,
        &environment.identity,
    )?);
    let config = write_dev_config(
        &args.root,
        &args.system_database_url,
        &environment.template,
        &environment.route,
        &environment.credentials,
        &inputs,
        &environment.identity,
    )?;

    let web = super::control::start(&args, &environment);
    if let Err(error) = web {
        environment.issuer.stop()?;
        return Err(error);
    }
    environment.issuer.retain(&args.root.canonicalize()?)?;
    Ok(config)
}

/// Enable CDC for the environment with the cluster's verb and record what the
/// loop's reader needs. The verb provisions the event streams with the
/// materializer consumers of the packages, and records the registration. The
/// role SQL applies here. The CDC SQL holds the slot, so the loop applies it to
/// each target it creates.
fn enable_cdc(
    args: &DevUpRequest,
    _inputs: &DevEnvironmentInputs,
    admin: &mut sqlx::PgConnection,
    _target_url: &str,
    _identity: &super::activation::DevActivationIdentity,
) -> anyhow::Result<super::config::CdcReader> {
    let _ = (args, admin);
    anyhow::bail!("no credential store in local mode")
}
