//! Immutable read model for one local development-loop session.
//!
//! The public handle and subscription hide the Tokio watch channel. Snapshots
//! use shared ownership because they carry a complete serving manifest plus
//! bounded payload observations; copying either on every UI refresh would make
//! the read seam itself observable work.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::watch;
use wamn_authoring_model::{GateReceipt, GateRefusal};
use wamn_catalog::{
    AttachmentKind, PackageCoordinate, ServingAttachment, ServingComponent,
    ServingComponentOperation, ServingManifest,
};
use wamn_runtime::plugins::wamn_jetstream::{
    RouterTapRecord, RouterTapRecordPhase, RouterTapSourceKind,
};

use super::{DEV_STAGE_ORDER, DevStage, DevStageFailure};
use crate::print_release_env::ReleaseCarrier;

/// Number of stages in one development-loop run.
pub const DEV_STAGE_COUNT: usize = DEV_STAGE_ORDER.len();

/// Maximum trace or tap observations retained by one session.
pub const DEV_OBSERVATION_LIMIT: usize = 100;

/// Current state of one ordered development stage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DevStageState {
    /// This stage has not run for the current suffix.
    Awaiting,
    /// This stage is running.
    Running,
    /// This stage completed successfully.
    Passed,
    /// This stage refused or failed.
    Failed(DevStageFailure),
}

/// State of one exact member of [`DEV_STAGE_ORDER`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DevStageSnapshot {
    stage: DevStage,
    state: DevStageState,
}

impl DevStageSnapshot {
    /// Stable stage identity.
    pub const fn stage(&self) -> DevStage {
        self.stage
    }

    /// Latest state for this stage.
    pub const fn state(&self) -> &DevStageState {
        &self.state
    }
}

/// Exact typed outcome returned by Gate.
#[derive(Clone, Debug, PartialEq)]
pub enum DevGateVerdict {
    /// Gate accepted the wiring and returned its immutable receipt.
    Accepted(GateReceipt),
    /// Gate refused the wiring with its owning contract type.
    Refused(GateRefusal),
}

/// Gate outcome bound to the package wiring that produced it.
#[derive(Clone, Debug, PartialEq)]
pub struct DevGateOutcome {
    pub(crate) package_id: String,
    pub(crate) package_version: String,
    pub(crate) wiring_id: String,
    pub(crate) wiring_version: u32,
    pub(crate) verdict: DevGateVerdict,
}

impl DevGateOutcome {
    /// Package that owns the gated wiring.
    pub fn package_id(&self) -> &str {
        &self.package_id
    }

    /// Exact package version that was gated.
    pub fn package_version(&self) -> &str {
        &self.package_version
    }

    /// Package-local wiring identity.
    pub fn wiring_id(&self) -> &str {
        &self.wiring_id
    }

    /// Exact wiring version submitted to Gate.
    pub const fn wiring_version(&self) -> u32 {
        self.wiring_version
    }

    /// Typed Gate receipt or refusal.
    pub const fn verdict(&self) -> &DevGateVerdict {
        &self.verdict
    }
}

/// Exact release document and the carrier derived from those bytes.
#[derive(Clone, Debug, PartialEq)]
pub struct DevReleaseSnapshot {
    manifest: ServingManifest,
    carrier: ReleaseCarrier,
}

impl DevReleaseSnapshot {
    /// Immutable serving manifest minted by Release.
    pub const fn manifest(&self) -> &ServingManifest {
        &self.manifest
    }

    /// Exact registry and manifest digest carried to serving processes.
    pub const fn carrier(&self) -> &ReleaseCarrier {
        &self.carrier
    }
}

/// One typed router-tap record received from the environment subject.
#[derive(Clone, Debug, PartialEq)]
pub struct DevTapObservation {
    pub(crate) subject: String,
    pub(crate) record: RouterTapRecord,
}

impl DevTapObservation {
    /// Exact NATS subject that carried this observation.
    pub fn subject(&self) -> &str {
        &self.subject
    }

    /// Delivery boundary represented by this observation.
    pub const fn phase(&self) -> RouterTapRecordPhase {
        self.record.phase
    }

    /// Delivery identity shared by its accepted and settled records.
    pub fn delivery_id(&self) -> &str {
        &self.record.delivery_id
    }

    /// Wiring that routed the delivery.
    pub fn wiring_id(&self) -> &str {
        &self.record.wiring_id
    }

