//! Publish canonical format-3 serving-manifest bytes as one OCI data artifact.
//!
//! The manifest's RFC 8785 SHA-256 identity derives its immutable OCI tag. An
//! exact retry pulls and verifies the existing artifact and performs no push.
//! A tag holding any other layout or bytes refuses instead of being replaced.
//!
//! The bytes come only from the `catalog.release_manifest_v3_snapshots` row the
//! mint froze. Publication therefore cannot attest caller-supplied bytes that
//! were never verified against the release identity.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context as _;
use clap::Args;
use oci_client::client::{ClientConfig, ClientProtocol};
use oci_client::errors::{OciDistributionError, OciErrorCode};
use oci_client::manifest::OciImageManifest;
use oci_client::secrets::RegistryAuth;
use oci_client::{Client as OciClient, Reference};
use tokio_postgres::{Client as PgClient, NoTls};
use wamn_catalog::{ManifestDigest, ServingManifest, ServingRelease};
use wamn_runtime::registry_credentials::{RegistryCredentials, read_registry_credentials};
use wamn_runtime::release_manifest_artifact::{
    RELEASE_MANIFEST_CONFIG_BYTES, ReleaseManifestArtifactBlobs, release_manifest_artifact_layout,
    release_manifest_artifact_reference, verify_release_manifest_artifact_layout,
};

use crate::publish_release::{
    DeploymentCoordinate, read_release_snapshot, report_deployment_coordinate,
};

/// Bound each registry connect/read phase without adding a deployment knob.
const REGISTRY_IO_TIMEOUT: Duration = Duration::from_secs(30);

/// Stable prefix every release-manifest OCI refusal renders with.
pub const RELEASE_MANIFEST_PUBLISH_REFUSAL: &str = "release-manifest-publish-refused";

/// Stable classification of a release-manifest publication refusal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReleaseManifestPublishErrorKind {
    Document,
    Reference,
    Credential,
    Transport,
    Conflict,
}

impl ReleaseManifestPublishErrorKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Document => "document",
            Self::Reference => "reference",
            Self::Credential => "credential",
            Self::Transport => "transport",
            Self::Conflict => "conflict",
        }
    }
}

/// Contextual refusal from the release-manifest OCI boundary.
#[derive(Debug)]
pub struct ReleaseManifestPublishError {
    kind: ReleaseManifestPublishErrorKind,
    refusal: &'static str,
    detail: String,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl ReleaseManifestPublishError {
    /// Stable refusal class for callers that must not match display text.
    pub const fn kind(&self) -> ReleaseManifestPublishErrorKind {
        self.kind
    }

    /// Stable literal naming the rejected invariant.
    pub const fn refusal(&self) -> &'static str {
        self.refusal
    }

    fn new(
        kind: ReleaseManifestPublishErrorKind,
        refusal: &'static str,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            refusal,
            detail: detail.into(),
            source: None,
        }
    }

    fn with_source(
        kind: ReleaseManifestPublishErrorKind,
        refusal: &'static str,
        detail: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind,
            refusal,
            detail: detail.into(),
            source: Some(Box::new(source)),
        }
    }
}

impl fmt::Display for ReleaseManifestPublishError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{RELEASE_MANIFEST_PUBLISH_REFUSAL} ({}; {}): {}",
            self.kind.as_str(),
            self.refusal,
            self.detail
        )
    }
}

impl std::error::Error for ReleaseManifestPublishError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_deref().map(|source| source as _)
    }
}

/// Whether this invocation created the artifact or proved an exact retry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReleaseManifestPublishDisposition {
    Pushed,
    AlreadyPresent,
}

/// Verified result of publishing one canonical serving manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedReleaseManifest {
    pub digest: ManifestDigest,
    pub disposition: ReleaseManifestPublishDisposition,
    /// Effective-release coordinate carried by the published bytes.
    pub release: ServingRelease,
}

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
    /// Key the published bytes for attestation under this invocation's placement.
    fn deployment_coordinate(&self, release: &ServingRelease) -> DeploymentCoordinate {
        DeploymentCoordinate::new(&self.org, &self.project, release)
    }
}

