//! Digest-addressed OCI supply of exact execution-plan bytes.
//!
//! `docs/deployment-simplification-spec.md` makes plan bytes content-addressed
//! OCI artifacts: a serving pod reads the plan digests it needs out of its
//! release manifest ([`crate::release_manifest`]) and pulls exactly those, by
//! digest, verifying at transfer. This module is that pull, and nothing else —
//! it decides no policy, holds no cache, and reads no database.
//!
//! # Two independent verifications, neither of which trusts the registry
//!
//! 1. `oci-client` digests the blob stream itself and refuses a body that does
//!    not hash to the layer descriptor it was fetched under.
//! 2. The caller re-hashes the returned bytes against the digest it asked for
//!    ([`crate::plugins::runner_plan_supply`]'s `insert_verified`) before they
//!    reach a cache or a guest.
//!
//! The second is the load-bearing one: it holds even if this module, the
//! transport, or the registry is wrong or hostile, which is why [`PlanSource`]
//! is documented as returning *untrusted* bytes.
//!
//! # The wire literals are defined here, unilaterally, and nothing yet enforces
//! # agreement
//!
//! [`EXECUTION_PLAN_LAYER_MEDIA_TYPE`] and
//! [`EXECUTION_PLAN_CONFIG_MEDIA_TYPE`] are a contract between a publisher and
//! this reader. The publisher (`wamn-0h0g.15.97`) does not exist yet, so these
//! values are chosen here and pinned by a named test rather than agreed with a
//! writer. A publish-side agreement guard is owed against that bead: a
//! self-produced artifact cannot catch a disagreement with a producer nobody has
//! written.

use std::time::Duration;

use async_trait::async_trait;
use oci_client::client::{ClientConfig, ClientProtocol};
use oci_client::manifest::{OciDescriptor, OciImageManifest};
use oci_client::secrets::RegistryAuth;
use oci_client::{Client, Reference};

/// Prefix of every digest this tree names content by. `wamn_catalog` keeps its
/// own copy private, and `runner_plan_supply` spells it inline; one local const
/// is the same fact, not a second algorithm.
const HASH_PREFIX: &str = "sha256:";

/// Media type of the single layer carrying one execution plan's exact bytes.
///
/// The bytes are the plan's canonical serialization, the same bytes
/// [`wamn_catalog::read_execution_plan`] hashes and parses. `+json` because that
/// is what they are; `v1` versions the *artifact layout*, not the plan document,
/// which carries its own `format-version` inside.
pub const EXECUTION_PLAN_LAYER_MEDIA_TYPE: &str = "application/vnd.wamn.execution-plan.v1+json";

/// Media type of a plan artifact's config blob.
///
/// A plan artifact carries no configuration: every fact about it is either its
/// digest or lives inside the plan. The config descriptor is required by the
/// image-manifest schema, so it is the two bytes `{}` under this media type.
pub const EXECUTION_PLAN_CONFIG_MEDIA_TYPE: &str =
    "application/vnd.wamn.execution-plan.config.v1+json";

/// The config blob of every plan artifact.
pub const EXECUTION_PLAN_CONFIG_BYTES: &[u8] = b"{}";

/// Stable classification of a refused plan fetch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanFetchErrorKind {
    /// The registry could not be reached in time, or holds no artifact under
    /// this name. Both are conditions a later attempt may resolve: a release's
    /// artifacts and its manifest converge independently, so a pod can hold a
    /// manifest naming a plan whose push has not landed.
    Unavailable,
    /// The registry answered, but the artifact does not carry the plan its name
    /// promises. Not retryable and not a transport fault — someone published
    /// wrong.
    Mismatched,
}

/// A refused plan fetch, carrying the reference it was refused for.
#[derive(Debug)]
pub struct PlanFetchError {
    kind: PlanFetchErrorKind,
    reference: Box<str>,
    detail: Box<str>,
}

impl PlanFetchError {
    /// Refuse a fetch of `reference` with a stable kind and a human detail.
    ///
    /// Public because [`PlanSource`] is: an implementation cannot report a
    /// refusal it cannot construct.
    pub fn new(kind: PlanFetchErrorKind, reference: &str, detail: impl Into<Box<str>>) -> Self {
        Self {
            kind,
            reference: reference.into(),
            detail: detail.into(),
        }
    }

