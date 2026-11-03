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

pub use error::AccessError;

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
#[expect(
    unused_imports,
    reason = "generated data exposes row types for every declared operation"
)]
#[expect(
    clippy::enum_variant_names,
    reason = "generated refusal names match the closed wire vocabulary"
)]
#[expect(
    clippy::option_option,
    reason = "generated updates distinguish an omitted field from an explicit null"
)]
#[expect(
    clippy::struct_field_names,
    reason = "generated row fields retain their authored projection names"
)]
pub mod generated {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/wamn/platform_fixture/data/mod.rs"
    ));
}
