//! Membership setup for disposable platform fixtures.
//!
//! These fixtures use the provisioning administrator for the system database.
//! They change membership only and do not create users, roles, or tokens.

use anyhow::Context as _;
use tokio_postgres::NoTls;
use wamn_control::provision_project_env::provisioning_transaction;
use wamn_control_provision::validate_project_env;
use wamn_platform_identity::auth_changes::{AuthChanges, AuthDrops};
use wamn_platform_identity::{
    PrincipalId, grant_project_env_membership, revoke_project_env_membership,
};

/// Inputs that name one existing user and one existing project environment.
#[derive(Debug)]
pub(crate) struct MembershipRequest {
    /// Organization that owns the project environment.
    pub org: String,

    /// Project within the organization.
    pub project: String,

    /// Exact environment within the project.
    pub env: String,

    /// Existing user principal UUID from the system database.
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
/// A grant writes no fact that a host keeps, because a host keeps only the
/// positive answer of a credential.
pub(crate) async fn grant(args: MembershipRequest) -> anyhow::Result<PrincipalId> {
    run(args, Action::Grant, &AuthChanges::default()).await
}

/// Revoke membership without failing if the grant is absent.
pub(crate) async fn revoke(
    args: MembershipRequest,
    changes: &AuthChanges,
) -> anyhow::Result<PrincipalId> {
    run(args, Action::Revoke, changes).await
}

fn validate_args(args: &MembershipRequest) -> anyhow::Result<PrincipalId> {
    validate_project_env(&args.org, &args.project, &args.env)
        .context("invalid --org, --project, or --env")?;
    args.principal_id.parse().context("invalid --principal-id")
}

async fn run(
    args: MembershipRequest,
    action: Action,
    changes: &AuthChanges,
) -> anyhow::Result<PrincipalId> {
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
        let mut drops = AuthDrops::default();
        match action {
            Action::Grant => {
                wamn_platform_identity::org::activate_org_membership(
                    &transaction,
                    &principal_id,
                    &args.org,
                )
                .await?;
                grant_project_env_membership(
                    &transaction,
                    &principal_id,
                    &args.org,
                    &args.project,
                    &args.env,
                )
                .await
                .context("grant project environment membership")?;
                drops.identity(principal_id.as_str());
            }
            Action::Revoke => {
                revoke_project_env_membership(
                    &transaction,
                    &principal_id,
                    &args.org,
                    &args.project,
                    &args.env,
                    &mut drops,
                )
                .await
                .context("revoke project environment membership")?;
            }
        }
        transaction
            .commit()
            .await
            .context("commit project environment membership")?;
        changes.publish(drops).await;
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
    use super::{MembershipRequest, validate_args};

    fn request() -> MembershipRequest {
        MembershipRequest {
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
        type SetField = fn(&mut MembershipRequest, &str);
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

    #[tokio::test]
    async fn membership_steps_write_as_provisioning_without_an_admin() {
        use wamn_control_provision::PlatformComponent;
        use wamn_platform_identity::auth_changes::AuthChanges;

        let _lock = wamn_test_postgres::lock();
        let database = wamn_control_provision::test_database::system();
        database
            .execute(&[
                &wamn_control_provision::test_database::seed_org_project_env_sql(
                    "acme", "billing", "dev",
                ),
            ])
            .unwrap();
        let (mut client, connection) =
            tokio_postgres::connect(database.url(), tokio_postgres::NoTls)
                .await
                .unwrap();
        let connection = tokio::spawn(connection);
        client.batch_execute("SET ROLE wamn_system").await.unwrap();
        let transaction =
            wamn_control::provision_project_env::provisioning_transaction(&mut client)
                .await
                .unwrap();
        let user = wamn_platform_identity::create_or_reuse_user(
            &transaction,
            "fixture@example.test",
            "Fixture user",
        )
        .await
        .unwrap();
        transaction.commit().await.unwrap();
        let arguments = || MembershipRequest {
            system_database_url: database.url().to_owned(),
            principal_id: user.principal_id.as_str().to_owned(),
            ..request()
        };
        for _ in 0..2 {
            assert_eq!(super::grant(arguments()).await.unwrap(), user.principal_id);
        }
        let row = client.query_one(
            "SELECT created_by::text, updated_by::text FROM identity.project_env_memberships \
             WHERE principal_id = $1::text::uuid AND org = 'acme' AND project = 'billing' AND env = 'dev'",
            &[&user.principal_id.as_str()],
        ).await.unwrap();
        let provisioning = PlatformComponent::Provisioning.principal_id().to_string();
        assert_eq!(row.get::<_, String>(0), provisioning);
        assert_eq!(row.get::<_, String>(1), provisioning);
        let row = client
            .query_one("SELECT count(*) FROM identity.org_roles", &[])
            .await
            .unwrap();
        assert_eq!(row.get::<_, i64>(0), 0);
        super::revoke(arguments(), &AuthChanges::default())
            .await
            .unwrap();
        let row = client.query_one(
            "SELECT count(*) FROM identity.project_env_memberships WHERE principal_id = $1::text::uuid",
            &[&user.principal_id.as_str()],
        ).await.unwrap();
        assert_eq!(row.get::<_, i64>(0), 0);
        drop(client);
        connection.await.unwrap().unwrap();
    }
}
