//! Arguments and output of the release-manifest, release-environment, and
//! delivery verbs.
//!
//! # The host carrier uses explicit flags
//!
//! The host takes the release pair as `hostGroups[].extraArgs` flags. Clap
//! exits nonzero on an unknown flag, so a misspelt
//! `--release-manifest-diges` crashloops the pod, while a misspelt
//! `WAMN_RELEASE_MANIFEST_DIGES` would deploy cleanly and serve nothing.

use std::path::PathBuf;

use anyhow::Context as _;
use clap::Args;
use serde_json::json;
use wamn_control::delivery::{PrepareReleaseRequest, deployment, publication, qualification};
use wamn_control::print_release_env::{ReleaseCarrier, lookup_release_carrier};
use wamn_control::push_release_manifest::PushReleaseManifestRequest;
use wamn_runtime::component_artifact_source::OCI_CA_PATHS_ENV;

/// The host's carrier: per host group `extraArgs`, flags rather than env.
const HOST_CARRIER: &str = "deploy/platform/values-host-receiving-pat.yaml";

const ARTIFACT_BASE_FLAG: &str = "--release-artifact-base";
const MANIFEST_DIGEST_FLAG: &str = "--release-manifest-digest";

/// Arguments for the release-manifest distribution copy.
#[derive(Debug, Args)]
pub struct PushReleaseManifestArgs {
    /// Owner URL to the database holding the minted release snapshot.
    #[arg(long)]
    pub database_url: String,

    /// Registry organization the release is deployed into. Required in both
    /// byte sources: the manifest fixes the rest of the attestation key, but
    /// never its control-plane placement.
    #[arg(long)]
    pub org: String,

    /// Registry project the release is deployed into.
    #[arg(long)]
    pub project: String,

    /// Tenant claim carried by the minted release snapshot.
    #[arg(long)]
    pub tenant: String,

    /// Integer identity of the minted effective release snapshot.
    #[arg(long)]
    pub effective_release_id: u32,

    /// Explicit `<registry>/<repository>` base for release manifests.
    #[arg(long)]
    pub artifact_base: String,

    /// Projected `.dockerconfigjson` file carrying the push credential.
    #[arg(long, env = "WAMN_REGISTRY_AUTH_FILE")]
    pub registry_auth_file: PathBuf,

    /// Use plain HTTP for exactly the registry in `--artifact-base`.
    #[arg(long, default_value_t = false)]
    pub insecure_registry: bool,

    /// PEM CA bundle trusted for the registry, on top of the compiled-in
    /// roots. Repeat or comma-delimit. Env `WASH_OCI_CA_PATHS`.
    #[arg(long = "oci-ca-path", env = OCI_CA_PATHS_ENV, value_delimiter = ',')]
    pub oci_ca_paths: Vec<PathBuf>,

    /// Owner URL to the CONTROL database this deployment is attested in
    /// (wamn-0h0g.8.27).
    ///
    /// REQUIRED in both byte sources. The attestation is what makes a digest
    /// RELEASED rather than a candidate (`wamn-0h0g.13.54`), so a push that
    /// could not reach the control plane must refuse rather than leave bytes in
    /// a registry that no fact says were deployed.
    #[arg(long)]
    pub control_database_url: String,
}

impl PushReleaseManifestArgs {
    /// Carry exactly what the operator typed into the library request.
    pub fn into_request(self) -> PushReleaseManifestRequest {
        PushReleaseManifestRequest {
            database_url: self.database_url,
            org: self.org,
            project: self.project,
            tenant: self.tenant,
            effective_release_id: self.effective_release_id,
            artifact_base: self.artifact_base,
            registry_auth_file: self.registry_auth_file,
            insecure_registry: self.insecure_registry,
            oci_ca_paths: self.oci_ca_paths,
            control_database_url: self.control_database_url,
        }
    }
}

/// Arguments naming the minted release whose lines are printed.
#[derive(Debug, Args)]
pub struct PrintReleaseEnvArgs {
    /// URL to the database holding the minted release snapshot.
    #[arg(long)]
    pub database_url: String,

