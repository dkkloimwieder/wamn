use super::{
    AdmittedComponentOperation, Arc, AttachmentType, DEV_STAGE_ORDER, DevStage, DevStageFailure,
    Duration, PackageCoordinate, ReleaseCarrier, ServingAttachment, ServingComponent,
    ServingManifest,
};

/// Number of stages in one development-loop run.
pub const DEV_STAGE_COUNT: usize = DEV_STAGE_ORDER.len();

/// Maximum trace observations retained by one session.
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
    /// This stage did not run, because its input is unchanged and its output
    /// is already present.
    Skipped,
    /// This stage refused or failed.
    Failed(DevStageFailure),
}

/// State of one exact member of [`DEV_STAGE_ORDER`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DevStageSnapshot {
    pub(super) stage: DevStage,
    pub(super) state: DevStageState,
}

/// Exact release document and the carrier derived from those bytes.
#[derive(Clone, Debug, PartialEq)]
pub struct DevReleaseSnapshot {
    pub(super) manifest: ServingManifest,
    pub(super) carrier: ReleaseCarrier,
}

/// One trace summary from sampled collector output.
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
    pub(super) base_url: Box<str>,
    pub(super) route_host: Box<str>,
    pub(super) target_instance: Box<str>,
}

/// Immutable state visible to every development-loop client.
#[derive(Clone, Debug, PartialEq)]
pub struct DevSnapshot {
    pub(super) revision: u64,
    pub(super) stages: [DevStageSnapshot; DEV_STAGE_COUNT],
    pub(super) release: Option<Arc<DevReleaseSnapshot>>,
    pub(super) runtime_endpoint: Option<DevRuntimeEndpoint>,
    pub(super) traces: Vec<DevTraceObservation>,
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

impl DevReleaseSnapshot {
    /// Immutable serving manifest published by Release.
    pub const fn manifest(&self) -> &ServingManifest {
        &self.manifest
    }

    /// Exact registry and manifest digest carried to serving processes.
    pub const fn carrier(&self) -> &ReleaseCarrier {
        &self.carrier
    }
}

impl DevTraceObservation {
    /// W3C-compatible trace identity.
    pub fn trace_id(&self) -> &str {
        &self.trace_id
    }

    /// Service of the root span, or the earliest observed span.
    pub fn root_service_name(&self) -> &str {
        &self.root_service_name
    }

    /// Name of the root span, or the earliest observed span.
    pub fn root_trace_name(&self) -> &str {
        &self.root_trace_name
    }

    /// Root start time in Unix epoch nanoseconds.
    pub const fn start_time_unix_nanos(&self) -> u64 {
        self.start_time_unix_nanos
    }

    /// Elapsed interval covered by the observed spans.
    pub const fn duration(&self) -> Duration {
        self.duration
    }
}

impl DevRuntimeEndpoint {
    pub(crate) fn new(base_url: String, route_host: &str, target_instance: &str) -> Self {
        Self {
            base_url: base_url.into_boxed_str(),
            route_host: route_host.into(),
            target_instance: target_instance.into(),
        }
    }

    /// Loopback base URL published by the supervised host.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Exact creation of the target database served by this activation.
    pub fn target_instance(&self) -> &str {
        &self.target_instance
    }

    /// Deployment-owned Host header bound during publication.
    pub fn route_host(&self) -> &str {
        &self.route_host
    }
}

impl DevSnapshot {
    pub(super) fn empty() -> Self {
        Self {
            revision: 0,
            stages: DEV_STAGE_ORDER.map(|stage| DevStageSnapshot {
                stage,
                state: DevStageState::Awaiting,
            }),
            release: None,
            runtime_endpoint: None,
            traces: Vec::with_capacity(DEV_OBSERVATION_LIMIT),
        }
    }

    /// Monotonic session-local revision of this snapshot.
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// All stages in execution order.
    pub const fn stages(&self) -> &[DevStageSnapshot; DEV_STAGE_COUNT] {
        &self.stages
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
    ) -> impl Iterator<Item = (&ServingComponent, &str, &AdmittedComponentOperation)> {
        self.release
            .iter()
            .flat_map(|release| release.manifest.components.iter())
            .flat_map(|component| {
                component
                    .descriptor
                    .operations
                    .iter()
                    .map(move |(token, facts)| (component, token.as_str(), facts))
            })
    }

    /// Published routes derived from release attachments.
    pub fn routes(&self) -> impl Iterator<Item = (&str, ServingAttachment)> {
        self.release
            .iter()
            .flat_map(|release| release.manifest.every_attachment())
            .filter(|(_, attachment)| {
                matches!(
                    attachment.attachment_type(),
                    AttachmentType::Http | AttachmentType::Studio
                )
            })
            .map(|(id, attachment)| (id, ServingAttachment::from(attachment)))
    }

    /// Recent trace summaries, oldest first.
    pub fn traces(&self) -> &[DevTraceObservation] {
        &self.traces
    }
}
