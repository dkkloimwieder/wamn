//! Immutable read model for one local development-loop session.
//!
//! The public handle and subscription hide the Tokio watch channel. Snapshots
//! use shared ownership because they carry a complete serving manifest plus
//! bounded payload observations; copying either on every UI refresh would make
//! the read seam itself observable work.

pub(super) mod plan;

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use wamn_authoring_model::{GateRefusal, GateResult};
use wamn_catalog::{
    AttachmentType, PackageCoordinate, ServingAttachment, ServingComponent,
    ServingComponentOperation, ServingManifest,
};

use super::{DEV_STAGE_ORDER, DevStage, DevStageFailure};
use wamn_release_control::release_carrier::ReleaseCarrier;

pub use plan::DEV_STAGE_COUNT;

pub use plan::DEV_OBSERVATION_LIMIT;

pub use plan::DevStageState;

pub use plan::DevStageSnapshot;

pub use plan::DevGateVerdict;

pub use plan::DevGateOutcome;

pub use plan::DevReleaseSnapshot;

pub use plan::DevTraceObservation;

pub use plan::DevRuntimeEndpoint;

pub use plan::DevSnapshot;

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

pub(crate) use publisher::{DevReadPublisher, dev_read_channel};

mod publisher {
    use super::{
        DEV_OBSERVATION_LIMIT, DevReadHandle, DevReleaseSnapshot, DevRuntimeEndpoint, DevSnapshot,
        DevStageState, DevTraceObservation,
    };
    use crate::dev::{DevStage, DevStageFailure};
    use std::sync::Arc;
    use tokio::sync::watch;
    use wamn_catalog::ServingManifest;
    use wamn_release_control::release_carrier::ReleaseCarrier;
    /// Write side retained by the development engine and observation adapters.
    #[derive(Clone, Debug)]
    pub struct DevReadPublisher {
        sender: watch::Sender<Arc<DevSnapshot>>,
    }

