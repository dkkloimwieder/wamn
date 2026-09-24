//! Pure byte admission for tenant component-library entries.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use anyhow::Context as _;
use sha2::{Digest as _, Sha256};
use wamn_catalog::{
    AdmittedComponentEffect, AdmittedComponentFacts, ComponentDeclaration,
    ComponentEffectProvenance, normalize_component_fact,
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

/// The async contract for generic nodes. Application operations also support async.
pub const ASYNC_HANDLER_OPERATION: &str = "wamn:node/async-handler@0.1.0";
const HANDLER_SIGNATURE: &str =
    "wamn:node/handler@0.1.0::run(node-context, string) -> result<emission, node-error>";

/// Component declaration plus its exact admitted platform capabilities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentAdmissionRequest {
    pub declaration: ComponentDeclaration,
    pub admitted_platform_packages: BTreeSet<String>,
    /// The declared operation dependencies the caller confirmed as effect-free,
    /// named by their exact operation import.
    ///
    /// A component's effect posture is its own imports union the posture of
    /// the operations it declares a dependency on. A dependency absent from
    /// this set carries an effect into the component that declares it.
    /// Admission holds one component's bytes and reads no dependency closure,
    /// so absence is a refusal, exactly as an unregistered import is.
    pub effect_free_operation_dependencies: BTreeSet<String>,
}

