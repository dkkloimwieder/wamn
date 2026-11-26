//! Native input and outcome values for the route table.

use super::{ConnectionSummary, nullable};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Empty {}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectionListResult {
    connections: Vec<ConnectionSummary>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileDeleteRequest {
    pub application: String,
    pub file_id: String,
    pub binding: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileDeleteResult {
    pub file_id: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub enum FileExposeRequestExposure {
    #[serde(rename = "private")]
    Private,
    #[serde(rename = "application")]
    App,
    #[serde(rename = "public")]
    Public,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileExposeRequest {
    pub application: String,
    pub file_id: String,
    pub binding: String,
    pub exposure: FileExposeRequestExposure,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileExposeResult {
    pub file_id: String,
    pub exposure: FileExposeRequestExposure,
    #[schemars(schema_with = "nullable::<String>")]
    pub url: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileGetRequest {
    pub application: String,
    pub file_id: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FilePutRequest {
    pub application: String,
    pub binding: String,
    pub name: String,
    pub media_type: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NumberCounterSetRequest {
    pub application: String,
    pub sequence: String,
    pub counter: String,
    pub key: String,
    pub value: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NumberCounterSetResult {
    pub value: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PermissionGrantRequest {
    pub role: String,
    pub operation: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PermissionGrantResult {
    pub rows_added: i32,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PermissionListRequest {
    #[schemars(title = "Role")]
    pub role: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Each flag is a separate native wire field"
)]
pub struct PermissionListResultOperation {
    #[schemars(title = "Operation")]
    pub operation: String,
    pub served: bool,
    pub grantable: bool,
    pub admin_only: bool,
    pub selected: bool,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PermissionListResult {
    #[schemars(title = "Role")]
    pub role: String,
    pub admin: bool,
    pub operations: Vec<PermissionListResultOperation>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PermissionMineResult {
    pub admin: bool,
    pub permissions: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PermissionRevokeResult {}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RoleCreateResult {
    pub created: bool,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RoleDeleteResult {
    pub deleted: bool,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RoleListResult {
    #[schemars(title = "Role")]
    pub roles: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[schemars(rename = "ApplicationUserListResultUser")]
#[serde(deny_unknown_fields)]
pub struct AppUserListResultUser {
    #[schemars(regex(
        pattern = r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"
    ))]
    #[schemars(title = "User ID")]
    pub id: String,
    #[schemars(title = "Email")]
    pub email: String,
    #[schemars(schema_with = "nullable::<String>")]
    #[schemars(title = "User name")]
    pub display_name: Option<String>,
    #[schemars(title = "Role")]
    pub roles: Vec<String>,
    pub admin_covered: bool,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[schemars(rename = "ApplicationUserListResult")]
#[serde(deny_unknown_fields)]
pub struct AppUserListResult {
    pub users: Vec<AppUserListResultUser>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UserRoleGrantRequest {
    #[schemars(regex(
        pattern = r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"
    ))]
    #[schemars(title = "User")]
    pub user_id: String,
    #[schemars(title = "Role")]
    pub role: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UserRoleGrantResult {
    pub granted: bool,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UserRoleRevokeResult {
    pub revoked: bool,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkflowAuthorRequest {
    pub graph: Value,
    #[schemars(schema_with = "nullable::<String>")]
    pub execution_role: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkflowAuthorResult {
    pub result: Value,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDisableRequest {
    pub workflow_id: String,
    pub version: i32,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub enum WorkflowRunAs {
    #[serde(rename = "admin")]
    Admin,
    #[serde(rename = "workflow")]
    Workflow,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkflowRunRequest {
    pub workflow_id: String,
    pub run_as: WorkflowRunAs,
    pub input_json: Value,
    pub idempotency_key: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkflowRunResult {
    pub run_id: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApiServiceCreateRequest {
    pub project: String,
    pub env: String,
    pub name: String,
    pub display_name: String,
    pub roles: Value,
    pub lifetime_seconds: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApiServiceCreateResult {
    pub principal_id: String,
    pub token: String,
    pub token_prefix: String,
    pub created_at: String,
    pub expires_at: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[schemars(rename = "ApplicationDeployRequest")]
#[serde(deny_unknown_fields)]
pub struct AppDeployRequest {
    pub project: String,
    pub env: String,
    #[schemars(title = "Release digest")]
    pub release: String,
    #[schemars(title = "Application")]
    pub application: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[schemars(rename = "ApplicationDeployResult")]
#[serde(deny_unknown_fields)]
pub struct AppDeployResult {
    pub job: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[schemars(rename = "ApplicationRollbackRequest")]
#[serde(deny_unknown_fields)]
pub struct AppRollbackRequest {
    pub project: String,
    pub env: String,
    #[schemars(title = "Application")]
    pub application: String,
    #[schemars(title = "Revision")]
    pub revision: Option<std::num::NonZeroU32>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[schemars(rename = "ApplicationStopRequest")]
#[serde(deny_unknown_fields)]
pub struct AppStopRequest {
    pub project: String,
    pub env: String,
    #[schemars(title = "Application")]
    pub application: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CaptureGapCloseRequest {
    pub project: String,
    pub env: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectionAuthorizeRequest {
    pub project: String,
    pub env: String,
    pub name: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectionAuthorizeResult {
    pub authorization_url: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectionCopyRequest {
    pub project: String,
    pub env: String,
    pub name: String,
    pub source_env: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectionDeleteResult {
    pub name: String,
    pub principal_id: String,
    pub id: String,
    pub env: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectionSetCredentialRequest {
    pub project: String,
    pub env: String,
    pub name: String,
    pub credential: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectionTestRequest {
    pub project: String,
    pub env: String,
    pub name: String,
    pub parameters: Value,
    #[schemars(schema_with = "nullable::<String>")]
    pub credential: Option<String>,
    #[schemars(schema_with = "nullable::<i32>")]
    pub version: Option<i32>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectionWriteRequest {
    pub project: String,
    pub env: String,
    pub name: String,
    #[schemars(schema_with = "nullable::<String>")]
    pub authority: Option<String>,
    pub parameters: Value,
    #[schemars(schema_with = "nullable::<String>")]
    pub credential: Option<String>,
    pub integration_type: String,
    pub auth_type: String,
    #[schemars(schema_with = "nullable::<String>")]
    pub external_system: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ControlMineRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "i32")]
    pub limit: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub cursor: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ControlMineResultProject {
    #[schemars(title = "Project")]
    pub project: String,
    #[schemars(title = "Project admin")]
    pub project_admin: bool,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ControlMineResult {
    #[schemars(title = "Org admin")]
    pub org_admin: bool,
    pub projects: Vec<ControlMineResultProject>,
    #[schemars(schema_with = "nullable::<String>")]
    pub next_cursor: Option<String>,
    #[schemars(schema_with = "nullable::<i32>")]
    pub total: Option<i32>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[schemars(rename = "EnvironmentConfigureRequest")]
#[serde(deny_unknown_fields)]
pub struct EnvConfigureRequest {
    pub project: String,
    pub env: String,
    pub document: Value,
    #[schemars(schema_with = "nullable::<bool>")]
    pub disposable: Option<bool>,
    #[schemars(schema_with = "nullable::<String>")]
    pub application: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[schemars(rename = "EnvironmentDeleteRequest")]
#[serde(deny_unknown_fields)]
pub struct EnvDeleteRequest {
    pub project: String,
    pub env: String,
    pub data: bool,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[schemars(rename = "EnvironmentListRequest")]
#[serde(deny_unknown_fields)]
pub struct EnvListRequest {
    pub project: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "i32")]
    pub limit: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub cursor: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[schemars(rename = "EnvironmentListResultEnvironmentState")]
pub enum EnvListResultEnvState {
    #[serde(rename = "active")]
    Active,
    #[serde(rename = "inactive")]
    Inactive,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[schemars(rename = "EnvironmentListResultEnvironment")]
#[serde(deny_unknown_fields)]
pub struct EnvListResultEnv {
    pub name: String,
    #[schemars(title = "State")]
    pub state: EnvListResultEnvState,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[schemars(rename = "EnvironmentListResult")]
#[serde(deny_unknown_fields)]
pub struct EnvListResult {
    pub environments: Vec<EnvListResultEnv>,
    #[schemars(schema_with = "nullable::<String>")]
    pub next_cursor: Option<String>,
    #[schemars(schema_with = "nullable::<i32>")]
    pub total: Option<i32>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[schemars(rename = "EnvironmentOperationStatusRequest")]
#[serde(deny_unknown_fields)]
pub struct EnvOperationStatusRequest {
    pub project: String,
    pub env: String,
    pub job: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[schemars(rename = "EnvironmentOperationStatusResult")]
#[serde(deny_unknown_fields)]
pub struct EnvOperationStatusResult {
    pub job: String,
    #[schemars(title = "State")]
    pub state: String,
    #[schemars(title = "Log tail")]
    pub log: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[schemars(rename = "EnvironmentStatusResultRollout")]
#[serde(deny_unknown_fields)]
pub struct EnvStatusResultRollout {
    pub replicas: i32,
    pub updated_replicas: i32,
    pub ready_replicas: i32,
    pub available_replicas: i32,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[schemars(rename = "EnvironmentStatusResultWorkloadCondition")]
#[serde(deny_unknown_fields)]
pub struct EnvStatusResultWorkloadCondition {
    #[schemars(title = "Condition")]
    pub r#type: Value,
    #[schemars(title = "State")]
    pub status: Value,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[schemars(rename = "EnvironmentStatusResultWorkload")]
#[serde(deny_unknown_fields)]
pub struct EnvStatusResultWorkload {
    #[schemars(title = "Workload")]
    pub name: String,
    #[schemars(schema_with = "nullable::<String>")]
    #[schemars(title = "Application")]
    pub application: Option<String>,
    #[schemars(title = "Package digest")]
    pub package_digest: String,
    #[schemars(range(min = 1))]
    #[schemars(title = "Revision")]
    pub revision: u32,
    #[schemars(title = "Conditions")]
    pub conditions: Vec<EnvStatusResultWorkloadCondition>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[schemars(rename = "EnvironmentStatusResult")]
#[serde(deny_unknown_fields)]
pub struct EnvStatusResult {
    #[schemars(title = "Host replicas")]
    pub rollout: EnvStatusResultRollout,
    #[schemars(title = "Workloads")]
    pub workloads: Vec<EnvStatusResultWorkload>,
    #[schemars(schema_with = "nullable::<i32>")]
    #[schemars(title = "Users with a role")]
    pub users_with_role: Option<i32>,
    #[schemars(schema_with = "nullable::<i32>")]
    #[schemars(title = "Roles")]
    pub roles: Option<i32>,
    #[schemars(schema_with = "nullable::<i32>")]
    #[schemars(title = "Memberships")]
    pub memberships: Option<i32>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExternalSystemDeleteResult {
    pub name: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExternalSystemListResult {
    pub external_systems: Value,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IntegrationTypeListRequest {
    pub project: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IntegrationTypeListResult {
    pub integration_types: Value,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MemberGrantRequest {
    #[schemars(with = "String")]
    #[schemars(title = "Project")]
    pub project: wamn_registry::ProjectId,
    #[schemars(with = "String")]
    #[schemars(title = "Environment")]
    pub env: wamn_registry::EnvName,
    #[schemars(regex(
        pattern = r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"
    ))]
    #[schemars(title = "User ID")]
    pub principal_id: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MemberGrantResult {
    #[schemars(regex(
        pattern = r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"
    ))]
    pub principal_id: String,
    pub project: String,
    pub env: String,
    pub member: bool,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MemberListRequest {
    pub project: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "i32")]
    pub limit: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub environment: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MemberListResultMember {
    #[schemars(regex(
        pattern = r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"
    ))]
    #[schemars(title = "User ID")]
    pub principal_id: String,
    #[schemars(title = "Email")]
    pub email: String,
    #[schemars(title = "Display name")]
    pub display_name: String,
    #[schemars(title = "Org admin")]
    pub org_admin: bool,
    #[schemars(title = "Project admin")]
    pub project_admin: bool,
    #[schemars(title = "Memberships")]
    pub environments: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MemberListResult {
    pub members: Vec<MemberListResultMember>,
    #[schemars(schema_with = "nullable::<String>")]
    pub next_cursor: Option<String>,
    #[schemars(schema_with = "nullable::<i32>")]
    pub total: Option<i32>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OrgAdminGrantRequest {
    #[schemars(regex(
        pattern = r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"
    ))]
    pub principal_id: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OrgAdminGrantResult {
    #[schemars(regex(
        pattern = r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"
    ))]
    pub principal_id: String,
    pub org_admin: bool,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PatIssueRequest {
    pub project: String,
    pub env: String,
    pub principal_id: String,
    pub label: String,
    pub lifetime_seconds: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PatListRequest {
    pub project: String,
    pub env: String,
    pub principal_id: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PatListResult {
    pub pats: Value,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PatRevokeRequest {
    pub project: String,
    pub env: String,
    pub token_prefix: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlatformSchemaRequest {
    pub target: String,
    pub projects: Value,
    #[schemars(schema_with = "nullable::<String>")]
    pub org: Option<String>,
    #[schemars(schema_with = "nullable::<String>")]
    pub env: Option<String>,
    pub apply: bool,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectAdminGrantRequest {
    #[schemars(with = "String")]
    pub project: wamn_registry::ProjectId,
    #[schemars(regex(
        pattern = r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"
    ))]
    pub principal_id: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectAdminGrantResult {
    #[schemars(regex(
        pattern = r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"
    ))]
    pub principal_id: String,
    pub project: String,
    pub project_admin: bool,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectListResult {
    pub projects: Vec<EnvListResultEnv>,
    #[schemars(schema_with = "nullable::<String>")]
    pub next_cursor: Option<String>,
    #[schemars(schema_with = "nullable::<i32>")]
    pub total: Option<i32>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SchemaDiffRequest {
    pub project: String,
    pub env: String,
    #[schemars(title = "Application")]
    pub application: String,
    #[schemars(title = "Release digest")]
    pub release: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ServicePrincipalCreateRequest {
    pub project: String,
    pub env: String,
    pub subject: String,
    pub display_name: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ServicePrincipalCreateResult {
    pub principal_id: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ServicePrincipalDisableResult {
    pub principal_id: String,
    pub status: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UserActivateResult {
    #[schemars(regex(
        pattern = r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"
    ))]
    pub principal_id: String,
    pub status: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UserCreateRequest {
    pub email: String,
    pub display_name: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UserCreateResult {
    #[schemars(regex(
        pattern = r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"
    ))]
    pub principal_id: String,
    pub enrolled: bool,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UserInviteRequest {
    #[schemars(title = "Email")]
    pub email: String,
    #[schemars(title = "Display name")]
    pub display_name: String,
    #[serde(default)]
    pub org_admin: bool,
    #[serde(default)]
    #[schemars(with = "Vec<String>")]
    pub project_admins: Vec<wamn_registry::ProjectId>,
    #[serde(default)]
    pub memberships: Vec<super::EnvRequest>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UserInviteResult {
    #[schemars(regex(
        pattern = r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"
    ))]
    pub principal_id: String,
    pub enrolled: bool,
    pub invited: bool,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UserListResultUser {
    #[schemars(regex(
        pattern = r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"
    ))]
    #[schemars(title = "User ID")]
    pub principal_id: String,
    #[schemars(title = "Email")]
    pub email: String,
    #[schemars(title = "Display name")]
    pub display_name: String,
    #[schemars(title = "State")]
    pub status: String,
    pub principal_status: String,
    #[schemars(title = "Org admin")]
    pub org_admin: bool,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UserListResult {
    pub users: Vec<UserListResultUser>,
    #[schemars(schema_with = "nullable::<String>")]
    pub next_cursor: Option<String>,
    #[schemars(schema_with = "nullable::<i32>")]
    pub total: Option<i32>,
}
