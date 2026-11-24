#![cfg(feature = "ops")]
//! Application mutation privileges require row security in each database.

use sqlx::PgConnection as Client;
use sqlx::Row as _;
use wamn_provision::sql;

#[tokio::test]
#[ignore = "requires: docker"]
async fn application_mutations_require_row_security_in_each_database() {
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
            wamn_provision::WorkloadRoleFamily::ALL
                .into_iter()
                .filter(|family| {
                    matches!(
                        family,
                        wamn_provision::WorkloadRoleFamily::App
                            | wamn_provision::WorkloadRoleFamily::Query
                    )
                })
                .map(wamn_provision::WorkloadRoleFamily::acl_role)
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

    let postgres = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    let migration = postgres.create_project_database("project").await.unwrap();
    postgres.create_system_role().await.unwrap();
    let system = postgres.create_database("wamn_system").await.unwrap();
    let mut client = wamn_postgres::connect(system.as_str()).await.unwrap();
    wamn_provision::provision_system::install_control_store(&mut client)
        .await
        .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(sql::ensure_app_acl_role_sql()))
        .execute(&mut client)
        .await
        .unwrap();
    assert_author_sql_boundaries(&mut client).await;
    let mut migrator = wamn_postgres::connect(migration.as_str()).await.unwrap();
    wamn_postgres::environment_schema::install_project(&mut migrator)
        .await
        .expect("install canonical project schemas through reconciler");
    let mut project = url::Url::parse(postgres.admin_url()).unwrap();
    project.set_path("project");
    let mut client = wamn_postgres::connect(project.as_str()).await.unwrap();
    assert_author_sql_boundaries(&mut client).await;
}
