pub(super) mod plan;
pub(crate) use plan::published_components;
pub use plan::{
    Role, RoleName, TELEMETRY_VARIABLES, ValuesInput, component_name, host_interfaces,
    release_name, roles, values, workload_name,
};

use anyhow::ensure;
use serde_yaml::{Mapping, Value};

#[doc(inline)]
pub use wamn_platform_install::release_chart::bundle;
