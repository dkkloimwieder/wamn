//! Arguments and output of the role and permission verbs: `grant-role`,
//! `revoke-role`, `create-role`, `delete-role`, `grant-permission` and
//! `revoke-permission` (docs/plan/platform-ui.md §4.1).

use clap::Args;
use wamn_control::role_permissions;
use wamn_control::user_roles::{self, EnvironmentTarget, UserRoleOutcome, UserRoleRequest};

/// One user role of one environment, for one user named by email.
#[derive(Debug, Args)]
pub struct UserRoleArgs {
    /// Administrative Postgres URL to the system registry.
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL")]
    pub system_database_url: String,

    /// Administrative Postgres URL to the registry-derived project database.
    /// It must be SUPERUSER or BYPASSRLS. Env `WAMN_PG_ADMIN_URL`.
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

    /// Tenant of the environment.
    #[arg(long)]
    pub tenant: String,

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
        tenant: args.tenant,
        user: args.user,
        role: args.role,
    }
}

fn print(verb: &str, role: &str, outcome: &UserRoleOutcome) {
    let state = if outcome.changed { verb } else { "unchanged" };
    println!("{state}: role {role} for user {}", outcome.user_id);
}

/// Give the user the role and print the result.
pub async fn grant(args: UserRoleArgs) -> anyhow::Result<()> {
    let role = args.role.clone();
    let outcome = user_roles::grant_role(&request(args)).await?;
    print("granted", &role, &outcome);
    Ok(())
}

/// Take the role from the user and print the result.
pub async fn revoke(args: UserRoleArgs) -> anyhow::Result<()> {
    let role = args.role.clone();
    let outcome = user_roles::revoke_role(&request(args)).await?;
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
    /// It must be SUPERUSER or BYPASSRLS. Env `WAMN_PG_ADMIN_URL`.
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

    /// Tenant of the environment.
    #[arg(long)]
    pub tenant: String,
}

impl EnvironmentArgs {
    fn target(self) -> EnvironmentTarget {
        EnvironmentTarget {
            system_database_url: self.system_database_url,
            admin_database_url: self.admin_database_url,
            org: self.org,
            project: self.project,
            env: self.env,
            tenant: self.tenant,
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
    let created = role_permissions::create_role_in(&args.environment.target(), &args.role).await?;
    let state = if created { "created" } else { "unchanged" };
    println!("{state}: role {}", args.role);
    Ok(())
}

/// Delete the authored role and print the result.
pub async fn delete_role(args: RoleArgs) -> anyhow::Result<()> {
    let deleted = role_permissions::delete_role_in(&args.environment.target(), &args.role).await?;
    let state = if deleted { "deleted" } else { "unchanged" };
    println!("{state}: role {}", args.role);
    Ok(())
}

/// Select the operation for the role with its closure and print the result.
pub async fn grant_permission(args: PermissionArgs) -> anyhow::Result<()> {
    let outcome = role_permissions::grant_permission_in(
        &args.environment.target(),
        &args.role,
        &args.operation,
    )
    .await?;
    let state = if outcome.rows_added > 0 {
        "granted"
    } else {
        "unchanged"
    };
    let required = outcome
        .closure
        .iter()
        .filter(|permission| **permission != args.operation)
        .map(String::as_str)
        .collect::<Vec<_>>();
    println!("{state}: {} to role {}", args.operation, args.role);
    if !required.is_empty() {
        println!("requires: {}", required.join(", "));
    }
    Ok(())
}

/// Remove the selection of the operation from the role and print the result.
pub async fn revoke_permission(args: PermissionArgs) -> anyhow::Result<()> {
    let outcome = role_permissions::revoke_permission_in(
        &args.environment.target(),
        &args.role,
        &args.operation,
    )
    .await?;
    println!("revoked: {} from role {}", args.operation, args.role);
    if !outcome.still_required_by.is_empty() {
        println!(
            "still effective: {} requires it",
            outcome.still_required_by.join(", ")
        );
    }
    Ok(())
}
