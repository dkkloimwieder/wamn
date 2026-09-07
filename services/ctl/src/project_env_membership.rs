//! Grant or revoke a human's membership in one project environment.
//!
//! These commands use the provisioning administrator for the system database.
//! They change membership only and do not create users, roles, or tokens.

use anyhow::Context as _;
use clap::Args;
use tokio_postgres::NoTls;
use wamn_control_provision::validate_project_env;
use wamn_platform_identity::{
    PrincipalId, grant_project_env_membership, revoke_project_env_membership,
};

/// Arguments that name one existing human and one existing project environment.
#[derive(Debug, Args)]
pub struct ProjectEnvMembershipArgs {
    /// Organization that owns the project environment.
    #[arg(long)]
    pub org: String,

    /// Project within the organization.
    #[arg(long)]
    pub project: String,

    /// Exact environment within the project.
    #[arg(long)]
    pub env: String,

    /// Existing human principal UUID from the system database.
    #[arg(long)]
    pub principal_id: String,

    /// Provisioning administrator URL for the system database.
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL")]
    pub system_database_url: String,
}

#[derive(Clone, Copy)]
enum Action {
    Grant,
    Revoke,
}

/// Grant membership without changing an existing grant.
pub async fn grant(args: ProjectEnvMembershipArgs) -> anyhow::Result<()> {
    run(args, Action::Grant).await
}

/// Revoke membership without failing if the grant is absent.
pub async fn revoke(args: ProjectEnvMembershipArgs) -> anyhow::Result<()> {
    run(args, Action::Revoke).await
}

fn validate_args(args: &ProjectEnvMembershipArgs) -> anyhow::Result<PrincipalId> {
    validate_project_env(&args.org, &args.project, &args.env)
        .context("invalid --org, --project, or --env")?;
    args.principal_id.parse().context("invalid --principal-id")
}

async fn run(args: ProjectEnvMembershipArgs, action: Action) -> anyhow::Result<()> {
    let principal_id = validate_args(&args)?;
    let (client, connection) = tokio_postgres::connect(&args.system_database_url, NoTls)
        .await
        .context("connect to the system database for project environment membership")?;
    let connection_task = tokio::spawn(connection);
    let result = async {
        client
            .batch_execute("SET ROLE wamn_system")
            .await
            .context("SET ROLE wamn_system for project environment membership")?;
        match action {
            Action::Grant => {
                grant_project_env_membership(
                    &client,
                    &principal_id,
                    &args.org,
                    &args.project,
                    &args.env,
                )
                .await
                .context("grant project environment membership")?;
            }
            Action::Revoke => {
                revoke_project_env_membership(
                    &client,
                    &principal_id,
                    &args.org,
                    &args.project,
                    &args.env,
                )
                .await
                .context("revoke project environment membership")?;
            }
        }
        Ok::<(), anyhow::Error>(())
    }
    .await;
    drop(client);
    let _ = connection_task.await;
    result?;
    let state = match action {
        Action::Grant => "granted",
        Action::Revoke => "revoked",
    };
    println!(
        "membership {state} principal_id={principal_id} org={} project={} env={}",
        args.org, args.project, args.env
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory as _, Parser};

    use super::{ProjectEnvMembershipArgs, validate_args};

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        args: ProjectEnvMembershipArgs,
    }

    fn arguments() -> [&'static str; 11] {
        [
            "membership",
            "--org",
            "acme",
            "--project",
            "billing",
            "--env",
            "dev",
            "--principal-id",
            "00112233-4455-6677-8899-aabbccddeeff",
            "--system-database-url",
            "postgres://admin:secret@localhost/wamn_system",
        ]
    }

    #[test]
    fn exact_membership_arguments_parse() {
        let args = TestCli::try_parse_from(arguments()).unwrap().args;
        assert_eq!(args.org, "acme");
        assert_eq!(args.project, "billing");
        assert_eq!(args.env, "dev");
        assert_eq!(
            validate_args(&args).unwrap().as_str(),
            "00112233-4455-6677-8899-aabbccddeeff"
        );
    }

    #[test]
    fn every_membership_argument_is_required() {
        let command = TestCli::command();
        for name in [
            "org",
            "project",
            "env",
            "principal_id",
            "system_database_url",
        ] {
            let argument = command
                .get_arguments()
                .find(|argument| argument.get_id() == name)
                .unwrap();
            assert!(argument.is_required_set(), "{name} must be required");
        }
        let database = command
            .get_arguments()
            .find(|argument| argument.get_id() == "system_database_url")
            .unwrap();
        assert_eq!(database.get_env().unwrap(), "WAMN_SYSTEM_ADMIN_URL");
    }

    #[test]
    fn malformed_scope_or_principal_is_refused_before_connecting() {
        for (index, value) in [
            (2, "Bad-org"),
            (4, "wamn-reserved"),
            (6, "dev--prod"),
            (8, "not-a-principal-id"),
        ] {
            let mut arguments = arguments();
            arguments[index] = value;
            let args = TestCli::try_parse_from(arguments).unwrap().args;
            assert!(validate_args(&args).is_err(), "{value} must be refused");
        }
    }
}
