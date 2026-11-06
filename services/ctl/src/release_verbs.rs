//! Arguments and output of the `publish-release` verb.

use std::path::PathBuf;

use clap::Args;
use wamn_catalog::PackageCoordinate;
use wamn_environment_control::environment::document::EnvironmentDocument;
use wamn_package_control::package_artifact::PackageRegistry;
use wamn_release_control::publish_release::{
    self, EntryPublicationInputs, PublishReleaseRequest, parse_package,
};
use wamn_runtime::component_artifact_source::OCI_CA_PATHS_ENV;

/// Arguments for the release publish verb. Each package is the artifact that
/// `push-package` pushed under the tag `<package_id>-<version>`.
#[derive(Debug, Args)]
pub struct PublishReleaseArgs {
    /// The environment.k document.
    pub file: PathBuf,
    /// Application to publish; omitted when the map names one application.
    #[arg(long)]
    pub application: Option<String>,
    #[arg(long)]
    pub database_url: String,
    /// Owner URL of the control database holding the environment policy.
    #[arg(long)]
    pub control_database_url: String,
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

/// Publish one effective release and print its manifest digest.
pub async fn publish(args: PublishReleaseArgs) -> anyhow::Result<()> {
    let environment = EnvironmentDocument::compile(&args.file)?;
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
    // A release with an HTTP entry pushes its composed workload to
    // `WAMN_COMPONENT_ARTIFACT_BASE`, with the route guest from
    // `WAMN_FLOW_HTTP_COMPONENT` or the image's pinned copy.
    let component_artifact_base = std::env::var("WAMN_COMPONENT_ARTIFACT_BASE").ok();
    let route_guest = std::env::var_os("WAMN_FLOW_HTTP_COMPONENT").map_or_else(
        || PathBuf::from("/usr/local/share/wamn/http_route.wasm"),
        PathBuf::from,
    );
    let entry = component_artifact_base
        .as_deref()
        .map(|component_artifact_base| EntryPublicationInputs {
            component_artifact_base,
            route_guest: &route_guest,
        });
    let digest = publish_release::publish_release(
        PublishReleaseRequest {
            applications: environment
                .applications
                .into_iter()
                .map(|(name, application)| (name, application.into()))
                .collect(),
            application: args.application,
            database_url: args.database_url,
            control_database_url: args.control_database_url,
            org: environment.org,
            project: environment.project,
            environment: environment.env,
            verified_publisher_principal: args.verified_publisher_principal,
            run_schema: args.run_schema,
            packages: args.packages,
        },
        &registry,
        &release_artifact_base,
        entry.as_ref(),
    )
    .await?;
    println!("{digest}");
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

    const PUBLISH: [&str; 15] = [
        "publish-release",
        "environment.k",
        "--database-url",
        "postgres://publish.invalid/env",
        "--control-database-url",
        "postgres://publish.invalid/control",
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
        assert_eq!(args.file, PathBuf::from("environment.k"));
        for removed in ["--tenant", "--org", "--environment"] {
            let mut arguments = PUBLISH.to_vec();
            arguments.extend([removed, "unexpected"]);
            assert!(PublishProbe::try_parse_from(arguments).is_err());
        }
        assert!(
            PublishProbe::try_parse_from(&PUBLISH[..12]).is_err(),
            "publish-release parsed with no package artifact base"
        );
        let mut selected = PUBLISH.to_vec();
        selected.extend(["--application", "orders"]);
        assert_eq!(
            PublishProbe::try_parse_from(selected)
                .unwrap()
                .args
                .application
                .as_deref(),
            Some("orders")
        );
        let mut missing_file = PUBLISH.to_vec();
        missing_file.remove(1);
        assert!(PublishProbe::try_parse_from(missing_file).is_err());
        for retired in [
            ["--applications", r#"{"orders":{"packages":["orders"]}}"#],
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
