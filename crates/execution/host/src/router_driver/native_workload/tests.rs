//! Loader admission tests use scalar guests because they do not dispatch a node.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use wamn_catalog::{
    AdmittedComponent, AdmittedComponentOperation, ComponentOperationDependency,
    ComponentPackageScope,
};
use wash_runtime::observability::{MeterKind, Meters};
use wash_runtime::plugin::PluginBindings;
use wash_runtime::types::LocalResources;
use wash_runtime::wit::WitInterface;

use super::{NativeComponent, NativeWorkload, NativeWorkloadSpec, load_native_workload};

const HANDLER: &str = "wamn:node/handler@0.1.0";
const OPERATION: &str = "palette:entry/run@1.0.0";
const PARENT: &str = "caller:entry/run@1.0.0";

fn component(name: &str, export: &str, import: Option<&str>, value: u32) -> NativeComponent {
    let import_wat = import.map_or_else(String::new, |name| {
        format!(r#"(import "{name}" (instance (export "run" (func (result u32)))))"#)
    });
    let bytes = wat::parse_str(format!(
        r#"(component
          {import_wat}
          (core module $module
            (func (export "run") (result i32) i32.const {value}))
          (core instance $instance (instantiate $module))
          (func $run (result u32) (canon lift (core func $instance "run")))
          (instance $exports (export "run" (func $run)))
          (export "{export}" (instance $exports)))"#
    ))
    .expect("valid scalar loader fixture");
    let imports: Vec<String> = import.into_iter().map(str::to_owned).collect();
    let imports_fingerprint = wamn_execution_contract::canonical_json_sha256(
        &serde_json::to_value(&imports).expect("import inventory serializes"),
    );
    NativeComponent {
        fact: AdmittedComponent {
            scope: ComponentPackageScope {
                tenant_id: "tenant-a".into(),
                package_id: export.split(':').next().expect("package namespace").into(),
                package_version: "1.0.0".into(),
            },
            component: name.into(),
            interface_version: "1.0.0".into(),
            operations: BTreeMap::from([(
                export.into(),
                AdmittedComponentOperation {
                    registered_operation: None,
                    fresh_only: false,
                    committed_result_schema: None,
                    dependencies: Vec::new(),
                    input_ports: Vec::new(),
                    output_ports: Vec::new(),
                    parameters: Vec::new(),
                    statements: BTreeMap::new(),
                },
            )]),
            component_digest: wamn_runtime::component_admission::component_digest(&bytes),
            imports,
            imports_fingerprint,
            effects: Vec::new(),
        },
        bytes,
    }
}

async fn load(components: Vec<NativeComponent>) -> anyhow::Result<NativeWorkload> {
    load_native_workload(
        Arc::new(wamn_runtime::build_engine(&[]).expect("production engine")),
        NativeWorkloadSpec {
            id: "native-import-admission-proof".into(),
            namespace: "proof".into(),
            name: "native-import-admission-proof".into(),
            components,
            local_resources: LocalResources::default(),
            // Host policy implements nested calls. Declaring an interface here
            // must not bypass the admitted provider uniqueness check.
            host_interfaces: vec![WitInterface::from(OPERATION)],
        },
        &HashMap::default(),
        &PluginBindings::new(),
        &Meters::new(MeterKind::Off),
    )
    .await
}

#[tokio::test]
async fn native_loader_allows_repeated_export_only_handlers() {
    let first = component("transform", HANDLER, None, 37);
    let repeat = NativeComponent {
        fact: first.fact.clone(),
        bytes: first.bytes.clone(),
    };
    let second = component("http-request", HANDLER, None, 37);
    assert_eq!(first.fact.component_digest, second.fact.component_digest);
    let workload = load(vec![first, repeat, second])
        .await
        .expect("shared handler exports retain separate admitted identities");
    let names: std::collections::BTreeSet<_> = workload
        .facts_by_component_id
        .values()
        .map(|fact| fact.component.as_str())
        .collect();
    assert_eq!(
        names,
        std::collections::BTreeSet::from(["http-request", "transform"])
    );
    workload
        .resolved
        .unbind_all_plugins()
        .await
        .expect("unbind");
}

#[tokio::test]
async fn native_loader_refuses_ambiguous_imported_operation() {
    for second_value in [37, 91] {
        let first = component("selected", OPERATION, None, 37);
        let second = component("other", OPERATION, None, second_value);
        let mut caller = component("caller", PARENT, Some(OPERATION), 0);
        caller
            .fact
            .operations
            .get_mut(PARENT)
            .expect("caller operation")
            .dependencies
            .push(ComponentOperationDependency {
                package: first.fact.scope.package_id.clone(),
                version: first.fact.scope.package_version.clone(),
                digest: first.fact.component_digest.clone(),
                operation: OPERATION.into(),
            });
        let first_digest = first.fact.component_digest.clone();
        let second_digest = second.fact.component_digest.clone();
        let error = load(vec![first, second, caller])
            .await
            .expect_err("exact dependency selection cannot hide ambiguous imported providers");
        let detail = format!("{error:#}");
        assert!(detail.contains("ambiguous component providers"), "{detail}");
        for identity in [
            OPERATION,
            "selected",
            "other",
            &first_digest,
            &second_digest,
        ] {
            assert!(detail.contains(identity), "{identity}: {detail}");
        }
    }
}

#[tokio::test]
async fn native_loader_checks_repeated_bytes_before_cache_access() {
    let first = component("transform", HANDLER, None, 37);
    let altered = NativeComponent {
        fact: first.fact.clone(),
        bytes: component("transform", HANDLER, None, 91).bytes,
    };
    let error = load(vec![first, altered])
        .await
        .expect_err("altered repeat refuses");
    assert!(format!("{error:#}").contains("native-component-bytes-digest-mismatch"));
}
