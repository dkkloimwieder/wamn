#![cfg(feature = "ops")]
//! Application mutation privileges require row security in each database.

use sqlx::PgConnection as Client;
use sqlx::Row as _;
use wamn_provision::sql;

#[sqlx::test(migrations = false)]
async fn application_mutations_require_row_security_in_each_database(pool: sqlx::PgPool) {
    async fn assert_author_sql_boundaries(client: &mut Client) {
        let rows = sqlx::query(
            "WITH mutation_roles AS ( \
                   SELECT c.oid AS relation, c.relowner AS role_oid \
                   FROM pg_catalog.pg_class c WHERE c.relkind IN ('r','p') \
                   UNION \
                   SELECT c.oid, acl.grantee FROM pg_catalog.pg_class c \
                   CROSS JOIN LATERAL pg_catalog.aclexplode( \
                       COALESCE(c.relacl, pg_catalog.acldefault('r', c.relowner))) acl \
                   WHERE c.relkind IN ('r','p') \
                     AND acl.privilege_type IN ('INSERT','UPDATE','DELETE','TRUNCATE') \
                   UNION \
                   SELECT c.oid, acl.grantee FROM pg_catalog.pg_class c \
                   JOIN pg_catalog.pg_attribute att ON att.attrelid=c.oid \
                     AND att.attnum > 0 AND NOT att.attisdropped AND att.attacl IS NOT NULL \
                   CROSS JOIN LATERAL pg_catalog.aclexplode(att.attacl) acl \
                   WHERE c.relkind IN ('r','p') AND acl.privilege_type IN ('INSERT','UPDATE') \
                 ) \
                 SELECT n.nspname || '.' || c.relname, granted_role.rolname, \
                        c.relrowsecurity AND c.relforcerowsecurity \
                        AND EXISTS (SELECT FROM pg_catalog.pg_policy p WHERE p.polrelid=c.oid) \
                 FROM mutation_roles mutation \
                 JOIN pg_catalog.pg_class c ON c.oid=mutation.relation \
                 JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace \
                 JOIN pg_catalog.pg_roles granted_role ON granted_role.oid=mutation.role_oid \
                 WHERE granted_role.rolname::text = ANY($1) ORDER BY 1,2",
        )
        .bind(
            &wamn_provision::WorkloadRoleFamily::ALL
                .into_iter()
                .filter(|family| {
                    matches!(
                        family,
                        wamn_provision::WorkloadRoleFamily::App
                            | wamn_provision::WorkloadRoleFamily::Query
                    )
                })
                .map(|family| family.acl_role())
                .collect::<Vec<_>>(),
        )
        .fetch_all(&mut *client)
        .await
        .expect("read author SQL mutation privileges");
        for row in rows {
            let relation: String = row.get(0);
            let forced_row_security: bool = row.get(2);
            // The write log has no project column and no row policy: the database is
            // the project (docs/architecture/data-access.md#the-write-log). Its
            // grant is the claim insert and the result column alone.
            assert!(
                forced_row_security
                    || relation == "platform.write_log"
                    || relation == "platform.tx_identity",
                "{relation} grants author SQL mutation without forced row security"
            );
        }
    }

    let _lock = wamn_test_postgres::lock();
    let url = wamn_test_postgres::url(&pool);
    wamn_provision::test_database::system(url.as_str()).await;
    let mut client = wamn_postgres::connect(url.as_str()).await.unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(sql::ensure_app_acl_role_sql()))
        .execute(&mut client)
        .await
        .unwrap();
    assert_author_sql_boundaries(&mut client).await;
    let database: String = sqlx::query("SELECT current_database()")
        .fetch_one(&mut client)
        .await
        .expect("read the disposable project database name")
        .get(0);
    sqlx::raw_sql(sqlx::AssertSqlSafe(sql::set_database_owner_sql(&database)))
        .execute(&mut client)
        .await
        .map(drop)
        .expect("assign the canonical project database owner before package installation");
    sqlx::raw_sql(
        "DROP SCHEMA IF EXISTS platform CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DROP SCHEMA IF EXISTS wamn_run CASCADE; \
             DROP SCHEMA IF EXISTS wamn_authority CASCADE; \
             DROP SCHEMA IF EXISTS wamn_history CASCADE",
    )
    .execute(&mut client)
    .await
    .map(drop)
    .expect("remove control schemas before the fresh project install");
    wamn_environment::platform_migrations::install_project(&mut client)
        .await
        .expect("install canonical project schemas through reconciler");
    assert_author_sql_boundaries(&mut client).await;
}
