//! Receiving application scenarios over real platform adapters.

#[path = "receiving_history/database.rs"]
mod database;

mod dev;
mod environment;
mod local_business;
mod package_copies;
mod queue_recovery;
mod routes;

include!("route_authentication_fixture.rs");
