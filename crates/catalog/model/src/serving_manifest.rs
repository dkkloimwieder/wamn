//! The mounted release-serving manifest — the one document every pod resolves
//! against the release closure in `docs/exe-model.md`.
//!
//! # This is not `catalog.release_manifests`
//!
//! The relation `catalog.release_manifests` is an unrelated, retained catalog
//! table: the identity row of a catalog release, whose member entities live
//! row-per-member in `catalog.release_flows`;
//! `catalog.release_exposure_manifests` is a second unrelated relation. Neither
//! is this document, and the ruling deletes neither. The Rust type here is
//! deliberately named [`ServingManifest`] — *what the serving plane reads* — so
//! that no reader confuses the two. Only the Kubernetes object names below keep
//! the ruling's literal `release-manifest-<digest>` spelling, and a Kubernetes
//! object name cannot collide with a PostgreSQL relation name.
//!
//! # What it replaces
//!
//! It succeeds release-bound resolution (`run_flow_resolutions` +
//! `crates/execution/run-state/src/resolution.rs`, deleted by the ruling):
//! `CatalogResolutionPlan`'s per-flow row becomes [`ServingFlow`], and the
//! transitive walk over `call-flow` nodes that `resolve_run_flow_resolutions`
//! performed against plan bytes becomes the precomputed [`ServingFlow::calls`]
//! adjacency, so a pod can prefetch the reachable plan set before it holds any
//! plan bytes. `exact_bytes` is deliberately absent: plan bytes now fetch by
//! digest from OCI and verify at transfer.
//!
//! # Canonical bytes
//!
//! The document's identity is the SHA-256 of its RFC 8785 canonical JSON,
//! computed by the single canonicalizer ([`wamn_flow::canonical_json_bytes`] /
//! [`wamn_flow::canonical_json_sha256`]). Every collection is a `BTreeMap` or
//! `BTreeSet` keyed by its identity, so map iteration order and document key
//! order cannot reach the digest by construction, and
//! [`ServingManifest::from_canonical_bytes`] admits only the canonical encoding.
//!
//! # One constructor from bytes
//!
//! [`ServingManifest::from_canonical_bytes`] is the only way to build a manifest
//! from bytes, and it *derives* the digest rather than checking bytes against a
//! caller-supplied expectation. A caller that holds an expectation asserts
//! against the derived value itself. Two entry points with different verification
//! semantics on one type would be a fork in the trust model; one entry point plus
//! external assertions is a trust model with call sites.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::{
    AttachmentKind, CALLABLE_CONTRACT_VERSION, CallableContract, CatalogIdentityError, HASH_PREFIX,
    ManifestDigest, validate_digest, validate_text,
};

/// The only serving-manifest format shipped by the MVP.
pub const SERVING_MANIFEST_FORMAT_VERSION: &str = "0.1";

/// Name prefix of the immutable, digest-named ConfigMap carrying the manifest.
///
/// The ruling spells this `release-manifest-<digest>`. A Kubernetes object name
/// is a DNS-1123 subdomain and cannot contain `:`, so the `sha256:` prefix is
/// stripped and only the 64 lowercase hex digits are appended. Build the name
/// with [`release_manifest_configmap_name`] rather than formatting it by hand.
pub const RELEASE_MANIFEST_CONFIGMAP_PREFIX: &str = "release-manifest-";

/// Directory the manifest ConfigMap is projected into on every pod.
pub const RELEASE_MANIFEST_MOUNT_PATH: &str = "/etc/wamn/release-manifest";

/// The manifest ConfigMap's single key, and therefore the file name under
/// [`RELEASE_MANIFEST_MOUNT_PATH`]. Its value is
/// [`ServingManifest::canonical_bytes`] byte for byte — no trailing newline, or
/// [`ServingManifest::from_canonical_bytes`] refuses the mount.
pub const RELEASE_MANIFEST_FILE_NAME: &str = "manifest.json";

