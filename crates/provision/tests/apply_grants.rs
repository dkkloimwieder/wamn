//! The apply login cannot change connection authority or assume the system owner.

use wamn_provision::apply::{APPLY_GRANTS_SQL, ensure_apply_role_sql};

#[sqlx::test(migrations = false)]
async fn apply_cannot_write_connection_authority_or_assume_the_system_owner(pool: sqlx::PgPool) {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::url(&pool);
    wamn_provision::test_database::system(database.as_str()).await;
    {
        let batches: &[&str] = &[
            "CREATE ROLE wamn_apply LOGIN INHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS",
            ensure_apply_role_sql(),
            &wamn_provision::sql::ensure_platform_group_role_sql(),
            "GRANT wamn_platform TO wamn_db_owner WITH INHERIT TRUE, SET FALSE",
            APPLY_GRANTS_SQL,
            wamn_registry::test_util::apply_authority_sql(),
        ];
        let mut setup_connection = wamn_postgres::connect(database.as_str()).await.unwrap();
        for batch in batches {
            sqlx::raw_sql(sqlx::AssertSqlSafe(*batch))
                .execute(&mut setup_connection)
                .await
                .unwrap();
        }
    }
}
