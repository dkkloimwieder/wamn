//! Print the release lines a pod template carries for one minted release.
//!
//! The operator path that shipped with `wamn-cdky` is manual: read the digest
//! off `publish-release` stdout and hand-edit the template. This verb removes
//! the transcription step and nothing else. It reads the frozen
//! `catalog.release_manifest_v3_snapshots` row, re-derives release identity
//! from those exact bytes, and prints. It writes no manifest and mutates
//! nothing; a pod reading its own digest out of PostgreSQL is explicitly
//! refused (`wamn-duyl`) because it would roll a release without a rollout.
//!
//! # Both carriers are printed, and they are not the same shape
//!
//! Two workloads carry a release, and only the executor takes the pair as
//! environment entries. The host takes it as `hostGroups[].extraArgs` FLAGS
//! deliberately: clap exits nonzero on an unknown flag, so a misspelt
//! `--release-manifest-diges` crashloops the pod, while a misspelt
//! `WAMN_RELEASE_MANIFEST_DIGES` would deploy cleanly and serve nothing.
//! Printing only environment lines would leave a host operator translating by
//! hand, so both forms are printed and each names the file it belongs in.

use anyhow::Context as _;
use clap::Args;
use tokio_postgres::NoTls;
use wamn_catalog::{ManifestDigest, ServingManifest};

use crate::push_release_manifest::select_snapshot;

/// The repository name both release-carrying workloads read.
const ARTIFACT_BASE_ENV: &str = "WAMN_RELEASE_ARTIFACT_BASE";
/// The welded manifest digest both release-carrying workloads read.
const MANIFEST_DIGEST_ENV: &str = "WAMN_RELEASE_MANIFEST_DIGEST";

/// The executor's carrier: `env:` entries on its Deployment container.
const EXECUTOR_CARRIER: &str = "deploy/platform/executor.yaml";
/// The host's carrier: per host group `extraArgs`, flags rather than env.
const HOST_CARRIER: &str = "deploy/platform/values-host-receiving-pat.yaml";

const ARTIFACT_BASE_FLAG: &str = "--release-artifact-base";
const MANIFEST_DIGEST_FLAG: &str = "--release-manifest-digest";

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

/// Print the release lines for one minted release.
pub async fn run(args: PrintReleaseEnvArgs) -> anyhow::Result<()> {
    let effective_release_id = i32::try_from(args.effective_release_id)
        .context("effective-release-id exceeds the PostgreSQL integer carrier")?;
    let (mut client, connection) = tokio_postgres::connect(&args.database_url, NoTls)
        .await
        .context("connect to the release snapshot database")?;
    let connection_task = tokio::spawn(connection);
    let read = select_snapshot(&mut client, &args.tenant, effective_release_id).await;
    let canonical_bytes = match read {
        Ok(canonical_bytes) => {
            drop(client);
            connection_task
                .await
                .context("join the release snapshot connection")?
                .context("drive the release snapshot connection")?;
            canonical_bytes
        }
        Err(error) => {
            connection_task.abort();
            return Err(error);
        }
    };
    // The digest is re-derived from the bytes rather than read from a second
    // column, exactly as the publisher does: one carrier of release identity.
    let (_, manifest_digest) = ServingManifest::from_canonical_bytes(&canonical_bytes)
        .context("the frozen release snapshot is not a canonical format-3 manifest")?;
    print!("{}", release_lines(&args.artifact_base, &manifest_digest));
    Ok(())
}

/// Render the release lines each carrier takes, labelled by carrier file.
fn release_lines(artifact_base: &str, manifest_digest: &ManifestDigest) -> String {
    format!(
        "# {EXECUTOR_CARRIER} env:\n\
         {ARTIFACT_BASE_ENV}={artifact_base}\n\
         {MANIFEST_DIGEST_ENV}={manifest_digest}\n\
         # {HOST_CARRIER} hostGroups[].extraArgs\n\
         {ARTIFACT_BASE_FLAG}={artifact_base}\n\
         {MANIFEST_DIGEST_FLAG}={manifest_digest}\n"
    )
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::*;

    /// Host command for the flattened argument surface under test.
    #[derive(Debug, clap::Parser)]
    struct PrintProbe {
        #[command(flatten)]
        args: PrintReleaseEnvArgs,
    }

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

    fn digest() -> ManifestDigest {
        ManifestDigest::parse(format!("sha256:{}", "7".repeat(64)))
            .expect("the fixture digest is canonical")
    }
    #[test]
    fn one_release_prints_both_carriers_and_nothing_else() {
        let printed = release_lines("registry.example/wamn/releases", &digest());
        assert_eq!(
            printed,
            format!(
                "# deploy/platform/executor.yaml env:\n\
                 WAMN_RELEASE_ARTIFACT_BASE=registry.example/wamn/releases\n\
                 WAMN_RELEASE_MANIFEST_DIGEST=sha256:{seven}\n\
                 # deploy/platform/values-host-receiving-pat.yaml hostGroups[].extraArgs\n\
                 --release-artifact-base=registry.example/wamn/releases\n\
                 --release-manifest-digest=sha256:{seven}\n",
                seven = "7".repeat(64)
            )
        );
        assert_eq!(printed.lines().count(), 6);
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
