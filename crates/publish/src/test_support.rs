//! Fixture input for publish unit tests.

use std::collections::BTreeMap;
use wamn_component_policy::{
    AdmittedComponent, AdmittedComponentOperation, ComponentPackageCoordinate,
};

pub const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

pub fn closure_component(
    component: &str,
    registered_operation: Option<&wamn_package::registered_operation::RegisteredOperation>,
) -> AdmittedComponent {
    let operation = registered_operation.map_or(
        "run",
        wamn_package::registered_operation::RegisteredOperation::as_str,
    );
    AdmittedComponent {
        data_change: false,
        target: ComponentPackageCoordinate {
            project_env_key: "org-a--dev--fixture"
                .parse()
                .expect("valid fixture coordinate"),
            package_id: "base".parse().unwrap(),
            package_version: "1.0.0".parse().unwrap(),
        },
        component: component.parse().unwrap(),
        interface_version: "0.1.0".parse().expect("fixed identifier"),
        operations: BTreeMap::from([(
            operation.parse().unwrap(),
            AdmittedComponentOperation {
                committed_result_schema: None,
                registered_operation: registered_operation.cloned(),
                host_transaction: false,
                input_ports: Vec::new(),
                output_ports: Vec::new(),
                parameters: Vec::new(),
            },
        )]),
        component_digest: DIGEST.parse().unwrap(),
        imports: Vec::new(),
        imports_fingerprint: DIGEST.parse().unwrap(),
        effects: Vec::new(),
    }
}