/// Stable classification for a refused component admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentAdmissionErrorKind {
    InvalidComponentBytes,
    ImportPolicyRefused,
    OperationExportMismatch,
    OperationDependencyMismatch,
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
    // Bindgen can omit an unused imported JSON adapter. An exported dynamic entry
    // still supplies the checked context and node-error types for a typed import.
    let dynamic_signature = component_type.exports(raw).find_map(|(_, item)| {
        let ComponentItem::ComponentInstance(instance) = item.ty else {
            return None;
        };
        let entry = instance
            .get_export(raw, "run-json")
            .or_else(|| instance.get_export(raw, "run"))?;
        let ComponentItem::ComponentFunc(entry) = entry.ty else {
            return None;
        };
        entry
            .typecheck::<
                (&node_types::NodeContext, &str),
                (Result<node_types::Emission, node_types::NodeError>,),
            >(&component_type.instance_type())
            .ok()
            .map(|()| entry)
    });
    let validate_handler_signature =
        |export: &str, item: ComponentItem, dependency: bool| -> anyhow::Result<()> {
            let ComponentItem::ComponentInstance(instance) = item else {
                anyhow::bail!("item is not an interface instance");
            };
            let Some(run) = instance.get_export(raw, "run") else {
                anyhow::bail!("interface does not export run");
            };
            let ComponentItem::ComponentFunc(run) = run.ty else {
                anyhow::bail!("interface member run is not a component function");
            };
            let adapter = instance.get_export(raw, "run-json");
            let typed_input = run.params().nth(1).is_some_and(|(_, ty)| {
                matches!(
                    ty,
                    wash_runtime::wasmtime::component::Type::List(_)
                        | wash_runtime::wasmtime::component::Type::Record(_)
                )
            });
            if adapter.is_some() || (dependency && typed_input) {
                let adapter = if let Some(adapter) = adapter {
                    let ComponentItem::ComponentFunc(adapter) = adapter.ty else {
                        anyhow::bail!("run-json is not a component function");
                    };
                    anyhow::ensure!(adapter.async_(), "typed JSON adapters must be async");
                    adapter
                } else {
                    dynamic_signature
                        .clone()
                        .context("typed import requires a checked dynamic entry")?
                };
                adapter.typecheck::<
                (&node_types::NodeContext, &str),
                (Result<node_types::Emission, node_types::NodeError>,),
            >(&component_type.instance_type())?;
                anyhow::ensure!(run.async_(), "typed operations must be async");
                let params: Vec<_> = run.params().collect();
                let adapter_params: Vec<_> = adapter.params().collect();
                anyhow::ensure!(
                    params.len() == 2 && params[0].1 == adapter_params[0].1,
                    "typed operation must accept node-context and one input"
                );
                anyhow::ensure!(
                    matches!(
                        params[1].1,
                        wash_runtime::wasmtime::component::Type::List(_)
                            | wash_runtime::wasmtime::component::Type::Record(_)
                    ),
                    "typed operation input must be an owned list or record"
                );
                anyhow::ensure!(
                    owned_operation_value(&params[1].1),
                    "typed operations cannot transfer store-owned resources"
                );
                let results: Vec<_> = run.results().collect();
                let adapter_results: Vec<_> = adapter.results().collect();
                let [wash_runtime::wasmtime::component::Type::Result(result)] = results.as_slice()
                else {
                    anyhow::bail!("typed operation must return one result");
                };
                let wash_runtime::wasmtime::component::Type::Result(adapter_result) =
                    &adapter_results[0]
                else {
                    unreachable!("the JSON adapter passed its result type check");
                };
                anyhow::ensure!(
                    result.err() == adapter_result.err(),
                    "typed operation must retain node-error"
                );
                anyhow::ensure!(
                    matches!(
                        result.ok(),
                        Some(
                            wash_runtime::wasmtime::component::Type::List(_)
                                | wash_runtime::wasmtime::component::Type::Record(_)
                        )
                    ) && result.ok().as_ref().is_some_and(owned_operation_value),
                    "typed operation must return an owned list or record"
                );
                return Ok(());
            }
            if run.async_() && export == "wamn:node/handler@0.1.0" {
                anyhow::bail!("run uses the async ABI without a typed contract and JSON adapter");
            }
            run.typecheck::<
            (&node_types::NodeContext, &str),
            (Result<node_types::Emission, node_types::NodeError>,),
        >(&component_type.instance_type())
        .map_err(anyhow::Error::from)
        };
    let embedded = embedded_components(component_bytes).map_err(|source| {
        ComponentAdmissionError::new(
            ComponentAdmissionErrorKind::InvalidComponentBytes,
            &component_name,
            source,
        )
    })?;
    let declared_exports: BTreeSet<_> = request.declaration.operations.keys().cloned().collect();
    let byte_exports: BTreeSet<_> = component_type
        .exports(raw)
        .map(|(name, _)| name.to_owned())
        .collect();
    let missing: Vec<_> = declared_exports
        .difference(&byte_exports)
        .cloned()
        .collect();
    // A composed component re-exports the interfaces of its embedded members,
    // because the overlay's exports name their types. The host routes only to
    // declared operations, so an export of an embedded member is inert.
    let extra: Vec<_> = byte_exports
        .difference(&declared_exports)
        .filter(|export| !embedded.exports.contains(*export))
        .cloned()
        .collect();
    if !missing.is_empty() || !extra.is_empty() {
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
            .expect("the byte exports contain every declared operation");
        if let Err(error) = validate_handler_signature(export, item.ty, false) {
            return Err(operation_signature_mismatch(&component_name, export, error));
        }
    }
    let imports = component_type
        .imports(raw)
        .map(|(name, _)| name.to_string())
        .collect::<Vec<_>>();
    // An application's components compose at build into one component, so a
    // declared dependency is not an import. Its base must be embedded in these
    // bytes, unchanged, under the digest the declaration names. Publish then
    // folds the call graph from the admitted facts of that exact base.
    let missing_bases = request
        .declaration
        .operations
        .values()
        .flat_map(|operation| &operation.dependencies)
        .filter(|dependency| !embedded.digests.contains(&dependency.digest))
        .map(|dependency| format!("{}@{}", dependency.operation, dependency.digest))
        .collect::<BTreeSet<_>>();
    if !missing_bases.is_empty() {
        return Err(ComponentAdmissionError::new(
            ComponentAdmissionErrorKind::OperationDependencyMismatch,
            &component_name,
            anyhow::anyhow!(
                "declared operation dependencies are not composed into the component bytes: {missing_bases:?}"
            ),
        ));
    }
    // The Component Model exposes one top-level import list, not a call
    // graph from each export. Preserve the operation-owned declarations, while
    // showing only the structural fact the bytes support: their exact union.
    // Only a base's pre-commit slot remains an import, and only in the base's
    // own bytes, because the overlay's build plugs it with its participant.
    let declared_dependency_imports = request
        .declaration
        .operations
        .values()
        .flat_map(|operation| operation.pre_commit.iter().cloned())
        .collect::<BTreeSet<_>>();
    let byte_dependency_imports = imports
        .iter()
        .filter(|name| is_application_operation_import(name))
        .cloned()
        .collect::<BTreeSet<_>>();
    if declared_dependency_imports != byte_dependency_imports {
        let missing = declared_dependency_imports
            .difference(&byte_dependency_imports)
            .cloned()
            .collect::<Vec<_>>();
        let extra = byte_dependency_imports
            .difference(&declared_dependency_imports)
            .cloned()
            .collect::<Vec<_>>();
        return Err(ComponentAdmissionError::new(
            ComponentAdmissionErrorKind::OperationDependencyMismatch,
            &component_name,
            anyhow::anyhow!(
                "declared operation dependencies differ from component imports: missing={missing:?}, extra={extra:?}"
            ),
        ));
    }
    // The same walk that byte-verifies each dependency also decides its
    // posture, so the union costs no second traversal of the closure.
    let mut effectful_dependencies: BTreeSet<&str> = BTreeSet::new();
    for dependency in &byte_dependency_imports {
        let item = component_type
            .get_import(raw, dependency)
            .expect("the component import list contains the dependency");
        if let Err(error) = validate_handler_signature(dependency, item.ty, true) {
            return Err(ComponentAdmissionError::new(
                ComponentAdmissionErrorKind::OperationSignatureMismatch,
                &component_name,
                anyhow::anyhow!(
                    "operation dependency import {dependency:?} does not match {HANDLER_SIGNATURE}: {error}"
                ),
            ));
        }
        if !request
            .effect_free_operation_dependencies
            .contains(dependency.as_str())
        {
            effectful_dependencies.insert(dependency.as_str());
        }
    }
    let policy_imports = wamn_component_policy::ComponentImports::new(
        imports
            .iter()
            .filter(|name| !byte_dependency_imports.contains(*name))
            .cloned(),
    );
    wamn_component_policy::analyze_tenant(
        &policy_imports,
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
        imports,
        derive_effects(&policy_imports, &effectful_dependencies),
    )
    .map_err(|source| {
        ComponentAdmissionError::new(
            ComponentAdmissionErrorKind::InvalidComponentFacts,
            &component_name,
            source,
        )
    })
}

