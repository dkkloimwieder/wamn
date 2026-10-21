//! Composes an overlay component with its bases into one component.
//!
//! The bases are those the overlay package pins in its manifest's
//! `base_dependencies`. The overlay imports each base operation it calls under
//! the base's own export name, so this tool plugs each overlay import that
//! exactly one base exports. A base the overlay calls nothing of is not a
//! member, and an overlay that calls no base is returned unchanged. Each
//! member is embedded with its bytes unchanged, so publish can hash each
//! embedded component.
//! [`compose_application`] adds the platform route layer while keeping the
//! application's operation exports, including those of an existing composition.
//!
//! Each member that imports `wamn:postgres/statements` gets an outer import
//! of its own, named by the member's position in the composition (`sql-0`,
//! `sql-1`). The interface stays one type: each outer import carries an
//! `(implements ..)` label of it. A custom section records the digest of the
//! member each outer import was made for, so admission can check the wiring
//! in the composed bytes and the host can bind each import to the statements
//! of that member (`docs/plan/trx-pkg-plan.md`, Per-member import instances).

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, bail};
use sha2::{Digest as _, Sha256};
use wac_graph::types::{Package, Types};
use wac_graph::{CompositionGraph, EncodeOptions, NodeId, PackageId};
use wamn_component_policy::{STATEMENT_INSTANCES_SECTION, STATEMENTS_INTERFACE};
use wasmparser::{Parser, Payload};

/// The HTTP entry exported only by the platform route layer.
pub const HTTP_HANDLER: &str = "wasi:http/handler@0.3.0";