    /// The stable classification of this refusal.
    pub fn kind(&self) -> PlanFetchErrorKind {
        self.kind
    }
}

impl std::fmt::Display for PlanFetchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "plan artifact {}: {}",
            self.reference, self.detail
        )
    }
}

impl std::error::Error for PlanFetchError {}

/// Digest-addressed source of exact execution-plan bytes.
///
/// Implementations return **untrusted** bytes. The caller verifies them against
/// the digest it asked for; an implementation must not assume otherwise, and
/// adding a "trusted" fetch path here would move that check somewhere it can be
/// forgotten.
#[async_trait]
pub trait PlanSource: std::fmt::Debug + Send + Sync {
    /// Fetch the bytes named by `plan_hash`, a `sha256:<64 lowercase hex>` digest.
    async fn fetch(&self, plan_hash: &str) -> Result<Vec<u8>, PlanFetchError>;
}

/// A `<registry>/<repository>` base that could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidPlanArtifactBase {
    base: Box<str>,
    reason: &'static str,
}

impl std::fmt::Display for InvalidPlanArtifactBase {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "plan artifact base {:?} is unusable: {}",
            self.base, self.reason
        )
    }
}

impl std::error::Error for InvalidPlanArtifactBase {}

/// Plan bytes pulled by digest from one OCI repository.
pub struct OciPlanSource {
    client: Client,
    registry: String,
    repository: String,
    auth: RegistryAuth,
}

impl std::fmt::Debug for OciPlanSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Deliberately no auth: this type is named in refusal logs.
        formatter
            .debug_struct("OciPlanSource")
            .field("registry", &self.registry)
            .field("repository", &self.repository)
            .finish_non_exhaustive()
    }
}

impl OciPlanSource {
    /// Pull anonymously from `artifact_base`, spelled `<registry>/<repository>`.
    ///
    /// `insecure` serves the dev registry, which is anonymous plain HTTP — the
    /// same posture the wash host takes with `--allow-insecure-registries`, and
    /// scoped to this one registry rather than globally.
    ///
    /// `fetch_timeout` bounds both the connect and the read phase. A pull runs
    /// on the run path before any effect, so an unbounded fetch would hold a
    /// lease open against a registry that never answers.
    ///
    /// The base is split by hand rather than parsed through [`Reference`]'s
    /// `FromStr`: that parser defaults a bare `namespace/name` to Docker Hub, so
    /// a base missing its registry would silently resolve to the public internet
    /// instead of refusing.
    pub fn new(
        artifact_base: &str,
        insecure: bool,
        fetch_timeout: Duration,
    ) -> Result<Self, InvalidPlanArtifactBase> {
        let invalid = |reason: &'static str| InvalidPlanArtifactBase {
            base: artifact_base.into(),
            reason,
        };
        let (registry, repository) = artifact_base
            .split_once('/')
            .ok_or_else(|| invalid("expected <registry>/<repository>"))?;
        // Splitting alone does not prove the first component IS a registry:
        // `wamn/execution-plans` splits just as cleanly, and treating `wamn` as a
        // host is how a typo reaches Docker Hub. Require what a registry host
        // actually looks like — the same discriminator the OCI reference grammar
        // uses to tell a registry from a leading path component.
        if !(registry.contains('.') || registry.contains(':') || registry == "localhost") {
            return Err(invalid(
                "registry must be a host: dotted, port-qualified, or localhost",
            ));
        }
        if repository.is_empty() {
            return Err(invalid("repository is empty"));
        }
        // The digest is appended as a tag, so the base may not already carry a
        // tag or a digest of its own.
        if repository.contains(':') || repository.contains('@') {
            return Err(invalid("repository must not carry a tag or a digest"));
        }

