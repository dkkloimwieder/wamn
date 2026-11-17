pub(super) mod plan;
pub use plan::{
    Role, RoleName, TELEMETRY_VARIABLES, ValuesInput, component_name, host_interfaces,
    release_name, roles, values, workload_name,
};

use anyhow::ensure;
use serde_yaml::{Mapping, Value};
use wamn_catalog::ServingManifest;
