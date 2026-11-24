//! The identity of a project database transaction: a bound identity holds for
//! its own transaction only, and the migration and platform logins resolve
//! theirs through `platform.login_principal`.

use sqlx::Connection as _;
use wamn_identity_protocol::workload_role::WorkloadRoleFamily;
use wamn_provision::PlatformComponent;
use wamn_test_infrastructure::database;

/// The principal, application and operation that the current transaction acts as.
const CURRENT: &str = "SELECT principal_id::text, application, operation \
                       FROM wamn_authority.current_identity() WHERE principal_id IS NOT NULL";

type Identity = Option<(String, String, Option<String>)>;

async fn current(connection: &mut sqlx::PgConnection) -> anyhow::Result<Identity> {
    Ok(sqlx::query_as(CURRENT).fetch_optional(connection).await?)
}

#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn a_bound_identity_holds_for_its_own_transaction_only() -> anyhow::Result<()> {
    let project = database::project().await?;
    let login = WorkloadRoleFamily::App.login_name();
    let mut app = wamn_postgres::connect(project.login_url(login)?.as_str()).await?;
    let principal = PlatformComponent::Executor.principal_id();
    for (application, operation) in [
        ("orders", "orders:order/place@1.0.0"),
        ("billing", "billing:invoice/send@1.0.0"),
    ] {
        let mut transaction = app.begin().await?;
        wamn_postgres::bind_transaction_identity(
            &mut transaction,
            &principal,
            "viewer",
            application,
            Some(operation),
            None,
        )
        .await?;
        assert_eq!(
            current(&mut transaction).await?,
            Some((
                principal.to_string(),
                application.to_owned(),
                Some(operation.to_owned())
            ))
        );
        transaction.commit().await?;
        // The next transaction on the same connection carries no identity.
        let mut next = app.begin().await?;
        assert_eq!(current(&mut next).await?, None, "{application}");
        next.rollback().await?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn migration_and_platform_logins_resolve_their_principal_by_login() -> anyhow::Result<()> {
    let project = database::project().await?;
    let mut admin = project.admin().await?;
    let platform: Vec<String> = PlatformComponent::ALL
        .iter()
        .map(|component| component.principal_id().to_string())
        .collect();
    for family in WorkloadRoleFamily::ALL {
        let login = family.login_name();
        let mut actor = wamn_postgres::connect(project.login_url(login)?.as_str()).await?;
        let resolved = current(&mut actor).await?;
        if !matches!(
            family,
            WorkloadRoleFamily::Migration | WorkloadRoleFamily::Platform
        ) {
            assert_eq!(resolved, None, "{login}");
            continue;
        }
        let (principal, _, _) = resolved.unwrap_or_else(|| panic!("{login} resolves no identity"));
        assert!(
            platform.contains(&principal),
            "{login} resolves {principal}"
        );
        let seeded: i64 =
            sqlx::query_scalar("SELECT count(*) FROM platform.\"user\" WHERE id = $1::text::uuid")
                .bind(&principal)
                .fetch_one(&mut admin)
                .await?;
        assert_eq!(seeded, 1, "{login} resolves {principal} with no user row");

        // A bound identity takes the place of the login's principal.
        let bound = PlatformComponent::AuditRetention.principal_id();
        let mut transaction = actor.begin().await?;
        wamn_postgres::bind_transaction_identity(
            &mut transaction,
            &bound,
            "",
            "retention",
            None,
            None,
        )
        .await?;
        let (principal, _, _) = current(&mut transaction).await?.unwrap();
        assert_eq!(principal, bound.to_string(), "{login}");
        transaction.rollback().await?;
    }
    Ok(())
}