/// Refuse entry authority anywhere in an application member's component tree.
pub fn validate_application_member(bytes: &[u8]) -> anyhow::Result<()> {
    for payload in Parser::new(0).parse_all(bytes) {
        match payload.context("read application member")? {
            Payload::ComponentImportSection(imports) => {
                for import in imports {
                    let import = import.context("read application member import")?;
                    let interface = wamn_component_policy::instance_type(
                        import.name.name,
                        import.name.implements,
                    );
                    if wamn_component_policy::import_pkg(interface) == "wamn:operation" {
                        bail!("application member imports entry-only package wamn:operation");
                    }
                }
            }
            Payload::ComponentExportSection(exports) => {
                for export in exports {
                    let export = export.context("read application member export")?;
                    if export.name.name.split('@').next() == Some("wasi:http/handler") {
                        bail!("application member supplies its own wasi:http/handler");
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Compose the platform HTTP entry with an application, preserving operation exports.
///
/// The application can already contain its pinned bases. Its bytes and the
/// route layer's bytes remain unchanged inside the composition. Matching route
/// imports bind to application exports; other imports remain host capabilities.
pub fn compose_application(route: Vec<u8>, application: Vec<u8>) -> anyhow::Result<Vec<u8>> {
    validate_application_member(&application)?;
    anyhow::ensure!(
        names(&route, "route", true)?.contains(HTTP_HANDLER),
        "route layer does not export {HTTP_HANDLER}"
    );
    let route_imports = names(&route, "route", false)?;
    let mut statement_instances = application_statement_instances(&application)?;
    let mut graph = CompositionGraph::new();
    let application = instantiate(&mut graph, "wamn-member:application", application)?;
    let route = instantiate(&mut graph, "wamn-member:route", route)?;
    // Preserve the application's type dependency order, including base exports
    // that an overlay's exported interface uses.
    let application_exports = graph.types()[graph[application.package].ty()]
        .exports
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    for name in &application_exports {
        let export = graph
            .alias_instance_export(application.instance, name)
            .with_context(|| format!("alias application export {name}"))?;
        if route_imports.contains(name) {
            graph
                .set_instantiation_argument(route.instance, name, export)
                .with_context(|| format!("plug {name} into the route layer"))?;
        }
        graph
            .export(export, name)
            .with_context(|| format!("export application operation {name}"))?;
    }
    // An already composed application keeps its named statement imports and
    // their original member digests. A plain application gets its own instance.
    if statement_instances.is_empty() {
        statement_instances = name_statement_instances(&mut graph, &[application])?;
    }
    let handler = graph
        .alias_instance_export(route.instance, HTTP_HANDLER)
        .context("alias route handler")?;
    graph
        .export(handler, HTTP_HANDLER)
        .context("export route handler")?;
    let mut composed = graph
        .encode(EncodeOptions {
            define_components: true,
            validate: true,
            processor: None,
        })
        .context("encode application with route layer")?;
    if !statement_instances.is_empty() {
        append_custom_section(
            &mut composed,
            STATEMENT_INSTANCES_SECTION,
            &serde_json::to_vec(&statement_instances)?,
        );
    }
    Ok(composed)
}

fn application_statement_instances(bytes: &[u8]) -> anyhow::Result<BTreeMap<String, String>> {
    let mut depth = 0_usize;
    for payload in Parser::new(0).parse_all(bytes) {
        match payload.context("read application statement instances")? {
            Payload::ModuleSection { .. } | Payload::ComponentSection { .. } => depth += 1,
            Payload::End(_) => depth = depth.saturating_sub(1),
            Payload::CustomSection(section)
                if depth == 0 && section.name() == STATEMENT_INSTANCES_SECTION =>
            {
                return serde_json::from_slice(section.data())
                    .context("read application statement instance record");
            }
            _ => {}
        }
    }
    Ok(BTreeMap::new())
}

/// One registered and instantiated member of the composition.
#[derive(Debug, Clone, Copy)]
struct Member {
    package: PackageId,
    instance: NodeId,
    /// SHA-256 of the member's exact bytes.
    digest: [u8; 32],
}

/// Composes the overlay and the bases it calls into one component.
pub fn compose(overlay: Vec<u8>, bases: Vec<Vec<u8>>) -> anyhow::Result<Vec<u8>> {
    let overlay_imports = names(&overlay, "overlay", false)?;
    // Each base export the overlay imports, and the base that exports it.
    let mut plugs = BTreeMap::<String, usize>::new();
    let mut called = BTreeSet::new();
    for (index, base) in bases.iter().enumerate() {
        for export in names(base, &format!("base-{index}"), true)? {
            if !overlay_imports.contains(&export) {
                continue;
            }
            if plugs.insert(export.clone(), index).is_some() {
                bail!("two bases export {export}, which the overlay imports");
            }
            called.insert(index);
        }
    }
    if called.is_empty() {
        return Ok(overlay);
    }

    let mut graph = CompositionGraph::new();
    let mut members = Vec::with_capacity(1 + called.len());

    let overlay = instantiate(&mut graph, "wamn-member:overlay", overlay)?;

    let mut base_members = BTreeMap::new();
    for (index, bytes) in bases.into_iter().enumerate() {
        if !called.contains(&index) {
            continue;
        }
        let member = instantiate(&mut graph, &format!("wamn-member:base-{index}"), bytes)?;
        members.push(member);
        base_members.insert(index, member);
    }

    // Overlay interfaces use base types. An exported interface may only use
    // types that an earlier import or export names, so the overlay exports last.
    members.push(overlay);

    for (export, index) in &plugs {
        let base = base_members[index];
        let argument = graph
            .alias_instance_export(base.instance, export)
            .with_context(|| format!("base-{index} does not export {export}"))?;
        graph
            .set_instantiation_argument(overlay.instance, export, argument)
            .with_context(|| format!("plug {export} into the overlay"))?;
    }

    let statement_instances = name_statement_instances(&mut graph, &members)?;

    for member in members {
        let names = graph.types()[graph[member.package].ty()]
            .exports
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for name in names {
            let export = graph
                .alias_instance_export(member.instance, &name)
                .with_context(|| format!("alias member export {name}"))?;
            graph
                .export(export, &name)
                .with_context(|| format!("export {name} from the composed component"))?;
        }
    }

    let mut composed = graph
        .encode(EncodeOptions {
            define_components: true,
            validate: true,
            processor: None,
        })
        .context("encode the composed component")?;
    if !statement_instances.is_empty() {
        append_custom_section(
            &mut composed,
            STATEMENT_INSTANCES_SECTION,
            &serde_json::to_vec(&statement_instances)?,
        );
    }
    Ok(composed)
}

/// The import names, or with `exports` the export names, of one component.
fn names(bytes: &[u8], name: &str, exports: bool) -> anyhow::Result<BTreeSet<String>> {
    let mut types = Types::default();
    let package = Package::from_bytes(name, None, bytes.to_vec(), &mut types)
        .with_context(|| format!("read member component {name}"))?;
    let world = &types[package.ty()];
    let names = if exports {
        world.exports.keys()
    } else {
        world.imports.keys()
    };
    Ok(names.cloned().collect())
}

/// Give each member that imports the statements interface an outer import of
/// its own, named by its position, and return each name with its member's digest.
fn name_statement_instances(
    graph: &mut CompositionGraph,
    members: &[Member],
) -> anyhow::Result<BTreeMap<String, String>> {
    let mut instances = BTreeMap::new();
    for (position, member) in members.iter().enumerate() {
        let Some(kind) = graph.types()[graph[member.package].ty()]
            .imports
            .get(STATEMENTS_INTERFACE)
            .copied()
        else {
            continue;
        };
        let name = format!("sql-{position}");
        let import = graph
            .import(name.clone(), kind)
            .with_context(|| format!("import {name} as {STATEMENTS_INTERFACE}"))?;
        graph
            .set_instantiation_argument(member.instance, STATEMENTS_INTERFACE, import)
            .with_context(|| format!("plug {name} into member {position}"))?;
        instances.insert(name, digest_text(&member.digest));
    }
    Ok(instances)
}

fn digest_text(digest: &[u8; 32]) -> String {
    use std::fmt::Write as _;
    let mut text = String::with_capacity("sha256:".len() + 64);
    text.push_str("sha256:");
    for byte in digest {
        write!(text, "{byte:02x}").expect("writing to a String cannot fail");
    }
    text
}

/// Append one custom section to the end of a component binary.
fn append_custom_section(bytes: &mut Vec<u8>, name: &str, payload: &[u8]) {
    let mut content = Vec::with_capacity(5 + name.len() + payload.len());
    leb128(&mut content, name.len());
    content.extend_from_slice(name.as_bytes());
    content.extend_from_slice(payload);
    bytes.push(0);
    leb128(bytes, content.len());
    bytes.extend_from_slice(&content);
}

fn leb128(bytes: &mut Vec<u8>, mut value: usize) {
    loop {
        let byte = u8::try_from(value & 0x7f).expect("seven bits fit a byte");
        value >>= 7;
        if value == 0 {
            bytes.push(byte);
            return;
        }
        bytes.push(byte | 0x80);
    }
}

fn instantiate(graph: &mut CompositionGraph, name: &str, bytes: Vec<u8>) -> anyhow::Result<Member> {
    let digest = Sha256::digest(&bytes).into();
    let package = Package::from_bytes(name, None, bytes, graph.types_mut())
        .with_context(|| format!("read member component {name}"))?;
    let package = graph
        .register_package(package)
        .with_context(|| format!("register member component {name}"))?;
    let instance = graph.instantiate(package);
    Ok(Member {
        package,
        instance,
        digest,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use sha2::{Digest, Sha256};
    use wac_graph::types::{Package, Types};
    use wasmparser::{Parser, Payload};

    use super::{HTTP_HANDLER, compose, compose_application, validate_application_member};

    const HOST_LOG: &str =
        r#"(import "test:host/log@1.0.0" (instance (export "log" (func (param "x" u32)))))"#;

    fn component(imports: &str, export: &str) -> Vec<u8> {
        wat::parse_str(format!(
            r#"(component
                {imports}
                (core module $m (func (export "run") (param i32) (result i32) local.get 0))
                (core instance $i (instantiate $m))
                (func $run (param "x" u32) (result u32) (canon lift (core func $i "run")))
                (instance $out (export "run" (func $run)))
                (export "{export}" (instance $out))
            )"#
        ))
        .expect("the test component text is valid")
    }

    fn operation_import(name: &str) -> String {
        format!(
            r#"(import "{name}" (instance (export "run" (func (param "x" u32) (result u32)))))"#
        )
    }

    fn overlay() -> Vec<u8> {
        component(
            &format!("{HOST_LOG}{}", operation_import("test:base/op@1.0.0")),
            "test:overlay/main@1.0.0",
        )
    }

    fn base() -> Vec<u8> {
        component(HOST_LOG, "test:base/op@1.0.0")
    }

    fn nested_component_digests(bytes: &[u8]) -> BTreeSet<Vec<u8>> {
        Parser::new(0)
            .parse_all(bytes)
            .filter_map(|payload| match payload.expect("the composed bytes parse") {
                Payload::ComponentSection {
                    unchecked_range, ..
                } => {
                    let start = usize::try_from(unchecked_range.start).expect("offset fits memory");
                    let end = usize::try_from(unchecked_range.end).expect("offset fits memory");
                    Some(Sha256::digest(&bytes[start..end]).to_vec())
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn composition_embeds_each_member_unchanged_and_plugs_by_export_name() {
        let overlay = overlay();
        let base = base();
        let members = [&overlay, &base];

        let composed = compose(overlay.clone(), vec![base.clone()]).expect("the members compose");

        let embedded = nested_component_digests(&composed);
        for member in members {
            assert!(
                embedded.contains(&Sha256::digest(member).to_vec()),
                "a nested component section holds each member's input bytes"
            );
        }

        let mut types = Types::default();
        let package = Package::from_bytes("composed", None, composed, &mut types)
            .expect("the composed bytes are a component");
        let world = &types[package.ty()];
        assert_eq!(
            world.imports.keys().map(String::as_str).collect::<Vec<_>>(),
            ["test:host/log@1.0.0"],
            "only the import no member satisfies stays an import"
        );
        assert_eq!(
            world
                .exports
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["test:base/op@1.0.0", "test:overlay/main@1.0.0"]),
            "every member export is a composed export"
        );
    }

    const STATEMENTS_IMPORT: &str = r#"(import "wamn:postgres/statements@0.4.0" (instance
        (export "run" (func (param "statement" string) (result u32)))))"#;

    #[test]
    fn each_statements_importer_gets_its_own_outer_import_with_its_digest() {
        let overlay = component(
            &format!(
                "{STATEMENTS_IMPORT}{}",
                operation_import("test:base/op@1.0.0")
            ),
            "test:overlay/main@1.0.0",
        );
        let base = component(STATEMENTS_IMPORT, "test:base/op@1.0.0");
        let composed =
            compose(overlay.clone(), vec![base.clone()]).expect("two statements importers compose");

        let mut imports = Vec::new();
        let mut section = None;
        let mut depth = 0_usize;
        for payload in Parser::new(0).parse_all(&composed) {
            match payload.expect("the composed bytes parse") {
                Payload::ModuleSection { .. } | Payload::ComponentSection { .. } => depth += 1,
                Payload::End(_) => depth = depth.saturating_sub(1),
                Payload::ComponentImportSection(reader) if depth == 0 => {
                    for import in reader {
                        let import = import.expect("an import parses");
                        imports.push((
                            import.name.name.to_owned(),
                            import.name.implements.map(str::to_owned),
                        ));
                    }
                }
                Payload::CustomSection(reader)
                    if depth == 0
                        && reader.name() == wamn_component_policy::STATEMENT_INSTANCES_SECTION =>
                {
                    section = Some(reader.data().to_vec());
                }
                _ => {}
            }
        }
        let interface = Some(wamn_component_policy::STATEMENTS_INTERFACE.to_owned());
        assert_eq!(
            imports,
            [
                ("sql-0".to_owned(), interface.clone()),
                ("sql-1".to_owned(), interface),
            ],
            "the base is member 0 and the overlay member 1, each with an outer import"
        );
        let recorded: BTreeMap<String, String> =
            serde_json::from_slice(&section.expect("the provenance section exists"))
                .expect("the provenance is a JSON object");
        let hex = |bytes: &[u8]| {
            Sha256::digest(bytes)
                .iter()
                .fold(String::new(), |mut hex, byte| {
                    std::fmt::Write::write_fmt(&mut hex, format_args!("{byte:02x}"))
                        .expect("writing to a String cannot fail");
                    hex
                })
        };
        assert_eq!(
            recorded,
            BTreeMap::from([
                ("sql-0".to_owned(), format!("sha256:{}", hex(&base))),
                ("sql-1".to_owned(), format!("sha256:{}", hex(&overlay))),
            ])
        );
    }

    /// A pinned base that the overlay calls nothing of is no member, and an
    /// overlay that calls no base is returned unchanged.
    #[test]
    fn an_overlay_that_calls_no_base_is_returned_unchanged() {
        let overlay = component(HOST_LOG, "test:overlay/main@1.0.0");
        assert_eq!(
            compose(overlay.clone(), vec![base()]).expect("the overlay composes"),
            overlay
        );
    }

    /// Two bases that both export an operation the overlay imports leave the
    /// link ambiguous, so the composition refuses.
    #[test]
    fn two_bases_exporting_one_imported_operation_refuse() {
        let error =
            compose(overlay(), vec![base(), base()]).expect_err("an ambiguous link was composed");
        assert!(error.to_string().contains("two bases export"), "{error}");
    }

    #[test]
    fn application_composition_plugs_routes_and_keeps_both_export_sets() {
        let application = base();
        let route = component(&operation_import("test:base/op@1.0.0"), HTTP_HANDLER);
        let composed = compose_application(route.clone(), application.clone())
            .expect("the route composes with the application");
        assert_eq!(
            super::names(&composed, "composed", true).expect("read exports"),
            BTreeSet::from([HTTP_HANDLER.to_owned(), "test:base/op@1.0.0".to_owned()])
        );
        assert_eq!(
            super::names(&composed, "composed", false).expect("read imports"),
            BTreeSet::from(["test:host/log@1.0.0".to_owned()])
        );
        let embedded = nested_component_digests(&composed);
        for member in [&route, &application] {
            assert!(embedded.contains(&Sha256::digest(member).to_vec()));
        }
        let again = compose_application(route.clone(), application.clone())
            .expect("the same inputs compose a second time");
        assert_eq!(Sha256::digest(&composed), Sha256::digest(&again));
        for (mut route, mut application, change_route) in [
            (route.clone(), application.clone(), true),
            (route, application, false),
        ] {
            let changed = if change_route {
                &mut route
            } else {
                &mut application
            };
            super::append_custom_section(changed, "test-source", b"changed");
            let changed = compose_application(route, application).expect("changed input composes");
            assert_ne!(Sha256::digest(&composed), Sha256::digest(&changed));
        }
    }

    #[test]
    fn application_composition_preserves_nested_statement_authority() {
        let base = component(STATEMENTS_IMPORT, "test:base/op@1.0.0");
        let overlay = component(
            &format!(
                "{STATEMENTS_IMPORT}{}",
                operation_import("test:base/op@1.0.0")
            ),
            "test:overlay/main@1.0.0",
        );
        let application = compose(overlay, vec![base]).expect("the overlay composes");
        let expected =
            super::application_statement_instances(&application).expect("read instances");
        let route = component(&operation_import("test:overlay/main@1.0.0"), HTTP_HANDLER);
        let composed = compose_application(route, application).expect("the route composes");
        assert_eq!(
            super::application_statement_instances(&composed).expect("read instances"),
            expected
        );
        assert_eq!(
            super::names(&composed, "composed", false).expect("read imports"),
            BTreeSet::from(["sql-0".to_owned(), "sql-1".to_owned()])
        );
    }

    #[test]
    fn application_members_cannot_import_operation_authority_or_export_http() {
        let imports = [
            r#"(import "wamn:operation/operation@0.1.0" (instance))"#,
            r#"(import "entry" (implements "wamn:operation/operation@0.1.0") (instance))"#,
            r#"(import "wamn:operation/other@2.0.0" (instance))"#,
            r#"(import "entry" (implements "wamn:operation/other@2.0.0") (instance))"#,
            r#"(import "wamn:operation/other" (instance))"#,
        ];
        for import in imports {
            let member = component(import, "test:base/op@1.0.0");
            let error = validate_application_member(&member).expect_err("entry authority refuses");
            assert!(
                error
                    .to_string()
                    .contains("entry-only package wamn:operation"),
                "{error}"
            );
            let wrapped = compose(
                component(
                    &operation_import("test:base/op@1.0.0"),
                    "test:overlay/main@1.0.0",
                ),
                vec![member],
            )
            .expect("the ordinary composer can wrap the forbidden member");
            assert!(validate_application_member(&wrapped).is_err());
        }
        for interface in [
            "wamn:postgres/statements@0.4.0",
            "wamn:operations/operation@0.1.0",
            "other:operation/operation@0.1.0",
        ] {
            let member = component(&operation_import(interface), "test:base/op@1.0.0");
            validate_application_member(&member)
                .expect("a different package is not entry authority");
        }
        let member = component("", HTTP_HANDLER);
        let error = validate_application_member(&member).expect_err("application HTTP refuses");
        assert!(
            error
                .to_string()
                .contains("supplies its own wasi:http/handler"),
            "{error}"
        );
        let route = component("", HTTP_HANDLER);
        assert!(compose_application(route, member).is_err());
        let hidden = wat::parse_str(format!(
            r#"(component (component (instance $handler)
                (export "{HTTP_HANDLER}" (instance $handler))))"#
        ))
        .expect("encode a hidden nested handler");
        assert!(validate_application_member(&hidden).is_err());
    }

    #[test]
    fn only_the_route_layer_can_import_operation_authority() {
        let route = component(
            r#"(import "wamn:operation/operation@0.1.0" (instance))"#,
            HTTP_HANDLER,
        );
        let application = component(STATEMENTS_IMPORT, "test:base/op@1.0.0");
        let digest = super::digest_text(&Sha256::digest(&application).into());
        let composed = compose_application(route, application).expect("entry authority composes");
        assert_eq!(
            super::names(&composed, "composed", false).expect("read imports"),
            BTreeSet::from([
                "wamn:operation/operation@0.1.0".to_owned(),
                "sql-0".to_owned()
            ])
        );
        assert_eq!(
            super::application_statement_instances(&composed).expect("read SQL ownership"),
            BTreeMap::from([("sql-0".to_owned(), digest)])
        );
    }
}
