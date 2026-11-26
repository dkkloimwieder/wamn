//! The fixed routes that the host runs itself
//! (docs/architecture/execution.md#host-run-routes).
//!
//! A host route has the operation contract, request envelope, result shape,
//! limits and CSRF treatment of any route, but the host serves it with a
//! fixed handler instead of a guest component. The platform builds the routes
//! of each set, and a serving manifest records only which sets it serves.
//!
//! Each route derives its operation token, path and attachment id from the
//! native package identity and route prefixes, as a generated package route does.

use crate::host_wire::{
    ApiServiceCreateRequest, ApiServiceCreateResult, AppDeployRequest, AppDeployResult,
    AppRollbackRequest, AppStopRequest, AppUserListResult, CaptureGapCloseRequest,
    ConnectionActivateRequest, ConnectionAuthorizeRequest, ConnectionAuthorizeResult,
    ConnectionCopyRequest, ConnectionGetResult, ConnectionListResult,
    ConnectionSetCredentialRequest, ConnectionStatusResult, ConnectionSummary, ConnectionTest,
    ConnectionTestRequest, ConnectionWriteRequest, ControlMineRequest, ControlMineResult,
    DeclaredRefusal, Empty, EnvConfigureRequest, EnvListRequest, EnvListResult,
    EnvOperationStatusRequest, EnvOperationStatusResult, EnvRequest, EnvStatus, EnvStatusResult,
    ExternalSystemDeleteResult, ExternalSystemListResult, FileDeleteRequest, FileDeleteResult,
    FileExposeRequest, FileExposeResult, FileGetRequest, FilePutRequest,
    IntegrationTypeListRequest, IntegrationTypeListResult, MemberGrantRequest, MemberGrantResult,
    MemberListRequest, MemberListResult, NumberCounterSetRequest, NumberCounterSetResult,
    OrgAdminGrantRequest, OrgAdminGrantResult, PatIssueRequest, PatListRequest, PatListResult,
    PatRevokeRequest, PermissionGrantRequest, PermissionGrantResult, PermissionListRequest,
    PermissionListResult, PermissionMineResult, PermissionRevokeResult, PlatformSchemaRequest,
    ProjectAdminGrantRequest, ProjectAdminGrantResult, ProjectCreateRequest, ProjectCreated,
    ProjectEnvDeactivateRequest, ProjectListResult, ProjectRequest, ProjectStatus,
    RoleActivateResult, RoleCreateResult, RoleDeactivateResult, RoleListResult, SchemaDiffRequest,
    ServicePrincipalCreateRequest, ServicePrincipalCreateResult, ServicePrincipalDisableResult,
    UserActivateResult, UserCreateRequest, UserCreateResult, UserInviteRequest, UserInviteResult,
    UserListResult, UserRoleGrantRequest, UserRoleGrantResult, UserRoleRevokeResult,
    WorkflowAuthorRequest, WorkflowAuthorResult, WorkflowDisableRequest, WorkflowRunRequest,
    WorkflowRunResult, schema, schemas,
};
use crate::{route_attachment_id, route_path};
use RefusalSource::{MalformedInput, PermissionDenied, TransactionInvariant};
use std::collections::BTreeMap;
use std::sync::LazyLock;
use wamn_package::canonical_name::CanonicalName;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use wamn_package::{
    AttachmentType, OperationType, PAT_AUTHENTICATION_MODE, SESSION_AUTHENTICATION_MODE,
};
use wamn_package::{operation_token, sealed_operation_reference};

const PACKAGE: &str = "wamn_control";
const VERSION: &str = "0.10.0";
const PATH_PREFIX: &str = "/wamn_control";
const ID_PREFIX: &str = "wamn-control";

/// The package of every host route. No application package has this id.
pub fn host_route_package() -> &'static wamn_package::PackageName {
    static ID: LazyLock<wamn_package::PackageName> =
        LazyLock::new(|| PACKAGE.parse().expect("fixed native package id"));
    &ID
}

/// The authored version of the host route contract.
pub fn host_route_version() -> &'static wamn_package::package_version::PackageVersion {
    static PARSED_VERSION: LazyLock<wamn_package::package_version::PackageVersion> =
        LazyLock::new(|| VERSION.parse().expect("fixed native package version"));
    &PARSED_VERSION
}

/// Whether a stable operation reference or sealed operation id names a host
/// route. Most host routes are fixed to `admin` or to every member, so no
/// authored role holds one (docs/architecture/execution.md#host-run-routes). A
/// [`HostRouteAuthority::Granted`] route is the exception: see
/// [`is_granted_host_route_operation`].
pub fn is_host_route_operation(operation: &str) -> bool {
    operation
        .split_once(':')
        .is_some_and(|(package, _)| package == ID_PREFIX)
}

/// Whether a stable operation reference or sealed operation id names a host
/// route that an authored role holds through a stored permission row
/// ([`HostRouteAuthority::Granted`]), such as `number_counter.set`.
pub fn is_granted_host_route_operation(operation: &str) -> bool {
    ATTACHMENTS.values().any(|attachment| {
        attachment.route.authority == HostRouteAuthority::Granted
            && (attachment.reference == operation || attachment.operation.as_str() == operation)
    })
}

/// The path prefix of every host route, with its closing `/`. Publish
/// refuses an authored route under it.
pub fn host_route_path_prefix() -> &'static str {
    static PREFIX: LazyLock<String> = LazyLock::new(|| format!("{PATH_PREFIX}/"));
    &PREFIX
}

/// One fixed contract of host routes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HostRouteSet {
    /// The application administration contract, served under every
    /// application audience against that environment's database.
    App,
    /// The org and project administration contract of the control serving
    /// root.
    Control,
}

/// The host code that serves one host route.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HostHandler {
    /// Start or observe one authorized application deployment or schema Job.
    OperationJob,
    /// Execute an authenticated native control operation.
    ControlExecution,
    /// Project integration and environment connection administration.
    IntegrationAdministration,
    /// The caller's effective permission set in this application.
    PermissionMine,
    /// The user rows of the application
    /// (docs/architecture/execution.md#host-run-routes).
    AppUserList,
    /// The active roles of the application.
    RoleList,
    /// Create one empty authored role.
    RoleCreate,
    /// Suspend an authored role's grants.
    RoleDeactivate,
    /// Restore an authored role's grants.
    RoleActivate,
    /// The permission rows of one role, with the roots that require each.
    PermissionList,
    /// Select an operation for a role and write its released closure.
    PermissionGrant,
    /// Remove a role's selection of an operation and its closure rows.
    PermissionRevoke,
    /// Give a role to an application user.
    UserRoleGrant,
    /// Take a role from an application user.
    UserRoleRevoke,
    /// The caller's current control authority in the token's org.
    ControlMine,
    /// The members of the org
    /// (docs/architecture/overview.md#organization-membership-and-administration).
    UserList,
    /// Create or reuse a user and make it a member of the org.
    UserCreate,
    /// Create or reuse a user, make it a member of the org with the
    /// requested grants, and mail the invitation when it has no password.
    UserInvite,
    /// Disable the global principal of a user of the org.
    UserDisable,
    /// Make an org membership active.
    UserActivate,
    /// Revoke a member's access in the org from the leaves upward.
    UserDeactivate,
    /// The projects of the org.
    ProjectList,
    /// Grant `org-admin` and its project roles and memberships.
    OrgAdminGrant,
    /// Revoke `org-admin` and `project-admin` throughout the org.
    OrgAdminRevoke,
    /// The environments of one project
    /// (docs/architecture/execution.md#host-run-routes).
    EnvList,
    /// The current state of one environment in its release namespace: the
    /// digest, the rollout, the workload conditions and the live set
    /// (docs/architecture/execution.md#host-run-routes).
    EnvStatus,
    /// The users with a membership or `project-admin` in one project.
    MemberList,
    /// Grant a membership of one environment to an active org member.
    MemberGrant,
    /// Revoke a membership of one environment.
    MemberRevoke,
    /// Grant `project-admin` and a membership of every environment of the
    /// project.
    ProjectAdminGrant,
    /// Revoke `project-admin` in one project.
    ProjectAdminRevoke,
    /// The exported operations of one release of an environment
    /// (docs/architecture/execution.md#host-run-routes).
    /// Make one environment active
    /// (docs/architecture/execution.md#host-run-routes).
    EnvActivate,
    /// Make one environment inactive.
    EnvDeactivate,
    /// Make every environment of one project active.
    ProjectActivate,
    /// Make every environment of one project inactive.
    ProjectInactivate,
    /// Create a project with no environment
    /// (docs/architecture/execution.md#host-run-routes).
    ProjectCreate,
    /// Set one counter of a number sequence of the application
    /// (docs/architecture/data-access.md#setting-a-counter).
    NumberCounterSet,
    WorkflowAuthor,
    WorkflowEnable,
    WorkflowDisable,
    WorkflowList,
    WorkflowRun,
    /// Stream a file through a declared write binding.
    FilePut,
    /// Read a file owned by the application.
    FileGet,
    /// Delete a file unless an application row references it.
    FileDelete,
    /// Change a file's exposure.
    FileExpose,
}

