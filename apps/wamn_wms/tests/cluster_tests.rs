//! WMS cases over Kubernetes.
mod business_fixture;
#[path = "cluster.rs"]
mod cluster;
mod delivery;
#[path = "environment_fixture.rs"]
mod environment;
mod startup_support;
#[path = "wms_runtime_live_fixture.rs"]
#[expect(
    dead_code,
    reason = "This target shares fixture code with the local cases."
)]
mod wms_runtime_live;