    /// Exact wiring version that routed the delivery.
    pub const fn wiring_version(&self) -> u32 {
        self.record.wiring_version
    }

    /// Kind of release source that originated the delivery.
    pub const fn source_kind(&self) -> RouterTapSourceKind {
        self.record.source_kind
    }

    /// Attachment or registration identity within its source kind.
    pub fn source_id(&self) -> &str {
        &self.record.source_id
    }

    /// Whether the platform redacted at least one payload value.
    pub const fn redacted(&self) -> bool {
        self.record.redacted
    }

    /// Driver outcome on settled observations.
    pub fn outcome(&self) -> Option<&str> {
        self.record.outcome.as_deref()
    }

    /// Original payload size when the bounded preview was omitted.
    pub const fn over_ceiling_bytes(&self) -> Option<u64> {
        self.record.over_ceiling_bytes
    }

    /// Redacted payload preview, or null when it exceeded the ceiling.
    pub const fn payload(&self) -> &Value {
        &self.record.payload
    }

    /// Frozen record decoded by the same type that owns publication.
    pub const fn record(&self) -> &RouterTapRecord {
        &self.record
    }
}

/// One typed trace summary returned by the configured Tempo endpoint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DevTraceObservation {
    pub(crate) trace_id: String,
    pub(crate) root_service_name: String,
    pub(crate) root_trace_name: String,
    pub(crate) start_time_unix_nanos: u64,
    pub(crate) duration: Duration,
}

/// Runtime-assigned local route endpoint for the activated release.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DevRuntimeEndpoint {
    base_url: Box<str>,
    route_host: Box<str>,
}

impl DevRuntimeEndpoint {
    pub(crate) fn new(base_url: String, route_host: &str) -> Self {
        Self {
            base_url: base_url.into_boxed_str(),
            route_host: route_host.into(),
        }
    }

    /// Loopback base URL published by the supervised host.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Deployment-owned Host header bound during publication.
    pub fn route_host(&self) -> &str {
        &self.route_host
    }
}

impl DevTraceObservation {
    /// W3C-compatible trace identity.
    pub fn trace_id(&self) -> &str {
        &self.trace_id
    }

    /// Root service reported by Tempo.
    pub fn root_service_name(&self) -> &str {
        &self.root_service_name
    }

    /// Root span name reported by Tempo.
    pub fn root_trace_name(&self) -> &str {
        &self.root_trace_name
    }

    /// Root start time in Unix epoch nanoseconds.
    pub const fn start_time_unix_nanos(&self) -> u64 {
        self.start_time_unix_nanos
    }

    /// Trace duration reported by Tempo.
    pub const fn duration(&self) -> Duration {
        self.duration
    }
}

/// Immutable state visible to every development-loop client.
#[derive(Clone, Debug, PartialEq)]
pub struct DevSnapshot {
    revision: u64,
    stages: [DevStageSnapshot; DEV_STAGE_COUNT],
    gate_outcomes: Vec<DevGateOutcome>,
    release: Option<Arc<DevReleaseSnapshot>>,
    runtime_endpoint: Option<DevRuntimeEndpoint>,
    traces: Vec<DevTraceObservation>,
    taps: Vec<DevTapObservation>,
}

impl DevSnapshot {
    fn empty() -> Self {
        Self {
            revision: 0,
            stages: DEV_STAGE_ORDER.map(|stage| DevStageSnapshot {
                stage,
                state: DevStageState::Awaiting,
            }),
            gate_outcomes: Vec::new(),
            release: None,
            runtime_endpoint: None,
            traces: Vec::with_capacity(DEV_OBSERVATION_LIMIT),
            taps: Vec::with_capacity(DEV_OBSERVATION_LIMIT),
        }
    }

    /// Monotonic session-local revision of this snapshot.
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// All twelve stages in execution order.
    pub const fn stages(&self) -> &[DevStageSnapshot; DEV_STAGE_COUNT] {
        &self.stages
    }

    /// Typed outcomes produced by the latest Gate suffix.
    pub fn gate_outcomes(&self) -> &[DevGateOutcome] {
        &self.gate_outcomes
    }

    /// Exact manifest and serving carrier produced by Release.
    pub fn release(&self) -> Option<&DevReleaseSnapshot> {
        self.release.as_deref()
    }

