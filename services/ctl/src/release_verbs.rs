//! Arguments and output of the `publish-release` and
//! `terminalize-effect-uncertain` verbs.

use std::path::PathBuf;

use clap::Args;
use wamn_catalog::PackageCoordinate;
use wamn_control::package_artifact::PackageRegistry;
use wamn_control::publish_release::{self, PublishReleaseRequest, parse_package};
use wamn_control::terminalize_effect_uncertain::{self, TerminalizeEffectUncertainRequest};
use wamn_run_state::operator_action::OperatorActionBasis;
use wamn_runtime::component_artifact_source::OCI_CA_PATHS_ENV;

/// Arguments for the release publish verb. Each package is the artifact that
/// `push-package` pushed under the tag `<package_id>-<version>`.
#[derive(Debug, Args)]
pub struct PublishReleaseArgs {
    #[arg(long)]
    pub database_url: String,
    /// Owner URL of the control database holding the environment policy.
    #[arg(long)]
    pub control_database_url: String,
    #[arg(long)]
    pub org: String,
    #[arg(long)]
    pub tenant: String,
    #[arg(long)]
    pub environment: String,
    /// Principal already authenticated by the publication boundary.
    #[arg(long)]
    pub verified_publisher_principal: String,
    #[arg(long)]
    pub run_schema: String,
    /// Exact package membership; repeat once per package. Each is read from
    /// the registry under its tag.

    #[arg(long = "package", value_name = "ID@VERSION", value_parser = parse_package, required = true)]
    pub packages: Vec<PackageCoordinate>,
    /// Explicit `<registry>/<repository>` base for package artifacts.
    #[arg(long)]
    pub artifact_base: String,
    /// `.dockerconfigjson` file carrying the credential of both registries:
    /// a pull for the packages, a push for the release.
    #[arg(long, env = "WAMN_REGISTRY_AUTH_FILE")]
    pub registry_auth_file: PathBuf,
    /// Use plain HTTP for exactly the registries of the package base and of
    /// `WAMN_RELEASE_ARTIFACT_BASE`.
    #[arg(long, default_value_t = false)]
    pub insecure_registry: bool,
    /// PEM CA bundle trusted for the registry, on top of the compiled-in
    /// roots. Repeat or comma-delimit. Env `WASH_OCI_CA_PATHS`.
    #[arg(long = "oci-ca-path", env = OCI_CA_PATHS_ENV, value_delimiter = ',')]
    pub oci_ca_paths: Vec<PathBuf>,
}

/// Exact operator input; effect identity and asserted outcome are absent by construction.
#[derive(Debug, Args)]
pub struct TerminalizeEffectUncertainArgs {
    /// Project-admin PostgreSQL URL for the project database.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: String,

    /// Project run-plane schema.
    #[arg(long, default_value = "wamn_run")]
    pub schema: String,

    /// Exact tenant owning the run.
    #[arg(long)]
    pub tenant: String,

    /// Exact effect-uncertain run.
    #[arg(long)]
    pub run: String,

    /// Evidence basis: external-evidence, counterparty-confirmation, or operator-judgment.
    #[arg(long)]
    pub basis: OperatorActionBasis,

    /// Opaque non-empty reference to the evidence used.
    #[arg(long)]
    pub evidence_ref: String,

    /// Opaque non-empty idempotency correlation.
    #[arg(long)]
    pub correlation_id: String,
}

/// Publish one effective release and print its manifest digest.
pub async fn publish(args: PublishReleaseArgs) -> anyhow::Result<()> {
    // The release repository is the one `env apply` pulls from, named once
    // for the platform (`environment::Platform::from_env`).
    let release_artifact_base = std::env::var("WAMN_RELEASE_ARTIFACT_BASE")
        .map_err(|_| anyhow::anyhow!("set WAMN_RELEASE_ARTIFACT_BASE to the release repository"))?;
    let registry = PackageRegistry {
        artifact_base: args.artifact_base,
        registry_auth_file: args.registry_auth_file,
        insecure_registry: args.insecure_registry,
        oci_ca_paths: args.oci_ca_paths,
    };
    let digest = publish_release::publish_release(
        PublishReleaseRequest {
            database_url: args.database_url,
            control_database_url: args.control_database_url,
            org: args.org,
            tenant: args.tenant,
            environment: args.environment,
            verified_publisher_principal: args.verified_publisher_principal,
            run_schema: args.run_schema,
            packages: args.packages,
        },
        &registry,
        &release_artifact_base,
    )
    .await?;
    println!("{digest}");
    Ok(())
}

/// Terminalize one effect-uncertain run and print its result.
pub async fn terminalize(args: TerminalizeEffectUncertainArgs) -> anyhow::Result<()> {
    let result = terminalize_effect_uncertain::terminalize_effect_uncertain(
        TerminalizeEffectUncertainRequest {
            admin_database_url: args.admin_database_url,
            schema: args.schema,
            tenant: args.tenant,
            run: args.run,
            basis: args.basis,
            evidence_ref: args.evidence_ref,
            correlation_id: args.correlation_id,
        },
    )
    .await?;
    println!("{result}");
    Ok(())
}
