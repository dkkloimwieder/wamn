//! Guest SQL gets no grant on the credential tables of a project database.

use wamn_provision::WorkloadRoleFamily;
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
