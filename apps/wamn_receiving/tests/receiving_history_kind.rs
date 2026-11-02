//! Business model and database helpers for the deployed cases.
#[path = "receiving_history/database.rs"]
pub(crate) mod database;
#[path = "receiving_history/model_fixture.rs"]
mod model;
include!("receiving_command_histories_fixture.rs");
