//! Grant or revoke a human's membership in one project environment.
//!
//! These commands use the provisioning administrator for the system database.
//! They change membership only and do not create users, roles, or tokens.

use crate::provision_project_env::provisioning_transaction;
use anyhow::Context as _;
use tokio_postgres::NoTls;
use wamn_control_provision::validate_project_env;
use wamn_platform_identity::{
    PrincipalId, grant_project_env_membership, revoke_project_env_membership,
};

/// Inputs that name one existing human and one existing project environment.
#[derive(Debug)]
pub struct ProjectEnvMembershipRequest {
    /// Organization that owns the project environment.
    pub org: String,

    /// Project within the organization.
    pub project: String,

    /// Exact environment within the project.
    pub env: String,

    /// Existing human principal UUID from the system database.
    pub principal_id: String,

    /// Provisioning administrator URL for the system database.
    pub system_database_url: String,
}

#[derive(Clone, Copy)]
enum Action {
    Grant,
    Revoke,
}

/// Grant membership without changing an existing grant.
pub async fn grant(args: ProjectEnvMembershipRequest) -> anyhow::Result<PrincipalId> {
    run(args, Action::Grant).await
}

/// Revoke membership without failing if the grant is absent.
pub async fn revoke(args: ProjectEnvMembershipRequest) -> anyhow::Result<PrincipalId> {
    run(args, Action::Revoke).await
}

fn validate_args(args: &ProjectEnvMembershipRequest) -> anyhow::Result<PrincipalId> {
    validate_project_env(&args.org, &args.project, &args.env)
        .context("invalid --org, --project, or --env")?;
    args.principal_id.parse().context("invalid --principal-id")
}

async fn run(args: ProjectEnvMembershipRequest, action: Action) -> anyhow::Result<PrincipalId> {
    let principal_id = validate_args(&args)?;
    let (mut client, connection) = tokio_postgres::connect(&args.system_database_url, NoTls)
        .await
        .context("connect to the system database for project environment membership")?;
    let connection_task = tokio::spawn(connection);
    let result = async {
        client
            .batch_execute("SET ROLE wamn_system")
            .await
            .context("SET ROLE wamn_system for project environment membership")?;
        let transaction = provisioning_transaction(&mut client).await?;
        match action {
            Action::Grant => {
                grant_project_env_membership(
                    &transaction,
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
                    &transaction,
                    &principal_id,
                    &args.org,
                    &args.project,
                    &args.env,
                )
                .await
                .context("revoke project environment membership")?;
            }
        }
        transaction
            .commit()
            .await
            .context("commit project environment membership")?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    drop(client);
    let _ = connection_task.await;
    result?;
    Ok(principal_id)
}

#[cfg(test)]
mod tests {
    use super::{ProjectEnvMembershipRequest, validate_args};

    fn request() -> ProjectEnvMembershipRequest {
        ProjectEnvMembershipRequest {
            org: "acme".to_owned(),
            project: "billing".to_owned(),
            env: "dev".to_owned(),
            principal_id: "00112233-4455-6677-8899-aabbccddeeff".to_owned(),
            system_database_url: "postgres://admin:secret@localhost/wamn_system".to_owned(),
        }
    }

    #[test]
    fn an_exact_membership_request_names_its_principal() {
        assert_eq!(
            validate_args(&request()).unwrap().as_str(),
            "00112233-4455-6677-8899-aabbccddeeff"
        );
    }

    #[test]
    fn malformed_scope_or_principal_is_refused_before_connecting() {
        type SetField = fn(&mut ProjectEnvMembershipRequest, &str);
        let fields: [(SetField, &str); 4] = [
            (|request, value| request.org = value.to_owned(), "Bad-org"),
            (
                |request, value| request.project = value.to_owned(),
                "wamn-reserved",
            ),
            (|request, value| request.env = value.to_owned(), "dev--prod"),
            (
                |request, value| request.principal_id = value.to_owned(),
                "not-a-principal-id",
            ),
        ];
        for (set, value) in fields {
            let mut request = request();
            set(&mut request, value);
            assert!(validate_args(&request).is_err(), "{value} must be refused");
        }
    }
}
