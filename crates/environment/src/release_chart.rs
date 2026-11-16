pub(super) mod plan;
pub use plan::{
    Role, RoleName, TELEMETRY_VARIABLES, ValuesInput, release_name, roles, values, workload_name,
};

use anyhow::ensure;
use serde_yaml::{Mapping, Value};
use wamn_catalog::{AttachmentType, ServingManifest};

#[cfg(test)]
mod tests {

    use super::*;

    const DIGEST: &str = "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    fn input() -> ValuesInput {
        ValuesInput {
            org: "acme".parse().unwrap(),
            project: "wms".parse().unwrap(),
            env: "prod".parse().unwrap(),
            manifest_digest: DIGEST.parse().unwrap(),
            route_host: "wms.acme.example".into(),
            applications: [(
                "wms".parse().unwrap(),
                wamn_catalog::ApplicationDefinition {
                    connections: std::collections::BTreeMap::new(),
                    packages: ["wamn_wms".parse().unwrap()].into(),
                },
            )]
            .into(),
            roles: vec![Role {
                name: RoleName::Http,
                config: Mapping::new(),
            }],
            actor: "ops".into(),
        }
    }

    #[test]
    fn values_require_one_application() {
        let mut input = input();
        let (name, application) = input.applications.pop_first().unwrap();
        assert!(values(&input).is_err());
        input.applications.insert(name, application.clone());
        input
            .applications
            .insert("another".parse().unwrap(), application);
        assert!(values(&input).is_err());
    }
}