/// Publish one canonical release manifest and print its content digest.
pub async fn run(args: PushReleaseManifestArgs) -> anyhow::Result<()> {
    let canonical_bytes = canonical_release_bytes(&args).await?;
    let published = publish_release_manifest(
        &canonical_bytes,
        &args.artifact_base,
        args.insecure_registry,
        &args.registry_auth_file,
    )
    .await?;
    let coordinate = args.deployment_coordinate(&published.release);
    report_deployment_coordinate(&coordinate, &published.digest);
    // wamn-0h0g.8.27: the OCI push IS the deployment event this attestation
    // records (wamn-0h0g.8.21's own stated trigger), so the write lands here and
    // on no other verb. Its foreign key refuses a release whose identity the mint
    // never projected — bytes that reached a registry without ever being minted
    // cannot be attested into existence.
    crate::publish_release::attest_deployment(
        &args.control_database_url,
        &coordinate,
        &published.digest,
    )
    .await?;
    println!("{}", published.digest);
    Ok(())
}

/// Read the bytes to publish from the release that minted them.
async fn canonical_release_bytes(args: &PushReleaseManifestArgs) -> anyhow::Result<Vec<u8>> {
    let effective_release_id = i32::try_from(args.effective_release_id)
        .context("effective-release-id exceeds the PostgreSQL integer carrier")?;

    let (mut client, connection) = tokio_postgres::connect(&args.database_url, NoTls)
        .await
        .context("connect to the release snapshot database")?;
    let connection_task = tokio::spawn(connection);
    let read = select_snapshot(&mut client, &args.tenant, effective_release_id).await;
    match read {
        Ok(canonical_bytes) => {
            drop(client);
            connection_task
                .await
                .context("join the release snapshot connection")?
                .context("drive the release snapshot connection")?;
            Ok(canonical_bytes)
        }
        Err(error) => {
            connection_task.abort();
            Err(error)
        }
    }
}

/// Read one minted release's exact canonical bytes in its own transaction.
///
/// Shared with `print-release-env` (wamn-duyl) so the two readers of a frozen
/// snapshot cannot drift apart on locking or on the absent-release refusal.
pub(crate) async fn select_snapshot(
    client: &mut PgClient,
    tenant: &str,
    effective_release_id: i32,
) -> anyhow::Result<Vec<u8>> {
    let transaction = client
        .transaction()
        .await
        .context("begin the release snapshot read")?;
    let snapshot = read_release_snapshot(&transaction, tenant, effective_release_id)
        .await
        .context("read the minted release snapshot")?;
    transaction
        .commit()
        .await
        .context("close the release snapshot read")?;
    snapshot.with_context(|| {
        format!(
            "tenant {tenant:?} effective release {effective_release_id} \
             has no minted format-3 release snapshot"
        )
    })
}

/// Publish canonical format-3 bytes or prove their exact artifact already exists.
pub async fn publish_release_manifest(
    canonical_bytes: &[u8],
    artifact_base: &str,
    insecure_registry: bool,
    registry_auth_file: &Path,
) -> Result<PublishedReleaseManifest, ReleaseManifestPublishError> {
    let (admitted, digest) =
        ServingManifest::from_canonical_bytes(canonical_bytes).map_err(|source| {
            ReleaseManifestPublishError::with_source(
                ReleaseManifestPublishErrorKind::Document,
                "release-manifest-document-refused",
                "input is not canonical format-3 ServingManifest JSON",
                source,
            )
        })?;
    let release = admitted.release;
    let artifact =
        release_manifest_artifact_reference(artifact_base, digest.as_str()).map_err(|source| {
            ReleaseManifestPublishError::with_source(
                ReleaseManifestPublishErrorKind::Reference,
                "release-manifest-artifact-reference-refused",
                "artifact base or manifest digest cannot form an immutable OCI reference",
                source,
            )
        })?;
    let credentials =
        read_registry_credentials(registry_auth_file, artifact.registry()).map_err(|source| {
            ReleaseManifestPublishError::with_source(
                ReleaseManifestPublishErrorKind::Credential,
                "release-manifest-registry-credential-refused",
                format!("load push credential for registry {}", artifact.registry()),
                source,
            )
        })?;
    let reference = Reference::with_tag(
        artifact.registry().to_owned(),
        artifact.repository().to_owned(),
        artifact.tag().to_owned(),
    );
    let client = registry_client(artifact.registry(), insecure_registry);
    let auth = registry_auth(&credentials);

    if probe_exact_artifact(&client, &reference, &auth, canonical_bytes, &digest).await? {
        return Ok(PublishedReleaseManifest {
            digest,
            disposition: ReleaseManifestPublishDisposition::AlreadyPresent,
            release,
        });
    }

    let (layer, config, manifest) = release_manifest_artifact_layout(canonical_bytes);
    client
        .push(
            &reference,
            std::slice::from_ref(&layer),
            config,
            &auth,
            Some(manifest),
        )
        .await
        .map_err(|source| {
            ReleaseManifestPublishError::with_source(
                ReleaseManifestPublishErrorKind::Transport,
                "release-manifest-artifact-push-failed",
                format!("push release-manifest artifact {reference}"),
                source,
            )
        })?;

    if !probe_exact_artifact(&client, &reference, &auth, canonical_bytes, &digest).await? {
        return Err(ReleaseManifestPublishError::new(
            ReleaseManifestPublishErrorKind::Transport,
            "release-manifest-artifact-not-visible",
            format!("pushed release-manifest artifact {reference} is not readable"),
        ));
    }
    Ok(PublishedReleaseManifest {
        digest,
        disposition: ReleaseManifestPublishDisposition::Pushed,
        release,
    })
}