    /// Create the private publisher and public handle for one session.
    pub fn dev_read_channel() -> (DevReadPublisher, DevReadHandle) {
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
        pub fn reset(&self, from: DevStage) {
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
                snapshot.traces.clear();
            });
        }

        /// Mark one stage running.
        pub fn stage_started(&self, stage: DevStage) {
            self.update(|snapshot| {
                snapshot.stages[stage.position()].state = DevStageState::Running;
            });
        }

        /// Mark one stage successfully completed.
        pub fn stage_completed(&self, stage: DevStage) {
            self.update(|snapshot| {
                snapshot.stages[stage.position()].state = DevStageState::Passed;
            });
        }

        /// Mark one stage skipped because its input is unchanged.
        pub fn stage_skipped(&self, stage: DevStage) {
            self.update(|snapshot| {
                snapshot.stages[stage.position()].state = DevStageState::Skipped;
            });
        }

        /// Mark one stage failed with its owning typed refusal.
        pub fn stage_failed(&self, stage: DevStage, failure: DevStageFailure) {
            self.update(|snapshot| {
                snapshot.stages[stage.position()].state = DevStageState::Failed(failure);
            });
        }

        /// Replace Gate outcomes with the latest suffix's exact results.
        #[cfg(test)]
        pub fn set_gate_outcomes(&self, outcomes: Vec<super::DevGateOutcome>) {
            self.update(|snapshot| snapshot.gate_outcomes = outcomes);
        }

        /// Publish the exact manifest and its serving carrier atomically.
        pub fn set_release(&self, manifest: ServingManifest, carrier: ReleaseCarrier) {
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
        pub fn set_runtime_endpoint(&self, endpoint: DevRuntimeEndpoint) {
            self.update(|snapshot| snapshot.runtime_endpoint = Some(endpoint));
        }

        /// Remove live availability at the actual target shutdown boundary.
        pub fn clear_runtime_endpoint(&self) {
            self.update(|snapshot| snapshot.runtime_endpoint = None);
        }

        /// Merge sampled collector traces while retaining the bounded newest distinct traces.
        pub fn merge_traces(&self, observations: Vec<DevTraceObservation>) {
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
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use serde_json::json;
    use wamn_authoring_model::ValidatedDraftRef;
    use wamn_catalog::{DefinitionHash, SERVING_MANIFEST_FORMAT_VERSION, ServingRelease};

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
            package_id: "platform_fixture".to_owned(),
            package_version: "1.0.0".to_owned(),
            wiring_id: "widget/get".to_owned(),
            wiring_version: 1,
            verdict: DevGateVerdict::Accepted(GateResult {
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
            DevGateVerdict::Accepted(result) if result.report_id == "report-1"
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
            "fixture.localhost",
            "target-one",
        ));

        publisher.reset(DevStage::Acl);
        let snapshot = handle.snapshot();

        assert_eq!(
            snapshot.stages()[DevStage::Virtualize.position()].state(),
            &DevStageState::Passed
        );
        assert_eq!(
            snapshot.stages()[DevStage::Acl.position()].state(),
            &DevStageState::Awaiting
        );
        assert_eq!(
            snapshot.stages()[DevStage::Release.position()].state(),
            &DevStageState::Awaiting
        );
        assert!(snapshot.release().is_none());
        assert_eq!(
            snapshot.runtime_endpoint().unwrap().target_instance(),
            "target-one"
        );
        publisher.stage_failed(
            DevStage::Acl,
            super::super::DevStageFailure::new(
                "preparation-failed",
                "the previous activation remains",
                None,
            ),
        );
        assert!(handle.snapshot().runtime_endpoint().is_some());
        publisher.clear_runtime_endpoint();
        assert!(handle.snapshot().runtime_endpoint().is_none());
    }

    #[test]
    fn release_views_are_derived_from_the_exact_manifest() {
        let (publisher, handle) = dev_read_channel();
        let manifest = manifest();
        publisher.set_release(manifest.clone(), carrier(&manifest));
        publisher.set_runtime_endpoint(DevRuntimeEndpoint::new(
            "http://127.0.0.1:38080".to_owned(),
            "fixture.localhost",
            "target-one",
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

        assert_eq!(memberships, ["platform_fixture"]);
        assert_eq!(operations, ["widget/get"]);
        assert_eq!(routes, ["widget/get", "widget/studio"]);
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
        assert_eq!(endpoint.route_host(), "fixture.localhost");
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
        }

        let snapshot = handle.snapshot();
        assert_eq!(snapshot.traces().len(), DEV_OBSERVATION_LIMIT);
        assert_eq!(snapshot.traces()[0].trace_id(), "trace-1");

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
        let package = PackageCoordinate::new("platform_fixture", "1.0.0").expect("valid package");
        let operation = ServingComponentOperation {
            committed_result_schema: None,
            registered_operation: Some("platform_fixture@1.0.0::widget/get".to_owned()),
            permissions: BTreeSet::from(["platform_fixture@1.0.0::widget/get".to_owned()]),
            statements: BTreeMap::new(),
        };
        let component = ServingComponent {
            runtime_schema: None,
            statement_instances: std::collections::BTreeMap::new(),
            package_id: "platform_fixture".to_owned(),
            operations: BTreeMap::from([("widget/get".to_owned(), operation)]),
            descriptor: wamn_catalog::ComponentDescriptor::named("fixture", "0.1.0", DIGEST),
        };
        let attachment = ServingAttachment {
            type_: AttachmentType::Http,
            package_id: "platform_fixture".to_owned(),
            target: wamn_catalog::AttachmentTarget::Route {
                component: "fixture".to_owned(),
                operation: "widget/get".to_owned(),
            },
            definition_hash: DefinitionHash::parse(DIGEST).expect("valid digest"),
            definition: json!({
                "route": {"method": "POST", "path": "/widgets/get"}
            }),
            auth_policy: json!({"modes": ["pat"]}),
            registered_operation: Some("platform_fixture@1.0.0::widget/get".to_owned()),
        };
        ServingManifest {
            composed_entry: None,
            format_version: SERVING_MANIFEST_FORMAT_VERSION,
            release: ServingRelease {
                packages: BTreeSet::from([package]),
            },
            components: BTreeSet::from([component]),
            routes: BTreeSet::new(),
            attachments: ServingAttachment::routes(BTreeMap::from([
                ("widget/get".to_owned(), attachment.clone()),
                (
                    "widget/studio".to_owned(),
                    ServingAttachment {
                        type_: AttachmentType::Studio,
                        definition: json!({
                            "route": {"method": "POST", "path": "/studio"}
                        }),
                        ..attachment
                    },
                ),
            ])),
            host_routes: BTreeSet::new(),
        }
    }
}

#[cfg(feature = "test-util")]
pub mod test_support {
    pub use super::publisher::{DevReadPublisher, dev_read_channel};
    use super::{DevGateOutcome, DevGateVerdict, DevRuntimeEndpoint, DevTraceObservation};
    use std::time::Duration;

    pub fn gate_outcome(
        package_id: String,
        package_version: String,
        wiring_id: String,
        wiring_version: u32,
        verdict: DevGateVerdict,
    ) -> DevGateOutcome {
        DevGateOutcome {
            package_id,
            package_version,
            wiring_id,
            wiring_version,
            verdict,
        }
    }

    pub fn trace_observation(
        trace_id: String,
        root_service_name: String,
        root_trace_name: String,
        start_time_unix_nanos: u64,
        duration: Duration,
    ) -> DevTraceObservation {
        DevTraceObservation {
            trace_id,
            root_service_name,
            root_trace_name,
            start_time_unix_nanos,
            duration,
        }
    }

    pub fn runtime_endpoint(
        base_url: String,
        route_host: &str,
        target_instance: &str,
    ) -> DevRuntimeEndpoint {
        DevRuntimeEndpoint::new(base_url, route_host, target_instance)
    }
}