/// Who may call one host route, beside a valid credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostRouteAuthority {
    /// Every authenticated caller. On the control set, that is a holder of
    /// `org-admin` in the token's org or of `project-admin` in one of its
    /// projects, and the handler tests it again.
    Member,
    /// A holder of the application role `admin`.
    Admin,
    /// A holder of `org-admin` in the token's org. The handler tests the
    /// role, again in its write transaction.
    OrgAdmin,
    /// A holder of `org-admin` in the token's org, or of `project-admin` in
    /// the project that the request names. The handler tests the role,
    /// again in its write transaction.
    ProjectAdmin,
    /// A holder of `admin`, or of an authored role that holds the route's
    /// operation as a stored permission row, as for a package operation.
    /// `permission.grant` gives it to a role
    /// (docs/architecture/data-access.md#setting-a-counter, wamn-83eth.3).
    Granted,
}

/// One fixed host route.
#[derive(Debug)]
pub struct HostRoute {
    pub set: HostRouteSet,
    /// The model and action, such as `permission.mine`, as a package
    /// operation names them.
    pub operation: &'static str,
    pub type_: OperationType,
    pub authority: HostRouteAuthority,
    pub handler: HostHandler,
    schemas: fn() -> (Value, Value),
    write_transaction: bool,
    refusals: &'static [RefusalSource],
}

#[derive(Debug)]
enum RefusalSource {
    MalformedInput(&'static str, Option<&'static str>),
    PermissionDenied,
    TransactionInvariant(&'static str, Option<&'static str>),
}

impl RefusalSource {
    fn parts(&self) -> (&'static str, Option<&'static str>, &'static str) {
        match *self {
            Self::MalformedInput(code, text) => (code, text, "malformed_input"),
            Self::PermissionDenied => ("permission_denied", None, "permission_denied"),
            Self::TransactionInvariant(code, text) => (code, text, "transaction_invariant"),
        }
    }
}

