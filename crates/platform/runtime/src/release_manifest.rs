//! The release-manifest weld — one load, one verification, four readers.
//!
//! A pod carries exactly one release. Its serving manifest arrives as an
//! immutable, digest-named ConfigMap ([`RELEASE_MANIFEST_MOUNT_PATH`]) — the
//! *sole* carrier of release identity. [`ReleaseManifestWeld::load`] reads it
//! once and derives both halves of the `(release version, manifest digest)` pair
//! from the verified content itself, then holds the parsed document for the life
//! of the process.
//!
//! # This is a weld, not a cache
//!
//! Do not add invalidation, a TTL, a refresh, an eviction policy, or a
//! revalidation hook, and do not rename this a cache. The digest *is* the
//! identity: different content is a different digest, a different ConfigMap, a
//! different pod template, and therefore a different pod. There is no state in
//! which the held manifest is stale with respect to the content it was derived
//! from, so there is nothing to invalidate. `docs/deployment-simplification-
//! spec.md`'s "cache forever" means exactly process-lifetime immutability, never
//! durability.
//!
//! The one genuine cache nearby is
//! [`ResolutionPlanCache`](crate::plugins::runner_plan_supply) over *plan
//! bytes*, which is entry-bounded, tenant-scoped and evicting because plan bytes
//! are fetched by digest and may be dropped and re-fetched. The manifest is read
//! once from a file and never re-read. Keep the two apart.
//!
//! # Identity is derived, never asserted — and pod-to-release binding is the
//! # pod template's, by ratified design
//!
//! There is deliberately no second carrier to check the manifest against, and no
//! comparison of the bytes to the ConfigMap's own name. Such a check would not be
//! a verification: the name inside the container is a string placed by the *same
//! pod template* that mounts the bytes, so comparing them tests the template's
//! internal consistency against itself. A pod cannot second-guess its own birth
//! certificate.
//!
//! What binds this pod to this release is the pod template, and that is not a gap
//! — it is where the wasmCloud-v2 model assigns the fact. The template is
//! GitOps-converged, revision-controlled and rollout-controlled; binding
//! bytes-to-intent is Git's job, and Git already does it. In-container
//! name-matching would rebuild, inside the pod, an attestation the deployment
//! plane already owns.
//!
//! What *is* load-bearing survives, and it is stronger than a name check: the
//! canonical round-trip proves the mounted bytes are well-formed manifest content
//! whose identity is computable. Corruption, truncation, or hand-editing either
//! fails the parse or shifts the digest — and a shifted digest is not a
//! masquerade, it is a correct name for different content, carried honestly into
//! every run record and authority decision downstream. Malformed bytes mean the
//! pod never goes Ready, which is the only refusal that means anything here.
//!
//! # The four enumerated readers
//!
//! Every reader consults this one instance by reference and none of them loads,
//! parses, or digest-verifies a manifest of its own:
//!
//! 1. plan supply — [`crate::plugins::runner_plan_supply`] walks
//!    [`ServingManifest::reachable_flows`] and fetches that set by digest.
//! 2. effect authority — [`crate::plugins::wamn_postgres`] binds a run to its
//!    plan through the manifest its recorded digest names.
//! 3. flow-http routing — serves `RouteDefinition` from
//!    [`ServingManifest::attachments`].
//! 4. jetstream delivery — gates delivery on
//!    [`ServingManifest::registrations`].
//!
//! Readers 1 and 2 run in the flowrunner in-process host, readers 3 and 4 in the
//! wash host. Those are separate processes and cannot share one object, so the
//! rule is one instance *per process*: construct once, hand it out by reference,
//! and never hold two.

use std::path::Path;

use wamn_catalog::{
    ManifestDigest, RELEASE_MANIFEST_FILE_NAME, RELEASE_MANIFEST_MOUNT_PATH, ServingManifest,
};

/// Stable classification for a refused weld construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WeldErrorKind {
    /// The manifest file is missing or unreadable.
    ManifestUnreadable,
    /// The manifest bytes failed parse, validation or canonicality, or carry a
    /// release version the run plane cannot record — every refusal
    /// [`ServingManifest::from_canonical_bytes`] can raise, plus the one width
    /// narrowing this weld performs.
    ManifestRejected,
}

/// A fail-closed weld construction error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WeldError {
    kind: WeldErrorKind,
    detail: Box<str>,
}

