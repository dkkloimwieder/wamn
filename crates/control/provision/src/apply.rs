//! The environment apply login and its system database grants.

/// The fixed login used outside the privileged provisioning phase.
pub const APPLY_ROLE: &str = "wamn_apply";

/// Create the apply login without system-owner membership.
pub fn ensure_apply_role_sql() -> &'static str {
    "GRANT wamn_db_owner TO wamn_apply WITH ADMIN FALSE, INHERIT TRUE, SET TRUE;"
}

/// Exact privileges used by the apply phases on the system database.
pub const APPLY_GRANTS_SQL: &str = include_str!("../../../../deploy/sql/apply-grants.sql");

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
                r"
        DO $denials$ DECLARE relation text; privilege text; BEGIN
            ASSERT (SELECT rolcanlogin AND NOT rolsuper AND NOT rolcreatedb AND NOT rolcreaterole AND NOT rolreplication AND NOT rolbypassrls FROM pg_roles WHERE rolname='wamn_apply');
            ASSERT pg_has_role('wamn_apply', 'wamn_db_owner', 'MEMBER');
            ASSERT NOT pg_has_role('wamn_apply', 'wamn_system', 'MEMBER');
            FOREACH relation IN ARRAY ARRAY[
                'registry.connection', 'registry.integration', 'registry.connection_generation',
                'identity.credential', 'identity.oauth_token', 'identity.oauth_authorization', 'identity.pats'
            ] LOOP
                FOREACH privilege IN ARRAY ARRAY['INSERT','UPDATE','DELETE','TRUNCATE','REFERENCES','TRIGGER'] LOOP
                    ASSERT NOT has_table_privilege('wamn_apply', relation, privilege), relation || ':' || privilege;
                END LOOP;
                ASSERT NOT has_any_column_privilege('wamn_apply', relation, 'INSERT,UPDATE,REFERENCES'), relation;
            END LOOP;
        END $denials$;
        SET SESSION AUTHORIZATION wamn_apply;
        DO $role_denial$ BEGIN
            BEGIN
                SET ROLE wamn_system;
                RAISE EXCEPTION 'apply assumed the system owner';
            EXCEPTION WHEN insufficient_privilege THEN NULL; END;
            ASSERT current_user = 'wamn_apply';
        END $role_denial$;
        SELECT registry.connection_projection('absent','absent','dev');
        SELECT principal_id FROM identity.org_memberships ORDER BY principal_id FOR UPDATE;
        SELECT * FROM registry.resolve_environment_connection('absent','absent','dev','cloud','absent',1);
        RESET SESSION AUTHORIZATION;
    ",
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
