//! Pure byte admission for tenant component-library entries.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use sha2::{Digest as _, Sha256};
use wamn_catalog::{
    AdmittedComponentEffect, AdmittedComponentFacts, ComponentDeclaration, normalize_component_fact,
};
use wash_runtime::engine::Engine;
use wash_runtime::wasmtime::component::Component;
use wash_runtime::wasmtime::component::types::ComponentItem;

mod node_contract {
    wash_runtime::wasmtime::component::bindgen!({
        path: "../../execution/router/wit",
        world: "node",
        exports: { default: async },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

use node_contract::wamn::node::types as node_types;

const HANDLER_SIGNATURE: &str =
    "wamn:node/handler@0.1.0::run(node-context, string) -> result<emission, node-error>";

/// Component declaration plus its exact admitted platform capabilities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentAdmissionRequest {
    pub declaration: ComponentDeclaration,
    pub admitted_platform_packages: BTreeSet<String>,
}

/// Stable classification for a refused component admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentAdmissionErrorKind {
    InvalidComponentBytes,
    ImportPolicyRefused,
    OperationExportMismatch,
    OperationSignatureMismatch,
    InvalidComponentFacts,
}

/// Contextual refusal from the byte-to-catalog-fact boundary.
#[derive(Debug)]
pub struct ComponentAdmissionError {
    kind: ComponentAdmissionErrorKind,
    component: Box<str>,
    source: anyhow::Error,
}

impl ComponentAdmissionError {
    /// Stable refusal class for callers that must not match display text.
    pub fn kind(&self) -> ComponentAdmissionErrorKind {
        self.kind
    }

    fn new(
        kind: ComponentAdmissionErrorKind,
        component: &str,
        source: impl Into<anyhow::Error>,
    ) -> Self {
        Self {
            kind,
            component: component.into(),
            source: source.into(),
        }
    }
}

impl fmt::Display for ComponentAdmissionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "component {:?} admission refused: {}",
            self.component, self.source
        )
    }
}

impl std::error::Error for ComponentAdmissionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

/// Validate exact component bytes and mint their complete catalog facts.
///
/// This function performs no network, storage, clock, or publication work. The
/// caller supplies the configured validation engine, exact bytes, declaration,
/// and the closed platform-capability set. Publication is the separate owner of
/// persisting a successful result — both halves of it: the library fact and the
/// portable connection requirements the declaration's aliases mint.
pub fn validate_component_admission(
    engine: &Engine,
    component_bytes: &[u8],
    request: ComponentAdmissionRequest,
) -> Result<AdmittedComponentFacts, ComponentAdmissionError> {
    let component_name = request.declaration.component.clone();
    let component = Component::new(engine.inner(), component_bytes).map_err(|source| {
        ComponentAdmissionError::new(
            ComponentAdmissionErrorKind::InvalidComponentBytes,
            &component_name,
            source,
        )
    })?;
    let raw = component.engine();
    let component_type = component.component_type();
    let declared_exports: BTreeSet<_> = request.declaration.operations.keys().cloned().collect();
    let byte_exports: BTreeSet<_> = component_type
        .exports(raw)
        .map(|(name, _)| name.to_owned())
        .collect();
    if declared_exports != byte_exports {
        let missing: Vec<_> = declared_exports
            .difference(&byte_exports)
            .cloned()
            .collect();
        let extra: Vec<_> = byte_exports
            .difference(&declared_exports)
            .cloned()
            .collect();
        return Err(ComponentAdmissionError::new(
            ComponentAdmissionErrorKind::OperationExportMismatch,
            &component_name,
            anyhow::anyhow!(
                "declared handler exports differ from component bytes: missing={missing:?}, extra={extra:?}"
            ),
        ));
    }
    for export in &declared_exports {
        let item = component_type
            .get_export(raw, export)
            .expect("equal declaration and byte export sets contain every operation");
        let ComponentItem::ComponentInstance(instance) = item.ty else {
            return Err(operation_signature_mismatch(
                &component_name,
                export,
                "export is not an interface instance",
            ));
        };
        let Some(run) = instance.get_export(raw, "run") else {
            return Err(operation_signature_mismatch(
                &component_name,
                export,
                "interface does not export run",
            ));
        };
        let ComponentItem::ComponentFunc(run) = run.ty else {
            return Err(operation_signature_mismatch(
                &component_name,
                export,
                "interface member run is not a component function",
            ));
        };
        if run.async_() {
            return Err(operation_signature_mismatch(
                &component_name,
                export,
                "run uses the async ABI",
            ));
        }
        if let Err(error) = run.typecheck::<
            (&node_types::NodeContext, &str),
            (Result<node_types::Emission, node_types::NodeError>,),
        >(&component_type.instance_type())
        {
            return Err(operation_signature_mismatch(
                &component_name,
                export,
                error,
            ));
        }
    }
    let imports = wamn_component_policy::ComponentImports::new(
        component_type
            .imports(raw)
            .map(|(name, _)| name.to_string()),
    );
    wamn_component_policy::analyze_tenant(
        &imports,
        &request.admitted_platform_packages,
        &component_name,
    )
    .map_err(|source| {
        ComponentAdmissionError::new(
            ComponentAdmissionErrorKind::ImportPolicyRefused,
            &component_name,
            source,
        )
    })?;

    let component_digest = component_digest(component_bytes);
    normalize_component_fact(
        request.declaration,
        component_digest,
        imports.iter().map(str::to_owned),
        derive_effects(&imports),
    )
    .map_err(|source| {
        ComponentAdmissionError::new(
            ComponentAdmissionErrorKind::InvalidComponentFacts,
            &component_name,
            source,
        )
    })
}

