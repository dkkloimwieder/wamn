//! Arguments and output for the in-pod wired release operation.

use std::path::PathBuf;

use clap::Args;
use wamn_control::platform::wired_release::{PublishWiredReleaseRequest, publish_wired_release};
use wamn_control::publish_release::{PublishReleaseRequest, parse_package};
use wamn_ctl::provisioning_verbs::PatIssuerArgs;

#[derive(Args)]
pub struct PublishWiredReleaseArgs {
    #[arg(long, env = "WAMN_PG_ADMIN_URL", hide_env_values = true)]
    database_url: String,
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL", hide_env_values = true)]
    control_database_url: String,
    #[arg(long)]
    org: String,
    #[arg(long)]
    project: String,
    #[arg(long)]
    tenant: String,
    #[arg(long)]
    environment: String,
    #[arg(long, default_value = "wamn_run")]
    run_schema: String,
    #[arg(long = "package", value_parser = parse_package, required = true)]
    packages: Vec<wamn_catalog::PackageCoordinate>,
    #[arg(long)]
    artifact_base: String,
    #[arg(long, env = "WAMN_REGISTRY_AUTH_FILE", hide_env_values = true)]
    registry_auth_file: PathBuf,
    #[arg(long, default_value_t = false)]
    insecure_registry: bool,
    #[arg(long = "oci-ca-path", env = "WASH_OCI_CA_PATHS", value_delimiter = ',')]
    oci_ca_paths: Vec<PathBuf>,
    #[command(flatten)]
    issuer: PatIssuerArgs,
}

impl std::fmt::Debug for PublishWiredReleaseArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PublishWiredReleaseArgs")
            .finish_non_exhaustive()
    }
}

pub async fn run(args: PublishWiredReleaseArgs) -> anyhow::Result<()> {
    let release_artifact_base = std::env::var("WAMN_RELEASE_ARTIFACT_BASE")
        .map_err(|_| anyhow::anyhow!("set WAMN_RELEASE_ARTIFACT_BASE to the release repository"))?;
    let digest = publish_wired_release(PublishWiredReleaseRequest {
        release: PublishReleaseRequest {
            database_url: args.database_url,
            control_database_url: args.control_database_url,
            org: args.org,
            tenant: args.tenant,
            environment: args.environment,
            verified_publisher_principal: String::new(),
            run_schema: args.run_schema,
            packages: args.packages,
            wirings: Vec::new(),
        },
        project: args.project,
        issuer: args.issuer.into(),
        registry: wamn_control::package_artifact::PackageRegistry {
            artifact_base: args.artifact_base,
            registry_auth_file: args.registry_auth_file,
            insecure_registry: args.insecure_registry,
            oci_ca_paths: args.oci_ca_paths,
        },
        release_artifact_base,
    })
    .await?;
    println!("{digest}");
    Ok(())
}
