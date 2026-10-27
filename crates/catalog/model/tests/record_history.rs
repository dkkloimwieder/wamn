//! Catalog stamps use the same actor contract as application records.

#[test]
fn catalog_stamps_require_an_actor_and_preserve_creation() {
    let database = wamn_catalog::test_database::tenant();
    let principals = format!(
        "BEGIN; {} COMMIT;",
        wamn_control_provision::platform_principals_sql("example.invalid").unwrap()
    );
    let mut batches = vec![
        include_str!("../../../../deploy/sql/app-schema.sql"),
        &principals,
    ];
    batches.extend(
        include_str!("fixtures/catalog_record_history.sql").split("-- transaction boundary"),
    );
    database.execute(&batches).unwrap();
}