/// Byte ceiling on [`ServingManifest::canonical_bytes`], refused at construction
/// so an undeployable manifest cannot pass the control-plane gate.
///
/// Both delivery paths cap at exactly this number, and they count *different*
/// things — which is what decides which one binds:
///
/// - **Kubelet-mounted pods.** `ValidateConfigMap` sums `len(value)` over `data`
///   and `binaryData` and refuses `totalSize > MaxSecretSize`, where
///   `MaxSecretSize = 1 * 1024 * 1024` (Kubernetes `pkg/apis/core/types.go`,
///   applied in `pkg/apis/core/validation/validation.go`). Keys and `metadata`
///   are *not* counted, so with [`RELEASE_MANIFEST_FILE_NAME`] as the only key
///   the whole allowance belongs to these bytes. etcd's own
///   `--max-request-bytes` (1.5 MiB) is looser and never the binding limit.
/// - **Operator-scheduled workloads** (flow-http, materializer) are not
///   kubelet-mounted. The operator reads the ConfigMap into the merged config
///   map (`runtime-operator internal/controller/runtime/utils.go`,
///   `MaterializeConfigLayer`) and ships it to the host inside one
///   `WorkloadStart` message. nats-server compares the whole published size,
///   headers included, against `max_payload`, which defaults to
///   `MAX_PAYLOAD_SIZE = 1024 * 1024` (`nats-server server/const.go`) and is
///   unset in this deployment (`deploy/infra/values-wamn.yaml`). There the
///   manifest shares the allowance with the envelope around it, so **NATS is the
///   binding bound**: a manifest can satisfy this constant and still be
///   undeliverable on that path. Sizing that envelope needs a live host
///   (wamn-0h0g.15.59) and cannot be derived here.
///
/// Measured 2026-08-17 against this projection: ~553 bytes per member flow, and
/// ~1,360 bytes per flow that also carries one HTTP attachment and one
/// registration. So the ceiling admits a release of about 770 fully-attached
/// flows, and a 100-flow release spends about 13% of it. Raising it is not a
/// local decision — both numbers belong to external systems, and a manifest over
/// either one fails at deploy after the gate has already certified the release.
///
/// Enforced at `ServingManifest::new` and
/// [`ServingManifest::from_canonical_bytes`], the only two ways a manifest comes
/// into existence. A third entry point has to check it too.
pub const MAX_SERVING_MANIFEST_BYTES: usize = 1024 * 1024;

/// The name of the ConfigMap carrying the manifest with this digest.
pub fn release_manifest_configmap_name(
    manifest_digest: &str,
) -> Result<String, CatalogIdentityError> {
    validate_digest(manifest_digest, "manifest-digest")?;
    let hex = manifest_digest
        .strip_prefix(HASH_PREFIX)
        .expect("a validated digest carries the sha256 prefix");
    Ok(format!("{RELEASE_MANIFEST_CONFIGMAP_PREFIX}{hex}"))
}

/// The release a manifest projects, in the environment it was minted for.
///
/// This is the wire projection of [`ReleaseId`](crate::ReleaseId) plus the
/// environment. `ReleaseId` itself is not reused: its fields are private behind
/// a validating constructor, so a derived `Deserialize` would bypass that
/// validation at exactly the boundary where it matters most.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ServingRelease {
    /// The bound tenant. A consumer's `app.tenant` claim must equal it
    /// (`deploy/sql/catalog-schema.sql:1753`), and it scopes the run row.
    pub tenant_id: String,
    /// The catalog. Written onto the run row and used to lock the admission
    /// head (`crates/execution/run-state/src/admission.rs:419`).
    pub catalog_id: String,
    /// The release version — the "release version" half of the release-identity
    /// pair. Written onto the run row, and surfaced as
    /// `RouteDefinition::catalog_version`
    /// (`components/ingress/flow-http/src/lib.rs:61`).
    pub catalog_version: u32,
    /// The environment this manifest was minted for. The run row carries it
    /// (`crates/execution/run-state/src/admission.rs:419`), and it names the
    /// catalog head admission locks.
    pub environment: String,
}

/// One release member flow: its plan, its artifact hashes, its callable
/// boundary, and its outgoing call edges.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ServingFlow {
    /// The runtime flow version. Required by the run row
    /// (`crates/execution/run-state/src/sql.rs:52`), by the admission input
    /// (`crates/execution/run-state/src/admission.rs:419`), by
    /// `InvocationTarget` (`crates/execution/run-state/src/invocation.rs:14`),
    /// and by `FlowDeclaration` (`crates/events/materializer/src/decide.rs:23`).
    pub flow_version: u32,
    /// The execution-bundle hash — the ruling's *plan-hash*. Plan bytes fetch by
    /// this digest and verify against it through
    /// [`read_execution_plan`](crate::read_execution_plan); effect authority
    /// verifies that the recorded manifest contains `(flow, plan-hash)`.
    pub plan_hash: String,
    /// The flow artifact hash — the ruling's *source-artifact*. It is read by
    /// the plan verifiers and by nothing else: the value a fetched plan's
    /// `root-artifact-hash` must equal, checked live in exactly two places —
    /// `components/execution/flowrunner/src/lib.rs:122`, which refuses the
    /// snapshot with `run-resolution-source-mismatch`, and
    /// `crates/events/materializer/src/lib.rs:77`.
    ///
    /// Binding resolution reads [`ServingFlow::binding_base_artifact`], never
    /// this field. Until wamn-0h0g.15.62 this one field carried both duties, and
    /// that bead proved the duties demand *different* values for a candidate: a
    /// candidate's plan is compiled from the draft's own artifact, so a plan
    /// verifier forces this field to the draft hash, while its bindings exist
    /// only under the released base. Do not read this as a binding key again.
    pub source_artifact: String,
    /// The released artifact hash this flow's connection bindings are keyed
    /// under — the ruling's *binding-base-artifact*.
    ///
    /// Equal to [`ServingFlow::source_artifact`] for a released member, which is
    /// every flow a plain release mint emits; a candidate overlay instead
    /// carries the released base it was validated against, the value
    /// `catalog.validated_flow_drafts.binding_base_artifact_hash`
    /// (`deploy/sql/catalog-schema.sql:495`) already stores. That default is a
    /// mint-side rule, not a serde default: the key is required and non-null, so
    /// a producer cannot construct a flow without deciding which case it is in.
    ///
    /// It is the key binding resolution uses — the one readiness proves every
    /// connection requirement of the release is bound under in this environment
    /// (`catalog.connection_bindings.artifact_hash`,
    /// `deploy/sql/catalog-schema.sql:1162`), and the one effect authority joins
    /// that table on (`crates/platform/runtime/src/plugins/wamn_postgres/claims.rs`,
    /// the `binding.artifact_hash` predicate).
    ///
    /// # Substituting the base is not an authority hole
    ///
    /// A candidate resolving bindings under a *released* artifact cannot borrow
    /// authority the base did not already grant: the same query compares the
    /// executing plan node's `source-connection-requirement.descriptor` against
    /// the stored `requirement_json` of the base
    /// (`crates/platform/runtime/src/plugins/wamn_postgres/claims.rs:200-205`),
    /// so a draft that *changes* a requirement matches no permitted node and
    /// fails closed as `node-not-permitted` rather than executing under the
    /// base's binding.
    pub binding_base_artifact: String,
    /// The flow's intrinsic callable boundary, or `null` when the flow is not
    /// callable. The key is always present: a missing key is a refusal, never a
    /// silent `None`. Replaces the plan-parsing callee-callability check the
    /// deleted `resolve_run_flow_resolutions` performed.
    #[serde(deserialize_with = "deserialize_required_callable_contract")]
    pub callable_contract: Option<CallableContract>,
    /// This flow's direct `call-flow` callees — the call-edge adjacency. Walking
    /// it from a run's root yields the transitive reachable set
    /// ([`ServingManifest::reachable_flows`]), which is what readiness
    /// prefetches and what resolution reads instead of walking plan bytes.
    pub calls: BTreeSet<String>,
}

