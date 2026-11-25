//! Declared refusal bodies for native routes.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(tag = "code", content = "detail", rename_all = "snake_case")]
pub enum DeclaredRefusal {
    ActiveRoleRequired {
        field: Value,
    },
    AdminCovered {
        field: Value,
    },
    AdminFixed {
        field: Value,
    },
    ApplicationWriteIncomplete {
        environment: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        completed: Option<Value>,
    },
    ApplyInProgress {},
    BindingRefused {
        field: Value,
    },
    CdnHostRequired {
        field: Value,
    },
    CounterAssignedPerKey {
        field: Value,
    },
    CounterNotFound {
        field: Value,
    },
    DeploymentInProgress {
        field: Value,
    },
    EnvironmentNotFound {
        field: Value,
    },
    ExclusiveConnectionRequiresLocalTarget {
        field: Value,
        reason: Value,
    },
    FileNotFound {
        field: Value,
    },
    FilePrivate {
        field: Value,
    },
    FileReferenced {
        field: Value,
    },
    IdempotencyConflict {
        field: Value,
    },
    ImmutableField {
        field: Value,
    },
    InvalidInput {
        field: Value,
    },
    Ipv6AuthorityNotSupported {
        field: Value,
        reason: Value,
    },
    NotFound {
        field: Value,
    },
    OperationNotGrantable {
        field: Value,
    },
    OperationNotServed {
        field: Value,
    },
    PermissionDenied {
        operation: Value,
    },
    PermissionNotHeld {
        field: Value,
    },
    ProjectExists {
        field: Value,
    },
    ProjectNotFound {
        field: Value,
    },
    Referenced {
        field: Value,
    },
    ResourceInUse {
        field: Value,
    },
    RoleNotFound {
        field: Value,
    },
    SequenceNotFound {
        field: Value,
    },
    UnsupportedAuthType {
        field: Value,
    },
    UserInAnotherOrg {
        field: Value,
    },
    UserNotActive {
        field: Value,
    },
    UserNotFound {
        field: Value,
    },
    UserRefused {
        field: Value,
    },
    WorkflowNotEnabled {
        field: Value,
    },
    WorkflowPathTaken {
        path: Value,
    },
    WorkflowVersionExists {},
    WorkflowVersionNotFound {},
    WorkloadNotReady {
        field: Value,
    },
}
