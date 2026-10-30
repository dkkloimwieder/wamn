//! Host storage constraints and release record history on PostgreSQL.

#[test]
fn host_storage_and_release_history() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_project_state::test_database::tenant_platform();
    database
        .execute(&[include_str!("fixtures/connections.sql")])
        .unwrap();
}
