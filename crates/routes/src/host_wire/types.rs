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
    Application,
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
    pub role: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Each flag is a separate native wire field"
)]
pub struct PermissionListResultOperation {
    pub operation: String,
    pub served: bool,
    pub grantable: bool,
    pub admin_only: bool,
    pub selected: bool,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PermissionListResult {
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
    pub roles: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApplicationUserListResultUser {
    #[schemars(regex(
        pattern = r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"
    ))]
    pub id: String,
    pub email: String,
    #[schemars(schema_with = "nullable::<String>")]
    pub display_name: Option<String>,
    pub roles: Vec<String>,
    pub admin_covered: bool,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApplicationUserListResult {
    pub users: Vec<ApplicationUserListResultUser>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UserRoleGrantRequest {
    #[schemars(regex(
        pattern = r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"
    ))]
    pub user_id: String,
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
#[serde(deny_unknown_fields)]
pub struct ApplicationDeployRequest {
    pub project: String,
    pub env: String,
    pub release: String,
    pub application: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApplicationDeployResult {
    pub job: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApplicationRollbackRequest {
    pub project: String,
    pub env: String,
    pub application: String,
    pub revision: Option<std::num::NonZeroU32>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApplicationStopRequest {
    pub project: String,
    pub env: String,
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
    pub project: String,
    pub project_admin: bool,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ControlMineResult {
    pub org_admin: bool,
    pub projects: Vec<ControlMineResultProject>,
    #[schemars(schema_with = "nullable::<String>")]
    pub next_cursor: Option<String>,
    #[schemars(schema_with = "nullable::<i32>")]
    pub total: Option<i32>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentConfigureRequest {
    pub project: String,
    pub env: String,
    pub document: Value,
    #[schemars(schema_with = "nullable::<bool>")]
    pub disposable: Option<bool>,
    #[schemars(schema_with = "nullable::<String>")]
    pub application: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentDeleteRequest {
    pub project: String,
    pub env: String,
    pub data: bool,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentListRequest {
    pub project: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "i32")]
    pub limit: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub cursor: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub enum EnvironmentListResultEnvironmentState {
    #[serde(rename = "active")]
    Active,
    #[serde(rename = "inactive")]
    Inactive,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentListResultEnvironment {
    pub name: String,
    pub state: EnvironmentListResultEnvironmentState,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentListResult {
    pub environments: Vec<EnvironmentListResultEnvironment>,
    #[schemars(schema_with = "nullable::<String>")]
    pub next_cursor: Option<String>,
    #[schemars(schema_with = "nullable::<i32>")]
    pub total: Option<i32>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentOperationStatusRequest {
    pub project: String,
    pub env: String,
    pub job: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentOperationStatusResult {
    pub job: String,
    pub state: String,
    pub log: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentStatusResultRollout {
    pub replicas: i32,
    pub updated_replicas: i32,
    pub ready_replicas: i32,
    pub available_replicas: i32,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentStatusResultWorkloadCondition {
    pub r#type: Value,
    pub status: Value,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentStatusResultWorkload {
    pub name: String,
    #[schemars(schema_with = "nullable::<String>")]
    pub application: Option<String>,
    pub package_digest: String,
    #[schemars(range(min = 1))]
    pub revision: u32,
    pub conditions: Vec<EnvironmentStatusResultWorkloadCondition>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentStatusResult {
    pub rollout: EnvironmentStatusResultRollout,
    pub workloads: Vec<EnvironmentStatusResultWorkload>,
    #[schemars(schema_with = "nullable::<i32>")]
    pub users_with_role: Option<i32>,
    #[schemars(schema_with = "nullable::<i32>")]
    pub roles: Option<i32>,
    #[schemars(schema_with = "nullable::<i32>")]
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
    pub project: wamn_registry::ProjectId,
    #[schemars(with = "String")]
    pub env: wamn_registry::EnvName,
    #[schemars(regex(
        pattern = r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"
    ))]
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
    pub principal_id: String,
    pub email: String,
    pub display_name: String,
    pub org_admin: bool,
    pub project_admin: bool,
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
    pub projects: Vec<EnvironmentListResultEnvironment>,
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
    pub application: String,
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
    pub email: String,
    pub display_name: String,
    #[serde(default)]
    pub org_admin: bool,
    #[serde(default)]
    #[schemars(with = "Vec<String>")]
    pub project_admins: Vec<wamn_registry::ProjectId>,
    #[serde(default)]
    pub memberships: Vec<super::EnvironmentRequest>,
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
    pub principal_id: String,
    pub email: String,
    pub display_name: String,
    pub status: String,
    pub principal_status: String,
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
