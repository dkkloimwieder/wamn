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
    /// Application map compiled from environment.k.
    #[arg(long, value_parser = parse_applications)]
    pub applications: wamn_catalog::EnvironmentApplications,
    /// Application to publish; omitted when the map names one application.
    #[arg(long)]
    pub application: Option<String>,
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

/// Parse the application map compiled from an environment document.
pub fn parse_applications(value: &str) -> Result<wamn_catalog::EnvironmentApplications, String> {
    serde_json::from_str(value).map_err(|error| error.to_string())
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
            applications: args.applications,
            application: args.application,
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

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::*;

    /// Host command for the publish argument surface under test.
    #[derive(Debug, clap::Parser)]
    struct PublishProbe {
        #[command(flatten)]
        args: PublishReleaseArgs,
    }

    const PUBLISH: [&str; 22] = [
        "publish-release",
        "--applications",
        r#"{"orders":{"packages":["orders"]}}"#,
        "--database-url",
        "postgres://publish.invalid/env",
        "--control-database-url",
        "postgres://publish.invalid/control",
        "--org",
        "acme",
        "--tenant",
        "acme--plant--dev",
        "--environment",
        "dev",
        "--verified-publisher-principal",
        "publisher",
        "--run-schema",
        "wamn_run",
        "--package",
        "orders@1.0.0",
        "--artifact-base",
        "registry.invalid/wamn/packages",
        "--registry-auth-file=config.json",
    ];

    /// A package is an id and a version that the registry resolves to its
    /// artifact by tag, so argv names no package file.
    #[test]
    fn publish_release_takes_packages_from_the_registry() {
        let args = PublishProbe::try_parse_from(PUBLISH)
            .expect("the registry surface parses")
            .args;
        assert_eq!(
            args.packages,
            vec![PackageCoordinate::new("orders", "1.0.0").unwrap()]
        );
        assert_eq!(args.artifact_base, "registry.invalid/wamn/packages");
        assert!(
            args.application.is_none(),
            "one application needs no selector"
        );
        assert_eq!(
            args.applications["orders"].packages,
            ["orders".to_owned()].into()
        );
        assert!(
            PublishProbe::try_parse_from(&PUBLISH[..19]).is_err(),
            "publish-release parsed with no package artifact base"
        );
        for retired in [
            ["--package-manifest", "apps/orders/generated/wamn.json"],
            ["--attachments", "apps/orders/publication/attachments.json"],
        ] {
            let mut retired_argv = PUBLISH.to_vec();
            retired_argv.extend(retired);
            assert!(
                PublishProbe::try_parse_from(retired_argv).is_err(),
                "accepted {retired:?}"
            );
        }
    }
}