/// One release attachment — the route shape this release exposes.
///
/// The definition and the resolved source travel as the exact JSON documents
/// the columns `catalog.release_attachments.definition_json` and
/// `catalog.release_sources.definition_json` hold today, so this projection adds
/// no parallel declaration of a shape another crate already owns
/// (`wamn_schema_control::exposure::Attachment` is the decoder).
///
/// # Activation is deliberately absent
///
/// Enablement is environment-owned operational state, and
/// [`AttachmentActivation`](crate::AttachmentActivation) already states that it
/// "never participates in hashes". It stays in `catalog.attachment_activation`
/// and is checked inside the admission transaction, which already holds the
/// connection: the admit statement reads the `catalog.active_attachments` view
/// (`crates/execution/run-state/src/admission.rs:246`), and it is that view —
/// not the statement — which joins `catalog.attachment_activation` and filters
/// `WHERE activation.enabled` (`deploy/sql/catalog-schema.sql:1003`, `:1010`).
/// A disabled attachment therefore leaves the view and classifies as the
/// existing `inactive-definition` refusal by row absence. The
/// serving path therefore gains no read, and an emergency off is one `UPDATE`
/// effective at the next admission instead of a mint plus a rollout. Disabled
/// attachments stay in the manifest because routes are immutable per release;
/// tombstoned ones are absent from it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ServingAttachment {
    /// The exposure kind. flow-http serves only `http` and `studio`
    /// (`crates/execution/run-state/src/invocation.rs:76`).
    pub kind: AttachmentKind,
    /// The exposed flow. Keys into [`ServingManifest::flows`] for the
    /// `flow_version` the run row needs.
    pub flow_id: String,
    /// The resolved effective-contract hash — `RouteDefinition::definition_hash`
    /// (`components/ingress/flow-http/src/lib.rs:62`) and the value transparent
    /// recovery compares against.
    pub definition_hash: String,
    /// The attachment definition document — route host/path/method, input
    /// mappings, and deadlines. `InvocationTarget::definition`
    /// (`crates/execution/run-state/src/invocation.rs:15`) is this value.
    pub definition: Value,
    /// The resolved source document: an `auth` policy for `http`/`studio`, a
    /// `caller-policy` for `internal`, a `schedule` for `cron`.
    /// `InvocationTarget::auth_policy`
    /// (`crates/execution/run-state/src/invocation.rs:16`) is this value, and it
    /// is the policy flow-http hands to `routing::authenticate`.
    pub auth_policy: Value,
}

