use anyhow::Context as _;
use wamn_identity_protocol::PlatformComponent;
use wamn_platform_install::provision_org::{APPLY_SECRET, CONTROL_SECRET, create_org_logins};
use wamn_platform_install::provision_system::SYSTEM_DATABASE;
use wamn_test_infrastructure::containers::start_postgres;

use super::{Outcome, command, record};

#[tokio::test]
#[ignore = "requires: docker"]
async fn a_platform_command_keeps_two_immutable_events() -> anyhow::Result<()> {
    let postgres = start_postgres().await?;
    let mut cluster = wamn_postgres::connect(postgres.admin_url()).await?;
    sqlx::raw_sql("CREATE ROLE wamn_apply NOLOGIN NOINHERIT")
        .execute(&mut cluster)
        .await?;
    let url = wamn_test_infrastructure::identity::system(&postgres).await;
    let mut admin = wamn_postgres::connect(url.as_str()).await?;
    sqlx::raw_sql(include_str!("../../../../deploy/sql/apply-grants.sql"))
        .execute(&mut admin)
        .await?;
    let org = "journal".parse()?;
    let logins = create_org_logins(&mut admin, &org, SYSTEM_DATABASE).await?;
    let login_url = |secret| {
        let login = logins
            .iter()
            .find(|login| login.secret == secret)
            .context("org login")?;
        postgres.database_url(SYSTEM_DATABASE, login.login.as_str(), &login.password)
    };
    let mut control = wamn_postgres::connect(login_url(CONTROL_SECRET)?.as_str()).await?;
    let mut worker = wamn_postgres::connect(login_url(APPLY_SECRET)?.as_str()).await?;
    let actor = PlatformComponent::Provisioning.principal_id();
    let mut registration =
        wamn_postgres::provisioning_transaction::provisioning_transaction(&mut admin).await?;
    sqlx::query("INSERT INTO registry.projects (org, id) VALUES ($1, 'stock')")
        .bind(org.as_str())
        .execute(&mut *registration)
        .await?;
    registration.commit().await?;

    let command = command(
        "environment.configure",
        actor,
        r#"{"env":"dev","project":"stock"}"#,
    )
    .expect("environment configure is journaled");
    let job = ("worker-namespace", "op-fixture");
    record(&mut control, &org, &command, Outcome::Started, None).await?;
    record(
        &mut worker,
        &org,
        &command,
        Outcome::Failed("The lifecycle operation Job failed."),
        Some(job),
    )
    .await?;
    record(
        &mut worker,
        &org,
        &command,
        Outcome::Failed("A retried result."),
        Some(job),
    )
    .await?;

    let events = sqlx::query_as::<
        _,
        (
            String,
            String,
            uuid::Uuid,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        ),
    >(
        "SELECT event_type, outcome, principal_id, org, env, project, job_namespace, job_name, failure_summary
         FROM provisioning.platform_event WHERE command_id = $1 ORDER BY id",
    )
    .bind(command.id)
    .fetch_all(&mut admin)
    .await?;
    let [start, end] = events.as_slice() else {
        panic!("expected the control start and one Job result, got {events:?}");
    };
    for row in [start, end] {
        assert_eq!(row.0, "environment_apply");
        assert_eq!(row.2, uuid::Uuid::from(actor));
        assert_eq!(row.3, org.as_str());
        assert_eq!(row.4, None, "the environment is not registered");
        assert_eq!(row.5.as_deref(), Some("stock"));
    }
    assert_eq!(
        (
            &*start.1,
            start.6.as_deref(),
            start.7.as_deref(),
            start.8.as_deref()
        ),
        ("started", None, None, None)
    );
    assert_eq!(
        (
            &*end.1,
            end.6.as_deref(),
            end.7.as_deref(),
            end.8.as_deref()
        ),
        (
            "failed",
            Some(job.0),
            Some(job.1),
            Some("The lifecycle operation Job failed.")
        )
    );

    for statement in [
        "UPDATE provisioning.platform_event SET outcome = 'refused' WHERE command_id = $1",
        "DELETE FROM provisioning.platform_event WHERE command_id = $1",
    ] {
        let error = sqlx::query(statement)
            .bind(command.id)
            .execute(&mut admin)
            .await
            .expect_err("the journal refuses row changes even through its administrator");
        assert_eq!(
            error
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .as_deref(),
            Some("55000")
        );
    }
    Ok(())
}