impl WeldError {
    fn new(kind: WeldErrorKind, detail: impl Into<Box<str>>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }

    /// The stable classification of this refusal.
    pub fn kind(&self) -> WeldErrorKind {
        self.kind
    }
}

impl std::fmt::Display for WeldError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.detail)
    }
}

impl std::error::Error for WeldError {}

/// The release a pod carries, derived from its verified manifest content.
///
/// This is the `(release version, manifest digest)` pair that the production
/// claim records write-once onto a run
/// ([`ReleaseIdentity`](crate::plugins::wamn_postgres::ReleaseIdentity)). It is
/// host-injected identity, never guest-supplied — and, since it comes out of the
/// same bytes the readers resolve against, the two cannot disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CarriedRelease {
    /// The release (catalog) version — `runs.release_version`. Derived from
    /// [`ServingRelease::catalog_version`](wamn_catalog::ServingRelease), a
    /// header field inside the canonical preimage.
    pub release_version: i32,
    /// The serving manifest's digest — `runs.manifest_digest`. Derived from the
    /// manifest's own canonical bytes.
    pub manifest_digest: ManifestDigest,
}

/// The one loaded, verified serving manifest a pod resolves against.
#[derive(Debug)]
pub struct ReleaseManifestWeld {
    release: CarriedRelease,
    manifest: ServingManifest,
}

impl ReleaseManifestWeld {
    /// Load and verify from the standard mount path.
    ///
    /// A pod whose manifest is absent, unreadable, unparseable or non-canonical
    /// must not serve, so every failure here is fatal to host construction.
    pub fn load() -> Result<Self, WeldError> {
        Self::load_from(Path::new(RELEASE_MANIFEST_MOUNT_PATH))
    }

    /// Load and verify from an explicit mount root.
    ///
    /// Reads are blocking `std::fs`: this runs exactly once during host
    /// construction, before the pod serves anything, and never on a request
    /// path.
    pub fn load_from(manifest_root: &Path) -> Result<Self, WeldError> {
        // ConfigMap projections are byte-exact, and `from_canonical_bytes` admits
        // only the canonical encoding — so these bytes are used as read, with no
        // trimming. A trailing newline is a different document.
        let manifest_path = manifest_root.join(RELEASE_MANIFEST_FILE_NAME);
        let bytes = std::fs::read(&manifest_path).map_err(|error| {
            WeldError::new(
                WeldErrorKind::ManifestUnreadable,
                format!("read serving manifest {}: {error}", manifest_path.display()),
            )
        })?;

        let (manifest, manifest_digest) =
            ServingManifest::from_canonical_bytes(&bytes).map_err(|error| {
                WeldError::new(
                    WeldErrorKind::ManifestRejected,
                    format!(
                        "serving manifest {} refused: {error}",
                        manifest_path.display()
                    ),
                )
            })?;

        // The one narrowing on this path: the manifest carries the release version
        // as `u32`, the run plane records it as `int`. Refuse rather than wrap —
        // `wamn-0h0g.15.65` owns reconciling the four widths this value has.
        let release_version = i32::try_from(manifest.release.catalog_version).map_err(|error| {
            WeldError::new(
                WeldErrorKind::ManifestRejected,
                format!(
                    "serving manifest {} names release version {} which the run plane \
                         cannot record: {error}",
                    manifest_path.display(),
                    manifest.release.catalog_version
                ),
            )
        })?;

        Ok(Self {
            release: CarriedRelease {
                release_version,
                manifest_digest,
            },
            manifest,
        })
    }

    /// The release this pod carries.
    pub fn release(&self) -> &CarriedRelease {
        &self.release
    }