/// One event registration's release identity — its source, and the flow it runs.
///
/// # The condition is deliberately absent
///
/// A registration's `condition` is a hot-editable filter by design
/// (`crates/events/registration/src/model.rs:70`): a filtered-out event stays on
/// the stream, so correcting a mis-filtered predicate is one `UPDATE` and a
/// replay. It stays a KEY INSIDE the stored `registration` jsonb document on
/// `catalog.event_registrations` — a table with no `condition` column and no
/// `catalog_version` at all (`deploy/sql/catalog-schema.sql:1742`) — swept once
/// per sweep (`crates/events/materializer/src/sql.rs:12`), compiled by
/// `wamn_materializer::serviceable`, and evaluated per event inside the pure
/// `decide`, which runs BEFORE the fire transaction opens. Freezing it into these
/// canonical bytes would turn seconds-scale filter repair into a mint plus a
/// rollout, so it does not travel here. The stored document's `schema-version`,
/// `registration-id` and `catalog-id` do not travel either:
/// [`ServingManifest::format_version`], the map key, and
/// [`ServingRelease::catalog_id`] already carry them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ServingRegistration {
    /// The subscribing flow — the durable consumer the materializer opens and
    /// the `<flow>:evt:<seq>` run-id prefix
    /// (`crates/events/materializer/src/decide.rs:50`). It is *not* required to
    /// be a member of [`ServingManifest::flows`]: registrations attach to the
    /// live catalog rather than a catalog version, so one may name a flow this
    /// release does not carry, and the materializer holds that event.
    pub flow_id: String,
    /// The stable catalog entity id whose row events fire it — the value the CDC
    /// envelope carries in its `entity` segment, so a table rename never orphans
    /// a registration (EVT-OIDMAP).
    pub entity: String,
    /// The row ops that fire it — non-empty, since a registration matching no op
    /// is inert. `wamn_event_wire::Op` owns the spelling and stays its only
    /// decoder, so this projection re-declares no op taxonomy; a `BTreeSet`
    /// because the same op set written in two orders is one registration.
    pub ops: BTreeSet<String>,
}

/// The complete document a pod mounts and resolves every run against.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ServingManifest {
    /// The manifest format version. A foreign version fails closed at
    /// [`ServingManifest::from_canonical_bytes`] rather than being partially
    /// understood.
    pub format_version: String,
    /// Release identity, and the environment the projection was minted for.
    pub release: ServingRelease,
    /// Every release member flow, keyed by flow id.
    pub flows: BTreeMap<String, ServingFlow>,
    /// Every non-tombstoned release attachment, keyed by attachment id.
    pub attachments: BTreeMap<String, ServingAttachment>,
    /// Every event registration's release identity, keyed by registration id.
    /// The stored `catalog.event_registrations.registration` document does not
    /// travel whole — see [`ServingRegistration`] for what stays in Postgres.
    pub registrations: BTreeMap<String, ServingRegistration>,
}

impl ServingManifest {
    /// Build and validate one complete manifest from its parts.
    ///
    /// M-TEST-UTIL: gated behind `test-util` so production code cannot assemble a
    /// manifest that no canonical bytes ever carried. The release mint is the only
    /// legitimate producer, and it builds through this path with the feature on;
    /// every reader arrives through [`Self::from_canonical_bytes`], where the
    /// digest is derived from the bytes actually shipped.
    ///
    /// If this fails to resolve in your crate, the fix is a *dev-dependency* on
    /// `wamn-catalog` with `features = ["test-util"]`. Do not add the feature to a
    /// normal dependency: that deletes the fence for the whole dependency graph.
    #[cfg(feature = "test-util")]
    pub fn new(
        release: ServingRelease,
        flows: BTreeMap<String, ServingFlow>,
        attachments: BTreeMap<String, ServingAttachment>,
        registrations: BTreeMap<String, ServingRegistration>,
    ) -> Result<Self, CatalogIdentityError> {
        let manifest = Self {
            format_version: SERVING_MANIFEST_FORMAT_VERSION.to_string(),
            release,
            flows,
            attachments,
            registrations,
        };
        manifest.validate()?;
        within_delivery_limit(manifest.canonical_bytes().len())?;
        Ok(manifest)
    }

