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
    pub nats_url: String,
    pub event_nats_url: String,
    pub event_nats_username: String,
    pub event_nats_password_file: PathBuf,
    pub event_provisioning_username: String,
    pub event_provisioning_password_file: PathBuf,
    pub stream_replicas: usize,
    pub dup_window_secs: u64,
    pub tempo_query_url: String,
    pub otel_exporter_otlp_endpoint: String,
    pub route_host: String,
    pub platform_domain: String,
    pub flow_http_component: PathBuf,
    pub local_bindings: Option<PathBuf>,
    pub host_binary: PathBuf,
    pub packages: Vec<PathBuf>,
}

/// Provision the environment and return the configuration file.
pub async fn provision_environment(args: DevUpRequest) -> anyhow::Result<PathBuf> {
    // Settled before a single credential is minted: an environment is
    // expensive to stand up.
    wamn_control_provision::validate_platform_domain(&args.platform_domain)?;

    std::fs::create_dir_all(&args.root)
        .with_context(|| format!("create the environment directory {}", args.root.display()))?;
    // Minted PATs and credential URLs land here, so the directory is the wall.
    std::fs::set_permissions(&args.root, Permissions::from_mode(0o700))
        .with_context(|| format!("restrict {} to its owner", args.root.display()))?;

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
    };
    let inputs = DevEnvironmentInputs {
        local_artifacts,
        host_binary: args.host_binary,
        nats_url: args.nats_url,
        event_nats_url: args.event_nats_url,
        event_nats_username: args.event_nats_username,
        event_nats_password_file: args.event_nats_password_file,
        stream_replicas: args.stream_replicas,
        dup_window_secs: args.dup_window_secs,
        tempo_query_url: args.tempo_query_url,
        otel_exporter_otlp_endpoint: args.otel_exporter_otlp_endpoint,
        route_host: args.route_host,
        platform_domain: args.platform_domain,
        package_sources,
    };

    let broker_options = crate::event_streams::connection_options(
        &args.event_provisioning_username,
        &args.event_provisioning_password_file,
    )?;
    let broker = async_nats::jetstream::new(
        broker_options
            .connect(&inputs.event_nats_url)
            .await
            .context("connect event provisioning credential")?,
    );
    let (admin, admin_task) = connect(&args.system_database_url).await?;
    let environment = provision(
        &args.system_database_url,
        admin.as_ref(),
        &args.root,
        &inputs.platform_domain,
    )
    .await?;
    let event_scope = wamn_control_registry::Triple::new(
        &environment.identity.org,
        &environment.identity.project,
        environment.identity.environment.clone(),
    );
    crate::event_streams::provision(
        &broker,
        &event_scope,
        inputs.stream_replicas,
        std::time::Duration::from_secs(inputs.dup_window_secs),
        &[],
    )
    .await?;
    let config = write_dev_config(
        &args.root,
        &args.system_database_url,
        &environment.template,
        &environment.route,
        &environment.credentials,
        &inputs,
        &environment.identity,
    )?;

    admin_task.abort();
    Ok(config)
}
