//! Pure component-library facts stored after byte admission.

use std::collections::BTreeSet;
use std::fmt;

use boon::{Compiler, Draft, Schemas};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const JSON_SCHEMA_2020_12: &str = "https://json-schema.org/draft/2020-12/schema";
const SCHEMA_URI: &str = "mem://wamn-component-schema.json";

/// Catalog coordinate that owns an admitted component.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ComponentCatalogScope {
    pub tenant_id: String,
    pub catalog_id: String,
    pub catalog_version: u32,
}

/// One author-declared input or output port before admission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ComponentPortDeclaration {
    pub name: String,
    pub schema: Value,
}

/// One author-declared component parameter before admission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ComponentParameterDeclaration {
    pub name: String,
    pub schema: Value,
    pub required: bool,
}

/// The closed set of connection types an admitted component may require.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ComponentConnectionType {
    Http,
}

/// Every connection type. A new variant does not compile until it is listed.
const CONNECTION_TYPES: [ComponentConnectionType; 1] = [ComponentConnectionType::Http];

impl ComponentConnectionType {
    /// Exact WIT `namespace:package` whose import this connection type needs.
    pub fn import_package(self) -> &'static str {
        match self {
            Self::Http => "wamn:connection",
        }
    }
}

/// One portable connection the component reaches under an author-chosen alias.
///
/// The alias is the author-owned half: it is the `requirement` string the guest
/// names at the `wamn:connection` boundary, and the coordinate an environment
/// binds to a concrete instance. Connection SEMANTICS are platform-owned and
/// selected whole by `requirement-type`, never authored field by field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ComponentConnection {
    pub store_alias: String,
    pub requirement_type: ComponentConnectionType,
}

/// Component-owned facts presented with exact component bytes for admission.
///
/// The operation is singular by construction. A digest therefore cannot hide
/// several logical operations behind the uniform `wamn:node/handler.run` ABI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ComponentDeclaration {
    pub scope: ComponentCatalogScope,
    pub component: String,
    pub interface_version: String,
    pub operation: String,
    pub input_ports: Vec<ComponentPortDeclaration>,
    pub output_ports: Vec<ComponentPortDeclaration>,
    pub parameters: Vec<ComponentParameterDeclaration>,
    pub connections: Vec<ComponentConnection>,
}

/// Canonical JSON Schema document and its exact RFC 8785 digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ComponentSchema {
    pub schema: Value,
    pub schema_digest: String,
}

/// One normalized admitted input or output port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct AdmittedComponentPort {
    pub name: String,
    pub schema: ComponentSchema,
}

/// One normalized admitted component parameter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct AdmittedComponentParameter {
    pub name: String,
    pub schema: ComponentSchema,
    pub required: bool,
}

/// One authority leaving the host, proved by the component's audited imports.
///
/// Effects are a projection of `imports`, never a second declaration: an author
/// cannot claim fewer effects than the bytes import, and an empty list is the
/// positive statement that the occurrence is pure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct AdmittedComponentEffect {
    pub package: String,
    pub interfaces: Vec<String>,
}

/// Complete component-owned fact persisted in `catalog.component_library`.
///
/// Environment is deliberately absent: an environment selects a wiring, while
/// component admission is owned by a catalog version. `admitted_at` is likewise
/// a storage timestamp, not validator output, so admission stays clock-free.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct AdmittedComponent {
    pub scope: ComponentCatalogScope,
    pub component: String,
    pub interface_version: String,
    pub operation: String,
    pub component_digest: String,
    pub imports: Vec<String>,
    pub imports_fingerprint: String,
    pub effects: Vec<AdmittedComponentEffect>,
    pub input_ports: Vec<AdmittedComponentPort>,
    pub output_ports: Vec<AdmittedComponentPort>,
    pub parameters: Vec<AdmittedComponentParameter>,
}

/// Everything one byte-verified component admission mints.
///
/// The two halves land in two relations — the library row and the portable
/// connection requirements keyed by `(component-digest, store-alias)` — so they
/// are returned together rather than persisted from two independent decisions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedComponentFacts {
    pub component: AdmittedComponent,
    pub connections: Vec<ComponentConnection>,
}

/// Stable classification for component-fact normalization refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentFactErrorKind {
    EmptyIdentity,
    NonCanonicalIdentity,
    ZeroCatalogVersion,
    InvalidComponentDigest,
    DuplicateInputPort,
    DuplicateOutputPort,
    DuplicateParameter,
    InvalidSchema,
    RemoteSchemaReference,
    UnimportedEffect,
    DuplicateConnection,
    UnimportedConnection,
    UndeclaredConnection,
}

