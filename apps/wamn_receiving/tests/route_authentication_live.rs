//! Receiving application scenarios over real platform adapters.

#[path = "receiving_command_histories_live.rs"]
mod command_histories;

mod delivery;
mod dev;
mod environment;
mod local_business;
mod materializer;
#[path = "../../client_acme_receiving/tests/overlay_compatibility.rs"]
mod overlay_compatibility;
mod package_copies;
#[path = "postcommit.rs"]
mod postcommit;
mod queue_recovery;
mod routes;

mod case_execution;
mod cluster_checks;

include!("route_authentication_fixture.rs");
