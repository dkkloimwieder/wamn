//! Arguments and output of the role and permission verbs: `grant-role`,
//! `revoke-role`, `create-role`, `delete-role`, `grant-permission` and
//! `revoke-permission` (docs/architecture/execution.md#host-run-routes).

use clap::Args;
use wamn_control::role_permission;
use wamn_control::user_role::{self, EnvironmentTarget, UserRoleOutcome, UserRoleRequest};
use wamn_platform_identity::auth_changes::{AuthChanges, AuthNats};

/// One user role of one environment, for one user named by email.
#[derive(Debug, Args)]
pub struct UserRoleArgs {
    /// Administrative Postgres URL to the system registry.
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL")]
    pub system_database_url: String,

    /// Administrative Postgres URL to the registry-derived project database.
    /// The login holds administration grants. Env `WAMN_PG_ADMIN_URL`.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: String,

    /// Registry organization.
    #[arg(long)]
    pub org: String,

    /// Registry project.
    #[arg(long)]
    pub project: String,

    /// Registry environment.
    #[arg(long)]
    pub env: String,

    /// Email of the user or service in the tenant.
    #[arg(long)]
    pub user: String,

    /// A role that exists in the tenant, such as `admin`.
    #[arg(long)]
    pub role: String,
}

fn request(args: UserRoleArgs) -> UserRoleRequest {
    UserRoleRequest {
        system_database_url: args.system_database_url,
        admin_database_url: args.admin_database_url,
        org: args.org,
        project: args.project,
        env: args.env,
        user: args.user,
        role: args.role,
    }
}

/// The publisher of the auth changes of a verb, from `WAMN_AUTH_NATS_*`.
async fn changes() -> anyhow::Result<AuthChanges> {
    AuthChanges::connect(&AuthNats::from_env()?).await
}

fn print(verb: &str, role: &str, outcome: &UserRoleOutcome) {
    let state = if outcome.changed { verb } else { "unchanged" };
    println!("{state}: role {role} for user {}", outcome.user_id);
}

/// Give the user the role and print the result.
pub async fn grant(args: UserRoleArgs) -> anyhow::Result<()> {
    let role = args.role.clone();
    let outcome = user_role::grant_role(&request(args), &changes().await?).await?;
    print("granted", &role, &outcome);
    Ok(())
}

/// Take the role from the user and print the result.
pub async fn revoke(args: UserRoleArgs) -> anyhow::Result<()> {
    let role = args.role.clone();
    let outcome = user_role::revoke_role(&request(args), &changes().await?).await?;
    print("revoked", &role, &outcome);
    Ok(())
}

/// One environment named in the registry.
#[derive(Debug, Args)]
pub struct EnvironmentArgs {
    /// Administrative Postgres URL to the system registry.
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL")]
    pub system_database_url: String,

    /// Administrative Postgres URL to the registry-derived project database.
    /// The login holds administration grants. Env `WAMN_PG_ADMIN_URL`.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: String,

    /// Registry organization.
    #[arg(long)]
    pub org: String,

    /// Registry project.
    #[arg(long)]
    pub project: String,

    /// Registry environment.
    #[arg(long)]
    pub env: String,
}

impl EnvironmentArgs {
    fn target(self) -> EnvironmentTarget {
        EnvironmentTarget {
            system_database_url: self.system_database_url,
            admin_database_url: self.admin_database_url,
            org: self.org,
            project: self.project,
            env: self.env,
        }
    }
}

/// One authored role of one environment.
#[derive(Debug, Args)]
pub struct RoleArgs {
    #[command(flatten)]
    pub environment: EnvironmentArgs,

    /// The authored role: lowercase letters, digits and hyphens after the
    /// first character, at most 64 bytes. Not `admin`.
    #[arg(long)]
    pub role: String,
}

/// One operation reference of one authored role.
#[derive(Debug, Args)]
pub struct PermissionArgs {
    #[command(flatten)]
    pub environment: EnvironmentArgs,

    /// The authored role.
    #[arg(long)]
    pub role: String,

    /// The stable operation reference `<package>:<interface>/<operation>`,
    /// without a package version.
    #[arg(long)]
    pub operation: String,
}

/// Create the empty authored role and print the result.
pub async fn create_role(args: RoleArgs) -> anyhow::Result<()> {
    let created =
        role_permission::create_role_in(&args.environment.target(), &args.role, &changes().await?)
            .await?;
    let state = if created { "created" } else { "unchanged" };
    println!("{state}: role {}", args.role);
    Ok(())
}

/// Delete the authored role and print the result.
pub async fn delete_role(args: RoleArgs) -> anyhow::Result<()> {
    let deleted =
        role_permission::delete_role_in(&args.environment.target(), &args.role, &changes().await?)
            .await?;
    let state = if deleted { "deleted" } else { "unchanged" };
    println!("{state}: role {}", args.role);
    Ok(())
}

/// Grant the operation to the role and print the result.
pub async fn grant_permission(args: PermissionArgs) -> anyhow::Result<()> {
    let outcome = role_permission::grant_permission_in(
        &args.environment.target(),
        &args.role,
        &args.operation,
        &changes().await?,
    )
    .await?;
    let state = if outcome.rows_added > 0 {
        "granted"
    } else {
        "unchanged"
    };
    println!("{state}: {} to role {}", args.operation, args.role);
    Ok(())
}

/// Remove the operation from the role and print the result.
pub async fn revoke_permission(args: PermissionArgs) -> anyhow::Result<()> {
    role_permission::revoke_permission_in(
        &args.environment.target(),
        &args.role,
        &args.operation,
        &changes().await?,
    )
    .await?;
    println!("revoked: {} from role {}", args.operation, args.role);
    Ok(())
}
