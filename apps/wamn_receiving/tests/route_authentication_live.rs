//! Receiving application scenarios over real platform adapters.

#[path = "receiving_history/database.rs"]
mod database;

mod environment;
#[path = "../../client_acme_receiving/tests/overlay_compatibility.rs"]
mod overlay_compatibility;
mod package_copies;
mod routes;

include!("route_authentication_fixture.rs");
