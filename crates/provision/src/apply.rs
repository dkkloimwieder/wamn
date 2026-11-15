//! The environment apply login and its system database grants.

/// The fixed login used outside the privileged provisioning phase.
pub const APPLY_ROLE: &str = "wamn_apply";

/// Create the apply login without system-owner membership.
pub fn ensure_apply_role_sql() -> &'static str {
    "GRANT wamn_db_owner TO wamn_apply WITH ADMIN FALSE, INHERIT TRUE, SET TRUE;"
}

/// Exact privileges used by the apply phases on the system database.
pub const APPLY_GRANTS_SQL: &str = include_str!("../../../deploy/sql/apply-grants.sql");

#[cfg(test)]
mod tests {
    use super::{APPLY_GRANTS_SQL, ensure_apply_role_sql};

    #[sqlx::test(migrations = false)]
    async fn apply_cannot_write_connection_authority_or_assume_the_system_owner(
        pool: sqlx::PgPool,
    ) {
        let _lock = wamn_test_postgres::lock();
        let database = wamn_test_postgres::url(&pool);
        crate::test_database::system(database.as_str()).await;
        {
            let batches: &[&str] = &[
                "CREATE ROLE wamn_apply LOGIN INHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS",
                ensure_apply_role_sql(),
                &crate::sql::ensure_platform_group_role_sql(),
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
}