/// Refusal to mint a complete normalized component-library fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentFactError {
    kind: ComponentFactErrorKind,
    detail: Box<str>,
}

impl ComponentFactError {
    /// Stable refusal class for callers that must not match display text.
    pub fn kind(&self) -> ComponentFactErrorKind {
        self.kind
    }

    fn new(kind: ComponentFactErrorKind, detail: impl Into<Box<str>>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for ComponentFactError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for ComponentFactError {}

/// Normalize one byte-verified component declaration for catalog persistence.
///
/// `imports` and `effects` are both derived from the exact bytes by the caller
/// that audited them; this function re-checks that every effect interface is
/// one of those imports, and that declared connections and effect-bearing
/// imports account for each other in both directions.
pub fn normalize_component_fact(
    declaration: ComponentDeclaration,
    component_digest: String,
    imports: impl IntoIterator<Item = String>,
    effects: Vec<AdmittedComponentEffect>,
) -> Result<AdmittedComponentFacts, ComponentFactError> {
    validate_identity(&declaration.scope.tenant_id, "tenant-id")?;
    validate_identity(&declaration.scope.catalog_id, "catalog-id")?;
    if declaration.scope.catalog_version == 0 {
        return Err(ComponentFactError::new(
            ComponentFactErrorKind::ZeroCatalogVersion,
            "catalog-version must be greater than zero",
        ));
    }
    validate_identity(&declaration.component, "component")?;
    validate_identity(&declaration.interface_version, "interface-version")?;
    validate_identity(&declaration.operation, "operation")?;
    if !valid_digest(&component_digest) {
        return Err(ComponentFactError::new(
            ComponentFactErrorKind::InvalidComponentDigest,
            "component-digest must be sha256:<64 lowercase hex digits>",
        ));
    }

    let input_ports = normalize_ports(
        declaration.input_ports,
        ComponentFactErrorKind::DuplicateInputPort,
        "input-port",
    )?;
    let output_ports = normalize_ports(
        declaration.output_ports,
        ComponentFactErrorKind::DuplicateOutputPort,
        "output-port",
    )?;
    let parameters = normalize_parameters(declaration.parameters)?;

    let mut imports: Vec<_> = imports.into_iter().collect();
    imports.sort();
    imports.dedup();
    let imports_fingerprint = wamn_execution_contract::canonical_json_sha256(
        &serde_json::to_value(&imports).expect("a string list serializes"),
    );
    let effects = normalize_effects(effects, &imports)?;
    let connections = normalize_connections(declaration.connections, &effects)?;

    Ok(AdmittedComponentFacts {
        component: AdmittedComponent {
            scope: declaration.scope,
            component: declaration.component,
            interface_version: declaration.interface_version,
            operation: declaration.operation,
            component_digest,
            imports,
            imports_fingerprint,
            effects,
            input_ports,
            output_ports,
            parameters,
        },
        connections,
    })
}

/// MVP wiring compatibility: exact canonical schema-digest equality only.
///
/// This intentionally makes no structural-subtyping promise. Relaxing exact
/// equality later is compatible; callers cannot depend on an unshipped
/// widening today.
pub fn schema_digests_match(left: &ComponentSchema, right: &ComponentSchema) -> bool {
    left.schema_digest == right.schema_digest
}

/// Sort and deduplicate derived effects, refusing any not backed by an import.
fn normalize_effects(
    effects: Vec<AdmittedComponentEffect>,
    imports: &[String],
) -> Result<Vec<AdmittedComponentEffect>, ComponentFactError> {
    let mut normalized = Vec::with_capacity(effects.len());
    for effect in effects {
        validate_identity(&effect.package, "effect-package")?;
        let mut interfaces = effect.interfaces;
        interfaces.sort();
        interfaces.dedup();
        for interface in &interfaces {
            if !imports.iter().any(|import| import == interface) {
                return Err(ComponentFactError::new(
                    ComponentFactErrorKind::UnimportedEffect,
                    format!("effect interface {interface:?} is not an audited import"),
                ));
            }
        }
        normalized.push(AdmittedComponentEffect {
            package: effect.package,
            interfaces,
        });
    }
    normalized.sort_by(|left, right| left.package.cmp(&right.package));
    normalized.dedup_by(|left, right| left.package == right.package);
    Ok(normalized)
}

/// Normalize declared connections against the effects the bytes actually prove.
fn normalize_connections(
    declarations: Vec<ComponentConnection>,
    effects: &[AdmittedComponentEffect],
) -> Result<Vec<ComponentConnection>, ComponentFactError> {
    let mut seen = BTreeSet::new();
    let mut connections = Vec::with_capacity(declarations.len());
    for declaration in declarations {
        validate_identity(&declaration.store_alias, "store-alias")?;
        if !seen.insert(declaration.store_alias.clone()) {
            return Err(ComponentFactError::new(
                ComponentFactErrorKind::DuplicateConnection,
                format!("store-alias {:?} is duplicated", declaration.store_alias),
            ));
        }
        let package = declaration.requirement_type.import_package();
        if !effects.iter().any(|effect| effect.package == package) {
            return Err(ComponentFactError::new(
                ComponentFactErrorKind::UnimportedConnection,
                format!(
                    "store-alias {:?} requires package {package:?}, which these bytes do not import",
                    declaration.store_alias
                ),
            ));
        }
        connections.push(declaration);
    }
    connections.sort_by(|left, right| left.store_alias.cmp(&right.store_alias));

    // The reverse direction: connection authority nothing binds is authority
    // the environment can never satisfy, so it is refused at admission rather
    // than surfacing as an unresolvable effect at delivery time.
    for connection_type in CONNECTION_TYPES {
        let package = connection_type.import_package();
        if effects.iter().any(|effect| effect.package == package)
            && !connections
                .iter()
                .any(|connection| connection.requirement_type == connection_type)
        {
            return Err(ComponentFactError::new(
                ComponentFactErrorKind::UndeclaredConnection,
                format!("package {package:?} is imported but no store-alias declares it"),
            ));
        }
    }
    Ok(connections)
}

fn normalize_ports(
    declarations: Vec<ComponentPortDeclaration>,
    duplicate_kind: ComponentFactErrorKind,
    field: &'static str,
) -> Result<Vec<AdmittedComponentPort>, ComponentFactError> {
    let mut seen = BTreeSet::new();
    let mut ports = Vec::with_capacity(declarations.len());
    for declaration in declarations {
        validate_identity(&declaration.name, field)?;
        if !seen.insert(declaration.name.clone()) {
            return Err(ComponentFactError::new(
                duplicate_kind,
                format!("{field} {:?} is duplicated", declaration.name),
            ));
        }
        ports.push(AdmittedComponentPort {
            name: declaration.name,
            schema: normalize_schema(declaration.schema, field)?,
        });
    }
    ports.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(ports)
}

fn normalize_parameters(
    declarations: Vec<ComponentParameterDeclaration>,
) -> Result<Vec<AdmittedComponentParameter>, ComponentFactError> {
    let mut seen = BTreeSet::new();
    let mut parameters = Vec::with_capacity(declarations.len());
    for declaration in declarations {
        validate_identity(&declaration.name, "parameter")?;
        if !seen.insert(declaration.name.clone()) {
            return Err(ComponentFactError::new(
                ComponentFactErrorKind::DuplicateParameter,
                format!("parameter {:?} is duplicated", declaration.name),
            ));
        }
        parameters.push(AdmittedComponentParameter {
            name: declaration.name,
            schema: normalize_schema(declaration.schema, "parameter")?,
            required: declaration.required,
        });
    }
    parameters.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(parameters)
}

fn normalize_schema(schema: Value, field: &str) -> Result<ComponentSchema, ComponentFactError> {
    if let Some(declared) = schema.get("$schema")
        && declared.as_str() != Some(JSON_SCHEMA_2020_12)
    {
        return Err(ComponentFactError::new(
            ComponentFactErrorKind::InvalidSchema,
            format!("{field} schema must use JSON Schema draft 2020-12"),
        ));
    }
    if has_remote_ref(&schema) {
        return Err(ComponentFactError::new(
            ComponentFactErrorKind::RemoteSchemaReference,
            format!("{field} schema may only use document-local references"),
        ));
    }

    let mut compiler = Compiler::new();
    compiler.set_default_draft(Draft::V2020_12);
    compiler
        .add_resource(SCHEMA_URI, schema.clone())
        .map_err(|error| {
            ComponentFactError::new(
                ComponentFactErrorKind::InvalidSchema,
                format!("{field} schema is invalid: {error}"),
            )
        })?;
    let mut schemas = Schemas::new();
    compiler
        .compile(SCHEMA_URI, &mut schemas)
        .map_err(|error| {
            ComponentFactError::new(
                ComponentFactErrorKind::InvalidSchema,
                format!("{field} schema is invalid: {error}"),
            )
        })?;

    let schema_digest = wamn_execution_contract::canonical_json_sha256(&schema);
    Ok(ComponentSchema {
        schema,
        schema_digest,
    })
}

fn has_remote_ref(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(has_remote_ref),
        Value::Object(values) => values.iter().any(|(key, value)| {
            (key == "$ref"
                && value
                    .as_str()
                    .is_none_or(|reference| !reference.starts_with('#')))
                || has_remote_ref(value)
        }),
        _ => false,
    }
}

fn validate_identity(value: &str, field: &str) -> Result<(), ComponentFactError> {
    if value.is_empty() {
        return Err(ComponentFactError::new(
            ComponentFactErrorKind::EmptyIdentity,
            format!("{field} is empty"),
        ));
    }
    if value.trim() != value || value.as_bytes().contains(&0) {
        return Err(ComponentFactError::new(
            ComponentFactErrorKind::NonCanonicalIdentity,
            format!("{field} is not in canonical form"),
        ));
    }
    Ok(())
}

fn valid_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn declaration() -> ComponentDeclaration {
        ComponentDeclaration {
            scope: ComponentCatalogScope {
                tenant_id: "tenant-a".to_string(),
                catalog_id: "orders".to_string(),
                catalog_version: 7,
            },
            component: "transform".to_string(),
            interface_version: "0.1.0".to_string(),
            operation: "map".to_string(),
            input_ports: vec![ComponentPortDeclaration {
                name: "input".to_string(),
                schema: json!({
                    "$schema": JSON_SCHEMA_2020_12,
                    "type": "object",
                    "required": ["id"],
                    "properties": {"id": {"type": "string"}}
                }),
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
        }
    }

    fn postgres_effect() -> AdmittedComponentEffect {
        AdmittedComponentEffect {
            package: "wamn:postgres".to_string(),
            interfaces: vec!["wamn:postgres/client@0.1.0".to_string()],
        }
    }

    #[test]
    fn normalization_is_sorted_and_carries_every_component_owned_fact() {
        let facts = normalize_component_fact(
            declaration(),
            format!("sha256:{}", "a".repeat(64)),
            [
                "wasi:io/streams@0.2.3".to_string(),
                "wamn:postgres/client@0.1.0".to_string(),
            ],
            vec![postgres_effect()],
        )
        .expect("component fact normalizes");
        let fact = facts.component;

        assert_eq!(fact.scope.catalog_version, 7);
        assert_eq!(fact.operation, "map");
        assert_eq!(fact.input_ports[0].name, "input");
        assert_eq!(fact.output_ports[0].name, "main");
        assert!(fact.parameters[0].required);
        assert_eq!(fact.component_digest, format!("sha256:{}", "a".repeat(64)));
        assert_eq!(fact.imports.len(), 2);
        assert!(fact.imports_fingerprint.starts_with("sha256:"));
        assert_eq!(fact.effects, vec![postgres_effect()]);
        assert!(facts.connections.is_empty());
    }

    /// A component with no effect-bearing import records the POSITIVE fact that
    /// it is pure, rather than an absent one — this is what a caller reads to
    /// decide an occurrence writes no effect-ledger row.
    #[test]
    fn a_component_importing_no_authority_admits_as_pure() {
        let facts = normalize_component_fact(
            declaration(),
            format!("sha256:{}", "a".repeat(64)),
            ["wasi:clocks/monotonic-clock@0.2.3".to_string()],
            Vec::new(),
        )
        .expect("a pure component normalizes");

        assert!(facts.component.effects.is_empty());
        assert_eq!(facts.component.imports.len(), 1);
    }

    #[test]
    fn connections_and_effect_imports_must_account_for_each_other() {
        let http_effect = AdmittedComponentEffect {
            package: "wamn:connection".to_string(),
            interfaces: vec!["wamn:connection/http@0.1.0".to_string()],
        };
        let imports = ["wamn:connection/http@0.1.0".to_string()];
        let connection = ComponentConnection {
            store_alias: "erp".to_string(),
            requirement_type: ComponentConnectionType::Http,
        };

        let mut declared = declaration();
        declared.connections = vec![connection.clone()];
        let facts = normalize_component_fact(
            declared.clone(),
            format!("sha256:{}", "a".repeat(64)),
            imports.clone(),
            vec![http_effect.clone()],
        )
        .expect("a declared connection backed by its import admits");
        assert_eq!(facts.connections, vec![connection.clone()]);

        // Imported authority nothing declares.
        let mut undeclared = declaration();
        undeclared.connections = Vec::new();
        assert_eq!(
            normalize_component_fact(
                undeclared,
                format!("sha256:{}", "b".repeat(64)),
                imports.clone(),
                vec![http_effect],
            )
            .unwrap_err()
            .kind(),
            ComponentFactErrorKind::UndeclaredConnection
        );

        // Declared authority the bytes never import.
        assert_eq!(
            normalize_component_fact(
                declared.clone(),
                format!("sha256:{}", "c".repeat(64)),
                Vec::new(),
                Vec::new(),
            )
            .unwrap_err()
            .kind(),
            ComponentFactErrorKind::UnimportedConnection
        );

        let mut duplicate = declared;
        duplicate.connections.push(connection);
        assert_eq!(
            normalize_component_fact(
                duplicate,
                format!("sha256:{}", "d".repeat(64)),
                imports,
                vec![AdmittedComponentEffect {
                    package: "wamn:connection".to_string(),
                    interfaces: vec!["wamn:connection/http@0.1.0".to_string()],
                }],
            )
            .unwrap_err()
            .kind(),
            ComponentFactErrorKind::DuplicateConnection
        );
    }

    /// Effects are a projection of the audited imports and can never widen
    /// beyond them, whatever the caller that derived them passes in.
    #[test]
    fn an_effect_interface_absent_from_the_audited_imports_refuses() {
        assert_eq!(
            normalize_component_fact(
                declaration(),
                format!("sha256:{}", "a".repeat(64)),
                ["wasi:io/streams@0.2.3".to_string()],
                vec![postgres_effect()],
            )
            .unwrap_err()
            .kind(),
            ComponentFactErrorKind::UnimportedEffect
        );
    }

    #[test]
    fn declaration_document_is_exact_kebab_case_json() {
        let document = serde_json::to_value(declaration()).expect("declaration serializes");
        assert!(document.get("input-ports").is_some());
        assert!(document.get("interface-version").is_some());
        assert_eq!(document["scope"]["tenant-id"], "tenant-a");
        assert_eq!(
            serde_json::from_value::<ComponentDeclaration>(document.clone())
                .expect("exact declaration parses"),
            declaration()
        );

        let mut unknown = document;
        unknown
            .as_object_mut()
            .expect("declaration is an object")
            .insert("environment".to_owned(), json!("dev"));
        assert!(serde_json::from_value::<ComponentDeclaration>(unknown).is_err());
    }

    #[test]
    fn schema_identity_is_rfc_8785_and_compatibility_is_exact() {
        let mut left = declaration();
        let mut right = declaration();
        left.input_ports[0].schema =
            json!({"type":"object","properties":{"b":{"type":"number"},"a":{"type":"string"}}});
        right.input_ports[0].schema =
            json!({"properties":{"a":{"type":"string"},"b":{"type":"number"}},"type":"object"});

        let left = normalize_component_fact(
            left,
            format!("sha256:{}", "b".repeat(64)),
            Vec::new(),
            Vec::new(),
        )
        .unwrap()
        .component;
        let right = normalize_component_fact(
            right,
            format!("sha256:{}", "c".repeat(64)),
            Vec::new(),
            Vec::new(),
        )
        .unwrap()
        .component;
        assert!(schema_digests_match(
            &left.input_ports[0].schema,
            &right.input_ports[0].schema
        ));

        let different = normalize_schema(json!({"type": "string"}), "input-port").unwrap();
        assert!(!schema_digests_match(
            &left.input_ports[0].schema,
            &different
        ));
    }

    #[test]
    fn partial_or_ambiguous_component_facts_refuse() {
        let mut duplicate = declaration();
        duplicate.input_ports.push(duplicate.input_ports[0].clone());
        assert_eq!(
            normalize_component_fact(
                duplicate,
                format!("sha256:{}", "d".repeat(64)),
                Vec::new(),
                Vec::new(),
            )
            .unwrap_err()
            .kind(),
            ComponentFactErrorKind::DuplicateInputPort
        );

        let mut remote = declaration();
        remote.parameters[0].schema = json!({"$ref": "https://example.invalid/schema"});
        assert_eq!(
            normalize_component_fact(
                remote,
                format!("sha256:{}", "e".repeat(64)),
                Vec::new(),
                Vec::new(),
            )
            .unwrap_err()
            .kind(),
            ComponentFactErrorKind::RemoteSchemaReference
        );
    }
}
