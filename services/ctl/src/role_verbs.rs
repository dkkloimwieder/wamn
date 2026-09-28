//! Arguments and output of the `grant-role` and `revoke-role` verbs.

use clap::Args;
use wamn_control::user_roles::{self, UserRoleOutcome, UserRoleRequest};

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

    /// Email of the person or service in the tenant.
    #[arg(long)]
    pub user: String,

    /// The role: `operator` or `admin`.
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