    /// The RFC 8785 canonical bytes — the exact content of the mounted file.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        wamn_flow::canonical_json_bytes(&self.as_value())
    }

    /// The manifest digest over [`Self::canonical_bytes`].
    ///
    /// Producer-coupled by construction: [`wamn_flow::canonical_json_sha256`] is
    /// the workspace's only RFC 8785 producer, so a producer revision cannot fork
    /// reader-side identity. wamn-0h0g.15.63 deleted the second implementation
    /// that used to sit in this crate; `manifest_identity_has_exactly_one_producer_in_this_crate`
    /// is what stops a third one arriving.
    pub fn digest(&self) -> ManifestDigest {
        ManifestDigest::parse(wamn_flow::canonical_json_sha256(&self.as_value()))
            .expect("the shared canonicalizer emits a canonical sha256 digest")
    }

    /// Parse, validate, and require canonical bytes, then *derive* the identity.
    ///
    /// The only way to build a manifest from bytes. It takes no expected digest:
    /// the digest returned is computed from the bytes that were actually supplied,
    /// so it always names the content that will be served.
    ///
    /// Unlike [`read_execution_plan`](crate::read_execution_plan), which accepts
    /// any bytes matching their hash, a manifest is identified by its *canonical*
    /// bytes: the parsed document is re-canonicalized and compared. Non-canonical
    /// input is a refusal, never a repair — a manifest arriving re-indented,
    /// key-permuted, or duplicate-bearing is evidence of a producer or transport
    /// fault, and the digest of repaired bytes would name content nobody shipped.
    /// Parse-then-re-serialize *is* the check, and the identity is always
    /// `sha256(input bytes)`, which under that requirement equals the round-trip.
    pub fn from_canonical_bytes(
        bytes: &[u8],
    ) -> Result<(Self, ManifestDigest), CatalogIdentityError> {
        within_delivery_limit(bytes.len())?;
        let manifest = serde_json::from_slice::<Self>(bytes).map_err(|error| {
            CatalogIdentityError::InvalidDefinition {
                message: format!("serving manifest JSON is invalid: {error}"),
            }
        })?;
        manifest.validate()?;
        if manifest.canonical_bytes() != bytes {
            return Err(CatalogIdentityError::NonCanonicalJson);
        }
        let digest = manifest.digest();
        Ok((manifest, digest))
    }

    /// The finite reachable call-flow set from `root`, inclusive.
    ///
    /// This is the pure read that replaces `resolve_run_flow_resolutions`' walk
    /// over plan bytes. It terminates on cycles, and it is total because
    /// [`Self::from_canonical_bytes`] has already proved every call edge names a
    /// member flow. An absent `root` yields the empty set; a member root with no
    /// callees yields exactly `{root}`.
    pub fn reachable_flows(&self, root: &str) -> BTreeSet<String> {
        let mut reached = BTreeSet::new();
        let mut stack = Vec::new();
        if self.flows.contains_key(root) {
            stack.push(root.to_string());
        }
        while let Some(flow_id) = stack.pop() {
            let Some(flow) = self.flows.get(&flow_id) else {
                continue;
            };
            if !reached.insert(flow_id) {
                continue;
            }
            stack.extend(flow.calls.iter().cloned());
        }
        reached
    }

    fn as_value(&self) -> Value {
        serde_json::to_value(self).expect("serving manifest serializes")
    }

    fn validate(&self) -> Result<(), CatalogIdentityError> {
        if self.format_version != SERVING_MANIFEST_FORMAT_VERSION {
            return invalid("serving manifest format-version must be textual 0.1");
        }
        validate_text(&self.release.tenant_id, "tenant-id")?;
        validate_text(&self.release.catalog_id, "catalog-id")?;
        validate_text(&self.release.environment, "environment")?;
        if self.release.catalog_version == 0 {
            return Err(CatalogIdentityError::ZeroVersion {
                field: "catalog-version",
            });
        }

        for (flow_id, flow) in &self.flows {
            validate_text(flow_id, "flow-id")?;
            validate_digest(&flow.plan_hash, "plan-hash")?;
            validate_digest(&flow.source_artifact, "source-artifact")?;
            validate_digest(&flow.binding_base_artifact, "binding-base-artifact")?;
            if flow.flow_version == 0 {
                return Err(CatalogIdentityError::ZeroVersion {
                    field: "flow-version",
                });
            }
            if let Some(contract) = &flow.callable_contract {
                if contract.version != CALLABLE_CONTRACT_VERSION {
                    return invalid("callable contract version must be textual 0.1");
                }
                validate_digest(&contract.input_schema_hash, "input-schema-hash")?;
            }
            for callee in &flow.calls {
                if !self.flows.contains_key(callee) {
                    return Err(CatalogIdentityError::UnresolvableManifestFlow {
                        flow_id: callee.clone(),
                    });
                }
            }
        }

        for (attachment_id, attachment) in &self.attachments {
            validate_text(attachment_id, "attachment-id")?;
            validate_digest(&attachment.definition_hash, "definition-hash")?;
            if !self.flows.contains_key(&attachment.flow_id) {
                return Err(CatalogIdentityError::UnresolvableManifestFlow {
                    flow_id: attachment.flow_id.clone(),
                });
            }
            if !attachment.definition.is_object() || !attachment.auth_policy.is_object() {
                return invalid("attachment definition and resolved source must be JSON objects");
            }
        }

        for (registration_id, registration) in &self.registrations {
            validate_text(registration_id, "registration-id")?;
            validate_text(&registration.flow_id, "flow-id")?;
            validate_text(&registration.entity, "entity")?;
            if registration.ops.is_empty() {
                return invalid("a registration matching no op is inert");
            }
            for op in &registration.ops {
                validate_text(op, "op")?;
            }
        }
        Ok(())
    }
}

fn deserialize_required_callable_contract<'de, D>(
    deserializer: D,
) -> Result<Option<CallableContract>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<CallableContract>::deserialize(deserializer)
}

fn invalid<T>(message: impl Into<String>) -> Result<T, CatalogIdentityError> {
    Err(CatalogIdentityError::InvalidDefinition {
        message: message.into(),
    })
}