    /// Local endpoint that serves the activated release.
    pub const fn runtime_endpoint(&self) -> Option<&DevRuntimeEndpoint> {
        self.runtime_endpoint.as_ref()
    }

    /// Exact package memberships derived from the serving manifest.
    pub fn memberships(&self) -> impl Iterator<Item = &PackageCoordinate> {
        self.release
            .iter()
            .flat_map(|release| release.manifest.release.packages.iter())
    }

    /// Exact operation facts derived from the serving manifest.
    pub fn operations(
        &self,
    ) -> impl Iterator<Item = (&ServingComponent, &str, &ServingComponentOperation)> {
        self.release
            .iter()
            .flat_map(|release| release.manifest.components.iter())
            .flat_map(|component| {
                component
                    .operations
                    .iter()
                    .map(move |(token, facts)| (component, token.as_str(), facts))
            })
    }

    /// Published routes derived from release attachments.
    pub fn routes(&self) -> impl Iterator<Item = (&str, &ServingAttachment)> {
        self.release
            .iter()
            .flat_map(|release| release.manifest.attachments.iter())
            .filter(|(_, attachment)| {
                matches!(
                    attachment.kind,
                    AttachmentKind::Http | AttachmentKind::Studio
                )
            })
            .map(|(id, attachment)| (id.as_str(), attachment))
    }

    /// Recent trace summaries, oldest first.
    pub fn traces(&self) -> &[DevTraceObservation] {
        &self.traces
    }

    /// Recent router-tap observations, oldest first.
    pub fn taps(&self) -> &[DevTapObservation] {
        &self.taps
    }
}

/// Cloneable read-only entry point for one development-loop session.
#[derive(Clone, Debug)]
pub struct DevReadHandle {
    receiver: watch::Receiver<Arc<DevSnapshot>>,
}

impl DevReadHandle {
    /// Read the latest immutable snapshot without waiting for a change.
    pub fn snapshot(&self) -> Arc<DevSnapshot> {
        Arc::clone(&self.receiver.borrow())
    }

    /// Subscribe to changes after the current snapshot.
    pub fn subscribe(&self) -> DevReadSubscription {
        let mut receiver = self.receiver.clone();
        receiver.borrow_and_update();
        DevReadSubscription { receiver }
    }
}

/// Change subscription without exposing the backing Tokio channel type.
#[derive(Debug)]
pub struct DevReadSubscription {
    receiver: watch::Receiver<Arc<DevSnapshot>>,
}

impl DevReadSubscription {
    /// Read the latest immutable snapshot without consuming an update.
    pub fn current(&self) -> Arc<DevSnapshot> {
        Arc::clone(&self.receiver.borrow())
    }

    /// Wait for a newer snapshot, or return `None` after the session ends.
    pub async fn next(&mut self) -> Option<Arc<DevSnapshot>> {
        self.receiver.changed().await.ok()?;
        Some(Arc::clone(&self.receiver.borrow_and_update()))
    }
}

/// Write side retained by the development engine and observation adapters.
#[derive(Clone, Debug)]
pub(crate) struct DevReadPublisher {
    sender: watch::Sender<Arc<DevSnapshot>>,
}

/// Create the private publisher and public handle for one session.
pub(crate) fn dev_read_channel() -> (DevReadPublisher, DevReadHandle) {
    let (sender, receiver) = watch::channel(Arc::new(DevSnapshot::empty()));
    (DevReadPublisher { sender }, DevReadHandle { receiver })
}

impl DevReadPublisher {
    fn update(&self, update: impl FnOnce(&mut DevSnapshot)) {
        self.sender.send_modify(|current| {
            let snapshot = Arc::make_mut(current);
            update(snapshot);
            snapshot.revision = snapshot
                .revision
                .checked_add(1)
                .expect("a development session cannot publish u64::MAX snapshots");
        });
    }

    /// Invalidate the rerun suffix and the facts that suffix owns.
    pub(crate) fn reset(&self, from: DevStage) {
        self.update(|snapshot| {
            for stage in &mut snapshot.stages[from.position()..] {
                stage.state = DevStageState::Awaiting;
            }
            if from.position() <= DevStage::Gate.position() {
                snapshot.gate_outcomes.clear();
            }
            if from.position() <= DevStage::Release.position() {
                snapshot.release = None;
            }
            if from.position() <= DevStage::Activate.position() {
                snapshot.runtime_endpoint = None;
            }
            snapshot.traces.clear();
            snapshot.taps.clear();
        });
    }