        let protocol = if insecure {
            ClientProtocol::HttpsExcept(vec![registry.to_string()])
        } else {
            ClientProtocol::Https
        };
        let config = ClientConfig {
            protocol,
            read_timeout: Some(fetch_timeout),
            connect_timeout: Some(fetch_timeout),
            ..ClientConfig::default()
        };
        Ok(Self {
            client: Client::new(config),
            registry: registry.to_string(),
            repository: repository.to_string(),
            auth: RegistryAuth::Anonymous,
        })
    }

    /// The reference one plan's artifact is published under.
    ///
    /// The digest is carried as the *tag*, because an OCI tag cannot contain
    /// `:` — the same narrowing
    /// [`release_manifest_configmap_name`](wamn_catalog::release_manifest_configmap_name)
    /// makes for a Kubernetes object name. The tag is therefore the 64 hex
    /// digits, and 64 hex digits are a valid tag.
    ///
    /// It is deliberately *not* `Reference::with_digest`: that would name the
    /// artifact manifest's digest, which is not the plan's and cannot be
    /// computed from it.
    fn reference_for(&self, plan_hash: &str) -> Result<Reference, PlanFetchError> {
        let hex = plan_hash.strip_prefix(HASH_PREFIX).ok_or_else(|| {
            PlanFetchError::new(
                PlanFetchErrorKind::Mismatched,
                plan_hash,
                format!("plan hash must be {HASH_PREFIX}<hex>"),
            )
        })?;
        Ok(Reference::with_tag(
            self.registry.clone(),
            self.repository.clone(),
            hex.to_string(),
        ))
    }
}

#[async_trait]
impl PlanSource for OciPlanSource {
    async fn fetch(&self, plan_hash: &str) -> Result<Vec<u8>, PlanFetchError> {
        let reference = self.reference_for(plan_hash)?;
        let named = reference.whole();
        let (manifest, _artifact_digest) = self
            .client
            .pull_image_manifest(&reference, &self.auth)
            .await
            .map_err(|error| {
                PlanFetchError::new(
                    PlanFetchErrorKind::Unavailable,
                    &named,
                    format!("pull artifact manifest: {error}"),
                )
            })?;
        let layer = plan_layer(&manifest, plan_hash, &named)?;
        // Advisory only: the descriptor's size is the registry's claim, and
        // `pull_blob` digests whatever actually arrives regardless.
        let mut exact_bytes = Vec::with_capacity(usize::try_from(layer.size).unwrap_or(0));
        self.client
            .pull_blob(&reference, layer, &mut exact_bytes)
            .await
            .map_err(|error| {
                PlanFetchError::new(
                    PlanFetchErrorKind::Unavailable,
                    &named,
                    format!("pull plan layer: {error}"),
                )
            })?;
        Ok(exact_bytes)
    }
}

