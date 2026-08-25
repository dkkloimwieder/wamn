//! Pure byte admission for tenant component-library entries.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use sha2::{Digest as _, Sha256};
use wamn_catalog::{
    AdmittedComponentEffect, AdmittedComponentFacts, ComponentDeclaration, normalize_component_fact,
};
use wash_runtime::engine::Engine;
use wash_runtime::wasmtime::component::Component;

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
        ComponentCatalogScope, ComponentParameterDeclaration, ComponentPortDeclaration,
    };

    use super::*;

    fn request() -> ComponentAdmissionRequest {
        ComponentAdmissionRequest {
            declaration: ComponentDeclaration {
                scope: ComponentCatalogScope {
                    tenant_id: "tenant-a".to_string(),
                    catalog_id: "orders".to_string(),
                    catalog_version: 4,
                },
                component: "transform".to_string(),
                interface_version: "0.1.0".to_string(),
                operation: "map".to_string(),
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
                connections: Vec::new(),
            },
            admitted_platform_packages: BTreeSet::new(),
        }
    }

    #[test]
    fn exact_bytes_mint_digest_and_normalized_fact_without_io() {
        let engine = crate::build_engine(&[]).expect("engine builds");
        let bytes = wat::parse_str("(component)").expect("fixture component encodes");

        let admitted = validate_component_admission(&engine, &bytes, request())
            .expect("empty-import component admits")
            .component;

        assert_eq!(admitted.component_digest, component_digest(&bytes));
        assert_eq!(admitted.operation, "map");
        assert!(admitted.imports.is_empty());
        assert_eq!(admitted.input_ports[0].name, "input");
        assert!(admitted.parameters[0].required);

        let projected = crate::wiring_lowering::project_component_operation(&admitted);
        assert_eq!(projected.component, admitted.component);
        assert_eq!(projected.interface_version, admitted.interface_version);
        assert_eq!(projected.operation, admitted.operation);
        assert_eq!(projected.component_digest, admitted.component_digest);
        assert_eq!(projected.input_ports, BTreeSet::from(["input".to_string()]));
        assert_eq!(projected.output_ports, BTreeSet::from(["main".to_string()]));
        assert!(projected.parameters["mapping"].required);
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
        let bytes = wat::parse_str(
            r#"(component
                (type $socket (instance))
                (import "wasi:sockets/tcp@0.2.3" (instance (type $socket)))
            )"#,
        )
        .expect("fixture component encodes");

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
        let bytes = wat::parse_str(
            r#"(component
                (type $empty (instance))
                (import "wasi:clocks/monotonic-clock@0.2.3" (instance (type $empty)))
                (import "wamn:node/types@0.1.0" (instance (type $empty)))
                (import "wamn:postgres/client@0.1.0" (instance (type $empty)))
                (import "wamn:connection/http@0.1.0" (instance (type $empty)))
            )"#,
        )
        .expect("fixture component encodes");
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
        let bytes = wat::parse_str(
            r#"(component
                (type $empty (instance))
                (import "wamn:connection/http@0.1.0" (instance (type $empty)))
            )"#,
        )
        .expect("fixture component encodes");
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
