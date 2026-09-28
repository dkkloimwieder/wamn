// @generated from the package manifest and migration IR; do not edit.

// The generated `sample` operations.

#[allow(unused_imports)]
use wamn_data_access::{Direction, Invalid, Page, cursor, scalar};
use wamn_postgres_statements::Connection;

#[allow(unused_imports)]
use super::error::{Constraints, Error};

/// The statement accessors of the model.
pub mod sql {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/wamn/sample.rs"
    ));
}

pub use sql::SampleRow;

const GET_OPERATION: &str = "sample.get";

/// `sample.get`: load one row by id.
///
/// # Errors
///
/// [`Error`] carrying the literal the operation contract declares.
pub async fn get(connection: &mut Connection, id: &str) -> Result<SampleRow, Error> {
    let id = scalar::uuid(id).map_err(|Invalid| Error::invalid("id"))?;
    sql::get(connection, id.clone())
        .await
        .map_err(|error| Error::from_statement(&error, &Constraints::NONE, GET_OPERATION))?
        .ok_or_else(|| Error::not_found(&id))
}