const ROUTES: &[HostRoute] = &[
    HostRoute {
        set: HostRouteSet::Control,
        operation: "env.configure",
        type_: OperationType::Command,
        authority: HostRouteAuthority::OrgAdmin,
        handler: HostHandler::ControlExecution,
        schemas: schemas::<EnvConfigureRequest, AppDeployResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant("env_not_found", None),
            TransactionInvariant("invalid_input", None),
            TransactionInvariant("not_found", None),
            PermissionDenied,
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "project_env.deactivate",
        type_: OperationType::Command,
        authority: HostRouteAuthority::ProjectAdmin,
        handler: HostHandler::ControlExecution,
        schemas: schemas::<ProjectEnvDeactivateRequest, AppDeployResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant("env_not_found", None),
            TransactionInvariant("invalid_input", None),
            TransactionInvariant("not_found", None),
            PermissionDenied,
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "env.operation_status",
        type_: OperationType::Get,
        authority: HostRouteAuthority::ProjectAdmin,
        handler: HostHandler::ControlExecution,
        schemas: schemas::<EnvOperationStatusRequest, EnvOperationStatusResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant("env_not_found", None),
            TransactionInvariant("invalid_input", None),
            TransactionInvariant("not_found", None),
            PermissionDenied,
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "service_principal.create",
        type_: OperationType::Command,
        authority: HostRouteAuthority::ProjectAdmin,
        handler: HostHandler::ControlExecution,
        schemas: schemas::<ServicePrincipalCreateRequest, ServicePrincipalCreateResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant("env_not_found", None),
            TransactionInvariant("invalid_input", None),
            TransactionInvariant("not_found", None),
            PermissionDenied,
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "service_principal.disable",
        type_: OperationType::Command,
        authority: HostRouteAuthority::ProjectAdmin,
        handler: HostHandler::ControlExecution,
        schemas: schemas::<PatListRequest, ServicePrincipalDisableResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant("env_not_found", None),
            TransactionInvariant("invalid_input", None),
            TransactionInvariant("not_found", None),
            PermissionDenied,
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "pat.issue",
        type_: OperationType::Command,
        authority: HostRouteAuthority::ProjectAdmin,
        handler: HostHandler::ControlExecution,
        schemas: schemas::<PatIssueRequest, ApiServiceCreateResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant("env_not_found", None),
            TransactionInvariant("invalid_input", None),
            TransactionInvariant("not_found", None),
            PermissionDenied,
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "pat.list",
        type_: OperationType::Get,
        authority: HostRouteAuthority::ProjectAdmin,
        handler: HostHandler::ControlExecution,
        schemas: schemas::<PatListRequest, PatListResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant("env_not_found", None),
            TransactionInvariant("invalid_input", None),
            TransactionInvariant("not_found", None),
            PermissionDenied,
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "pat.revoke",
        type_: OperationType::Command,
        authority: HostRouteAuthority::OrgAdmin,
        handler: HostHandler::ControlExecution,
        schemas: schemas::<PatRevokeRequest, UserRoleRevokeResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant("env_not_found", None),
            TransactionInvariant("invalid_input", None),
            TransactionInvariant("not_found", None),
            PermissionDenied,
        ],
    },
    HostRoute {
        set: HostRouteSet::App,
        operation: "file.put",
        type_: OperationType::Command,
        authority: HostRouteAuthority::Granted,
        handler: HostHandler::FilePut,
        schemas: schemas::<FilePutRequest, FileDeleteResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant("binding_refused", None),
            TransactionInvariant("cdn_host_required", None),
            TransactionInvariant("file_not_found", None),
            TransactionInvariant("file_private", None),
            TransactionInvariant("file_referenced", None),
            MalformedInput("invalid_input", None),
            PermissionDenied,
        ],
    },
    HostRoute {
        set: HostRouteSet::App,
        operation: "file.get",
        type_: OperationType::Get,
        authority: HostRouteAuthority::Granted,
        handler: HostHandler::FileGet,
        schemas: schemas::<FileGetRequest, FileExposeResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant("binding_refused", None),
            TransactionInvariant("cdn_host_required", None),
            TransactionInvariant("file_not_found", None),
            TransactionInvariant("file_private", None),
            TransactionInvariant("file_referenced", None),
            MalformedInput("invalid_input", None),
            PermissionDenied,
        ],
    },
    HostRoute {
        set: HostRouteSet::App,
        operation: "file.delete",
        type_: OperationType::Command,
        authority: HostRouteAuthority::Granted,
        handler: HostHandler::FileDelete,
        schemas: schemas::<FileDeleteRequest, FileDeleteResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant("binding_refused", None),
            TransactionInvariant("cdn_host_required", None),
            TransactionInvariant("file_not_found", None),
            TransactionInvariant("file_private", None),
            TransactionInvariant("file_referenced", None),
            MalformedInput("invalid_input", None),
            PermissionDenied,
        ],
    },
    HostRoute {
        set: HostRouteSet::App,
        operation: "file.expose",
        type_: OperationType::Command,
        authority: HostRouteAuthority::Granted,
        handler: HostHandler::FileExpose,
        schemas: schemas::<FileExposeRequest, FileExposeResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant("binding_refused", None),
            TransactionInvariant("cdn_host_required", None),
            TransactionInvariant("file_not_found", None),
            TransactionInvariant("file_private", None),
            TransactionInvariant("file_referenced", None),
            MalformedInput("invalid_input", None),
            PermissionDenied,
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "capture_gap.recover",
        type_: OperationType::Command,
        authority: HostRouteAuthority::ProjectAdmin,
        handler: HostHandler::OperationJob,
        schemas: schemas::<CaptureGapCloseRequest, AppDeployResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant("env_not_found", None),
            TransactionInvariant("invalid_input", None),
            TransactionInvariant("not_found", None),
            PermissionDenied,
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "capture_gap.close",
        type_: OperationType::Command,
        authority: HostRouteAuthority::ProjectAdmin,
        handler: HostHandler::OperationJob,
        schemas: schemas::<CaptureGapCloseRequest, AppDeployResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant("env_not_found", None),
            TransactionInvariant("invalid_input", None),
            TransactionInvariant("not_found", None),
            PermissionDenied,
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "platform.schema",
        type_: OperationType::Command,
        authority: HostRouteAuthority::OrgAdmin,
        handler: HostHandler::ControlExecution,
        schemas: schemas::<PlatformSchemaRequest, AppDeployResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant("env_not_found", None),
            TransactionInvariant("invalid_input", None),
            TransactionInvariant("not_found", None),
            PermissionDenied,
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "platform.operation_status",
        type_: OperationType::Get,
        authority: HostRouteAuthority::OrgAdmin,
        handler: HostHandler::ControlExecution,
        schemas: schemas::<AppDeployResult, EnvOperationStatusResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant("env_not_found", None),
            TransactionInvariant("invalid_input", None),
            TransactionInvariant("not_found", None),
            PermissionDenied,
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "api_service.create",
        type_: OperationType::Command,
        authority: HostRouteAuthority::ProjectAdmin,
        handler: HostHandler::ControlExecution,
        schemas: schemas::<ApiServiceCreateRequest, ApiServiceCreateResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant("env_not_found", None),
            TransactionInvariant("invalid_input", None),
            TransactionInvariant("not_found", None),
            PermissionDenied,
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "app.stop",
        type_: OperationType::Command,
        authority: HostRouteAuthority::ProjectAdmin,
        handler: HostHandler::OperationJob,
        schemas: schemas::<AppStopRequest, AppDeployResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant("env_not_found", None),
            TransactionInvariant("invalid_input", None),
            TransactionInvariant("not_found", None),
            PermissionDenied,
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "app.deploy",
        type_: OperationType::Command,
        authority: HostRouteAuthority::ProjectAdmin,
        handler: HostHandler::OperationJob,
        schemas: schemas::<AppDeployRequest, AppDeployResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant("env_not_found", None),
            TransactionInvariant("invalid_input", None),
            TransactionInvariant("not_found", None),
            PermissionDenied,
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "app.rollback",
        type_: OperationType::Command,
        authority: HostRouteAuthority::ProjectAdmin,
        handler: HostHandler::OperationJob,
        schemas: schemas::<AppRollbackRequest, AppDeployResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant("env_not_found", None),
            TransactionInvariant("invalid_input", None),
            TransactionInvariant("not_found", None),
            PermissionDenied,
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "schema.diff",
        type_: OperationType::Command,
        authority: HostRouteAuthority::ProjectAdmin,
        handler: HostHandler::OperationJob,
        schemas: schemas::<SchemaDiffRequest, AppDeployResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant("env_not_found", None),
            TransactionInvariant("invalid_input", None),
            TransactionInvariant("not_found", None),
            PermissionDenied,
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "schema.apply",
        type_: OperationType::Command,
        authority: HostRouteAuthority::ProjectAdmin,
        handler: HostHandler::OperationJob,
        schemas: schemas::<AppDeployRequest, AppDeployResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant("env_not_found", None),
            TransactionInvariant("invalid_input", None),
            TransactionInvariant("not_found", None),
            PermissionDenied,
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "schema.reset",
        type_: OperationType::Command,
        authority: HostRouteAuthority::ProjectAdmin,
        handler: HostHandler::OperationJob,
        schemas: schemas::<CaptureGapCloseRequest, AppDeployResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant("env_not_found", None),
            TransactionInvariant("invalid_input", None),
            TransactionInvariant("not_found", None),
            PermissionDenied,
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "operation.status",
        type_: OperationType::Get,
        authority: HostRouteAuthority::ProjectAdmin,
        handler: HostHandler::OperationJob,
        schemas: schemas::<EnvOperationStatusRequest, EnvOperationStatusResult>,
        write_transaction: true,
        refusals: &[
            TransactionInvariant("env_not_found", None),
            TransactionInvariant("invalid_input", None),
            TransactionInvariant("not_found", None),
            PermissionDenied,
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "integration_type.list",
        type_: OperationType::Get,
        authority: HostRouteAuthority::ProjectAdmin,
        handler: HostHandler::IntegrationAdministration,
        schemas: schemas::<IntegrationTypeListRequest, IntegrationTypeListResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant(
                "app_write_incomplete",
                Some(
                    "The application rows stopped at this environment. The completed environments are done.",
                ),
            ),
            TransactionInvariant("deployment_in_progress", None),
            TransactionInvariant("immutable_field", None),
            TransactionInvariant("invalid_input", None),
            TransactionInvariant("ipv6_authority_not_supported", None),
            TransactionInvariant("not_found", None),
            PermissionDenied,
            TransactionInvariant("referenced", None),
            TransactionInvariant("resource_in_use", None),
            TransactionInvariant("unsupported_auth_type", None),
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "external_system.write",
        type_: OperationType::Command,
        authority: HostRouteAuthority::ProjectAdmin,
        handler: HostHandler::IntegrationAdministration,
        schemas: schemas::<ConnectionAuthorizeRequest, ExternalSystemDeleteResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant(
                "app_write_incomplete",
                Some(
                    "The application rows stopped at this environment. The completed environments are done.",
                ),
            ),
            TransactionInvariant("deployment_in_progress", None),
            TransactionInvariant("immutable_field", None),
            TransactionInvariant("invalid_input", None),
            TransactionInvariant("ipv6_authority_not_supported", None),
            TransactionInvariant("not_found", None),
            PermissionDenied,
            TransactionInvariant("referenced", None),
            TransactionInvariant("resource_in_use", None),
            TransactionInvariant("unsupported_auth_type", None),
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "external_system.list",
        type_: OperationType::Get,
        authority: HostRouteAuthority::ProjectAdmin,
        handler: HostHandler::IntegrationAdministration,
        schemas: schemas::<CaptureGapCloseRequest, ExternalSystemListResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant(
                "app_write_incomplete",
                Some(
                    "The application rows stopped at this environment. The completed environments are done.",
                ),
            ),
            TransactionInvariant("deployment_in_progress", None),
            TransactionInvariant("immutable_field", None),
            TransactionInvariant("invalid_input", None),
            TransactionInvariant("ipv6_authority_not_supported", None),
            TransactionInvariant("not_found", None),
            PermissionDenied,
            TransactionInvariant("referenced", None),
            TransactionInvariant("resource_in_use", None),
            TransactionInvariant("unsupported_auth_type", None),
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "external_system.delete",
        type_: OperationType::Command,
        authority: HostRouteAuthority::ProjectAdmin,
        handler: HostHandler::IntegrationAdministration,
        schemas: schemas::<ConnectionAuthorizeRequest, ExternalSystemDeleteResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant(
                "app_write_incomplete",
                Some(
                    "The application rows stopped at this environment. The completed environments are done.",
                ),
            ),
            TransactionInvariant("deployment_in_progress", None),
            TransactionInvariant("immutable_field", None),
            TransactionInvariant("invalid_input", None),
            TransactionInvariant("ipv6_authority_not_supported", None),
            TransactionInvariant("not_found", None),
            PermissionDenied,
            TransactionInvariant("referenced", None),
            TransactionInvariant("resource_in_use", None),
            TransactionInvariant("unsupported_auth_type", None),
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "connection.write",
        type_: OperationType::Command,
        authority: HostRouteAuthority::ProjectAdmin,
        handler: HostHandler::IntegrationAdministration,
        schemas: schemas::<ConnectionWriteRequest, ConnectionSummary>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant(
                "app_write_incomplete",
                Some(
                    "The application rows stopped at this environment. The completed environments are done.",
                ),
            ),
            TransactionInvariant("deployment_in_progress", None),
            TransactionInvariant(
                "exclusive_connection_requires_local_target",
                Some("Exclusive connections belong to local targets."),
            ),
            TransactionInvariant("immutable_field", None),
            TransactionInvariant("invalid_input", None),
            TransactionInvariant("ipv6_authority_not_supported", None),
            TransactionInvariant("not_found", None),
            PermissionDenied,
            TransactionInvariant("referenced", None),
            TransactionInvariant("resource_in_use", None),
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "connection.authorize",
        type_: OperationType::Command,
        authority: HostRouteAuthority::ProjectAdmin,
        handler: HostHandler::IntegrationAdministration,
        schemas: schemas::<ConnectionAuthorizeRequest, ConnectionAuthorizeResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant("connection_inactive", None),
            TransactionInvariant(
                "app_write_incomplete",
                Some(
                    "The application rows stopped at this environment. The completed environments are done.",
                ),
            ),
            TransactionInvariant("deployment_in_progress", None),
            TransactionInvariant("immutable_field", None),
            TransactionInvariant("invalid_input", None),
            TransactionInvariant("ipv6_authority_not_supported", None),
            TransactionInvariant("not_found", None),
            PermissionDenied,
            TransactionInvariant("referenced", None),
            TransactionInvariant("resource_in_use", None),
            TransactionInvariant("unsupported_auth_type", None),
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "connection.test",
        type_: OperationType::Command,
        authority: HostRouteAuthority::ProjectAdmin,
        handler: HostHandler::IntegrationAdministration,
        schemas: schemas::<ConnectionTestRequest, ConnectionTest>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant("connection_inactive", None),
            TransactionInvariant("invalid_input", None),
            PermissionDenied,
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "connection.copy",
        type_: OperationType::Command,
        authority: HostRouteAuthority::ProjectAdmin,
        handler: HostHandler::IntegrationAdministration,
        schemas: schemas::<ConnectionCopyRequest, ConnectionSummary>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant(
                "app_write_incomplete",
                Some(
                    "The application rows stopped at this environment. The completed environments are done.",
                ),
            ),
            TransactionInvariant("deployment_in_progress", None),
            TransactionInvariant("immutable_field", None),
            TransactionInvariant("invalid_input", None),
            TransactionInvariant("ipv6_authority_not_supported", None),
            TransactionInvariant("not_found", None),
            PermissionDenied,
            TransactionInvariant("referenced", None),
            TransactionInvariant("resource_in_use", None),
            TransactionInvariant("unsupported_auth_type", None),
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "connection.set_credential",
        type_: OperationType::Command,
        authority: HostRouteAuthority::ProjectAdmin,
        handler: HostHandler::IntegrationAdministration,
        schemas: schemas::<ConnectionSetCredentialRequest, ConnectionSummary>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant(
                "app_write_incomplete",
                Some(
                    "The application rows stopped at this environment. The completed environments are done.",
                ),
            ),
            TransactionInvariant("deployment_in_progress", None),
            TransactionInvariant("immutable_field", None),
            TransactionInvariant("invalid_input", None),
            TransactionInvariant("ipv6_authority_not_supported", None),
            TransactionInvariant("not_found", None),
            PermissionDenied,
            TransactionInvariant("referenced", None),
            TransactionInvariant("resource_in_use", None),
            TransactionInvariant("unsupported_auth_type", None),
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "connection.deactivate",
        type_: OperationType::Command,
        authority: HostRouteAuthority::ProjectAdmin,
        handler: HostHandler::IntegrationAdministration,
        schemas: schemas::<ConnectionAuthorizeRequest, ConnectionStatusResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant(
                "app_write_incomplete",
                Some(
                    "The application rows stopped at this environment. The completed environments are done.",
                ),
            ),
            TransactionInvariant("deployment_in_progress", None),
            TransactionInvariant("invalid_input", None),
            TransactionInvariant("not_found", None),
            PermissionDenied,
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "connection.activate",
        type_: OperationType::Command,
        authority: HostRouteAuthority::ProjectAdmin,
        handler: HostHandler::IntegrationAdministration,
        schemas: schemas::<ConnectionActivateRequest, ConnectionStatusResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant(
                "app_write_incomplete",
                Some(
                    "The application rows stopped at this environment. The completed environments are done.",
                ),
            ),
            TransactionInvariant("deployment_in_progress", None),
            TransactionInvariant("immutable_field", None),
            TransactionInvariant("invalid_input", None),
            TransactionInvariant("ipv6_authority_not_supported", None),
            TransactionInvariant("not_found", None),
            PermissionDenied,
            TransactionInvariant("resource_in_use", None),
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "connection.list",
        type_: OperationType::Get,
        authority: HostRouteAuthority::ProjectAdmin,
        handler: HostHandler::IntegrationAdministration,
        schemas: schemas::<CaptureGapCloseRequest, ConnectionListResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant(
                "app_write_incomplete",
                Some(
                    "The application rows stopped at this environment. The completed environments are done.",
                ),
            ),
            TransactionInvariant("deployment_in_progress", None),
            TransactionInvariant("immutable_field", None),
            TransactionInvariant("invalid_input", None),
            TransactionInvariant("ipv6_authority_not_supported", None),
            TransactionInvariant("not_found", None),
            PermissionDenied,
            TransactionInvariant("referenced", None),
            TransactionInvariant("resource_in_use", None),
            TransactionInvariant("unsupported_auth_type", None),
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "connection.get",
        type_: OperationType::Get,
        authority: HostRouteAuthority::ProjectAdmin,
        handler: HostHandler::IntegrationAdministration,
        schemas: schemas::<ConnectionAuthorizeRequest, ConnectionGetResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant(
                "app_write_incomplete",
                Some(
                    "The application rows stopped at this environment. The completed environments are done.",
                ),
            ),
            TransactionInvariant("deployment_in_progress", None),
            TransactionInvariant("immutable_field", None),
            TransactionInvariant("invalid_input", None),
            TransactionInvariant("ipv6_authority_not_supported", None),
            TransactionInvariant("not_found", None),
            PermissionDenied,
            TransactionInvariant("referenced", None),
            TransactionInvariant("resource_in_use", None),
            TransactionInvariant("unsupported_auth_type", None),
        ],
    },
    HostRoute {
        set: HostRouteSet::App,
        operation: "permission.mine",
        type_: OperationType::Get,
        authority: HostRouteAuthority::Member,
        handler: HostHandler::PermissionMine,
        schemas: schemas::<Empty, PermissionMineResult>,
        write_transaction: false,
        refusals: &[PermissionDenied],
    },
    HostRoute {
        set: HostRouteSet::App,
        operation: "user.list",
        type_: OperationType::Get,
        authority: HostRouteAuthority::Admin,
        handler: HostHandler::AppUserList,
        schemas: schemas::<Empty, AppUserListResult>,
        write_transaction: false,
        refusals: &[PermissionDenied],
    },
    HostRoute {
        set: HostRouteSet::App,
        operation: "role.list",
        type_: OperationType::Get,
        authority: HostRouteAuthority::Admin,
        handler: HostHandler::RoleList,
        schemas: schemas::<Empty, RoleListResult>,
        write_transaction: false,
        refusals: &[PermissionDenied],
    },
    HostRoute {
        set: HostRouteSet::App,
        operation: "role.create",
        type_: OperationType::Command,
        authority: HostRouteAuthority::Admin,
        handler: HostHandler::RoleCreate,
        schemas: schemas::<PermissionListRequest, RoleCreateResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant(
                "admin_fixed",
                Some("The built-in admin role always stays active and holds every operation."),
            ),
            MalformedInput("invalid_input", None),
            PermissionDenied,
        ],
    },
    HostRoute {
        set: HostRouteSet::App,
        operation: "role.deactivate",
        type_: OperationType::Command,
        authority: HostRouteAuthority::Admin,
        handler: HostHandler::RoleDeactivate,
        schemas: schemas::<PermissionListRequest, RoleDeactivateResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant(
                "admin_fixed",
                Some("The built-in admin role always stays active and holds every operation."),
            ),
            MalformedInput("invalid_input", None),
            PermissionDenied,
        ],
    },
    HostRoute {
        set: HostRouteSet::App,
        operation: "role.activate",
        type_: OperationType::Command,
        authority: HostRouteAuthority::Admin,
        handler: HostHandler::RoleActivate,
        schemas: schemas::<PermissionListRequest, RoleActivateResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant(
                "admin_fixed",
                Some("The built-in admin role always stays active and holds every operation."),
            ),
            MalformedInput("invalid_input", None),
            PermissionDenied,
        ],
    },
    HostRoute {
        set: HostRouteSet::App,
        operation: "permission.list",
        type_: OperationType::Get,
        authority: HostRouteAuthority::Admin,
        handler: HostHandler::PermissionList,
        schemas: schemas::<PermissionListRequest, PermissionListResult>,
        write_transaction: false,
        refusals: &[
            MalformedInput("invalid_input", None),
            PermissionDenied,
            TransactionInvariant("role_not_found", Some("This role does not exist.")),
        ],
    },
    HostRoute {
        set: HostRouteSet::App,
        operation: "permission.grant",
        type_: OperationType::Command,
        authority: HostRouteAuthority::Admin,
        handler: HostHandler::PermissionGrant,
        schemas: schemas::<PermissionGrantRequest, PermissionGrantResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant(
                "admin_fixed",
                Some("The built-in admin role always stays active and holds every operation."),
            ),
            MalformedInput("invalid_input", None),
            TransactionInvariant(
                "operation_not_grantable",
                Some("This operation is fixed to admin or to every member, so no role takes it."),
            ),
            TransactionInvariant(
                "operation_not_served",
                Some("The current release does not serve this operation."),
            ),
            PermissionDenied,
            TransactionInvariant("role_not_found", Some("This role is missing or inactive.")),
        ],
    },
    HostRoute {
        set: HostRouteSet::App,
        operation: "permission.revoke",
        type_: OperationType::Command,
        authority: HostRouteAuthority::Admin,
        handler: HostHandler::PermissionRevoke,
        schemas: schemas::<PermissionGrantRequest, PermissionRevokeResult>,
        write_transaction: false,
        refusals: &[
            MalformedInput("invalid_input", None),
            PermissionDenied,
            TransactionInvariant(
                "permission_not_held",
                Some("The role does not hold this operation."),
            ),
            TransactionInvariant("role_not_found", Some("This role does not exist.")),
        ],
    },
    HostRoute {
        set: HostRouteSet::App,
        operation: "user_role.grant",
        type_: OperationType::Command,
        authority: HostRouteAuthority::Admin,
        handler: HostHandler::UserRoleGrant,
        schemas: schemas::<UserRoleGrantRequest, UserRoleGrantResult>,
        write_transaction: false,
        refusals: &[
            MalformedInput("invalid_input", None),
            PermissionDenied,
            TransactionInvariant("role_not_found", Some("This role is missing or inactive.")),
            TransactionInvariant("user_not_found", Some("The application has no such user.")),
        ],
    },
    HostRoute {
        set: HostRouteSet::App,
        operation: "user_role.revoke",
        type_: OperationType::Command,
        authority: HostRouteAuthority::Admin,
        handler: HostHandler::UserRoleRevoke,
        schemas: schemas::<UserRoleGrantRequest, UserRoleRevokeResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant(
                "admin_covered",
                Some(
                    "The user holds project-admin or org-admin, so admin stays. Revoke that first.",
                ),
            ),
            MalformedInput("invalid_input", None),
            PermissionDenied,
            TransactionInvariant("user_not_found", Some("The application has no such user.")),
        ],
    },
    HostRoute {
        set: HostRouteSet::App,
        operation: "number_counter.set",
        type_: OperationType::Command,
        authority: HostRouteAuthority::Granted,
        handler: HostHandler::NumberCounterSet,
        schemas: schemas::<NumberCounterSetRequest, NumberCounterSetResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant(
                "counter_assigned_per_key",
                Some("A per= counter gives each key one number for good, so it is never set."),
            ),
            TransactionInvariant(
                "counter_not_found",
                Some("The pattern of the sequence has no counter of this name."),
            ),
            TransactionInvariant(
                "idempotency_conflict",
                Some("This request id was used for a different set."),
            ),
            MalformedInput("invalid_input", None),
            PermissionDenied,
            TransactionInvariant(
                "sequence_not_found",
                Some("The application has no number sequence of this name."),
            ),
        ],
    },
    HostRoute {
        set: HostRouteSet::App,
        operation: "workflow.author",
        type_: OperationType::Command,
        authority: HostRouteAuthority::Admin,
        handler: HostHandler::WorkflowAuthor,
        schemas: schemas::<WorkflowAuthorRequest, WorkflowAuthorResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant("apply_in_progress", None),
            MalformedInput("invalid_input", None),
            PermissionDenied,
            TransactionInvariant("workflow_path_taken", None),
            TransactionInvariant("workflow_version_exists", None),
        ],
    },
    HostRoute {
        set: HostRouteSet::App,
        operation: "workflow.enable",
        type_: OperationType::Command,
        authority: HostRouteAuthority::Admin,
        handler: HostHandler::WorkflowEnable,
        schemas: schemas::<WorkflowDisableRequest, WorkflowAuthorResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant("apply_in_progress", None),
            MalformedInput("invalid_input", None),
            PermissionDenied,
            TransactionInvariant("workflow_path_taken", None),
            TransactionInvariant("workflow_version_not_found", None),
        ],
    },
    HostRoute {
        set: HostRouteSet::App,
        operation: "workflow.disable",
        type_: OperationType::Command,
        authority: HostRouteAuthority::Admin,
        handler: HostHandler::WorkflowDisable,
        schemas: schemas::<WorkflowDisableRequest, WorkflowAuthorResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant("apply_in_progress", None),
            MalformedInput("invalid_input", None),
            PermissionDenied,
            TransactionInvariant("workflow_version_not_found", None),
        ],
    },
    HostRoute {
        set: HostRouteSet::App,
        operation: "workflow.list",
        type_: OperationType::Get,
        authority: HostRouteAuthority::Admin,
        handler: HostHandler::WorkflowList,
        schemas: schemas::<Empty, WorkflowAuthorResult>,
        write_transaction: false,
        refusals: &[MalformedInput("invalid_input", None), PermissionDenied],
    },
    HostRoute {
        set: HostRouteSet::App,
        operation: "workflow.run",
        type_: OperationType::Command,
        authority: HostRouteAuthority::Admin,
        handler: HostHandler::WorkflowRun,
        schemas: schemas::<WorkflowRunRequest, WorkflowRunResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant("active_role_required", None),
            TransactionInvariant("idempotency_conflict", None),
            MalformedInput("invalid_input", None),
            PermissionDenied,
            TransactionInvariant("workflow_not_enabled", None),
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "control.mine",
        type_: OperationType::Get,
        authority: HostRouteAuthority::Member,
        handler: HostHandler::ControlMine,
        schemas: schemas::<ControlMineRequest, ControlMineResult>,
        write_transaction: false,
        refusals: &[MalformedInput("invalid_input", None), PermissionDenied],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "user.list",
        type_: OperationType::Get,
        authority: HostRouteAuthority::Member,
        handler: HostHandler::UserList,
        schemas: schemas::<ControlMineRequest, UserListResult>,
        write_transaction: false,
        refusals: &[MalformedInput("invalid_input", None), PermissionDenied],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "user.create",
        type_: OperationType::Command,
        authority: HostRouteAuthority::OrgAdmin,
        handler: HostHandler::UserCreate,
        schemas: schemas::<UserCreateRequest, UserCreateResult>,
        write_transaction: false,
        refusals: &[
            MalformedInput("invalid_input", None),
            PermissionDenied,
            TransactionInvariant(
                "user_in_another_org",
                Some("The user already belongs to another org."),
            ),
            TransactionInvariant(
                "user_refused",
                Some("Identity refused the user of this email, for example a disabled user."),
            ),
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "user.invite",
        type_: OperationType::Command,
        authority: HostRouteAuthority::OrgAdmin,
        handler: HostHandler::UserInvite,
        schemas: schemas::<UserInviteRequest, UserInviteResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant(
                "app_write_incomplete",
                Some(
                    "The application rows stopped at this environment. The completed environments are done.",
                ),
            ),
            TransactionInvariant(
                "env_not_found",
                Some("This environment is not an environment of the org."),
            ),
            MalformedInput("invalid_input", None),
            PermissionDenied,
            TransactionInvariant(
                "project_not_found",
                Some("This project is not a project of the org."),
            ),
            TransactionInvariant(
                "user_in_another_org",
                Some("The user already belongs to another org."),
            ),
            TransactionInvariant("user_not_found", Some("The org has no such user.")),
            TransactionInvariant(
                "user_refused",
                Some("Identity refused the user of this email, for example a disabled user."),
            ),
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "user.disable",
        type_: OperationType::Command,
        authority: HostRouteAuthority::OrgAdmin,
        handler: HostHandler::UserDisable,
        schemas: schemas::<OrgAdminGrantRequest, UserActivateResult>,
        write_transaction: false,
        refusals: &[
            MalformedInput("invalid_input", None),
            PermissionDenied,
            TransactionInvariant(
                "user_in_another_org",
                Some("The user has an active membership in another org."),
            ),
            TransactionInvariant("user_not_found", Some("The org has no such user.")),
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "user.activate",
        type_: OperationType::Command,
        authority: HostRouteAuthority::OrgAdmin,
        handler: HostHandler::UserActivate,
        schemas: schemas::<OrgAdminGrantRequest, UserActivateResult>,
        write_transaction: false,
        refusals: &[
            MalformedInput("invalid_input", None),
            PermissionDenied,
            TransactionInvariant(
                "user_in_another_org",
                Some("The user already belongs to another org."),
            ),
            TransactionInvariant("user_not_found", Some("The org has no such user.")),
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "user.deactivate",
        type_: OperationType::Command,
        authority: HostRouteAuthority::OrgAdmin,
        handler: HostHandler::UserDeactivate,
        schemas: schemas::<OrgAdminGrantRequest, UserActivateResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant(
                "app_write_incomplete",
                Some(
                    "The application rows stopped at this environment. The completed environments are done.",
                ),
            ),
            MalformedInput("invalid_input", None),
            PermissionDenied,
            TransactionInvariant("user_not_found", Some("The org has no such user.")),
        ],
    },
    // project.list state is active when any environment is active, inactive otherwise.
    // A project with no environments is inactive. registry.env.active owns the state.
    HostRoute {
        set: HostRouteSet::Control,
        operation: "project.list",
        type_: OperationType::Get,
        authority: HostRouteAuthority::OrgAdmin,
        handler: HostHandler::ProjectList,
        schemas: schemas::<ControlMineRequest, ProjectListResult>,
        write_transaction: false,
        refusals: &[MalformedInput("invalid_input", None), PermissionDenied],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "org_admin.grant",
        type_: OperationType::Command,
        authority: HostRouteAuthority::OrgAdmin,
        handler: HostHandler::OrgAdminGrant,
        schemas: schemas::<OrgAdminGrantRequest, OrgAdminGrantResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant(
                "app_write_incomplete",
                Some(
                    "The application rows stopped at this environment. The completed environments are done.",
                ),
            ),
            MalformedInput("invalid_input", None),
            PermissionDenied,
            TransactionInvariant(
                "user_not_active",
                Some("The user is not an active member of the org."),
            ),
            TransactionInvariant("user_not_found", Some("The org has no such user.")),
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "org_admin.revoke",
        type_: OperationType::Command,
        authority: HostRouteAuthority::OrgAdmin,
        handler: HostHandler::OrgAdminRevoke,
        schemas: schemas::<OrgAdminGrantRequest, OrgAdminGrantResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant(
                "app_write_incomplete",
                Some(
                    "The application rows stopped at this environment. The completed environments are done.",
                ),
            ),
            MalformedInput("invalid_input", None),
            PermissionDenied,
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "env.list",
        type_: OperationType::Get,
        authority: HostRouteAuthority::ProjectAdmin,
        handler: HostHandler::EnvList,
        schemas: schemas::<EnvListRequest, EnvListResult>,
        write_transaction: false,
        refusals: &[MalformedInput("invalid_input", None), PermissionDenied],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "env.status",
        type_: OperationType::Get,
        authority: HostRouteAuthority::ProjectAdmin,
        handler: HostHandler::EnvStatus,
        schemas: schemas::<EnvRequest, EnvStatusResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant(
                "env_not_found",
                Some("This environment is not an environment of the org."),
            ),
            MalformedInput("invalid_input", None),
            PermissionDenied,
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "member.list",
        type_: OperationType::Get,
        authority: HostRouteAuthority::ProjectAdmin,
        handler: HostHandler::MemberList,
        schemas: schemas::<MemberListRequest, MemberListResult>,
        write_transaction: false,
        refusals: &[MalformedInput("invalid_input", None), PermissionDenied],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "member.grant",
        type_: OperationType::Command,
        authority: HostRouteAuthority::ProjectAdmin,
        handler: HostHandler::MemberGrant,
        schemas: schemas::<MemberGrantRequest, MemberGrantResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant(
                "app_write_incomplete",
                Some(
                    "The application rows stopped at this environment. The completed environments are done.",
                ),
            ),
            TransactionInvariant(
                "env_not_found",
                Some("This environment is not an environment of the org."),
            ),
            MalformedInput("invalid_input", None),
            PermissionDenied,
            TransactionInvariant(
                "user_not_active",
                Some("The user is not an active member of the org."),
            ),
            TransactionInvariant("user_not_found", Some("The org has no such user.")),
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "member.revoke",
        type_: OperationType::Command,
        authority: HostRouteAuthority::ProjectAdmin,
        handler: HostHandler::MemberRevoke,
        schemas: schemas::<MemberGrantRequest, MemberGrantResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant(
                "admin_covered",
                Some(
                    "The user holds org-admin or project-admin, which covers this grant. Revoke that first.",
                ),
            ),
            TransactionInvariant(
                "app_write_incomplete",
                Some(
                    "The application rows stopped at this environment. The completed environments are done.",
                ),
            ),
            MalformedInput("invalid_input", None),
            PermissionDenied,
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "project_admin.grant",
        type_: OperationType::Command,
        authority: HostRouteAuthority::ProjectAdmin,
        handler: HostHandler::ProjectAdminGrant,
        schemas: schemas::<ProjectAdminGrantRequest, ProjectAdminGrantResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant(
                "app_write_incomplete",
                Some(
                    "The application rows stopped at this environment. The completed environments are done.",
                ),
            ),
            MalformedInput("invalid_input", None),
            PermissionDenied,
            TransactionInvariant(
                "project_not_found",
                Some("This project is not a project of the org."),
            ),
            TransactionInvariant(
                "user_not_active",
                Some("The user is not an active member of the org."),
            ),
            TransactionInvariant("user_not_found", Some("The org has no such user.")),
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "project_admin.revoke",
        type_: OperationType::Command,
        authority: HostRouteAuthority::ProjectAdmin,
        handler: HostHandler::ProjectAdminRevoke,
        schemas: schemas::<ProjectAdminGrantRequest, ProjectAdminGrantResult>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant(
                "admin_covered",
                Some(
                    "The user holds org-admin or project-admin, which covers this grant. Revoke that first.",
                ),
            ),
            TransactionInvariant(
                "app_write_incomplete",
                Some(
                    "The application rows stopped at this environment. The completed environments are done.",
                ),
            ),
            MalformedInput("invalid_input", None),
            PermissionDenied,
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "env.activate",
        type_: OperationType::Command,
        authority: HostRouteAuthority::OrgAdmin,
        handler: HostHandler::EnvActivate,
        schemas: schemas::<EnvRequest, EnvStatus>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant(
                "app_write_incomplete",
                Some(
                    "The application rows stopped at this environment. The completed environments are done.",
                ),
            ),
            TransactionInvariant(
                "env_not_found",
                Some("This environment is not an environment of the org."),
            ),
            MalformedInput("invalid_input", None),
            PermissionDenied,
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "env.deactivate",
        type_: OperationType::Command,
        authority: HostRouteAuthority::OrgAdmin,
        handler: HostHandler::EnvDeactivate,
        schemas: schemas::<EnvRequest, EnvStatus>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant(
                "app_write_incomplete",
                Some(
                    "The application rows stopped at this environment. The completed environments are done.",
                ),
            ),
            TransactionInvariant(
                "env_not_found",
                Some("This environment is not an environment of the org."),
            ),
            MalformedInput("invalid_input", None),
            PermissionDenied,
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "project.activate",
        type_: OperationType::Command,
        authority: HostRouteAuthority::OrgAdmin,
        handler: HostHandler::ProjectActivate,
        schemas: schemas::<ProjectRequest, ProjectStatus>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant(
                "app_write_incomplete",
                Some(
                    "The application rows stopped at this environment. The completed environments are done.",
                ),
            ),
            MalformedInput("invalid_input", None),
            PermissionDenied,
            TransactionInvariant(
                "project_not_found",
                Some("This project is not a project of the org."),
            ),
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "project.inactivate",
        type_: OperationType::Command,
        authority: HostRouteAuthority::OrgAdmin,
        handler: HostHandler::ProjectInactivate,
        schemas: schemas::<ProjectRequest, ProjectStatus>,
        write_transaction: false,
        refusals: &[
            TransactionInvariant(
                "app_write_incomplete",
                Some(
                    "The application rows stopped at this environment. The completed environments are done.",
                ),
            ),
            MalformedInput("invalid_input", None),
            PermissionDenied,
            TransactionInvariant(
                "project_not_found",
                Some("This project is not a project of the org."),
            ),
        ],
    },
    HostRoute {
        set: HostRouteSet::Control,
        operation: "project.create",
        type_: OperationType::Command,
        authority: HostRouteAuthority::OrgAdmin,
        handler: HostHandler::ProjectCreate,
        schemas: schemas::<ProjectCreateRequest, ProjectCreated>,
        write_transaction: false,
        refusals: &[
            MalformedInput("invalid_input", None),
            PermissionDenied,
            TransactionInvariant(
                "project_exists",
                Some("This project is a project of the org already."),
            ),
        ],
    },
];

/// One host route as an attachment of the route table.
#[derive(Debug)]
pub struct HostAttachment {
    pub route: &'static HostRoute,
    /// The sealed operation id, such as `wamn-control:permission/mine@0.10.0`.
    /// A host-run write stamps it.
    pub operation: wamn_package::registered_operation::RegisteredOperation,
    /// The operation id without its version, as a stored permission names it.
    pub reference: String,
    pub definition: Value,
    pub auth_policy: Value,
}

impl HostAttachment {
    pub fn attachment_type(&self) -> AttachmentType {
        AttachmentType::Http
    }

    /// The operation an `admin`-only or a granted route requires. A member
    /// route requires none.
    pub fn registered_operation(
        &self,
    ) -> Option<&wamn_package::registered_operation::RegisteredOperation> {
        matches!(
            self.route.authority,
            HostRouteAuthority::Admin | HostRouteAuthority::Granted
        )
        .then_some(&self.operation)
    }
}

/// Every host route by its set and attachment id. The two sets are served
/// under different audiences, so a route of each set can derive one id, as
/// `user.list` does.
static ATTACHMENTS: LazyLock<BTreeMap<(HostRouteSet, CanonicalName), HostAttachment>> =
    LazyLock::new(|| {
        ROUTES
            .iter()
            .map(|route| {
                let (model, action) = route
                    .operation
                    .split_once('.')
                    .expect("a host route names its model and action");
                let id = route_attachment_id(ID_PREFIX, model, action);
                let operation = operation_token(
                    host_route_package(),
                    host_route_version(),
                    &route.operation.parse().expect("fixed local host operation"),
                )
                .expect("fixed registered host operation");
                let method = if route.type_.is_read() { "GET" } else { "POST" };
                let modes = match route.set {
                    HostRouteSet::App => {
                        json!([PAT_AUTHENTICATION_MODE, SESSION_AUTHENTICATION_MODE])
                    }
                    // A control audience is for browser sessions only.
                    HostRouteSet::Control => json!([SESSION_AUTHENTICATION_MODE]),
                };
                let attachment = HostAttachment {
                    route,
                    reference: sealed_operation_reference(operation.as_str()).to_owned(),
                    operation,
                    definition: json!({
                        "id": id,
                        "type": "http",
                        "route": {
                            "host": "*",
                            "method": method,
                            "path": route_path(PATH_PREFIX, model, action),
                        },
                    }),
                    auth_policy: json!({ "modes": modes }),
                };
                ((route.set, id), attachment)
            })
            .collect()
    });

impl HostRouteSet {
    /// Generate the native contracts of the routes in this set.
    pub fn openapi(self) -> Value {
        let refusals = schema::<DeclaredRefusal>();
        let refusal_shapes: BTreeMap<_, _> = refusals["oneOf"]
            .as_array()
            .expect("declared refusals are variants")
            .iter()
            .map(|shape| {
                let code = shape["properties"]["code"]["enum"][0]
                    .as_str()
                    .expect("declared refusal has a code");
                (code, shape)
            })
            .collect();
        let paths = self.attachments().map(|(_, attachment)| {
            let route = attachment.route;
            let read = route.type_.is_read();
            let (input, output) = (route.schemas)();
            let mut errors = Vec::new();
            let variants = route.refusals.iter().map(|refusal| {
                let (code, text, source) = refusal.parts();
                let mut shape = refusal_shapes[code].clone();
                let required = shape["properties"]["detail"]["required"]
                    .as_array().map_or(&[][..], Vec::as_slice);
                errors.push(json!({"literal": code, "required": required, "sources": [source], "text": text}));
                shape["properties"]["code"] = json!({"const": code});
                shape["additionalProperties"] = json!(false);
                shape
            }).collect::<Vec<_>>();
            let outcome = |member, value| wire_item(member, value, !read);
            let mut operation = json!({
                "operationId": attachment.operation.as_str(),
                "responses": {"200": {
                    "description": "Operation outcomes",
                    "content": media_type(wire_array(json!({"oneOf": [
                        outcome("value", output), outcome("error", json!({"oneOf": variants}))
                    ]})))
                }},
                "x-wamn-contract": {
                    "resultClass": "one", "partialSchema": null, "errors": errors, "replay": null,
                    "direct": true, "type": if read { "get" } else { "command" },
                    "transaction": if !read || route.write_transaction { "explicit_per_input" } else { "implicit" },
                    "reads": [], "writes": null
                }
            });
            if read {
                let properties = input["properties"].as_object().into_iter().flatten();
                let required = input["required"].as_array().map_or(&[][..], Vec::as_slice);
                let parameters: Vec<_> = properties.map(|(name, value)| json!({
                    "in": "query", "name": name,
                    "required": required.iter().any(|key| key == name.as_str()),
                    "content": media_type(value)
                })).collect();
                if !parameters.is_empty() {
                    operation["parameters"] = json!(parameters);
                }
            } else {
                operation["requestBody"] = json!({
                    "required": true, "content": media_type(wire_array(wire_item("value", input, true)))
                });
            }
            let path = attachment.definition["route"]["path"].as_str().expect("native route path");
            (path, json!({ (if read { "get" } else { "post" }): operation }))
        }).collect::<BTreeMap<_, _>>();
        json!({"openapi": "3.1.0", "info": {"title": PACKAGE, "version": VERSION}, "paths": paths})
    }

    /// Every route of this set, by attachment id.
    pub fn attachments(
        self,
    ) -> impl Iterator<Item = (&'static CanonicalName, &'static HostAttachment)> {
        ATTACHMENTS
            .iter()
            .filter(move |((set, _), _)| *set == self)
            .map(|((_, id), attachment)| (id, attachment))
    }

    /// One route of this set by attachment id.
    pub fn attachment(self, id: &CanonicalName) -> Option<&'static HostAttachment> {
        self.attachments()
            .find_map(|(key, attachment)| (key == id).then_some(attachment))
    }
}