    /// Tenant claim carried by the minted release snapshot.
    #[arg(long)]
    pub tenant: String,

    /// Integer identity of the minted effective release snapshot.
    #[arg(long)]
    pub effective_release_id: u32,

    /// The `<registry>/<repository>` the release manifest was pushed to.
    #[arg(long)]
    pub artifact_base: String,
}

/// Capture the existing minted release and explicit artifact locations for qualification.
#[derive(Debug, Args)]
pub struct PrepareReleaseArgs {
    #[command(flatten)]
    pub release: PrintReleaseEnvArgs,
    #[arg(long)]
    pub target_directory: PathBuf,
    #[arg(long)]
    pub manifest_output: PathBuf,
    #[arg(long)]
    pub candidate_output: PathBuf,
    #[arg(long)]
    pub host_image: String,
    #[arg(long)]
    pub gates_image: Option<String>,
    #[arg(long)]
    pub identity_image: Option<String>,
    #[arg(long)]
    pub native_registry_endpoint: Option<String>,
    #[arg(long, requires = "native_registry_endpoint")]
    pub native_registry_insecure: bool,
    #[arg(long = "deployment-file")]
    pub deployment_files: Vec<PathBuf>,
}

/// Publish an already qualified candidate through the existing OCI publisher.
#[derive(Debug, Args)]
pub struct PublishArgs {
    #[arg(long)]
    pub qualification: PathBuf,
    #[command(flatten)]
    pub publication: PushReleaseManifestArgs,
}

/// Select a published release without rebuilding or activating its workloads.
#[derive(Debug, Args)]
pub struct SelectArgs {
    #[arg(long)]
    pub qualification: PathBuf,
    #[command(flatten)]
    pub release: PushReleaseManifestArgs,
}

/// Deploy exact qualified Kubernetes inputs to one explicitly named environment.
#[derive(Debug, Args)]
pub struct DeployArgs {
    #[arg(long)]
    pub qualification: PathBuf,
    #[command(flatten)]
    pub release: PushReleaseManifestArgs,
    #[arg(long)]
    pub kubeconfig: PathBuf,
    #[arg(long)]
    pub context: String,
    #[arg(long)]
    pub namespace: String,
    /// Existing HTTP WorkloadDeployment that must become ready before the application request.
    #[arg(long)]
    pub http_workload: Option<String>,
    /// Existing rendered native Kubernetes Deployment JSON for the host.
    #[arg(long)]
    pub host_deployment: PathBuf,
    #[arg(long)]
    pub identity_deployment: Option<PathBuf>,
    #[arg(long)]
    pub principal: String,
    /// A released POST route reached through this deployment's ingress.
    #[arg(long)]
    pub interaction_url: String,
    #[arg(long)]
    pub route_host: String,
    #[arg(long)]
    pub request_body: PathBuf,
    #[arg(long)]
    pub expected_response: PathBuf,
    /// Private credential file, excluded from qualified artifacts and output.
    #[arg(long)]
    pub bearer_file: PathBuf,
}

/// Qualify a clean selected revision through the existing application cases.
#[derive(Debug, Args)]
pub struct QualifyReleaseArgs {
    #[arg(long, default_value = ".")]
    pub repository: PathBuf,
    /// Integrated main, a release tag, or an explicitly selected revision.
    #[arg(long, default_value = "main")]
    pub revision: String,
    #[arg(long)]
    pub candidate: PathBuf,
    /// Fresh temporary result file outside tracked source.
    #[arg(long)]
    pub result: PathBuf,
}

/// Run explicitly selected existing behavior checks before integration.
#[derive(Debug, Args)]
pub struct CheckChangesArgs {
    #[arg(long, default_value = ".")]
    pub repository: PathBuf,
    /// A member of the root or apps workspace.
    #[arg(long)]
    pub package: String,
    /// Omit for the library test target.
    #[arg(long)]
    pub test: Option<String>,
    #[arg(long = "case", required = true)]
    pub cases: Vec<String>,
    #[arg(long)]
    pub include_ignored: bool,
    #[arg(long)]
    pub result: PathBuf,
}