fn operation_signature_mismatch(
    component: &str,
    export: &str,
    detail: impl fmt::Display,
) -> ComponentAdmissionError {
    ComponentAdmissionError::new(
        ComponentAdmissionErrorKind::OperationSignatureMismatch,
        component,
        anyhow::anyhow!("operation export {export:?} does not match {HANDLER_SIGNATURE}: {detail}"),
    )
}

/// Group the audited imports into the authority packages that leave the host.
///
/// Called only after [`wamn_component_policy::analyze_tenant`] has accepted the
/// inventory, so every package here is either authority-free or an admitted
/// platform capability, and the classification is a total function of the
/// bytes — an author declares no part of it.
fn derive_effects(
    imports: &wamn_component_policy::ComponentImports,
) -> Vec<AdmittedComponentEffect> {
    let mut grouped: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for name in imports.iter() {
        let package = wamn_component_policy::import_pkg(name);
        if wamn_component_policy::is_effect_package(package) {
            grouped.entry(package).or_default().insert(name.to_owned());
        }
    }
    grouped
        .into_iter()
        .map(|(package, interfaces)| AdmittedComponentEffect {
            package: package.to_owned(),
            interfaces: interfaces.into_iter().collect(),
        })
        .collect()
}

/// SHA-256 identity of exact component bytes.
pub fn component_digest(component_bytes: &[u8]) -> String {
    let digest = Sha256::digest(component_bytes);
    let mut output = String::with_capacity("sha256:".len() + digest.len() * 2);
    output.push_str("sha256:");
    for byte in digest {
        use fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("writing to a string is infallible");
    }
    output
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wamn_catalog::{
        ComponentOperationDeclaration, ComponentPackageScope, ComponentParameterDeclaration,
        ComponentPortDeclaration,
    };
    use wit_component::{ComponentEncoder, StringEncoding, dummy_module, embed_component_metadata};
    use wit_parser::{ManglingAndAbi, Resolve};

    use super::*;

    const OPERATION: &str = "wamn:node/handler@0.1.0";

    const DEPENDENCY_WITS: [(&str, &str); 4] = [
        (
            "wasi-clocks.wit",
            "package wasi:clocks@0.2.3; interface monotonic-clock {}",
        ),
        (
            "wasi-sockets.wit",
            "package wasi:sockets@0.2.3; interface tcp {}",
        ),
        (
            "wamn-connection.wit",
            "package wamn:connection@0.1.0; interface http {}",
        ),
        (
            "wamn-postgres.wit",
            "package wamn:postgres@0.1.0; interface client {}",
        ),
    ];

    fn component_bytes(imports: &str) -> Vec<u8> {
        let mut resolve = Resolve::new();
        resolve
            .push_str(
                "wamn-node.wit",
                include_str!("../../../execution/router/wit/package.wit"),
            )
            .expect("the live node WIT parses");
        for (name, wit) in DEPENDENCY_WITS {
            resolve
                .push_str(name, wit)
                .expect("fixture dependency parses");
        }
        let fixture = format!(
            "package test:component@1.0.0; world fixture {{ {imports} export wamn:node/handler@0.1.0; }}"
        );
        let package = resolve
            .push_str("fixture.wit", &fixture)
            .expect("fixture world parses");
        let world = resolve
            .select_world(&[package], Some("fixture"))
            .expect("fixture world resolves");
        let mut module = dummy_module(&resolve, world, ManglingAndAbi::Standard32);
        embed_component_metadata(&mut module, &resolve, world, StringEncoding::UTF8)
            .expect("fixture component metadata embeds");
        ComponentEncoder::default()
            .module(&module)
            .expect("fixture core module is accepted")
            .validate(true)
            .encode()
            .expect("fixture component encodes")
    }

    fn request() -> ComponentAdmissionRequest {
        ComponentAdmissionRequest {
            declaration: ComponentDeclaration {
                scope: ComponentPackageScope {
                    tenant_id: "tenant-a".to_string(),
                    package_id: "orders".to_string(),
                    package_version: "1.0.0".to_string(),
                },
                component: "transform".to_string(),
                interface_version: "0.1.0".to_string(),
                operations: BTreeMap::from([(
                    OPERATION.to_string(),
                    ComponentOperationDeclaration {
                        registered_operation: None,
                        input_ports: vec![ComponentPortDeclaration {
                            name: "input".to_string(),
                            schema: json!({"type": "object"}),
                        }],
                        output_ports: vec![ComponentPortDeclaration {
                            name: "main".to_string(),
                            schema: json!({"type": "object"}),
                        }],
                        parameters: vec![ComponentParameterDeclaration {
                            name: "mapping".to_string(),
                            schema: json!({"type": "object"}),
                            required: true,
                        }],
                    },
                )]),
                connections: Vec::new(),
            },
            admitted_platform_packages: BTreeSet::new(),
        }
    }

    #[test]
    fn exact_bytes_mint_digest_and_normalized_fact_without_io() {
        let engine = crate::build_engine(&[]).expect("engine builds");
        let bytes = component_bytes("");

        let admitted = validate_component_admission(&engine, &bytes, request())
            .expect("empty-import component admits")
            .component;

        assert_eq!(admitted.component_digest, component_digest(&bytes));
        assert!(admitted.imports.is_empty());
        let operation = admitted.operation(OPERATION).expect("operation admits");
        assert_eq!(operation.input_ports[0].name, "input");
        assert!(operation.parameters[0].required);

        let projected = crate::wiring_lowering::project_component_operations(&admitted);
        let projected = projected.first().expect("one operation projects");
        assert_eq!(projected.component, admitted.component);
        assert_eq!(projected.interface_version, admitted.interface_version);
        assert_eq!(projected.operation, OPERATION);
        assert_eq!(projected.component_digest, admitted.component_digest);
        assert_eq!(projected.input_ports, BTreeSet::from(["input".to_string()]));
        assert_eq!(projected.output_ports, BTreeSet::from(["main".to_string()]));
        assert!(projected.parameters["mapping"].required);
    }

    #[test]
    fn declaration_and_byte_handler_export_sets_must_match_exactly() {
        let engine = crate::build_engine(&[]).expect("engine builds");
        let missing = wat::parse_str("(component)").expect("empty component encodes");
        let error = validate_component_admission(&engine, &missing, request())
            .expect_err("missing declared handler export refuses");
        assert_eq!(
            error.kind(),
            ComponentAdmissionErrorKind::OperationExportMismatch
        );
        assert!(error.to_string().contains("missing="));

        let extra = wat::parse_str(format!(
            r#"(component
                (instance $first)
                (instance $second)
                (export "{OPERATION}" (instance $first))
                (export "orders:purchase-order/query@1.0.0" (instance $second))
            )"#
        ))
        .expect("extra-export component encodes");
        let error = validate_component_admission(&engine, &extra, request())
            .expect_err("undeclared handler export refuses");
        assert_eq!(
            error.kind(),
            ComponentAdmissionErrorKind::OperationExportMismatch
        );
        assert!(error.to_string().contains("extra="));
    }

    #[test]
    fn operation_export_must_have_the_live_handler_signature() {
        let engine = crate::build_engine(&[]).expect("engine builds");
        let wrong = wat::parse_str(format!(
            r#"(component
                (instance $wrong)
                (export "{OPERATION}" (instance $wrong))
            )"#
        ))
        .expect("wrong-signature component encodes");
        let error = validate_component_admission(&engine, &wrong, request())
            .expect_err("an export without the live handler signature refuses");
        assert_eq!(
            error.kind(),
            ComponentAdmissionErrorKind::OperationSignatureMismatch
        );
        assert!(error.to_string().contains(OPERATION));
        assert!(error.to_string().contains(HANDLER_SIGNATURE));
    }

    #[test]
    fn malformed_component_bytes_refuse_before_facts_are_minted() {
        let engine = crate::build_engine(&[]).expect("engine builds");
        let error = validate_component_admission(&engine, b"not-wasm", request())
            .expect_err("malformed bytes must refuse");
        assert_eq!(
            error.kind(),
            ComponentAdmissionErrorKind::InvalidComponentBytes
        );
    }

    #[test]
    fn actual_unadmitted_component_import_refuses() {
        let engine = crate::build_engine(&[]).expect("engine builds");
        let bytes = component_bytes("import wasi:sockets/tcp@0.2.3;");

        let error = validate_component_admission(&engine, &bytes, request())
            .expect_err("socket import must refuse");
        assert_eq!(
            error.kind(),
            ComponentAdmissionErrorKind::ImportPolicyRefused
        );
    }

    /// The three classes in one inventory: an authority-free WASI package, the
    /// router's own invocation seam, and two real effect packages. Only the
    /// last two are recorded, grouped by package with their exact interfaces.
    #[test]
    fn effects_record_only_the_imports_that_leave_the_host() {
        let engine = crate::build_engine(&[]).expect("engine builds");
        let bytes = component_bytes(
            "import wasi:clocks/monotonic-clock@0.2.3; \
             import wamn:node/types@0.1.0; \
             import wamn:postgres/client@0.1.0; \
             import wamn:connection/http@0.1.0;",
        );
        let mut request = request();
        request.admitted_platform_packages = BTreeSet::from([
            "wamn:node".to_string(),
            "wamn:postgres".to_string(),
            "wamn:connection".to_string(),
        ]);
        request.declaration.connections = vec![wamn_catalog::ComponentConnection {
            store_alias: "erp".to_string(),
            requirement_type: wamn_catalog::ComponentConnectionType::Http,
        }];

        let facts = validate_component_admission(&engine, &bytes, request)
            .expect("an effectful component with a declared connection admits");

        assert_eq!(
            facts.component.effects,
            vec![
                AdmittedComponentEffect {
                    package: "wamn:connection".to_string(),
                    interfaces: vec!["wamn:connection/http@0.1.0".to_string()],
                },
                AdmittedComponentEffect {
                    package: "wamn:postgres".to_string(),
                    interfaces: vec!["wamn:postgres/client@0.1.0".to_string()],
                },
            ]
        );
        assert_eq!(facts.component.imports.len(), 4);
        assert_eq!(facts.connections.len(), 1);
        assert_eq!(facts.connections[0].store_alias, "erp");
    }

    /// Connection authority the environment could never bind is refused at
    /// admission, not discovered when a delivery reaches the effect.
    #[test]
    fn connection_authority_without_a_declared_alias_refuses() {
        let engine = crate::build_engine(&[]).expect("engine builds");
        let bytes = component_bytes("import wamn:connection/http@0.1.0;");
        let mut request = request();
        request.admitted_platform_packages = BTreeSet::from(["wamn:connection".to_string()]);

        let error = validate_component_admission(&engine, &bytes, request)
            .expect_err("undeclared connection authority must refuse");
        assert_eq!(
            error.kind(),
            ComponentAdmissionErrorKind::InvalidComponentFacts
        );
    }
}