    /// Mark one stage running.
    pub(crate) fn stage_started(&self, stage: DevStage) {
        self.update(|snapshot| {
            snapshot.stages[stage.position()].state = DevStageState::Running;
        });
    }

    /// Mark one stage successfully completed.
    pub(crate) fn stage_completed(&self, stage: DevStage) {
        self.update(|snapshot| {
            snapshot.stages[stage.position()].state = DevStageState::Passed;
        });
    }

    /// Mark one stage failed with its owning typed refusal.
    pub(crate) fn stage_failed(&self, stage: DevStage, failure: DevStageFailure) {
        self.update(|snapshot| {
            snapshot.stages[stage.position()].state = DevStageState::Failed(failure);
        });
    }

    /// Replace Gate outcomes with the latest suffix's exact results.
    pub(crate) fn set_gate_outcomes(&self, outcomes: Vec<DevGateOutcome>) {
        self.update(|snapshot| snapshot.gate_outcomes = outcomes);
    }

    /// Publish the exact manifest and its serving carrier atomically.
    pub(crate) fn set_release(&self, manifest: ServingManifest, carrier: ReleaseCarrier) {
        assert_eq!(
            manifest.digest(),
            carrier.manifest_digest,
            "the release carrier must be derived from the held manifest"
        );
        self.update(|snapshot| {
            snapshot.release = Some(Arc::new(DevReleaseSnapshot { manifest, carrier }));
        });
    }

    /// Publish the route endpoint selected by the exact activated host.
    pub(crate) fn set_runtime_endpoint(&self, endpoint: DevRuntimeEndpoint) {
        self.update(|snapshot| snapshot.runtime_endpoint = Some(endpoint));
    }

    /// Merge one Tempo page while retaining the bounded newest distinct traces.
    pub(crate) fn merge_traces(&self, observations: Vec<DevTraceObservation>) {
        self.sender.send_if_modified(|current| {
            let mut merged = current.traces.clone();
            for observation in observations {
                if let Some(existing) = merged
                    .iter_mut()
                    .find(|existing| existing.trace_id == observation.trace_id)
                {
                    *existing = observation;
                } else {
                    merged.push(observation);
                }
            }
            merged.sort_by(|left, right| {
                left.start_time_unix_nanos
                    .cmp(&right.start_time_unix_nanos)
                    .then_with(|| left.trace_id.cmp(&right.trace_id))
            });
            if merged.len() > DEV_OBSERVATION_LIMIT {
                let excess = merged.len() - DEV_OBSERVATION_LIMIT;
                merged.drain(..excess);
            }
            if merged == current.traces {
                return false;
            }
            let snapshot = Arc::make_mut(current);
            snapshot.traces = merged;
            snapshot.revision = snapshot
                .revision
                .checked_add(1)
                .expect("a development session cannot publish u64::MAX snapshots");
            true
        });
    }

    /// Append one router tap while retaining the bounded newest suffix.
    pub(crate) fn push_tap(&self, observation: DevTapObservation) {
        self.update(|snapshot| push_bounded(&mut snapshot.taps, observation));
    }
}