/// Print the release lines for one minted release.
pub async fn print_release_env(args: PrintReleaseEnvArgs) -> anyhow::Result<()> {
    let carrier = lookup_release_carrier(
        &args.database_url,
        &args.tenant,
        args.effective_release_id,
        &args.artifact_base,
    )
    .await?;
    print!("{}", release_lines(&carrier));
    Ok(())
}

/// Render the release lines each carrier takes, labelled by carrier file.
fn release_lines(carrier: &ReleaseCarrier) -> String {
    let ReleaseCarrier {
        artifact_base,
        manifest_digest,
    } = carrier;
    format!(
        "# {HOST_CARRIER} hostGroups[].extraArgs\n\
         {ARTIFACT_BASE_FLAG}={artifact_base}\n\
         {MANIFEST_DIGEST_FLAG}={manifest_digest}\n"
    )
}

/// Write machine inputs from an immutable snapshot without qualifying or publishing it.
pub async fn prepare(args: PrepareReleaseArgs) -> anyhow::Result<()> {
    wamn_control::delivery::prepare(PrepareReleaseRequest {
        database_url: args.release.database_url,
        tenant: args.release.tenant,
        effective_release_id: args.release.effective_release_id,
        artifact_base: args.release.artifact_base,
        target_directory: args.target_directory,
        manifest_output: args.manifest_output,
        candidate_output: args.candidate_output,
        host_image: args.host_image,
        gates_image: args.gates_image,
        identity_image: args.identity_image,
        native_registry_endpoint: args.native_registry_endpoint,
        native_registry_insecure: args.native_registry_insecure,
        deployment_files: args.deployment_files,
    })
    .await
}

/// Publish only the release that passed required qualification.
pub async fn publish(args: PublishArgs) -> anyhow::Result<()> {
    let published =
        publication::publish(&args.qualification, &args.publication.into_request()).await?;
    println!("{}", published.digest);
    Ok(())
}

/// Select one published release for its environment and print what it selected.
pub async fn select(args: SelectArgs) -> anyhow::Result<()> {
    let selected = deployment::select(&args.qualification, &args.release.into_request()).await?;
    println!(
        "{}",
        json!({"selected_release":selected.release,
        "manifest_digest":selected.manifest_digest,"result":"pass"})
    );
    Ok(())
}

/// Deploy exact qualified artifacts and print what the environment now serves.
///
/// The interrupt, termination, and hangup arms live here rather than in the
/// library: cancelling this verb rolls the deployment transaction back, so the
/// signal is the operator's answer to the CLI, not the library's own cleanup.
pub async fn deploy(args: DeployArgs) -> anyhow::Result<()> {
    let DeployArgs {
        qualification,
        release,
        kubeconfig,
        context,
        namespace,
        http_workload,
        host_deployment,
        identity_deployment,
        principal,
        interaction_url,
        route_host,
        request_body,
        expected_response,
        bearer_file,
    } = args;
    let release = release.into_request();
    let request = deployment::DeployRequest {
        kubeconfig,
        context,
        namespace,
        http_workload,
        host_deployment,
        identity_deployment,
        principal,
        interaction_url,
        route_host,
        request_body,
        expected_response,
        bearer_file,
    };
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut hangup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
    let deployed = tokio::select! {
        result = deployment::deploy_release(&qualification, &release, &request) => result?,
        result = tokio::signal::ctrl_c() => {
            result.context("listen for deployment interruption")?;
            anyhow::bail!("deployment interrupted; no automatic mutation retry")
        }
        _ = terminate.recv() => anyhow::bail!("deployment terminated; no automatic mutation retry"),
        _ = hangup.recv() => anyhow::bail!("deployment connection closed; no automatic mutation retry"),
    };
    println!(
        "{}",
        json!({"source_commit":deployed.source_commit,"deployed_release":deployed.release,
        "manifest_digest":deployed.manifest_digest,"result":"pass"})
    );
    Ok(())
}