/// One registry client for this publish, built here rather than borrowed from
/// `wash_runtime::oci`.
///
/// That module exposes no client-construction seam: its transfer surface is
/// `pull_component`/`push_component`, both fixed to `WASM_LAYER_MEDIA_TYPE` and
/// a `WasmConfig` blob, and `push_component` refuses non-wasm bytes outright
/// through `wit_component::decode_reader`. A release manifest is canonical
/// JSON. This path also needs `HttpsExcept` for a single insecure registry and
/// `OciErrorCode::ManifestUnknown` discrimination for the exact-retry probe,
/// neither of which survives that API. See standing trigger 5 in
/// `docs/architecture/native-alignment-ledger.md` (`wamn-kdhw`).
fn registry_client(registry: &str, insecure_registry: bool) -> OciClient {
    let protocol = if insecure_registry {
        ClientProtocol::HttpsExcept(vec![registry.to_owned()])
    } else {
        ClientProtocol::Https
    };
    OciClient::new(ClientConfig {
        protocol,
        read_timeout: Some(REGISTRY_IO_TIMEOUT),
        connect_timeout: Some(REGISTRY_IO_TIMEOUT),
        ..ClientConfig::default()
    })
}

fn registry_auth(credentials: &RegistryCredentials) -> RegistryAuth {
    RegistryAuth::Basic(
        credentials.username().to_owned(),
        credentials.password().to_owned(),
    )
}

async fn probe_exact_artifact(
    client: &OciClient,
    reference: &Reference,
    auth: &RegistryAuth,
    expected_bytes: &[u8],
    expected_digest: &ManifestDigest,
) -> Result<bool, ReleaseManifestPublishError> {
    let (manifest, _) = match client.pull_image_manifest(reference, auth).await {
        Ok(found) => found,
        Err(source) if artifact_is_absent(&source) => return Ok(false),
        Err(source) => {
            return Err(ReleaseManifestPublishError::with_source(
                ReleaseManifestPublishErrorKind::Transport,
                "release-manifest-artifact-probe-failed",
                format!("probe release-manifest artifact {reference}"),
                source,
            ));
        }
    };
    let verified = verify_manifest_layout(
        &manifest,
        expected_digest.as_str(),
        expected_bytes.len(),
        reference,
    )?;

    let mut body = Vec::new();
    client
        .pull_blob(reference, verified.layer, &mut body)
        .await
        .map_err(|source| {
            ReleaseManifestPublishError::with_source(
                ReleaseManifestPublishErrorKind::Transport,
                "release-manifest-artifact-body-unavailable",
                format!("pull release-manifest body {reference}"),
                source,
            )
        })?;
    if body != expected_bytes {
        return Err(conflict(
            reference,
            "release-manifest-artifact-body-mismatch",
        ));
    }

    let mut config = Vec::new();
    client
        .pull_blob(reference, verified.config, &mut config)
        .await
        .map_err(|source| {
            ReleaseManifestPublishError::with_source(
                ReleaseManifestPublishErrorKind::Transport,
                "release-manifest-artifact-config-unavailable",
                format!("pull release-manifest config {reference}"),
                source,
            )
        })?;
    if config != RELEASE_MANIFEST_CONFIG_BYTES {
        return Err(conflict(
            reference,
            "release-manifest-artifact-config-body-mismatch",
        ));
    }
    Ok(true)
}