fn push_bounded<T>(observations: &mut Vec<T>, observation: T) {
    if observations.len() == DEV_OBSERVATION_LIMIT {
        observations.remove(0);
    }
    observations.push(observation);
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use serde_json::json;
    use wamn_authoring_model::ValidatedDraftRef;
    use wamn_catalog::{
        ArtifactHash, DefinitionHash, EffectiveReleaseId, SERVING_MANIFEST_FORMAT_VERSION,
        ServingRelease,
    };

    use super::*;

    const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[test]
    fn initial_snapshot_has_the_exact_stage_order() {
        let (_, handle) = dev_read_channel();
        let snapshot = handle.snapshot();

        assert_eq!(snapshot.revision(), 0);
        assert_eq!(snapshot.stages().len(), DEV_STAGE_COUNT);
        for (actual, expected) in snapshot.stages().iter().zip(DEV_STAGE_ORDER) {
            assert_eq!(actual.stage(), expected);
            assert_eq!(actual.state(), &DevStageState::Awaiting);
        }
    }

    #[tokio::test]
    async fn subscriber_observes_typed_stage_and_gate_changes() {
        let (publisher, handle) = dev_read_channel();
        publisher.stage_completed(DevStage::Migrate);
        let mut subscriber = handle.subscribe();
        assert!(
            !subscriber
                .receiver
                .has_changed()
                .expect("publisher is live")
        );
        publisher.stage_started(DevStage::Gate);

        let running = subscriber.next().await.expect("publisher remains live");
        assert_eq!(
            running.stages()[DevStage::Gate.position()].state(),
            &DevStageState::Running
        );

        publisher.set_gate_outcomes(vec![DevGateOutcome {
            package_id: "receiving".to_owned(),
            package_version: "1.0.0".to_owned(),
            wiring_id: "purchase-order/get".to_owned(),
            wiring_version: 1,
            verdict: DevGateVerdict::Accepted(GateReceipt {
                report_id: "report-1".to_owned(),
                validated_draft: ValidatedDraftRef {
                    validated_draft_id: DIGEST.to_owned(),
                },
            }),
        }]);

        let gated = subscriber.next().await.expect("publisher remains live");
        assert_eq!(gated.gate_outcomes().len(), 1);
        assert!(matches!(
            gated.gate_outcomes()[0].verdict(),
            DevGateVerdict::Accepted(receipt) if receipt.report_id == "report-1"
        ));

        publisher.stage_failed(
            DevStage::Gate,
            DevStageFailure::new(
                "dev-stage-owner-failed",
                "Gate endpoint is unavailable",
                Some("check the Gate endpoint"),
            ),
        );
        let failed = subscriber.next().await.expect("publisher remains live");
        assert!(matches!(
            failed.stages()[DevStage::Gate.position()].state(),
            DevStageState::Failed(failure)
                if failure.code() == "dev-stage-owner-failed"
                    && failure.remedy() == Some("check the Gate endpoint")
        ));
    }

    #[test]
    fn reset_invalidates_only_suffix_owned_release_facts() {
        let (publisher, handle) = dev_read_channel();
        for stage in DEV_STAGE_ORDER {
            publisher.stage_completed(stage);
        }
        let manifest = manifest();
        publisher.set_release(manifest.clone(), carrier(&manifest));
        publisher.set_runtime_endpoint(DevRuntimeEndpoint::new(
            "http://127.0.0.1:38080".to_owned(),
            "receiving.localhost",
        ));

        publisher.reset(DevStage::Apply);
        let snapshot = handle.snapshot();

        assert_eq!(
            snapshot.stages()[DevStage::Publish.position()].state(),
            &DevStageState::Passed
        );
        assert_eq!(
            snapshot.stages()[DevStage::Apply.position()].state(),
            &DevStageState::Awaiting
        );
        assert!(snapshot.release().is_none());
        assert!(snapshot.runtime_endpoint().is_none());
    }

    #[test]
    fn release_views_are_derived_from_the_exact_manifest() {
        let (publisher, handle) = dev_read_channel();
        let manifest = manifest();
        publisher.set_release(manifest.clone(), carrier(&manifest));
        publisher.set_runtime_endpoint(DevRuntimeEndpoint::new(
            "http://127.0.0.1:38080".to_owned(),
            "receiving.localhost",
        ));
        let snapshot = handle.snapshot();

        let memberships = snapshot
            .memberships()
            .map(PackageCoordinate::package_id)
            .collect::<Vec<_>>();
        let operations = snapshot
            .operations()
            .map(|(_, token, _)| token)
            .collect::<Vec<_>>();
        let routes = snapshot.routes().map(|(id, _)| id).collect::<Vec<_>>();

        assert_eq!(memberships, ["receiving"]);
        assert_eq!(operations, ["purchase-order/get"]);
        assert_eq!(routes, ["purchase-order/get", "purchase-order/studio"]);
        assert_eq!(
            snapshot
                .release()
                .expect("release is present")
                .manifest()
                .digest(),
            snapshot
                .release()
                .expect("release is present")
                .carrier()
                .manifest_digest
        );
        let endpoint = snapshot.runtime_endpoint().expect("activation is present");
        assert_eq!(endpoint.base_url(), "http://127.0.0.1:38080");
        assert_eq!(endpoint.route_host(), "receiving.localhost");
    }

    #[test]
    fn observation_buffers_retain_the_newest_bounded_suffix() {
        let (publisher, handle) = dev_read_channel();
        for sequence in 0..=DEV_OBSERVATION_LIMIT {
            publisher.merge_traces(vec![DevTraceObservation {
                trace_id: format!("trace-{sequence}"),
                root_service_name: "wamn-host".to_owned(),
                root_trace_name: "request".to_owned(),
                start_time_unix_nanos: sequence as u64,
                duration: Duration::from_millis(sequence as u64),
            }]);
            publisher.push_tap(DevTapObservation {
                subject: format!("tap.org.project.dev.wiring.{sequence}"),
                record: RouterTapRecord {
                    delivery_id: sequence.to_string().into(),
                    format_version:
                        wamn_runtime::plugins::wamn_jetstream::RouterTapFormatVersion::V1,
                    outcome: None,
                    over_ceiling_bytes: None,
                    payload: json!({"sequence": sequence}),
                    phase: RouterTapRecordPhase::Accepted,
                    redacted: false,
                    source_id: "route".into(),
                    source_kind: RouterTapSourceKind::Attachment,
                    wiring_id: "wiring".into(),
                    wiring_version: 1,
                },
            });
        }

        let snapshot = handle.snapshot();
        assert_eq!(snapshot.traces().len(), DEV_OBSERVATION_LIMIT);
        assert_eq!(snapshot.taps().len(), DEV_OBSERVATION_LIMIT);
        assert_eq!(snapshot.traces()[0].trace_id(), "trace-1");
        assert_eq!(snapshot.taps()[0].delivery_id(), "1");

        publisher.merge_traces(vec![DevTraceObservation {
            trace_id: "trace-0".to_owned(),
            root_service_name: "wamn-host".to_owned(),
            root_trace_name: "request".to_owned(),
            start_time_unix_nanos: 0,
            duration: Duration::ZERO,
        }]);
        let snapshot = handle.snapshot();
        assert_eq!(snapshot.traces()[0].trace_id(), "trace-1");
        assert_eq!(
            snapshot.traces()[DEV_OBSERVATION_LIMIT - 1].trace_id(),
            "trace-100"
        );
    }

    fn carrier(manifest: &ServingManifest) -> ReleaseCarrier {
        ReleaseCarrier {
            artifact_base: "registry.example.test/wamn/releases".to_owned(),
            manifest_digest: manifest.digest(),
        }
    }

    fn manifest() -> ServingManifest {
        let package = PackageCoordinate::new("receiving", "1.0.0").expect("valid package");
        let operation = ServingComponentOperation {
            registered_operation: Some("receiving@1.0.0::purchase-order/get".to_owned()),
            dependencies: Vec::new(),
            statements: BTreeMap::new(),
        };
        let component = ServingComponent {
            package_id: "receiving".to_owned(),
            component: "receiving".to_owned(),
            interface_version: "0.1.0".to_owned(),
            digest: ArtifactHash::parse(DIGEST).expect("valid digest"),
            operations: BTreeMap::from([("purchase-order/get".to_owned(), operation)]),
        };
        let attachment = ServingAttachment {
            kind: AttachmentKind::Http,
            package_id: "receiving".to_owned(),
            wiring_id: "purchase-order/get".to_owned(),
            wiring_version: 1,
            definition_hash: DefinitionHash::parse(DIGEST).expect("valid digest"),
            definition: json!({
                "route": {"method": "POST", "path": "/purchase-orders/get"}
            }),
            auth_policy: json!({"mode": "pat"}),
            registered_operation: Some("receiving@1.0.0::purchase-order/get".to_owned()),
        };
        ServingManifest {
            format_version: SERVING_MANIFEST_FORMAT_VERSION,
            release: ServingRelease {
                tenant_id: "tenant-a".to_owned(),
                effective_release_id: EffectiveReleaseId::new(1).expect("valid release"),
                environment: "dev".to_owned(),
                packages: BTreeSet::from([package]),
            },
            components: BTreeSet::from([component]),
            wirings: BTreeSet::new(),
            attachments: BTreeMap::from([
                ("purchase-order/get".to_owned(), attachment.clone()),
                (
                    "purchase-order/studio".to_owned(),
                    ServingAttachment {
                        kind: AttachmentKind::Studio,
                        definition: json!({
                            "route": {"method": "POST", "path": "/studio"}
                        }),
                        ..attachment
                    },
                ),
            ]),
            registrations: BTreeMap::new(),
        }
    }
}
