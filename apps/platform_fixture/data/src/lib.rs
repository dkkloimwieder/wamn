//! Typed platform fixture accessors over the frozen `wamn:postgres` capability.
//!
//! Runtime access uses the generated statement accessors of the fixture
//! package, and this crate authors no SQL. The generated operations are
//! generated whole, in [`generated`]. The custom operations are authored here.

mod error;
mod scalar;
mod statements;
pub mod widget;
pub mod widget_maker;

pub use error::{AccessError, AccessErrorType};

/// Count the records present when a sequence-only command runs.
///
/// # Errors
///
/// Returns the statement's database refusal.
pub async fn count_widgets(
    transaction: &mut wamn_postgres_statements::Transaction,
) -> Result<i64, wamn_postgres_statements::StatementError> {
    statements::wamn::number_issue::count_widgets(transaction)
        .await
        .map(|row| row.records.expect("COUNT returns a non-null value"))
}

/// The generated operations, one module for each model, and their refusal.
pub mod generated {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/wamn/platform_fixture/data/mod.rs"
    ));
}