/// The one plan layer of an artifact, proven to name the digest asked for.
///
/// Exactly one layer, not the first: an artifact carrying two plan layers is a
/// producer that does not agree with this reader about what a plan artifact is,
/// and picking one of them would hide that.
fn plan_layer<'a>(
    manifest: &'a OciImageManifest,
    plan_hash: &str,
    named: &str,
) -> Result<&'a OciDescriptor, PlanFetchError> {
    let mismatched =
        |detail: String| PlanFetchError::new(PlanFetchErrorKind::Mismatched, named, detail);
    let mut plan_layers = manifest
        .layers
        .iter()
        .filter(|layer| layer.media_type == EXECUTION_PLAN_LAYER_MEDIA_TYPE);
    let layer = plan_layers.next().ok_or_else(|| {
        mismatched(format!(
            "carries no {EXECUTION_PLAN_LAYER_MEDIA_TYPE} layer"
        ))
    })?;
    if plan_layers.next().is_some() {
        return Err(mismatched(format!(
            "carries more than one {EXECUTION_PLAN_LAYER_MEDIA_TYPE} layer"
        )));
    }
    if layer.digest != plan_hash {
        return Err(mismatched(format!(
            "layer names {} but this reference names {plan_hash}",
            layer.digest
        )));
    }
    Ok(layer)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAN: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
    const OTHER: &str = "sha256:2222222222222222222222222222222222222222222222222222222222222222";
    const BASE: &str = "registry:5000/wamn/execution-plans";

    fn source() -> OciPlanSource {
        OciPlanSource::new(BASE, true, Duration::from_secs(5)).expect("base parses")
    }

    fn layer(media_type: &str, digest: &str) -> OciDescriptor {
        OciDescriptor {
            media_type: media_type.to_string(),
            digest: digest.to_string(),
            size: 2,
            ..OciDescriptor::default()
        }
    }

    fn artifact(layers: Vec<OciDescriptor>) -> OciImageManifest {
        OciImageManifest {
            layers,
            ..OciImageManifest::default()
        }
    }

    /// The publish-side agreement (wamn-0h0g.15.97) has no writer yet, so these
    /// literals are pinned here rather than derived from one. Changing either
    /// breaks every already-published artifact.
    #[test]
    fn the_plan_artifact_wire_literals_are_pinned() {
        assert_eq!(
            EXECUTION_PLAN_LAYER_MEDIA_TYPE,
            "application/vnd.wamn.execution-plan.v1+json"
        );
        assert_eq!(
            EXECUTION_PLAN_CONFIG_MEDIA_TYPE,
            "application/vnd.wamn.execution-plan.config.v1+json"
        );
        assert_eq!(EXECUTION_PLAN_CONFIG_BYTES, b"{}");
    }

    #[test]
    fn an_artifact_is_named_by_the_hex_of_its_plan_digest() {
        let reference = source().reference_for(PLAN).expect("digest is well formed");

        assert_eq!(reference.registry(), "registry:5000");
        assert_eq!(reference.repository(), "wamn/execution-plans");
        // No `sha256:` — an OCI tag cannot contain a colon.
        assert_eq!(reference.tag(), Some(&PLAN["sha256:".len()..]));
        assert_eq!(reference.digest(), None);
    }

    #[test]
    fn a_base_that_does_not_name_a_registry_is_refused() {
        // Every one of these would otherwise resolve to Docker Hub through
        // `Reference`'s FromStr, or to an empty repository.
        for base in [
            "wamn/execution-plans",
            "registry/wamn/plans",
            "/wamn/plans",
            "registry:5000/",
            "registry:5000",
        ] {
            assert!(
                OciPlanSource::new(base, true, Duration::from_secs(1)).is_err(),
                "accepted {base:?}"
            );
        }
    }

    #[test]
    fn a_base_naming_a_real_host_is_accepted() {
        for base in [
            "registry:5000/wamn/execution-plans",
            "localhost/wamn/plans",
            "ghcr.io/wamn/plans",
        ] {
            assert!(
                OciPlanSource::new(base, true, Duration::from_secs(1)).is_ok(),
                "refused {base:?}"
            );
        }
    }

    #[test]
    fn a_base_carrying_its_own_tag_is_refused() {
        // The plan digest IS the tag; a base with one would silently lose it.
        for base in [
            "registry:5000/wamn/plans:latest",
            "registry:5000/wamn/plans@sha256:abc",
        ] {
            assert!(
                OciPlanSource::new(base, true, Duration::from_secs(1)).is_err(),
                "accepted {base:?}"
            );
        }
    }

    #[test]
    fn an_artifact_whose_layer_disagrees_with_its_name_is_mismatched() {
        let published = artifact(vec![layer(EXECUTION_PLAN_LAYER_MEDIA_TYPE, OTHER)]);

        let error = plan_layer(&published, PLAN, BASE).expect_err("disagreement refuses");

        assert_eq!(error.kind(), PlanFetchErrorKind::Mismatched);
    }

    #[test]
    fn an_artifact_without_a_plan_layer_is_mismatched() {
        let published = artifact(vec![layer("application/vnd.oci.image.layer.v1.tar", PLAN)]);

        let error = plan_layer(&published, PLAN, BASE).expect_err("wrong media type refuses");

        assert_eq!(error.kind(), PlanFetchErrorKind::Mismatched);
    }

    #[test]
    fn an_artifact_with_two_plan_layers_is_mismatched() {
        let published = artifact(vec![
            layer(EXECUTION_PLAN_LAYER_MEDIA_TYPE, PLAN),
            layer(EXECUTION_PLAN_LAYER_MEDIA_TYPE, PLAN),
        ]);

        let error = plan_layer(&published, PLAN, BASE).expect_err("ambiguity refuses");

        assert_eq!(error.kind(), PlanFetchErrorKind::Mismatched);
    }

    #[test]
    fn the_one_agreeing_plan_layer_is_selected() {
        let published = artifact(vec![
            layer(EXECUTION_PLAN_CONFIG_MEDIA_TYPE, OTHER),
            layer(EXECUTION_PLAN_LAYER_MEDIA_TYPE, PLAN),
        ]);

        let selected = plan_layer(&published, PLAN, BASE).expect("one agreeing layer");

        assert_eq!(selected.digest, PLAN);
    }
}
