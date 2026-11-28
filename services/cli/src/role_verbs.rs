//! Authored application roles and permissions through authenticated HTTP routes.

use clap::Args;
use serde_json::json;

/// The application host and the caller's session.
#[derive(Debug, Args)]
pub struct EnvArgs {
    /// Application API base URL, including /<project>/<app>/api.
    #[arg(long)]
    pub host_url: String,
    /// The caller's session token.
    #[arg(long, env = "WAMN_TOKEN", hide_env_values = true)]
    pub token: String,
}

/// An authored role assignment for one user.
#[derive(Debug, Args)]
pub struct UserRoleArgs {
    #[command(flatten)]
    pub env: EnvArgs,
    /// The user principal UUID.
    #[arg(long)]
    pub user_id: wamn_identity_protocol::PrincipalId,
    /// The authored role.
    #[arg(long)]
    pub role: String,
}

/// One authored role.
#[derive(Debug, Args)]
pub struct RoleArgs {
    #[command(flatten)]
    pub env: EnvArgs,
    #[arg(long)]
    pub role: String,
}

/// One operation permission of an authored role.
#[derive(Debug, Args)]
pub struct PermissionArgs {
    #[command(flatten)]
    pub env: EnvArgs,
    #[arg(long)]
    pub role: String,
    #[arg(long)]
    pub operation: String,
}

pub async fn grant(args: UserRoleArgs) -> anyhow::Result<()> {
    crate::component_verbs::call(
        &args.env.host_url,
        &args.env.token,
        "user_role.grant",
        &json!({"user_id": args.user_id, "role": args.role}),
    )
    .await
}

pub async fn revoke(args: UserRoleArgs) -> anyhow::Result<()> {
    crate::component_verbs::call(
        &args.env.host_url,
        &args.env.token,
        "user_role.revoke",
        &json!({"user_id": args.user_id, "role": args.role}),
    )
    .await
}

pub async fn create_role(args: RoleArgs) -> anyhow::Result<()> {
    crate::component_verbs::call(
        &args.env.host_url,
        &args.env.token,
        "role.create",
        &json!({"role": args.role}),
    )
    .await
}

pub async fn deactivate_role(args: RoleArgs) -> anyhow::Result<()> {
    crate::component_verbs::call(
        &args.env.host_url,
        &args.env.token,
        "role.deactivate",
        &json!({"role": args.role}),
    )
    .await
}

pub async fn activate_role(args: RoleArgs) -> anyhow::Result<()> {
    crate::component_verbs::call(
        &args.env.host_url,
        &args.env.token,
        "role.activate",
        &json!({"role": args.role}),
    )
    .await
}

pub async fn grant_permission(args: PermissionArgs) -> anyhow::Result<()> {
    crate::component_verbs::call(
        &args.env.host_url,
        &args.env.token,
        "permission.grant",
        &json!({"role": args.role, "operation": args.operation}),
    )
    .await
}

pub async fn revoke_permission(args: PermissionArgs) -> anyhow::Result<()> {
    crate::component_verbs::call(
        &args.env.host_url,
        &args.env.token,
        "permission.revoke",
        &json!({"role": args.role, "operation": args.operation}),
    )
    .await
}
