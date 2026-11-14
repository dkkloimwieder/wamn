//! Native host-route requests and operation authorization.
use crate::engine::MAX_HOST_CALL_DURATION;
use crate::flow_http_routing::AuthenticatedCaller;
use std::fmt;
use wamn_catalog::{AdmittedComponentOperation, HostAttachment, ServingManifest};

pub const OPERATION_METRICS_ID: &str = "wamn-operation";
pub const PERMISSION_DENIED: &str = "permission-denied";
pub const EXECUTION_FAILED: &str = "execution-failed";
pub const FRESH_CREDENTIAL_REQUIRED: &str = "fresh-credential-required";

#[derive(Debug, Clone)]
pub enum Source {
    Attachment(String),
    Registration(String),
}

#[derive(Debug, Clone)]
pub struct TraceContext {
    pub traceparent: String,
    pub tracestate: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ParentCausation {
    pub root: String,
    pub depth: u32,
}

#[derive(Debug, Clone)]
pub struct DeliveryRequest {
    pub source: Source,
    pub delivery_id: String,
    pub payload: String,
    pub caller: Option<AuthenticatedCaller>,
    pub trace: Option<TraceContext>,
    pub parent_causation: Option<ParentCausation>,
    pub if_none_match: Option<String>,
}

/// The JSON response of a native platform endpoint.
pub type DeliveryOutcome = String;

#[derive(Debug, Clone)]
pub struct PermissionDenial {
    pub operation: String,
}

#[derive(Debug, Clone)]
pub enum DeliveryError {
    SourceNotFound,
    InvalidRequest,
    InvalidPayload,
    ExecutionFailed,
    PermissionDenied(PermissionDenial),
    FreshCredentialRequired(PermissionDenial),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DeadlineAdjustment {
    pub node: String,
    pub requested_ms: u64,
    pub effective_ms: u64,
}

#[derive(Debug, Clone)]
pub struct DeliveryReport {
    pub outcome: Result<DeliveryOutcome, DeliveryError>,
    pub deadline_adjustments: Vec<DeadlineAdjustment>,
    pub etag: Option<String>,
}
/// Admit one native platform endpoint under its authenticated attachment.
pub fn resolve_authorized_host_route(
    manifest: &ServingManifest,
    id: &str,
    caller: Option<&AuthenticatedCaller>,
) -> Option<Result<&'static HostAttachment, DeliveryError>> {
    let attachment = manifest.host_attachment(id)?;
    if caller.is_none_or(|caller| caller.attachment_id() != id) {
        return Some(Err(DeliveryError::InvalidRequest));
    }
    Some(
        authorize_registered_operation(caller, attachment.registered_operation())
            .map(|()| attachment)
            .map_err(|denial| lower_operation_refusal(&denial)),
    )
}

/// The wire refusal for an operation the caller may not invoke.
pub fn lower_operation_refusal(denial: &OperationRefusal) -> DeliveryError {
    let detail = PermissionDenial {
        operation: denial.operation().to_owned(),
    };
    match denial.refusal_type() {
        OperationRefusalType::PermissionDenied => DeliveryError::PermissionDenied(detail),
    }
}

/// How the router driver answered one delivery. The variants are the arms of
/// the driver match in the host's delivery. The host classifies nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryClass {
    Delivered,
    PermissionDenied,
    ExecutionFailed,
}

impl DeliveryClass {
    /// The `wamn.delivery.error` value, or `None` for the one delivered class.
    pub fn error(self) -> Option<&'static str> {
        match self {
            DeliveryClass::Delivered => None,
            DeliveryClass::PermissionDenied => Some(PERMISSION_DENIED),
            DeliveryClass::ExecutionFailed => Some(EXECUTION_FAILED),
        }
    }
}

/// Why an originating caller cannot invoke a registered operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationRefusalType {
    PermissionDenied,
}

/// Exact operation authority missing from the originating caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationRefusal {
    type_: OperationRefusalType,
    operation: Box<str>,
}

impl OperationRefusal {
    pub fn new(type_: OperationRefusalType, operation: impl Into<Box<str>>) -> Self {
        Self {
            type_,
            operation: operation.into(),
        }
    }

    pub fn refusal_type(&self) -> OperationRefusalType {
        self.type_
    }

    pub fn operation(&self) -> &str {
        &self.operation
    }
}

impl fmt::Display for OperationRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reason = match self.type_ {
            OperationRefusalType::PermissionDenied => "permission denied",
        };
        write!(formatter, "{reason} for operation {}", self.operation)
    }
}

impl std::error::Error for OperationRefusal {}

/// Make sure of the grant of a released operation before its entry runs: the caller
/// must hold the entry's own registered operation.
pub fn authorize_released_operation(
    caller: Option<&AuthenticatedCaller>,
    released: &AdmittedComponentOperation,
) -> Result<(), OperationRefusal> {
    authorize_registered_operation(caller, released.registered_operation.as_deref())
}

/// Make sure that the originating caller holds the registered operation grant.
pub fn authorize_registered_operation(
    caller: Option<&AuthenticatedCaller>,
    operation: Option<&str>,
) -> Result<(), OperationRefusal> {
    let Some(operation) = operation else {
        return Ok(());
    };
    caller
        .filter(|caller| caller.permits(operation))
        .map(|_| ())
        .ok_or_else(|| OperationRefusal::new(OperationRefusalType::PermissionDenied, operation))
}

/// The host call ceiling in milliseconds. The constant is well under an hour,
/// so it fits every width this bound converts to.
fn max_host_call_ms() -> u64 {
    u64::try_from(MAX_HOST_CALL_DURATION.as_millis()).unwrap_or(u64::MAX)
}

/// A node deadline bounded to the host call ceiling.
pub fn bounded_node_deadline_ms(deadline_ms: Option<u64>) -> u64 {
    deadline_ms
        .unwrap_or(max_host_call_ms())
        .clamp(1, max_host_call_ms())
}