    /// The verified manifest. Every reader takes it from here.
    pub fn manifest(&self) -> &ServingManifest {
        &self.manifest
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::PathBuf;

    use wamn_catalog::{ServingFlow, ServingRelease};

    use super::*;

    const PLAN_A: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const ART_A: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const PLAN_B: &str = "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
    const ART_B: &str = "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
    const RELEASE_VERSION: u32 = 7;

    fn flow(plan: &str, artifact: &str, calls: BTreeSet<String>) -> ServingFlow {
        ServingFlow {
            flow_version: 1,
            plan_hash: plan.into(),
            source_artifact: artifact.into(),
            binding_base_artifact: artifact.into(),
            callable_contract: None,
            calls,
        }
    }

    fn fixture() -> ServingManifest {
        ServingManifest::new(
            ServingRelease {
                tenant_id: "t1".into(),
                catalog_id: "cat".into(),
                catalog_version: RELEASE_VERSION,
                environment: "prod".into(),
            },
            BTreeMap::from([
                (
                    "root".to_string(),
                    flow(PLAN_A, ART_A, BTreeSet::from(["callee".to_string()])),
                ),
                ("callee".to_string(), flow(PLAN_B, ART_B, BTreeSet::new())),
            ]),
            BTreeMap::new(),
            BTreeMap::new(),
        )
        .expect("fixture manifest is valid")
    }

    /// A private scratch mount, named for its test so runs cannot collide.
    struct Mounts {
        root: PathBuf,
    }

    impl Mounts {
        fn new(test: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("wamn-weld-{}-{test}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("manifest")).expect("scratch manifest dir");
            Self { root }
        }

        fn manifest_dir(&self) -> PathBuf {
            self.root.join("manifest")
        }

        fn write_manifest_bytes(&self, bytes: &[u8]) -> &Self {
            std::fs::write(self.manifest_dir().join(RELEASE_MANIFEST_FILE_NAME), bytes)
                .expect("write manifest");
            self
        }

        fn load(&self) -> Result<ReleaseManifestWeld, WeldError> {
            ReleaseManifestWeld::load_from(&self.manifest_dir())
        }
    }

    impl Drop for Mounts {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn a_well_formed_mount_loads_and_verifies_once() {
        let mounts = Mounts::new("ok");
        let expected = fixture();
        mounts.write_manifest_bytes(&expected.canonical_bytes());

        let weld = mounts.load().expect("well-formed mount loads");

        assert_eq!(weld.manifest(), &expected);
        // The reader-facing property: reader 1 walks this, and the walk is total
        // because construction already proved every call edge names a member flow.
        assert_eq!(
            weld.manifest().reachable_flows("root"),
            BTreeSet::from(["root".to_string(), "callee".to_string()])
        );
    }

    #[test]
    fn both_identity_halves_come_from_the_verified_content() {
        let mounts = Mounts::new("derived-identity");
        let expected = fixture();
        mounts.write_manifest_bytes(&expected.canonical_bytes());

        let weld = mounts.load().expect("well-formed mount loads");

        // Neither half is asserted by a carrier. The version is a header field
        // inside the canonical preimage; the digest is over those same bytes. So
        // the pair recorded onto a run and the document resolved against are, by
        // construction, the same fact.
        assert_eq!(
            weld.release().release_version,
            i32::try_from(expected.release.catalog_version).expect("fixture version fits"),
        );
        assert_eq!(weld.release().manifest_digest, expected.digest());
    }

    #[test]
    fn an_absent_manifest_refuses() {
        let mounts = Mounts::new("no-manifest");

        assert_eq!(
            mounts.load().expect_err("absent manifest refuses").kind(),
            WeldErrorKind::ManifestUnreadable
        );
    }

    #[test]
    fn bytes_that_are_not_a_manifest_refuse() {
        let mounts = Mounts::new("garbage");
        mounts.write_manifest_bytes(b"{ not a manifest");

        assert_eq!(
            mounts.load().expect_err("garbage refuses").kind(),
            WeldErrorKind::ManifestRejected
        );
    }

    #[test]
    fn a_trailing_newline_on_the_manifest_refuses() {
        let mounts = Mounts::new("manifest-newline");
        let mut bytes = fixture().canonical_bytes();
        bytes.push(b'\n');
        mounts.write_manifest_bytes(&bytes);

        // The manifest's encoding IS its identity, so these bytes are never
        // trimmed and never re-canonicalized: a repaired document would derive a
        // digest naming content nobody shipped.
        assert_eq!(
            mounts.load().expect_err("trailing newline refuses").kind(),
            WeldErrorKind::ManifestRejected
        );
    }

    #[test]
    fn a_re_indented_manifest_refuses() {
        let mounts = Mounts::new("manifest-reindented");
        let value: serde_json::Value =
            serde_json::from_slice(&fixture().canonical_bytes()).expect("canonical bytes parse");
        let pretty = serde_json::to_vec_pretty(&value).expect("re-indent");
        mounts.write_manifest_bytes(&pretty);

        // Same content, different encoding: refused rather than served under a
        // digest it would not derive.
        assert_eq!(
            mounts.load().expect_err("non-canonical refuses").kind(),
            WeldErrorKind::ManifestRejected
        );
    }
}
