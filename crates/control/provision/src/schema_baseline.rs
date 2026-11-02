//! Content digests of the full schemas applied to fresh databases.

use wamn_execution_contract::bytes_sha256;

/// Hash the system schema and portable store in bootstrap order.
pub fn system_sha256() -> String {
    bytes_sha256(crate::CONTROL_BOOTSTRAP_SQL.concat().as_bytes())
}

/// Hash the catalog and application schemas in installation order.
pub fn project_sha256() -> String {
    bytes_sha256(
        [wamn_catalog::CATALOG_SCHEMA_SQL, crate::APP_SCHEMA_SQL]
            .concat()
            .as_bytes(),
    )
}
