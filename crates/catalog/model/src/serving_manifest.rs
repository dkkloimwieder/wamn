//! The mounted release-serving manifest — the one document every pod resolves
//! against (`docs/deployment-simplification-spec.md` lines 31-39 and 61-70).
//!
//! # This is not `catalog.release_manifests`
//!
//! The relation `catalog.release_manifests` (`deploy/sql/catalog-schema.sql:207`)
//! is an unrelated, retained catalog table whose `members_json` lists the member
//! entities sealed into a catalog release; `catalog.release_exposure_manifests`
//! (`deploy/sql/catalog-schema.sql:528`) is a second unrelated relation. Neither
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
//! order cannot reach the digest by construction, and [`ServingManifest::read`]
//! admits only the canonical encoding.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::{
    AttachmentKind, CALLABLE_CONTRACT_VERSION, CallableContract, CatalogIdentityError, HASH_PREFIX,
    validate_digest, validate_text,
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
/// [`ServingManifest::read`] refuses the mount.
pub const RELEASE_MANIFEST_FILE_NAME: &str = "manifest.json";

/// Name of the stable-named release-identity ConfigMap the GitOps source
/// rewrites per release — the `(release version, manifest digest)` pair.
pub const RELEASE_IDENTITY_CONFIGMAP_NAME: &str = "release-identity";

/// Directory the release-identity ConfigMap is projected into on every pod.
pub const RELEASE_IDENTITY_MOUNT_PATH: &str = "/etc/wamn/release-identity";

/// Release-identity key holding the release version — the catalog version, as
/// decimal text. Recorded write-once onto the run at claim.
pub const RELEASE_IDENTITY_VERSION_KEY: &str = "release-version";

/// Release-identity key holding the manifest digest (`sha256:<hex>`). Recorded
/// write-once onto the run at claim; the audit link to the plan hashes.
pub const RELEASE_IDENTITY_DIGEST_KEY: &str = "manifest-digest";

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
/// replay. It stays a column on `catalog.event_registrations` — a table with no
/// `catalog_version` at all (`deploy/sql/catalog-schema.sql:1742`) — read by the
/// materializer at evaluation, inside the per-event transaction it already opens
/// (`crates/events/materializer/src/sql.rs:12`). Freezing it into these
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
    /// [`ServingManifest::read`] rather than being partially understood.
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
    /// Build and validate one complete manifest.
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
        Ok(manifest)
    }

    /// The RFC 8785 canonical bytes — the exact content of the mounted file.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        wamn_flow::canonical_json_bytes(&self.as_value())
    }

    /// The manifest digest, `sha256:<hex>`, over [`Self::canonical_bytes`].
    pub fn digest(&self) -> String {
        wamn_flow::canonical_json_sha256(&self.as_value())
    }

    /// Verify mounted bytes against the digest the pod's release identity names,
    /// then validate the document.
    ///
    /// Unlike [`read_execution_plan`](crate::read_execution_plan), which accepts
    /// any bytes matching their hash, a manifest is identified by its *canonical*
    /// bytes: the parsed document is re-canonicalized and compared, so a
    /// re-indented, key-permuted, or duplicate-bearing encoding of the same
    /// content is refused instead of being served under a foreign digest.
    pub fn read(expected_digest: &str, bytes: &[u8]) -> Result<Self, CatalogIdentityError> {
        validate_digest(expected_digest, "manifest-digest")?;
        let manifest = serde_json::from_slice::<Self>(bytes).map_err(|error| {
            CatalogIdentityError::InvalidDefinition {
                message: format!("serving manifest JSON is invalid: {error}"),
            }
        })?;
        manifest.validate()?;
        if manifest.canonical_bytes() != bytes {
            return Err(CatalogIdentityError::NonCanonicalJson);
        }
        if manifest.digest() != expected_digest {
            return Err(CatalogIdentityError::ServingManifestDigestMismatch);
        }
        Ok(manifest)
    }

    /// The finite reachable call-flow set from `root`, inclusive.
    ///
    /// This is the pure read that replaces `resolve_run_flow_resolutions`' walk
    /// over plan bytes. It terminates on cycles, and it is total because
    /// [`Self::new`] and [`Self::read`] have already proved every call edge
    /// names a member flow. An absent `root` yields the empty set; a member root
    /// with no callees yields exactly `{root}`.
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
        assert!(forward.digest().starts_with(HASH_PREFIX));
        assert_eq!(forward.digest().len(), HASH_PREFIX.len() + 64);
    }

    #[test]
    fn only_the_canonical_encoding_is_admitted_at_the_mount() {
        let manifest = manifest();
        let digest = manifest.digest();
        let canonical = manifest.canonical_bytes();
        assert_eq!(ServingManifest::read(&digest, &canonical), Ok(manifest));

        let value: Value = serde_json::from_slice(&canonical).expect("canonical bytes are JSON");
        let indented = serde_json::to_vec_pretty(&value).expect("the document re-serializes");
        assert_eq!(
            ServingManifest::read(&digest, &indented),
            Err(CatalogIdentityError::NonCanonicalJson),
            "whitespace is not identity, so it may not travel under this digest"
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
    fn a_foreign_format_version_and_a_mismatched_digest_fail_closed() {
        let mut foreign = manifest();
        foreign.format_version = "0.2".into();
        let bytes = foreign.canonical_bytes();
        assert!(matches!(
            ServingManifest::read(&foreign.digest(), &bytes),
            Err(CatalogIdentityError::InvalidDefinition { .. })
        ));

        assert_eq!(
            ServingManifest::read(PLAN_A, &manifest().canonical_bytes()),
            Err(CatalogIdentityError::ServingManifestDigestMismatch)
        );
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
