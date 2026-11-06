//! Receiving application scenarios over real platform adapters.

#[path = "receiving_command_histories_live.rs"]
mod command_histories;

mod dev;
mod environment;
mod local_business;
#[path = "../../client_acme_receiving/tests/overlay_compatibility.rs"]
mod overlay_compatibility;
mod package_copies;
mod queue_recovery;
mod routes;

include!("route_authentication_fixture.rs");
