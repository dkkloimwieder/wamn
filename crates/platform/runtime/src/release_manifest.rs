//! The release-manifest weld — one load, one verification, four readers.
//!
//! A pod carries exactly one release. Its serving manifest arrives as an
//! immutable, digest-named ConfigMap ([`RELEASE_MANIFEST_MOUNT_PATH`]) and its
//! release identity as a stable-named one ([`RELEASE_IDENTITY_MOUNT_PATH`]).
//! [`ReleaseManifestWeld::load`] reads both once, verifies the manifest against
//! the digest the identity names, and holds the parsed document for the life of
//! the process.
//!
//! # This is a weld, not a cache
//!
//! Do not add invalidation, a TTL, a refresh, an eviction policy, or a
//! revalidation hook, and do not rename this a cache. The digest *is* the
//! identity: different content is a different digest, a different ConfigMap, a
//! different pod template, and therefore a different pod. There is no state in
//! which the held manifest is stale with respect to the digest it was verified
//! against, so there is nothing to invalidate. `docs/deployment-simplification-
//! spec.md`'s "cache forever" means exactly process-lifetime immutability, never
//! durability.
//!
//! The one genuine cache nearby is
//! [`ResolutionPlanCache`](crate::plugins::runner_plan_supply) over *plan
//! bytes*, which is entry-bounded, tenant-scoped and evicting because plan bytes
//! are fetched by digest and may be dropped and re-fetched. The manifest is read
//! once from a file and never re-read. Keep the two apart.
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

use std::path::{Path, PathBuf};

use wamn_catalog::{
    RELEASE_IDENTITY_DIGEST_KEY, RELEASE_IDENTITY_MOUNT_PATH, RELEASE_IDENTITY_VERSION_KEY,
    RELEASE_MANIFEST_FILE_NAME, RELEASE_MANIFEST_MOUNT_PATH, ServingManifest,
};

/// Stable classification for a refused weld construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WeldErrorKind {
    /// A release-identity file is missing or unreadable.
    IdentityUnreadable,
    /// A release-identity value is present but not the shape it must be.
    IdentityMalformed,
    /// The manifest file is missing or unreadable.
    ManifestUnreadable,
    /// The manifest bytes failed parse, validation, canonicality, or the digest
    /// check — every refusal [`ServingManifest::read`] can raise.
    ManifestRejected,
    /// The identity's release version disagrees with the manifest's own.
    ReleaseVersionMismatch,
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

/// The release a pod carries, as read from its release-identity mount.
///
/// This mirrors the `(release version, manifest digest)` pair that the
/// production claim records write-once onto a run
/// ([`ReleaseIdentity`](crate::plugins::wamn_postgres::ReleaseIdentity)). It is
/// host-injected identity, never guest-supplied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CarriedRelease {
    /// The release (catalog) version — `runs.release_version`.
    pub release_version: i32,
    /// The serving manifest's `sha256:<hex>` digest — `runs.manifest_digest`.
    pub manifest_digest: String,
}

/// The one loaded, digest-verified serving manifest a pod resolves against.
#[derive(Debug)]
pub struct ReleaseManifestWeld {
    release: CarriedRelease,
    manifest: ServingManifest,
}

impl ReleaseManifestWeld {
    /// Load and verify from the standard mount paths.
    ///
    /// A pod whose manifest is absent, unreadable, unparseable, non-canonical,
    /// or digest-mismatched must not serve, so every failure here is fatal to
    /// host construction.
    pub fn load() -> Result<Self, WeldError> {
        Self::load_from(
            Path::new(RELEASE_IDENTITY_MOUNT_PATH),
            Path::new(RELEASE_MANIFEST_MOUNT_PATH),
        )
    }