/// Refuse a manifest that no delivery path can carry.
///
/// See [`MAX_SERVING_MANIFEST_BYTES`] for the two limits and which one binds.
fn within_delivery_limit(bytes: usize) -> Result<(), CatalogIdentityError> {
    if bytes > MAX_SERVING_MANIFEST_BYTES {
        return Err(CatalogIdentityError::ManifestTooLarge {
            bytes,
            limit: MAX_SERVING_MANIFEST_BYTES,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallableEffectCeiling, CallableReturnContract};

    const PLAN_A: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const ART_A: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const PLAN_B: &str = "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
    const ART_B: &str = "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
    const DEF_HASH: &str =
        "sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
    const GUARD_HASH: &str =
        "sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
    /// The released base a candidate overlay resolves its bindings under — the
    /// case where `binding-base-artifact` and `source-artifact` diverge.
    const BASE_B: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";

    fn release() -> ServingRelease {
        ServingRelease {
            tenant_id: "t1".into(),
            catalog_id: "cat".into(),
            catalog_version: 7,
            environment: "prod".into(),
        }
    }

    /// A released member: its bindings are keyed under its own artifact.
    fn root_flow() -> ServingFlow {
        ServingFlow {
            flow_version: 3,
            plan_hash: PLAN_A.into(),
            source_artifact: ART_A.into(),
            binding_base_artifact: ART_A.into(),
            callable_contract: None,
            calls: BTreeSet::from(["callee".to_string()]),
        }
    }

    /// A candidate overlay: its plan is compiled from its own draft artifact,
    /// but its bindings live under the released base it was validated against.
    fn callee_flow() -> ServingFlow {
        ServingFlow {
            flow_version: 1,
            plan_hash: PLAN_B.into(),
            source_artifact: ART_B.into(),
            binding_base_artifact: BASE_B.into(),
            callable_contract: Some(CallableContract {
                version: CALLABLE_CONTRACT_VERSION.into(),
                input_schema_hash: GUARD_HASH.into(),
                return_contract: CallableReturnContract::UntypedJsonBody,
                effect_ceiling: CallableEffectCeiling::Effectful,
            }),
            // A cycle: the deleted resolver terminated on one, and so must this.
            calls: BTreeSet::from(["root".to_string()]),
        }
    }

    fn attachment() -> ServingAttachment {
        ServingAttachment {
            kind: AttachmentKind::Http,
            flow_id: "root".into(),
            definition_hash: DEF_HASH.into(),
            definition: serde_json::json!({
                "id": "orders",
                "kind": "http",
                "flow-id": "root",
                "source-id": "public",
                "route": {"host": "*", "path": "/orders", "method": "POST"},
                "run-deadline-ms": 30000
            }),
            auth_policy: serde_json::json!({"mode": "none"}),
        }
    }

    fn registration() -> ServingRegistration {
        ServingRegistration {
            flow_id: "callee".into(),
            entity: "orders".into(),
            ops: BTreeSet::from(["insert".to_string()]),
        }
    }

    fn manifest() -> ServingManifest {
        ServingManifest::new(
            release(),
            BTreeMap::from([
                ("root".to_string(), root_flow()),
                ("callee".to_string(), callee_flow()),
            ]),
            BTreeMap::from([("orders".to_string(), attachment())]),
            BTreeMap::from([("orders-changed".to_string(), registration())]),
        )
        .expect("fixture manifest is valid")
    }

    #[test]
    fn insertion_order_cannot_reach_the_bytes_or_the_digest() {
        let forward = manifest();
        let reversed = ServingManifest::new(
            release(),
            BTreeMap::from([
                ("callee".to_string(), callee_flow()),
                ("root".to_string(), root_flow()),
            ]),
            BTreeMap::from([("orders".to_string(), attachment())]),
            BTreeMap::from([("orders-changed".to_string(), registration())]),
        )
        .expect("reordered fixture is valid");

        assert_eq!(forward.canonical_bytes(), reversed.canonical_bytes());
        assert_eq!(forward.digest(), reversed.digest());
        assert!(forward.digest().as_str().starts_with(HASH_PREFIX));
        assert_eq!(forward.digest().as_str().len(), HASH_PREFIX.len() + 64);
    }

    #[test]
    fn only_the_canonical_encoding_is_admitted_at_the_mount() {
        let manifest = manifest();
        let digest = manifest.digest();
        let canonical = manifest.canonical_bytes();
        assert_eq!(
            ServingManifest::from_canonical_bytes(&canonical),
            Ok((manifest, digest))
        );

        let value: Value = serde_json::from_slice(&canonical).expect("canonical bytes are JSON");
        let indented = serde_json::to_vec_pretty(&value).expect("the document re-serializes");
        assert_eq!(
            ServingManifest::from_canonical_bytes(&indented),
            Err(CatalogIdentityError::NonCanonicalJson),
            "whitespace is not identity; re-canonicalizing and proceeding would name \
             content nobody shipped"
        );
    }

    #[test]
    fn content_change_moves_the_digest() {
        let baseline = manifest().digest();
        let mut retargeted = manifest();
        retargeted
            .registrations
            .get_mut("orders-changed")
            .expect("fixture registration")
            .flow_id = "root".into();
        assert_ne!(baseline, retargeted.digest());
    }

    #[test]
    fn adjacency_yields_the_transitive_set_and_terminates_on_cycles() {
        let manifest = manifest();
        assert_eq!(
            manifest.reachable_flows("root"),
            BTreeSet::from(["callee".to_string(), "root".to_string()])
        );
        assert!(manifest.reachable_flows("absent").is_empty());
    }

    #[test]
    fn a_call_edge_must_name_a_member_flow() {
        let mut dangling = root_flow();
        dangling.calls = BTreeSet::from(["ghost".to_string()]);
        assert_eq!(
            ServingManifest::new(
                release(),
                BTreeMap::from([("root".to_string(), dangling)]),
                BTreeMap::new(),
                BTreeMap::new(),
            ),
            Err(CatalogIdentityError::UnresolvableManifestFlow {
                flow_id: "ghost".to_string()
            })
        );
    }

    #[test]
    fn the_binding_base_is_validated_as_its_own_digest() {
        let mut malformed = root_flow();
        malformed.binding_base_artifact = "not-a-digest".into();
        malformed.calls = BTreeSet::new();
        assert_eq!(
            ServingManifest::new(
                release(),
                BTreeMap::from([("root".to_string(), malformed)]),
                BTreeMap::new(),
                BTreeMap::new(),
            ),
            Err(CatalogIdentityError::InvalidDigest {
                field: "binding-base-artifact"
            }),
            "the binding base is a digest in its own right, not a copy of source-artifact"
        );

        // The candidate case — base and source diverging — is valid by design.
        let mut candidate = root_flow();
        candidate.binding_base_artifact = ART_B.into();
        candidate.calls = BTreeSet::new();
        assert!(
            ServingManifest::new(
                release(),
                BTreeMap::from([("root".to_string(), candidate)]),
                BTreeMap::new(),
                BTreeMap::new(),
            )
            .is_ok()
        );
    }

    #[test]
    fn a_foreign_format_version_fails_closed() {
        let mut foreign = manifest();
        foreign.format_version = "0.2".into();
        let bytes = foreign.canonical_bytes();
        assert!(matches!(
            ServingManifest::from_canonical_bytes(&bytes),
            Err(CatalogIdentityError::InvalidDefinition { .. })
        ));
    }

    /// The producer-coupling rider, enforced structurally rather than by alarm.
    ///
    /// Until wamn-0h0g.15.63 this crate carried its own RFC 8785 implementation
    /// beside the shared one, and the test here could only pin that the two
    /// AGREED: a mutant swapping [`ServingManifest::digest`] onto the private
    /// path left all 47 tests green (measured 2026-08-17). Agreement at a point
    /// in time is the wrong guarantee — it does not stop a *third* producer
    /// appearing, and no digest assertion can, because a digest cannot say which
    /// producer computed it. The duplicate is gone; this keeps it gone.
    ///
    /// A grep lint catches the two routes a second producer actually arrives by:
    /// re-adding the float formatter as a dependency, or pasting the deleted
    /// functions back. UTF-16 key ordering is in the list because it is RFC
    /// 8785's fingerprint and has no other reason to exist in this crate.
    #[test]
    fn manifest_identity_has_exactly_one_producer_in_this_crate() {
        // Split literals, or this test's own source matches the scan.
        const SECOND_PRODUCER: [&str; 4] = [
            concat!("ryu", "_js"),
            concat!("fn ", "write_json"),
            concat!("fn ", "ecma_number"),
            concat!("encode_", "utf16"),
        ];
        for (name, source) in [
            ("lib.rs", include_str!("lib.rs")),
            ("execution_node_id.rs", include_str!("execution_node_id.rs")),
            ("execution_plan.rs", include_str!("execution_plan.rs")),
            ("serving_manifest.rs", include_str!("serving_manifest.rs")),
        ] {
            for marker in SECOND_PRODUCER {
                assert!(
                    !source.contains(marker),
                    "{name} carries {marker}: a second RFC 8785 producer in this crate \
                     forks mint-side and weld-side release identity silently, for every \
                     release, with no digest that disagrees"
                );
            }
        }
    }

    #[test]
    fn different_content_derives_a_different_name() {
        // What the deleted expected-digest parameter used to assert, stated as
        // the fact it was standing in for: a foreign expectation is not a
        // property of the bytes. The bytes always derive their own correct name,
        // and a caller holding an expectation compares against that.
        let (_, derived) = ServingManifest::from_canonical_bytes(&manifest().canonical_bytes())
            .expect("the fixture's canonical bytes are admitted");
        assert_ne!(derived.as_str(), PLAN_A);
        assert_eq!(derived, manifest().digest());
    }

    /// Drift guard on the M-TEST-UTIL fence, which is a property of the workspace
    /// dependency graph rather than of any one crate — so it is guarded here, at
    /// the crate that declares the feature, because that is the only place in the
    /// graph that currently compiles.
    ///
    /// That the fence *works* is a compile-time fact, proven by the
    /// `manifest-from-parts-in-production` mutant: add a production reference to
    /// [`ServingManifest::new`] and `cargo check -p wamn-catalog` stops resolving
    /// it. What this test catches is the regression that would silently disarm it
    /// — promoting `test-util` onto a normal dependency, which enables from-parts
    /// construction for every crate downstream of that edge.
    #[test]
    fn the_test_util_feature_is_never_enabled_by_a_normal_dependency() {
        let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .expect("crates/catalog/model sits three levels below the workspace root")
            .to_path_buf();

        let own = std::fs::read_to_string(workspace.join("crates/catalog/model/Cargo.toml"))
            .expect("this crate's manifest is readable");
        assert!(
            own.contains("\n[features]\ntest-util = []\n"),
            "the fence's feature declaration is gone; from-parts construction is \
             unconditionally public again"
        );

        for manifest in [
            "crates/catalog/model/Cargo.toml",
            "crates/platform/runtime/Cargo.toml",
        ] {
            let text = std::fs::read_to_string(workspace.join(manifest))
                .unwrap_or_else(|error| panic!("{manifest} is readable: {error}"));
            let (normal, dev) = text
                .split_once("\n[dev-dependencies]")
                .expect("both manifests declare dev-dependencies");
            assert!(
                !normal
                    .lines()
                    .any(|line| line.starts_with("wamn-catalog") && line.contains("test-util")),
                "{manifest} enables test-util on a NORMAL dependency — that is not a fix \
                 for a missing-feature compile error, it deletes the fence"
            );
            assert!(
                dev.lines()
                    .any(|line| line.starts_with("wamn-catalog") && line.contains("test-util")),
                "{manifest} lost its test-util dev-dependency, so its tests cannot reach \
                 from-parts construction"
            );
        }
    }

    #[test]
    fn the_callable_contract_key_is_required_even_when_null() {
        let mut document: Value = serde_json::from_slice(&manifest().canonical_bytes())
            .expect("canonical bytes are JSON");
        assert!(document["flows"]["root"]["callable-contract"].is_null());
        document["flows"]["root"]
            .as_object_mut()
            .expect("a flow is an object")
            .remove("callable-contract");
        assert!(serde_json::from_value::<ServingManifest>(document).is_err());
    }

    /// The gate must not certify a release the kubelet or NATS will then refuse.
    ///
    /// The ceiling is exclusive on purpose — a manifest sized exactly at the
    /// limit is deliverable — and it is checked at both entry points, because the
    /// reader is a different process from the producer: a manifest minted by an
    /// older revision, or a hand-edited ConfigMap, reaches the pod without ever
    /// passing the constructor.
    #[test]
    fn the_delivery_ceiling_admits_its_exact_value_and_refuses_one_byte_more() {
        let build = |pad: usize| {
            let mut lone = root_flow();
            lone.calls = BTreeSet::new();
            let mut padded = attachment();
            padded.definition = serde_json::json!({"pad": "x".repeat(pad)});
            ServingManifest::new(
                release(),
                BTreeMap::from([("root".to_string(), lone)]),
                BTreeMap::from([("orders".to_string(), padded)]),
                BTreeMap::new(),
            )
        };

        // One ASCII pad byte is one canonical byte, so the ceiling is reachable
        // exactly: measure a small document and grow the pad by the shortfall.
        let probe = build(1).expect("a small manifest is admitted");
        let exact = 1 + (MAX_SERVING_MANIFEST_BYTES - probe.canonical_bytes().len());
        assert_eq!(
            build(exact)
                .expect("a manifest sized exactly at the ceiling is deliverable")
                .canonical_bytes()
                .len(),
            MAX_SERVING_MANIFEST_BYTES
        );
        assert_eq!(
            build(exact + 1),
            Err(CatalogIdentityError::ManifestTooLarge {
                bytes: MAX_SERVING_MANIFEST_BYTES + 1,
                limit: MAX_SERVING_MANIFEST_BYTES,
            }),
            "an oversized manifest must fail at the mint, not at the kubelet"
        );

        // Reader side: refused on length before anything parses it.
        let mount = vec![b'x'; MAX_SERVING_MANIFEST_BYTES + 1];
        assert_eq!(
            ServingManifest::from_canonical_bytes(&mount),
            Err(CatalogIdentityError::ManifestTooLarge {
                bytes: MAX_SERVING_MANIFEST_BYTES + 1,
                limit: MAX_SERVING_MANIFEST_BYTES,
            })
        );
    }

    #[test]
    fn the_configmap_name_is_a_dns_1123_subdomain() {
        let name = release_manifest_configmap_name(PLAN_A).expect("a valid digest names a map");
        assert_eq!(
            name,
            "release-manifest-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert!(
            name.bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'),
            "a ConfigMap name cannot carry the digest's colon"
        );
        assert_eq!(
            release_manifest_configmap_name("not-a-digest"),
            Err(CatalogIdentityError::InvalidDigest {
                field: "manifest-digest"
            })
        );
    }
}