/// Qualify the exact release artifacts from a clean selected revision.
pub async fn qualify(args: QualifyReleaseArgs) -> anyhow::Result<()> {
    qualification::qualify(qualification::QualifyReleaseRequest {
        repository: args.repository,
        revision: args.revision,
        candidate: args.candidate,
        result: args.result,
    })
    .await
}

/// Execute selected existing tests before integration.
pub async fn check_changes(args: CheckChangesArgs) -> anyhow::Result<()> {
    qualification::check_changes(qualification::CheckChangesRequest {
        repository: args.repository,
        package: args.package,
        test: args.test,
        cases: args.cases,
        include_ignored: args.include_ignored,
        result: args.result,
    })
    .await
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;
    use wamn_catalog::{ManifestDigest, ServingManifest};

    use super::*;

    /// Host command for the flattened argument surface under test.
    #[derive(Debug, clap::Parser)]
    struct PushProbe {
        #[command(flatten)]
        args: PushReleaseManifestArgs,
    }

    /// Host command for the flattened argument surface under test.
    #[derive(Debug, clap::Parser)]
    struct PrintProbe {
        #[command(flatten)]
        args: PrintReleaseEnvArgs,
    }

    const DESTINATION: [&str; 6] = [
        "--artifact-base",
        "registry.example/wamn/releases",
        "--registry-auth-file",
        "auth.json",
        // wamn-0h0g.8.27: the control database the push attests into. A
        // separate URL on purpose — the two planes are two databases.
        "--control-database-url",
        "postgres://control.invalid/store",
    ];

    const PLACEMENT: [&str; 4] = ["--org", "fixture", "--project", "billing"];

    const COORDINATE: [&str; 8] = [
        "--database-url",
        "postgres://release.invalid/env",
        "--tenant",
        "tenant-a",
        "--effective-release-id",
        "3",
        "--artifact-base",
        "registry.example/wamn/releases",
    ];

    const CANONICAL_MANIFEST: &[u8] = br#"{"attachments":{},"components":[{"component":"http-request","digest":"sha256:1111111111111111111111111111111111111111111111111111111111111111","interface-version":"0.1","operations":{"wamn:node/handler@0.1.0":{}},"package-id":"orders"}],"format-version":2,"registrations":{},"release":{"effective-release-id":3,"environment":"prod","packages":[{"package-id":"orders","package-version":"1.0.0"}],"tenant-id":"tenant-a"},"routes":[],"wirings":[{"graph-hash":"sha256:3333333333333333333333333333333333333333333333333333333333333333","package-id":"orders","wiring-id":"orders","wiring-version":1}]}"#;

    fn parse(source: &[&str]) -> Result<PushReleaseManifestArgs, clap::Error> {
        let mut argv = vec!["push-release-manifest"];
        argv.extend_from_slice(source);
        argv.extend_from_slice(&DESTINATION);
        argv.extend_from_slice(&PLACEMENT);
        PushProbe::try_parse_from(argv).map(|probe| probe.args)
    }

    fn digest() -> ManifestDigest {
        ManifestDigest::parse(format!("sha256:{}", "7".repeat(64)))
            .expect("the fixture digest is canonical")
    }

    #[test]
    fn minted_snapshot_does_not_publish_without_its_control_plane_placement() {
        let source = [
            "--database-url",
            "postgres://release.invalid/env",
            "--tenant",
            "tenant-a",
            "--effective-release-id",
            "3",
        ];
        for placement in [
            vec!["--org", "fixture"],
            vec!["--project", "billing"],
            vec![],
        ] {
            let mut argv = vec!["push-release-manifest"];
            argv.extend_from_slice(&source);
            argv.extend_from_slice(&DESTINATION);
            argv.extend_from_slice(&placement);
            assert!(
                PushProbe::try_parse_from(argv).is_err(),
                "published with placement {placement:?}"
            );
        }

        let mut argv = vec!["push-release-manifest"];
        argv.extend_from_slice(&source);
        argv.extend_from_slice(&[
            "--artifact-base",
            "registry.example/wamn/releases",
            "--registry-auth-file",
            "auth.json",
        ]);
        argv.extend_from_slice(&PLACEMENT);
        assert!(
            PushProbe::try_parse_from(argv).is_err(),
            "published with no control database to attest into"
        );

        let placed = parse(&source).expect("a placed minted snapshot parses");
        assert_eq!(placed.org, "fixture");
        assert_eq!(placed.project, "billing");
        assert_eq!(
            placed.control_database_url,
            "postgres://control.invalid/store"
        );
    }

    #[test]
    fn the_parsed_placement_reaches_the_attestation_key() {
        // The link the surface exists for: what the operator typed on the
        // command line, not some other string in scope, is what keys the write.
        let args = parse(&[
            "--database-url",
            "postgres://release.invalid/env",
            "--tenant",
            "tenant-a",
            "--effective-release-id",
            "3",
        ])
        .expect("the minted snapshot source parses");
        let (manifest, _) = ServingManifest::from_canonical_bytes(CANONICAL_MANIFEST)
            .expect("the fixture is canonical format-1 bytes");
        let coordinate = args.into_request().deployment_coordinate(&manifest.release);

        assert_eq!(coordinate.triple.org, "fixture");
        assert_eq!(coordinate.triple.project, "billing");
        assert_eq!(coordinate.triple.env.as_str(), "prod");
        assert_eq!(coordinate.tenant_id, "tenant-a");
    }

    #[test]
    fn a_release_publishes_only_from_its_minted_snapshot() {
        let snapshot = parse(&[
            "--database-url",
            "postgres://release.invalid/env",
            "--tenant",
            "tenant-a",
            "--effective-release-id",
            "3",
        ])
        .expect("the minted-snapshot source parses");
        assert_eq!(snapshot.database_url, "postgres://release.invalid/env");
        assert_eq!(snapshot.tenant, "tenant-a");
        assert_eq!(snapshot.effective_release_id, 3);

        assert!(
            parse(&["--manifest", "manifest.json"]).is_err(),
            "caller-supplied bytes must not mint deployment evidence"
        );
    }

    #[test]
    fn a_complete_minted_snapshot_coordinate_is_required() {
        let refusals: [Vec<&str>; 3] = [
            vec![],
            vec![
                "--database-url",
                "postgres://release.invalid/env",
                "--tenant",
                "tenant-a",
            ],
            vec!["--tenant", "tenant-a", "--effective-release-id", "3"],
        ];
        for refused in refusals {
            assert!(parse(&refused).is_err(), "accepted {refused:?}");
        }
    }

    #[test]
    fn one_release_prints_both_carriers_and_nothing_else() {
        let printed = release_lines(&ReleaseCarrier {
            artifact_base: "registry.example/wamn/releases".to_owned(),
            manifest_digest: digest(),
        });
        assert_eq!(
            printed,
            format!(
                "# {HOST_CARRIER} hostGroups[].extraArgs\n\
                 --release-artifact-base=registry.example/wamn/releases\n\
                 --release-manifest-digest=sha256:{seven}\n",
                seven = "7".repeat(64)
            )
        );
        assert_eq!(printed.lines().count(), 3);
    }

    #[test]
    fn the_release_coordinate_and_its_repository_are_all_required() {
        let complete =
            PrintProbe::try_parse_from(std::iter::once("print-release-env").chain(COORDINATE))
                .expect("the complete coordinate parses")
                .args;
        assert_eq!(complete.effective_release_id, 3);
        assert_eq!(complete.artifact_base, "registry.example/wamn/releases");

        for omitted in [
            "--database-url",
            "--tenant",
            "--effective-release-id",
            "--artifact-base",
        ] {
            let mut argv = vec!["print-release-env"];
            let mut skip = false;
            for entry in COORDINATE {
                if entry == omitted {
                    skip = true;
                    continue;
                }
                if skip {
                    skip = false;
                    continue;
                }
                argv.push(entry);
            }
            assert!(
                PrintProbe::try_parse_from(argv).is_err(),
                "accepted a coordinate without {omitted}"
            );
        }
    }
}