    /// Load and verify from explicit mount roots.
    ///
    /// Reads are blocking `std::fs`: this runs exactly once during host
    /// construction, before the pod serves anything, and never on a request
    /// path.
    pub fn load_from(identity_root: &Path, manifest_root: &Path) -> Result<Self, WeldError> {
        let release = read_carried_release(identity_root)?;

        // ConfigMap projections are byte-exact, and `ServingManifest::read`
        // admits only the canonical encoding — so these bytes are compared as
        // read, with no trimming. A trailing newline is a different document.
        let manifest_path = manifest_root.join(RELEASE_MANIFEST_FILE_NAME);
        let bytes = std::fs::read(&manifest_path).map_err(|error| {
            WeldError::new(
                WeldErrorKind::ManifestUnreadable,
                format!("read serving manifest {}: {error}", manifest_path.display()),
            )
        })?;

        let manifest =
            ServingManifest::read(&release.manifest_digest, &bytes).map_err(|error| {
                WeldError::new(
                    WeldErrorKind::ManifestRejected,
                    format!(
                        "serving manifest {} refused against digest {}: {error}",
                        manifest_path.display(),
                        release.manifest_digest
                    ),
                )
            })?;

        // The identity names a version and the manifest carries its own. A pod
        // whose two halves disagree is mis-templated: it would record one
        // release onto a run while resolving against another.
        if i64::from(manifest.release.catalog_version) != i64::from(release.release_version) {
            return Err(WeldError::new(
                WeldErrorKind::ReleaseVersionMismatch,
                format!(
                    "release identity names version {} but the manifest was minted for {}",
                    release.release_version, manifest.release.catalog_version
                ),
            ));
        }

        Ok(Self { release, manifest })
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

/// Read the `(release version, manifest digest)` pair from the identity mount.
///
/// Both halves must be present: the run plane's `runs_release_record_check`
/// forbids a half record, so half a pair could never be recorded onto a run.
fn read_carried_release(identity_root: &Path) -> Result<CarriedRelease, WeldError> {
    let version = read_identity_value(identity_root, RELEASE_IDENTITY_VERSION_KEY)?;
    let manifest_digest = read_identity_value(identity_root, RELEASE_IDENTITY_DIGEST_KEY)?;

    let release_version = version.parse::<i32>().map_err(|error| {
        WeldError::new(
            WeldErrorKind::IdentityMalformed,
            format!("release identity {RELEASE_IDENTITY_VERSION_KEY} {version:?}: {error}"),
        )
    })?;
    if release_version <= 0 {
        return Err(WeldError::new(
            WeldErrorKind::IdentityMalformed,
            format!(
                "release identity {RELEASE_IDENTITY_VERSION_KEY} must be positive, got \
                 {release_version}"
            ),
        ));
    }

    Ok(CarriedRelease {
        release_version,
        manifest_digest,
    })
}

/// Read one release-identity key, projected as a file named for that key.
///
/// Identity values are scalars, so surrounding whitespace is trimmed — a
/// hand-authored ConfigMap commonly leaves a trailing newline on a short scalar.
/// The manifest bytes are deliberately NOT trimmed: there the encoding is the
/// identity.
fn read_identity_value(identity_root: &Path, key: &str) -> Result<String, WeldError> {
    let path: PathBuf = identity_root.join(key);
    let raw = std::fs::read_to_string(&path).map_err(|error| {
        WeldError::new(
            WeldErrorKind::IdentityUnreadable,
            format!("read release identity {}: {error}", path.display()),
        )
    })?;
    let value = raw.trim();
    if value.is_empty() {
        return Err(WeldError::new(
            WeldErrorKind::IdentityMalformed,
            format!("release identity {} is empty", path.display()),
        ));
    }
    Ok(value.to_string())
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

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

    /// A private scratch mount pair, named for its test so runs cannot collide.
    struct Mounts {
        root: PathBuf,
    }

    impl Mounts {
        fn new(test: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("wamn-weld-{}-{test}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("identity")).expect("scratch identity dir");
            std::fs::create_dir_all(root.join("manifest")).expect("scratch manifest dir");
            Self { root }
        }

        fn identity(&self) -> PathBuf {
            self.root.join("identity")
        }

        fn manifest_dir(&self) -> PathBuf {
            self.root.join("manifest")
        }

        fn write_identity(&self, version: &str, digest: &str) -> &Self {
            std::fs::write(self.identity().join(RELEASE_IDENTITY_VERSION_KEY), version)
                .expect("write version");
            std::fs::write(self.identity().join(RELEASE_IDENTITY_DIGEST_KEY), digest)
                .expect("write digest");
            self
        }

        fn write_manifest_bytes(&self, bytes: &[u8]) -> &Self {
            std::fs::write(self.manifest_dir().join(RELEASE_MANIFEST_FILE_NAME), bytes)
                .expect("write manifest");
            self
        }

        /// Write the canonical fixture and the identity that matches it.
        fn write_consistent(&self) -> ServingManifest {
            let manifest = fixture();
            self.write_identity(&RELEASE_VERSION.to_string(), &manifest.digest());
            self.write_manifest_bytes(&manifest.canonical_bytes());
            manifest
        }

        fn load(&self) -> Result<ReleaseManifestWeld, WeldError> {
            ReleaseManifestWeld::load_from(&self.identity(), &self.manifest_dir())
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
        let expected = mounts.write_consistent();

        let weld = mounts.load().expect("well-formed mount loads");

        assert_eq!(weld.release().release_version, RELEASE_VERSION as i32);
        assert_eq!(weld.release().manifest_digest, expected.digest());
        assert_eq!(weld.manifest(), &expected);
        // The reader-facing property: reader 1 walks this, and the walk is total
        // because `read` already proved every call edge names a member flow.
        assert_eq!(
            weld.manifest().reachable_flows("root"),
            BTreeSet::from(["root".to_string(), "callee".to_string()])
        );
    }

    #[test]
    fn an_absent_manifest_refuses() {
        let mounts = Mounts::new("no-manifest");
        let manifest = fixture();
        mounts.write_identity(&RELEASE_VERSION.to_string(), &manifest.digest());

        assert_eq!(
            mounts.load().expect_err("absent manifest refuses").kind(),
            WeldErrorKind::ManifestUnreadable
        );
    }

    #[test]
    fn an_absent_identity_refuses() {
        let mounts = Mounts::new("no-identity");
        mounts.write_manifest_bytes(&fixture().canonical_bytes());

        assert_eq!(
            mounts.load().expect_err("absent identity refuses").kind(),
            WeldErrorKind::IdentityUnreadable
        );
    }

    #[test]
    fn half_an_identity_refuses() {
        let mounts = Mounts::new("half-identity");
        let manifest = fixture();
        mounts.write_manifest_bytes(&manifest.canonical_bytes());
        std::fs::write(
            mounts.identity().join(RELEASE_IDENTITY_VERSION_KEY),
            RELEASE_VERSION.to_string(),
        )
        .expect("write version only");

        // The run plane's `runs_release_record_check` forbids a half record, so
        // a pod carrying half a pair must never reach the claim path.
        assert_eq!(
            mounts.load().expect_err("half identity refuses").kind(),
            WeldErrorKind::IdentityUnreadable
        );
    }

    #[test]
    fn a_non_numeric_or_empty_version_refuses() {
        for (test, version) in [("bad-version", "seven"), ("empty-version", "  \n")] {
            let mounts = Mounts::new(test);
            let manifest = fixture();
            mounts.write_identity(version, &manifest.digest());
            mounts.write_manifest_bytes(&manifest.canonical_bytes());

            assert_eq!(
                mounts.load().expect_err("malformed version refuses").kind(),
                WeldErrorKind::IdentityMalformed,
                "version {version:?}"
            );
        }
    }

    #[test]
    fn an_empty_identity_digest_refuses_as_an_identity_fault() {
        let mounts = Mounts::new("empty-digest");
        let manifest = fixture();
        mounts.write_identity(&RELEASE_VERSION.to_string(), "\n  \n");
        mounts.write_manifest_bytes(&manifest.canonical_bytes());

        // Classified against the mount that is actually wrong. Without the
        // emptiness check an empty digest reaches `ServingManifest::read` and
        // comes back as a manifest fault, pointing the operator at the healthy
        // mount. The version half cannot show this: an empty version fails its
        // own parse either way.
        assert_eq!(
            mounts.load().expect_err("empty digest refuses").kind(),
            WeldErrorKind::IdentityMalformed
        );
    }

    #[test]
    fn a_digest_the_bytes_do_not_match_refuses() {
        let mounts = Mounts::new("digest-mismatch");
        let manifest = fixture();
        let foreign = "sha256:0000000000000000000000000000000000000000000000000000000000000000";
        mounts.write_identity(&RELEASE_VERSION.to_string(), foreign);
        mounts.write_manifest_bytes(&manifest.canonical_bytes());

        assert_eq!(
            mounts.load().expect_err("digest mismatch refuses").kind(),
            WeldErrorKind::ManifestRejected
        );
    }

    #[test]
    fn a_trailing_newline_on_the_manifest_refuses() {
        let mounts = Mounts::new("manifest-newline");
        let manifest = fixture();
        let mut bytes = manifest.canonical_bytes();
        bytes.push(b'\n');
        mounts.write_identity(&RELEASE_VERSION.to_string(), &manifest.digest());
        mounts.write_manifest_bytes(&bytes);

        // The manifest's encoding IS its identity, so unlike the identity
        // scalars these bytes are never trimmed.
        assert_eq!(
            mounts.load().expect_err("trailing newline refuses").kind(),
            WeldErrorKind::ManifestRejected
        );
    }

    #[test]
    fn a_re_indented_manifest_refuses() {
        let mounts = Mounts::new("manifest-reindented");
        let manifest = fixture();
        let value: serde_json::Value =
            serde_json::from_slice(&manifest.canonical_bytes()).expect("canonical bytes parse");
        let pretty = serde_json::to_vec_pretty(&value).expect("re-indent");
        mounts.write_identity(&RELEASE_VERSION.to_string(), &manifest.digest());
        mounts.write_manifest_bytes(&pretty);

        // Same content, different encoding: refused rather than served under a
        // foreign digest.
        assert_eq!(
            mounts.load().expect_err("non-canonical refuses").kind(),
            WeldErrorKind::ManifestRejected
        );
    }

    #[test]
    fn surrounding_whitespace_on_identity_scalars_is_tolerated() {
        let mounts = Mounts::new("identity-whitespace");
        let manifest = fixture();
        mounts.write_identity(
            &format!("  {RELEASE_VERSION}\n"),
            &format!("{}\n", manifest.digest()),
        );
        mounts.write_manifest_bytes(&manifest.canonical_bytes());

        let weld = mounts.load().expect("trimmed identity loads");
        assert_eq!(weld.release().manifest_digest, manifest.digest());
    }

    #[test]
    fn an_identity_version_the_manifest_disagrees_with_refuses() {
        let mounts = Mounts::new("version-mismatch");
        let manifest = fixture();
        // A valid, verifiable manifest — but minted for a different release than
        // the identity this pod carries and would record onto its runs.
        mounts.write_identity(&(RELEASE_VERSION + 1).to_string(), &manifest.digest());
        mounts.write_manifest_bytes(&manifest.canonical_bytes());

        assert_eq!(
            mounts.load().expect_err("version mismatch refuses").kind(),
            WeldErrorKind::ReleaseVersionMismatch
        );
    }
}
