//! Guest SQL gets no grant on the credential tables of a project database.

use wamn_identity_protocol::workload_role::WorkloadRoleFamily;
use wamn_test_infrastructure::database;

const CREDENTIAL_TABLES: [&str; 3] = [
    "platform.credential",
    "platform.oauth_token",
    "platform.oauth_authorization",
];

#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn guest_sql_logins_have_no_grant_on_credential_tables() -> anyhow::Result<()> {
    let project = database::project().await?;
    let mut admin = project.admin().await?;
    for family in [WorkloadRoleFamily::App, WorkloadRoleFamily::Query] {
        let login = family.login_name();
        // Positive control: the same check sees the login's identity grant.
        let binds: bool = sqlx::query_scalar(
            "SELECT has_table_privilege($1::text::name, 'platform.tx_identity', 'INSERT')",
        )
        .bind(login)
        .fetch_one(&mut admin)
        .await?;
        assert!(binds, "{login} cannot bind its transaction identity");
        for table in CREDENTIAL_TABLES {
            let granted: bool = sqlx::query_scalar(
                "SELECT has_table_privilege($1::text::name, $2::text::regclass, \
                     'SELECT,INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER') \
                     OR has_any_column_privilege($1::text::name, $2::text::regclass, 'SELECT,INSERT,UPDATE,REFERENCES')",
            )
            .bind(login)
            .bind(table)
            .fetch_one(&mut admin)
            .await?;
            assert!(!granted, "{login} has a grant on {table}");
        }
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn only_the_platform_login_can_read_credential_history() -> anyhow::Result<()> {
    const READ: &str = "SELECT * FROM platform.credential_history LIMIT 1";

    let project = database::project().await?;
    let login = WorkloadRoleFamily::Platform.login_name();
    let mut platform = wamn_postgres::connect(project.login_url(login)?.as_str()).await?;
    sqlx::query(READ).fetch_optional(&mut platform).await?;

    for family in [WorkloadRoleFamily::App, WorkloadRoleFamily::Query] {
        let login = family.login_name();
        let mut guest = wamn_postgres::connect(project.login_url(login)?.as_str()).await?;
        let error = sqlx::query(READ)
            .fetch_optional(&mut guest)
            .await
            .expect_err("guest login can read credential history");
        assert_eq!(
            error
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .as_deref(),
            Some("42501"),
            "{login}: {error}"
        );
    }
    Ok(())
}