fn media_type(schema: impl Serialize) -> Value {
    json!({"application/json": {"schema": schema}})
}

fn wire_array(items: Value) -> Value {
    let mut schema = json!({"type": "array", "minItems": 1, "maxItems": 1});
    schema["items"] = items;
    schema
}

fn wire_item(member: &str, value: Value, keyed: bool) -> Value {
    let mut item = json!({"type": "object", "properties": {}, "required": [member], "additionalProperties": false});
    item["properties"][member] = value;
    if keyed {
        item["properties"]["request_id"] = json!({"type": "string"});
        item["required"]
            .as_array_mut()
            .expect("required list")
            .push(json!("request_id"));
    }
    item
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_routes_derive_their_ids_from_the_native_package_identity() {
        let app = HostRouteSet::App
            .attachment(&"wamn-control-permission-mine-http".parse().unwrap())
            .expect("permission.mine is an application host route");
        assert_eq!(
            app.operation,
            "wamn-control:permission/mine@0.10.0".parse().unwrap()
        );
        assert_eq!(app.reference, "wamn-control:permission/mine");
        assert_eq!(app.registered_operation(), None);
        assert_eq!(
            app.definition["route"]["path"],
            "/wamn_control/permission/mine"
        );
        {
            let observed = &(app.auth_policy);
            assert_eq!(observed["modes"].as_array().expect("array").len(), 2);
            assert_eq!(observed["modes"][0], serde_json::json!("pat"));
            assert_eq!(observed["modes"][1], serde_json::json!("session"));
        };

        let control = HostRouteSet::Control
            .attachment(&"wamn-control-control-mine-http".parse().unwrap())
            .expect("control.mine is a control host route");
        assert_eq!(
            control.operation,
            "wamn-control:control/mine@0.10.0".parse().unwrap()
        );
        {
            let observed = &(control.auth_policy);
            assert_eq!(observed["modes"].as_array().expect("array").len(), 1);
            assert_eq!(observed["modes"][0], serde_json::json!("session"));
        };
        assert!(
            HostRouteSet::App
                .attachment(&"wamn-control-control-mine-http".parse().unwrap())
                .is_none(),
            "an application release never serves a control route"
        );
        assert_eq!(host_route_package(), &"wamn_control".parse().unwrap());
        assert!(is_host_route_operation("wamn-control:role/create"));
        assert!(is_host_route_operation("wamn-control:role/create@0.10.0"));
        assert!(!is_host_route_operation("wamn-receiving:receipt/get"));

        let users = HostRouteSet::App
            .attachment(&"wamn-control-user-list-http".parse().unwrap())
            .expect("user.list is an application host route");
        let members = HostRouteSet::Control
            .attachment(&"wamn-control-user-list-http".parse().unwrap())
            .expect("user.list is a control host route");
        assert_eq!(users.route.handler, HostHandler::AppUserList);
        assert_eq!(members.route.handler, HostHandler::UserList);
        assert_eq!(users.operation, members.operation);
        assert_eq!(
            users.registered_operation(),
            Some(&"wamn-control:user/list@0.10.0".parse().unwrap())
        );
        assert_eq!(host_route_path_prefix(), "/wamn_control/");

        let counter = HostRouteSet::App
            .attachment(&"wamn-control-number-counter-set-http".parse().unwrap())
            .expect("number_counter.set is an application host route");
        assert_eq!(counter.reference, "wamn-control:number-counter/set");
        assert_eq!(
            counter.registered_operation(),
            Some(&"wamn-control:number-counter/set@0.10.0".parse().unwrap())
        );
        assert!(is_granted_host_route_operation(
            "wamn-control:number-counter/set"
        ));
        assert!(is_granted_host_route_operation(
            "wamn-control:number-counter/set@0.10.0"
        ));
        assert!(!is_granted_host_route_operation(
            "wamn-control:number-counter/set@0.9.0"
        ));
        assert!(!is_granted_host_route_operation("wamn-control:role/create"));
        assert!(!is_granted_host_route_operation(
            "wamn-receiving:receipt/get"
        ));
    }

    #[test]
    fn file_operations_are_grantable_and_contracts_carry_no_bytes_or_signed_url_switch() {
        for (id, route) in HostRouteSet::App
            .attachments()
            .filter(|(_, attachment)| attachment.route.operation.starts_with("file."))
        {
            assert_eq!(route.route.authority, HostRouteAuthority::Granted);
            assert!(route.registered_operation().is_some());
            assert!(is_granted_host_route_operation(&route.reference));
            assert!(HostRouteSet::Control.attachment(id).is_none());
            let (input, _) = (route.route.schemas)();
            let fields = input["properties"].as_object().unwrap();
            assert!(fields.contains_key("app"));
            assert!(fields.keys().all(|field| {
                !["bytes", "signed", "lifetime", "url"]
                    .iter()
                    .any(|name| field.ends_with(name))
            }));
        }
    }
}