// Nested calls use separate stores. Only owned values can cross that boundary.
fn owned_operation_value(ty: &wash_runtime::wasmtime::component::Type) -> bool {
    use wash_runtime::wasmtime::component::Type;
    match ty {
        Type::Own(_) | Type::Borrow(_) | Type::Future(_) | Type::Stream(_) | Type::ErrorContext => {
            false
        }
        Type::List(list) => owned_operation_value(&list.ty()),
        Type::FixedLengthList(list) => owned_operation_value(&list.ty()),
        Type::Map(map) => owned_operation_value(&map.key()) && owned_operation_value(&map.value()),
        Type::Record(record) => record
            .fields()
            .all(|field| owned_operation_value(&field.ty)),
        Type::Tuple(tuple) => tuple.types().all(|ty| owned_operation_value(&ty)),
        Type::Variant(variant) => variant
            .cases()
            .all(|case| case.ty.as_ref().is_none_or(owned_operation_value)),
        Type::Option(option) => owned_operation_value(&option.ty()),
        Type::Result(result) => {
            result.ok().as_ref().is_none_or(owned_operation_value)
                && result.err().as_ref().is_none_or(owned_operation_value)
        }
        _ => true,
    }
}

/// Whether an import is a cross-package APPLICATION call rather than a
/// platform capability -- the same question, keyed on the same capability
/// registry, as `wamn_catalog`'s classifier of the same name.
///
/// This copy kept the namespace heuristic ("not `wasi` and not `wamn` ⇒ an
/// application call") after the catalog's was moved onto the registry, so the
/// first push of a blobstore guest through `push-component` refused
/// `wasmcloud:blobstore/*` as undeclared operation dependencies
/// (`wamn-362o.41`). Matching is on the registered PACKAGE at any version:
/// this asks what KIND of import it is, and refusing a version is admission's
/// job elsewhere.
///
/// The `wasi` and `wamn` families are platform-kind even when unregistered
/// (`wasi:sockets`): those imports must reach the import policy, which fails
/// closed by ABSENCE and names them -- the raw-socket screen's contract.
/// Classifying them as application dependencies would refuse them one step
/// earlier, as a dependency mismatch, and lose that name.
fn is_application_operation_import(name: &str) -> bool {
    let platform_family = wamn_component_policy::import_pkg(name)
        .split_once(':')
        .is_some_and(|(namespace, _)| matches!(namespace, "wasi" | "wamn"));
    !(platform_family || wamn_component_policy::is_registered_package(name))
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

/// Group the audited imports into the authority packages that leave the host,
/// union the posture of the declared operation dependencies.
///
/// Called with the policy import list after exact operation dependencies have
/// been removed, so every remaining package is authority-free or an admitted
/// platform capability. `effectful_dependencies` carries the other half of the
/// union: a wrapper with an empty capability list reaches an effect
/// through the operations it calls, and an empty projection claims it pure.
///
/// A dependency contributes its PACKAGE and no interface, so its fact carries
/// the INHERITED provenance and the audited-imports check stays with the
/// IMPORTED one. The interface this component imports is the dependency
/// operation itself, and `wamn_catalog` excludes an operation-dependency
/// import from the effect interfaces by rule. The package never collides with
/// a capability package: an operation dependency is by construction an
/// unregistered package.
fn derive_effects(
    imports: &wamn_component_policy::ComponentImports,
    effectful_dependencies: &BTreeSet<&str>,
) -> Vec<AdmittedComponentEffect> {
    let mut grouped: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for name in imports.iter() {
        // Posture comes from the registry row matched on package AND version.
        // Grouping stays package-grain because the persisted projection is
        // package-grain; only the classification moved.
        if wamn_component_policy::import_posture(name)
            == Some(wamn_component_policy::Posture::Effect)
        {
            grouped
                .entry(wamn_component_policy::import_pkg(name))
                .or_default()
                .insert(name.to_owned());
        }
    }
    grouped
        .into_iter()
        .map(|(package, interfaces)| AdmittedComponentEffect {
            package: package.to_owned(),
            provenance: ComponentEffectProvenance::Imported,
            interfaces: interfaces.into_iter().collect(),
        })
        .chain(
            effectful_dependencies
                .iter()
                .copied()
                .map(|dependency| AdmittedComponentEffect {
                    package: wamn_component_policy::import_pkg(dependency).to_owned(),
                    provenance: ComponentEffectProvenance::Inherited,
                    interfaces: Vec::new(),
                }),
        )
        .collect()
}

/// The digest of each component that these bytes embed unchanged.
///
/// A composed component defines each member as a nested component section. The
/// section holds the member's exact bytes, so its digest is the member's
/// admitted digest.
/// The components nested in a composed component: the digest of each one and
/// the names that each one exports.
#[derive(Debug, Default)]
struct EmbeddedComponents {
    digests: BTreeSet<String>,
    exports: BTreeSet<String>,
}

fn embedded_components(component_bytes: &[u8]) -> anyhow::Result<EmbeddedComponents> {
    let mut embedded = EmbeddedComponents::default();
    for payload in wasmparser::Parser::new(0).parse_all(component_bytes) {
        if let wasmparser::Payload::ComponentSection {
            unchecked_range, ..
        } = payload?
        {
            let member = component_bytes
                .get(unchecked_range)
                .ok_or_else(|| anyhow::anyhow!("a nested component section is out of range"))?;
            embedded.digests.insert(component_digest(member));
            embedded.exports.extend(top_level_exports(member)?);
        }
    }
    Ok(embedded)
}

/// The export names of one component, without those of its nested members.
fn top_level_exports(component_bytes: &[u8]) -> anyhow::Result<Vec<String>> {
    let mut exports = Vec::new();
    let mut depth = 0_usize;
    for payload in wasmparser::Parser::new(0).parse_all(component_bytes) {
        match payload? {
            wasmparser::Payload::ModuleSection { .. }
            | wasmparser::Payload::ComponentSection { .. } => depth += 1,
            wasmparser::Payload::End(_) => depth = depth.saturating_sub(1),
            wasmparser::Payload::ComponentExportSection(section) if depth == 0 => {
                for export in section {
                    exports.push(export?.name.name.to_owned());
                }
            }
            _ => {}
        }
    }
    Ok(exports)
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
    /// `wamn-362o.41`: the classifier keys on the capability registry, so a
    /// registered platform capability outside the `wasi`/`wamn` namespaces is
    /// a platform import, and an unregistered package is an application call.
    #[test]
    fn registered_capabilities_are_platform_imports_whatever_their_namespace() {
        for platform in [
            "wasmcloud:blobstore/blobstore@0.1.0",
            "wasmcloud:blobstore/container@0.1.0",
            "wasmcloud:blobstore/types@0.1.0",
            "wasi:clocks/wall-clock@0.2.0",
            "wamn:postgres/client@0.1.0",
            // unregistered but platform-family: the import policy's to refuse, by name
            "wasi:sockets/tcp@0.2.3",
        ] {
            assert!(
                !super::is_application_operation_import(platform),
                "{platform} is a platform capability"
            );
        }
        for application in [
            "example:orders/record@1.0.0",
            "platform-fixture:widget/record-batch@1.0.0",
        ] {
            assert!(
                super::is_application_operation_import(application),
                "{application} is an application call"
            );
        }
    }

    use serde_json::json;
    use wamn_catalog::{
        ComponentOperationDeclaration, ComponentOperationDependency, ComponentPackageScope,
        ComponentParameterDeclaration, ComponentPortDeclaration,
    };
    use wit_component::{ComponentEncoder, StringEncoding, dummy_module, embed_component_metadata};
    use wit_parser::{LiftLowerAbi, ManglingAndAbi, Resolve};

    use super::*;

    const OPERATION: &str = "wamn:node/handler@0.1.0";

    // Every node component imports this: the handler signature is written in
    // terms of the ABI's own types, so the import is structural, carries no
    // application dependency and leaves the host not at all. It is filtered
    // out of the operation-dependency comparison and reaches component policy
    // like any other non-dependency import.
    const NODE_TYPES_IMPORT: &str = "wamn:node/types@0.1.0";

    const DEPENDENCY_WITS: [(&str, &str); 5] = [
        (
            "wasi-clocks.wit",
            "package wasi:clocks@0.2.12; interface monotonic-clock { now: func() -> u64; }",
        ),
        (
            // The interface carries a function ON PURPOSE. An EMPTY interface
            // has nothing to import, so the component encoder elides the
            // import entirely and the fixture silently stops carrying the
            // surface the test names.
            "wasi-sockets.wit",
            "package wasi:sockets@0.2.3; interface tcp { connect: func(); }",
        ),
        (
            "wamn-connection.wit",
            "package wamn:connection@0.1.0; interface http { send: func(); }",
        ),
        (
            "wamn-postgres.wit",
            "package wamn:postgres@0.1.0; interface client { query: func(); }",
        ),
        (
            "platform-fixture.wit",
            "package platform-fixture:widget@1.0.0; \
             interface record-batch { \
               use wamn:node/types@0.1.0.{node-context, emission, node-error}; \
               run: func(ctx: node-context, input: string) -> result<emission, node-error>; \
             } \
             interface wrong-batch { run: func(); }",
        ),
    ];

    const DEPENDENCY_OPERATION: &str = "platform-fixture:widget/record-batch@1.0.0";
    const WRONG_DEPENDENCY_OPERATION: &str = "platform-fixture:widget/wrong-batch@1.0.0";

    fn component_bytes(imports: &str) -> Vec<u8> {
        component_bytes_with_exports(imports, "")
    }

    fn component_bytes_with_exports(imports: &str, extra_exports: &str) -> Vec<u8> {
        component_bytes_with_abi(imports, extra_exports, ManglingAndAbi::Standard32)
    }

    /// The same fixture with a chosen lift/lower ABI, so an ASYNC-LIFTED
    /// export can be built: the legacy mangling with the async-callback ABI
    /// lifts every export `async`.
    fn component_bytes_with_abi(
        imports: &str,
        extra_exports: &str,
        abi: ManglingAndAbi,
    ) -> Vec<u8> {
        component_bytes_exporting(OPERATION, imports, extra_exports, abi)
    }

    /// The fixture with a chosen handler interface as its operation export.
    fn component_bytes_exporting(
        operation: &str,
        imports: &str,
        extra_exports: &str,
        abi: ManglingAndAbi,
    ) -> Vec<u8> {
        try_component_bytes_exporting(operation, imports, extra_exports, abi)
            .expect("fixture component encodes")
    }

    /// The same, handing back the encoder's verdict: a shape the component
    /// model itself refuses never reaches admission.
    fn try_component_bytes_exporting(
        operation: &str,
        imports: &str,
        extra_exports: &str,
        abi: ManglingAndAbi,
    ) -> anyhow::Result<Vec<u8>> {
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
            "package test:component@1.0.0; world fixture {{ {imports} export {operation}; {extra_exports} }}"
        );
        let package = resolve
            .push_str("fixture.wit", &fixture)
            .expect("fixture world parses");
        let world = resolve
            .select_world(&[package], Some("fixture"))
            .expect("fixture world resolves");
        let mut module = dummy_module(&resolve, world, abi);
        embed_component_metadata(&mut module, &resolve, world, StringEncoding::UTF8)
            .expect("fixture component metadata embeds");
        ComponentEncoder::default()
            .module(&module)
            .expect("fixture core module is accepted")
            .validate(true)
            .encode()
    }

    fn request_with(dependencies: Vec<ComponentOperationDependency>) -> ComponentAdmissionRequest {
        let mut request = request();
        request
            .declaration
            .operations
            .get_mut(OPERATION)
            .expect("fixture operation exists")
            .dependencies = dependencies;
        request
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
                        pre_commit: None,
                        pre_commit_required: false,
                        committed_result_schema: None,
                        fresh_only: false,
                        registered_operation: None,
                        dependencies: Vec::new(),
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
            effect_free_operation_dependencies: BTreeSet::new(),
        }
    }

    fn dependency(operation: &str) -> ComponentOperationDependency {
        ComponentOperationDependency {
            participant: None,
            package: "platform_fixture".to_string(),
            version: "1.0.0".to_string(),
            digest: format!("sha256:{}", "c".repeat(64)),
            operation: operation.to_string(),
        }
    }

    /// The base component that exports the dependency operation.
    fn base_bytes() -> Vec<u8> {
        component_bytes_exporting(DEPENDENCY_OPERATION, "", "", ManglingAndAbi::Standard32)
    }

    /// Append `member` to `outer` as a nested component section, unchanged.
    ///
    /// This is the one structural fact admission reads from a composition:
    /// the member's exact bytes inside the composed bytes.
    fn composed(outer: &[u8], member: &[u8]) -> Vec<u8> {
        let mut bytes = outer.to_vec();
        bytes.push(4);
        let mut size = member.len();
        loop {
            let byte = u8::try_from(size & 0x7f).expect("seven bits fit in a byte");
            size >>= 7;
            bytes.push(if size == 0 { byte } else { byte | 0x80 });
            if size == 0 {
                break;
            }
        }
        bytes.extend_from_slice(member);
        bytes
    }

    /// A declaration whose base is embedded under its exact digest.
    fn embedded_dependency(base: &[u8]) -> ComponentOperationDependency {
        ComponentOperationDependency {
            digest: component_digest(base),
            ..dependency(DEPENDENCY_OPERATION)
        }
    }

    /// The request of a base package, which owns its pre-commit slot.
    fn base_request() -> ComponentAdmissionRequest {
        let mut request = request();
        request.declaration.scope.package_id = "platform_fixture".to_string();
        request
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

    /// RULED wamn-362o.46: an async node exports wamn:node/async-handler and
    /// lifts `run` async; the pin admits the lift THERE, and the type check is
    /// the same one handler.run passes.
    #[test]
    fn an_async_lifted_run_on_async_handler_is_admitted() {
        let engine = crate::build_engine(&[]).expect("engine builds");
        let bytes = component_bytes_exporting(
            ASYNC_HANDLER_OPERATION,
            "",
            "",
            ManglingAndAbi::Legacy(LiftLowerAbi::AsyncCallback),
        );
        let mut request = request();
        let operation = request
            .declaration
            .operations
            .remove(OPERATION)
            .expect("the fixture declares the handler operation");
        request
            .declaration
            .operations
            .insert(ASYNC_HANDLER_OPERATION.to_string(), operation);
        // The fixture's lift is asserted, not assumed: the dummy builder
        // applies the async ABI only where the type allows it, so a test that
        // skipped this check would pass on a sync lift and show nothing.
        let component = Component::new(engine.inner(), &bytes).expect("fixture instantiates");
        let raw = component.engine();
        let ComponentItem::ComponentInstance(instance) = component
            .component_type()
            .get_export(raw, ASYNC_HANDLER_OPERATION)
            .expect("the async handler interface is exported")
            .ty
        else {
            panic!("the async handler export is not an interface instance");
        };
        let ComponentItem::ComponentFunc(run) =
            instance.get_export(raw, "run").expect("run is exported").ty
        else {
            panic!("run is not a component function");
        };
        assert!(run.async_(), "the fixture's run is async-lifted");
        validate_component_admission(&engine, &bytes, request)
            .expect("an async-lifted run on the async handler contract is admitted");
    }

    #[test]
    fn typed_async_operation_requires_owned_values() {
        const TYPED_OPERATION: &str = "platform-fixture:widget/record-batch@1.0.0";
        let engine = crate::build_engine(&[]).expect("engine builds");
        for (input, output, expected, imported) in [
            ("list<item>", "list<item>", true, false),
            ("item", "item", true, false),
            ("string", "list<item>", false, false),
            ("list<own<handle>>", "list<item>", false, false),
            ("list<item>", "list<item>", true, true),
            ("string", "list<item>", false, true),
            ("list<own<handle>>", "list<item>", false, true),
        ] {
            let adapter = if imported {
                ""
            } else {
                "run-json: async func(ctx: node-context, input: string) -> result<emission, node-error>;"
            };
            let fixture = if imported {
                "import record-batch; export wamn:node/handler@0.1.0;"
            } else {
                "export record-batch;"
            };
            let mut resolve = Resolve::new();
            resolve
                .push_str(
                    "node.wit",
                    include_str!("../../../execution/router/wit/package.wit"),
                )
                .unwrap();
            let package = resolve.push_str("typed.wit", &format!(r"
                package platform-fixture:widget@1.0.0;
                interface record-batch {{
                    use wamn:node/types@0.1.0.{{node-context, node-error, emission}};
                    resource handle;
                    record item {{ request-id: string, quantity: string }}
                    run: async func(ctx: node-context, input: {input}) -> result<{output}, node-error>;
                    {adapter}
                }}
                world fixture {{ {fixture} }}
            ")).unwrap();
            let world = resolve.select_world(&[package], Some("fixture")).unwrap();
            let mut module = dummy_module(
                &resolve,
                world,
                ManglingAndAbi::Legacy(LiftLowerAbi::AsyncCallback),
            );
            embed_component_metadata(&mut module, &resolve, world, StringEncoding::UTF8).unwrap();
            let bytes = ComponentEncoder::default()
                .module(&module)
                .unwrap()
                .validate(true)
                .encode()
                .unwrap();
            let mut request = request();
            if imported {
                request = base_request();
                request
                    .declaration
                    .operations
                    .get_mut(OPERATION)
                    .unwrap()
                    .pre_commit = Some(TYPED_OPERATION.to_owned());
            } else {
                let declaration = request.declaration.operations.remove(OPERATION).unwrap();
                request
                    .declaration
                    .operations
                    .insert(TYPED_OPERATION.to_owned(), declaration);
            }
            let result = validate_component_admission(&engine, &bytes, request);
            if expected {
                result.expect("owned typed values are admitted");
            } else {
                assert_eq!(
                    result.unwrap_err().kind(),
                    ComponentAdmissionErrorKind::OperationSignatureMismatch
                );
            }
        }
    }

    /// Wasmtime 48 adds fixed-length lists. The production engine leaves the
    /// proposal off and refuses such a component as invalid bytes, so this
    /// judges the type from an engine that turns it on.
    #[test]
    fn a_fixed_length_list_of_resources_is_not_an_owned_value() {
        let mut resolve = Resolve::new();
        let package = resolve
            .push_str(
                "fixed.wit",
                r"
                package platform-fixture:fixed@1.0.0;
                interface batch {
                    resource handle;
                    record item { quantity: string }
                    owned: func(input: list<item, 2>);
                    handles: func(input: list<own<handle>, 2>);
                }
                world fixture { export batch; }
            ",
            )
            .unwrap();
        let world = resolve.select_world(&[package], Some("fixture")).unwrap();
        let mut module = dummy_module(&resolve, world, ManglingAndAbi::Standard32);
        embed_component_metadata(&mut module, &resolve, world, StringEncoding::UTF8).unwrap();
        let bytes = ComponentEncoder::default()
            .module(&module)
            .unwrap()
            .validate(true)
            .encode()
            .unwrap();
        let mut config = wash_runtime::wasmtime::Config::new();
        config.wasm_component_model_fixed_length_lists(true);
        let engine = wash_runtime::wasmtime::Engine::new(&config).unwrap();
        let component = Component::new(&engine, &bytes).unwrap();
        let ComponentItem::ComponentInstance(batch) = component
            .component_type()
            .get_export(&engine, "platform-fixture:fixed/batch@1.0.0")
            .expect("the batch interface is exported")
            .ty
        else {
            panic!("the batch export is not an interface instance");
        };
        let input = |name| {
            let ComponentItem::ComponentFunc(func) = batch
                .get_export(&engine, name)
                .expect("function is exported")
                .ty
            else {
                panic!("{name} is not a component function");
            };
            func.params().next().expect("one input").1
        };
        let owned = input("owned");
        assert!(matches!(
            owned,
            wash_runtime::wasmtime::component::Type::FixedLengthList(_)
        ));
        assert!(owned_operation_value(&owned));
        assert!(!owned_operation_value(&input("handles")));
    }

    // No test builds the refused shape -- an async lift of the sync-typed
    // handler.run -- because no tool will: the component model permits the
    // `async` canonical option only on an `async func` type (wasmparser's
    // check_asyncness refused blob-put's first attempt at virtualization), and
    // wit-component's dummy builder simply lifts a sync-typed function
    // synchronously whatever ABI it is asked for. Admission's own refusal of
    // run.async_() on `handler` stands as defence in depth behind the
    // validator, unreachable by any component that validates.

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
        // Guard the guard. The refusal must NAME the socket import, or the
        // fixture stopped carrying it and this test shows nothing.
        assert!(
            error.to_string().contains("wasi:sockets/tcp@0.2.3"),
            "refusal must name the socket import it caught: {error}"
        );
    }

    #[test]
    fn a_declared_dependency_is_admitted_when_its_base_is_embedded() {
        let engine = crate::build_engine(&[]).expect("engine builds");
        let base = base_bytes();
        let bytes = composed(&component_bytes(""), &base);
        let mut request = request();
        request
            .declaration
            .operations
            .get_mut(OPERATION)
            .expect("fixture operation exists")
            .dependencies = vec![embedded_dependency(&base)];

        let component = validate_component_admission(&engine, &bytes, request)
            .expect("a composed operation dependency admits")
            .component;

        assert_eq!(component.imports, [NODE_TYPES_IMPORT.to_string()]);
        assert_eq!(
            component.operations[OPERATION].dependencies,
            [embedded_dependency(&base)]
        );
        assert!(component.effects.is_empty());

        // A composed component re-exports its base. That export is admitted
        // only while the base that exports it is embedded.
        let reexporting =
            component_bytes_with_exports("", &format!("export {DEPENDENCY_OPERATION};"));
        let request = request_with(vec![embedded_dependency(&base)]);
        validate_component_admission(&engine, &composed(&reexporting, &base), request.clone())
            .expect("an export of an embedded base admits");
        let error = validate_component_admission(&engine, &reexporting, request)
            .expect_err("an export of no embedded member refuses");
        assert_eq!(
            error.kind(),
            ComponentAdmissionErrorKind::OperationExportMismatch
        );
    }

    // The engine parses a second handler export only with this feature enabled.
    // The host and executor enable it in their Cargo manifests.
    // Run this test with `--all-features`.
    #[cfg(feature = "wasm_component_model_implements")]
    #[test]
    fn multi_export_admission_checks_global_union_and_preserves_attachment() {
        let engine = crate::build_engine(&[]).expect("engine builds");
        let base = base_bytes();
        let bytes = composed(
            &component_bytes_with_exports("", "export second: wamn:node/handler@0.1.0;"),
            &base,
        );
        let mut request = request();
        let mut second = request
            .declaration
            .operations
            .get(OPERATION)
            .expect("fixture operation exists")
            .clone();
        second.dependencies = vec![embedded_dependency(&base)];
        request
            .declaration
            .operations
            .insert("second".to_string(), second);

        let component = validate_component_admission(&engine, &bytes, request)
            .expect("a dependency of one export admits")
            .component;

        assert!(component.operations[OPERATION].dependencies.is_empty());
        assert_eq!(
            component.operations["second"].dependencies,
            [embedded_dependency(&base)]
        );
    }

    #[test]
    fn operation_dependencies_refuse_an_unembedded_base_and_an_application_import() {
        let engine = crate::build_engine(&[]).expect("engine builds");
        let base = base_bytes();

        let mut missing_request = request();
        missing_request
            .declaration
            .operations
            .get_mut(OPERATION)
            .expect("fixture operation exists")
            .dependencies = vec![embedded_dependency(&base)];
        let missing = validate_component_admission(&engine, &component_bytes(""), missing_request)
            .expect_err("a declared dependency whose base is not embedded refuses");
        assert_eq!(
            missing.kind(),
            ComponentAdmissionErrorKind::OperationDependencyMismatch
        );
        assert!(missing.to_string().contains("not composed"));

        let mut other_request = request();
        other_request
            .declaration
            .operations
            .get_mut(OPERATION)
            .expect("fixture operation exists")
            .dependencies = vec![dependency(DEPENDENCY_OPERATION)];
        let other = validate_component_admission(
            &engine,
            &composed(&component_bytes(""), &base),
            other_request,
        )
        .expect_err("a base embedded under another digest refuses");
        assert_eq!(
            other.kind(),
            ComponentAdmissionErrorKind::OperationDependencyMismatch
        );

        let mut imported_request = request();
        imported_request
            .declaration
            .operations
            .get_mut(OPERATION)
            .expect("fixture operation exists")
            .dependencies = vec![embedded_dependency(&base)];
        let imported = validate_component_admission(
            &engine,
            &composed(
                &component_bytes(&format!("import {DEPENDENCY_OPERATION};")),
                &base,
            ),
            imported_request,
        )
        .expect_err("a dependency left as an import refuses");
        assert_eq!(
            imported.kind(),
            ComponentAdmissionErrorKind::OperationDependencyMismatch
        );
        assert!(imported.to_string().contains("extra="));
    }

    #[test]
    fn pre_commit_import_must_have_the_handler_signature() {
        let engine = crate::build_engine(&[]).expect("engine builds");
        let mut request = base_request();
        request
            .declaration
            .operations
            .get_mut(OPERATION)
            .expect("fixture operation exists")
            .pre_commit = Some(WRONG_DEPENDENCY_OPERATION.to_owned());

        let error = validate_component_admission(
            &engine,
            &component_bytes(&format!("import {WRONG_DEPENDENCY_OPERATION};")),
            request,
        )
        .expect_err("a pre-commit import with the wrong run type refuses");

        assert_eq!(
            error.kind(),
            ComponentAdmissionErrorKind::OperationSignatureMismatch
        );
        assert!(error.to_string().contains(WRONG_DEPENDENCY_OPERATION));
        assert!(error.to_string().contains(HANDLER_SIGNATURE));
    }

    /// The three classes in one import list: an authority-free WASI package, the
    /// router's own invocation seam, and two real effect packages. Only the
    /// last two are recorded, grouped by package with their exact interfaces.
    #[test]
    fn effects_record_only_the_imports_that_leave_the_host() {
        let engine = crate::build_engine(&[]).expect("engine builds");
        let bytes = component_bytes(
            "import wasi:clocks/monotonic-clock@0.2.12; \
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
                    provenance: ComponentEffectProvenance::Imported,
                    interfaces: vec!["wamn:connection/http@0.1.0".to_string()],
                },
                AdmittedComponentEffect {
                    package: "wamn:postgres".to_string(),
                    provenance: ComponentEffectProvenance::Imported,
                    interfaces: vec!["wamn:postgres/client@0.1.0".to_string()],
                },
            ]
        );
        assert_eq!(facts.component.imports.len(), 4);
        assert_eq!(facts.connections.len(), 1);
        assert_eq!(facts.connections[0].store_alias, "erp");
    }

    /// A base carries an EMPTY capability list of its own and still reaches
    /// whatever its participant does through its pre-commit slot. The gate's
    /// effect-free-case clause keys on
    /// `jsonb_array_length(library.effects) > 0`
    /// (`scenario-worker/src/store/admission.rs:181`), so a non-empty
    /// projection is what denies that path.
    #[test]
    fn a_base_reaching_an_effect_through_its_pre_commit_slot_is_not_effect_free() {
        let engine = crate::build_engine(&[]).expect("engine builds");
        let bytes = component_bytes(&format!("import {DEPENDENCY_OPERATION};"));
        let mut request = base_request();
        request
            .declaration
            .operations
            .get_mut(OPERATION)
            .expect("fixture operation exists")
            .pre_commit = Some(DEPENDENCY_OPERATION.to_owned());

        let component = validate_component_admission(&engine, &bytes, request)
            .expect("a base with an unconfirmed pre-commit slot still admits")
            .component;

        // Guard the guard. The base's own capability list is empty: the node
        // types import is the ABI's own and leaves the host not at all, so the
        // posture below comes from the pre-commit slot alone.
        assert_eq!(
            component.imports,
            [
                DEPENDENCY_OPERATION.to_string(),
                NODE_TYPES_IMPORT.to_string()
            ]
        );
        assert_eq!(
            component.effects,
            [AdmittedComponentEffect {
                package: "platform-fixture:widget".to_string(),
                provenance: ComponentEffectProvenance::Inherited,
                interfaces: Vec::new(),
            }]
        );
    }

    /// The negative control. A composed dependency adds no inherited effect:
    /// its base's imports are the composed component's own imports, so an
    /// ambient import and an effect-free base keep the effect-free case path.
    #[test]
    fn a_composed_dependency_takes_its_effects_from_the_composed_imports() {
        let engine = crate::build_engine(&[]).expect("engine builds");
        let base = base_bytes();
        let bytes = composed(
            &component_bytes("import wasi:clocks/monotonic-clock@0.2.12;"),
            &base,
        );
        let mut request = request();
        request
            .declaration
            .operations
            .get_mut(OPERATION)
            .expect("fixture operation exists")
            .dependencies = vec![embedded_dependency(&base)];

        let component = validate_component_admission(&engine, &bytes, request)
            .expect("a composition over an effect-free base admits")
            .component;

        assert!(
            component.effects.is_empty(),
            "an effect-free composition must keep the effect-free case path: {:?}",
            component.effects
        );
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
