//! Receiving fixtures for the deployed cases.
#[path = "route_authentication_live/case_execution.rs"]
mod case_execution;
#[path = "route_authentication_live/cluster.rs"]
mod cluster;
#[path = "receiving_history_kind.rs"]
mod command_histories;
#[path = "route_authentication_live/delivery.rs"]
mod delivery;
#[path = "route_authentication_live/environment_fixture.rs"]
mod environment;
#[path = "route_authentication_live/materializer.rs"]
mod materializer;
#[path = "../../client_acme_receiving/tests/overlay_compatibility_fixture.rs"]
mod overlay_compatibility;
#[path = "postcommit.rs"]
mod postcommit;
#[path = "route_authentication_live/routes.rs"]
mod routes;
include!("route_authentication_fixture.rs");
