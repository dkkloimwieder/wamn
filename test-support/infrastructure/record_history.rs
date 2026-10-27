//! Assertions for stamps written through production database writers.

use tokio_postgres::GenericClient;

/// Assert all four fields on the selected rows, and require at least one row.
///
/// `relation` and `predicate` are SQL supplied by the test, never user input.
pub async fn assert_stamp(
    client: &(impl GenericClient + Sync),
    relation: &str,
    predicate: &str,
    created_by: &str,
    updated_by: &str,
) {
    let rows = client
        .query(
            &format!(
                "SELECT created_by::text, updated_by::text, \
                 isfinite(created_at) AND isfinite(updated_at) \
                   AND created_at <= updated_at AND updated_at <= clock_timestamp() \
                 FROM {relation} WHERE {predicate}"
            ),
            &[],
        )
        .await
        .expect("read the production writer's four stamp fields");
    assert!(!rows.is_empty(), "{relation}: no stamped rows");
    for row in rows {
        assert_eq!(
            row.get::<_, String>(0),
            created_by,
            "{relation}: created_by"
        );
        assert_eq!(
            row.get::<_, String>(1),
            updated_by,
            "{relation}: updated_by"
        );
        assert!(row.get::<_, bool>(2), "{relation}: invalid stamp times");
    }
}