/// Translate the shared layout contract's refusal into this boundary's error.
fn verify_manifest_layout<'a>(
    manifest: &'a OciImageManifest,
    expected_digest: &str,
    expected_size: usize,
    reference: &Reference,
) -> Result<ReleaseManifestArtifactBlobs<'a>, ReleaseManifestPublishError> {
    verify_release_manifest_artifact_layout(manifest, expected_digest, Some(expected_size))
        .map_err(|refusal| conflict(reference, refusal.refusal()))
}

fn artifact_is_absent(error: &OciDistributionError) -> bool {
    match error {
        OciDistributionError::ImageManifestNotFoundError(_) => true,
        OciDistributionError::RegistryError { envelope, .. } => {
            !envelope.errors.is_empty()
                && envelope.errors.iter().all(|error| {
                    matches!(
                        error.code,
                        OciErrorCode::ManifestUnknown
                            | OciErrorCode::NameUnknown
                            | OciErrorCode::NotFound
                    )
                })
        }
        OciDistributionError::ServerError { code: 404, .. } => true,
        _ => false,
    }
}

fn conflict(reference: &Reference, refusal: &'static str) -> ReleaseManifestPublishError {
    ReleaseManifestPublishError::new(
        ReleaseManifestPublishErrorKind::Conflict,
        refusal,
        format!("existing release-manifest artifact {reference} is not exact"),
    )
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;
    use wamn_runtime::release_manifest_artifact::RELEASE_MANIFEST_ARTIFACT_MEDIA_TYPE;

    use super::*;

    /// Host command for the flattened argument surface under test.
    #[derive(Debug, clap::Parser)]
    struct PushProbe {
        #[command(flatten)]
        args: PushReleaseManifestArgs,
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

    const PLACEMENT: [&str; 4] = ["--org", "acme", "--project", "billing"];

    fn parse(source: &[&str]) -> Result<PushReleaseManifestArgs, clap::Error> {
        let mut argv = vec!["push-release-manifest"];
        argv.extend_from_slice(source);
        argv.extend_from_slice(&DESTINATION);
        argv.extend_from_slice(&PLACEMENT);
        PushProbe::try_parse_from(argv).map(|probe| probe.args)
    }

    const CANONICAL_MANIFEST: &[u8] = br#"{"attachments":{},"components":[{"component":"http-request","digest":"sha256:1111111111111111111111111111111111111111111111111111111111111111","interface-version":"0.1","package-id":"orders"}],"format-version":3,"registrations":{},"release":{"effective-release-id":3,"environment":"prod","packages":[{"package-id":"orders","package-version":"1.0.0"}],"tenant-id":"tenant-a"},"wirings":[{"graph-hash":"sha256:3333333333333333333333333333333333333333333333333333333333333333","package-id":"orders","wiring-id":"orders","wiring-version":1}]}"#;

    fn fixture_reference() -> Reference {
        Reference::with_tag(
            "registry.example".to_owned(),
            "wamn/releases".to_owned(),
            "a".repeat(64),
        )
    }

    #[test]
    fn published_bytes_carry_their_own_half_of_the_attestation_key() {
        let (manifest, _) = ServingManifest::from_canonical_bytes(CANONICAL_MANIFEST)
            .expect("the fixture is canonical format-3 bytes");
        let coordinate = DeploymentCoordinate::new("acme", "billing", &manifest.release);

        // The environment is whatever the pushed bytes were projected for, read
        // out of those exact bytes rather than defaulted or re-typed by hand.
        assert_eq!(coordinate.triple.env.as_str(), "prod");
        assert_eq!(coordinate.tenant_id, "tenant-a");
        assert_eq!(coordinate.effective_release_id, 3);
        assert_eq!(coordinate.triple.org, "acme");
        assert_eq!(coordinate.triple.project, "billing");
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
        for placement in [vec!["--org", "acme"], vec!["--project", "billing"], vec![]] {
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
        assert_eq!(placed.org, "acme");
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
            .expect("the fixture is canonical format-3 bytes");
        let coordinate = args.deployment_coordinate(&manifest.release);

        assert_eq!(coordinate.triple.org, "acme");
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
    fn exact_layout_carries_only_canonical_manifest_bytes() {
        let (_, digest) = ServingManifest::from_canonical_bytes(CANONICAL_MANIFEST)
            .expect("fixture is a canonical format-3 manifest");
        let (layer, config, manifest) = release_manifest_artifact_layout(CANONICAL_MANIFEST);
        let verified = verify_manifest_layout(
            &manifest,
            digest.as_str(),
            CANONICAL_MANIFEST.len(),
            &fixture_reference(),
        )
        .expect("publisher layout verifies");

        assert_eq!(&layer.data[..], CANONICAL_MANIFEST);
        assert_eq!(&config.data[..], RELEASE_MANIFEST_CONFIG_BYTES);
        assert_eq!(verified.layer.digest, digest.as_str());
        assert_eq!(manifest.layers.len(), 1);
    }

    #[test]
    fn format_one_and_noncanonical_documents_refuse_before_transport() {
        let legacy = std::str::from_utf8(CANONICAL_MANIFEST)
            .expect("fixture is UTF-8")
            .replacen("\"format-version\":3", "\"format-version\":2", 1);
        let legacy = ServingManifest::from_canonical_bytes(legacy.as_bytes())
            .expect_err("format one refuses");
        assert!(format!("{legacy}").contains("unsupported-serving-manifest-version"));

        let mut indented = CANONICAL_MANIFEST.to_vec();
        indented.insert(1, b' ');
        assert!(ServingManifest::from_canonical_bytes(&indented).is_err());
    }

    #[test]
    fn wrong_or_multi_layer_layout_refuses_as_conflict() {
        let (_, digest) = ServingManifest::from_canonical_bytes(CANONICAL_MANIFEST)
            .expect("fixture is a canonical format-3 manifest");
        let (_, _, mut manifest) = release_manifest_artifact_layout(CANONICAL_MANIFEST);
        manifest.layers[0].media_type = "application/octet-stream".to_owned();
        let error = verify_manifest_layout(
            &manifest,
            digest.as_str(),
            CANONICAL_MANIFEST.len(),
            &fixture_reference(),
        )
        .expect_err("foreign layer layout refuses");
        assert_eq!(error.kind(), ReleaseManifestPublishErrorKind::Conflict);
        assert_eq!(error.refusal(), "release-manifest-artifact-layer-mismatch");

        let duplicate = manifest.layers[0].clone();
        manifest.layers[0].media_type = RELEASE_MANIFEST_ARTIFACT_MEDIA_TYPE.to_owned();
        manifest.layers.push(duplicate);
        let error = verify_manifest_layout(
            &manifest,
            digest.as_str(),
            CANONICAL_MANIFEST.len(),
            &fixture_reference(),
        )
        .expect_err("multiple layers refuse");
        assert_eq!(
            error.refusal(),
            "release-manifest-artifact-layer-cardinality-mismatch"
        );
    }

    #[tokio::test]
    #[ignore = "requires a disposable authenticated registry"]
    async fn production_publisher_exact_retry_is_a_no_push() {
        let artifact_base = std::env::var("WAMN_RELEASE_MANIFEST_ARTIFACT_BASE")
            .expect("set WAMN_RELEASE_MANIFEST_ARTIFACT_BASE to a disposable repository");
        let registry_auth_file = std::env::var("WAMN_REGISTRY_AUTH_FILE")
            .expect("set WAMN_REGISTRY_AUTH_FILE to its Docker config credential");

        let first = publish_release_manifest(
            CANONICAL_MANIFEST,
            &artifact_base,
            true,
            Path::new(&registry_auth_file),
        )
        .await
        .expect("first publication converges");
        let retry = publish_release_manifest(
            CANONICAL_MANIFEST,
            &artifact_base,
            true,
            Path::new(&registry_auth_file),
        )
        .await
        .expect("exact retry converges");

        assert_eq!(first.digest, retry.digest);
        assert_eq!(
            retry.disposition,
            ReleaseManifestPublishDisposition::AlreadyPresent
        );
    }
}
