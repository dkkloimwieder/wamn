//! Composes an overlay component with its base and participants into one component.
//!
//! The overlay imports each base operation it depends on. The base imports a
//! pre-commit interface, which a participant component exports under another
//! name. The declarations name both sides of each link, so this tool plugs
//! them by declaration and never by matching names. Each member is embedded
//! with its bytes unchanged, so publish can hash each embedded component.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail, ensure};
use clap::Parser;
use serde_json::Value;
use wac_graph::types::Package;
use wac_graph::{CompositionGraph, EncodeOptions, NodeId, PackageId};

#[derive(Debug, Parser)]
#[command(
    name = "wamn-component-composer",
    version,
    about = "Compose an overlay component with its base and participants"
)]
struct Args {
    /// Component declaration of the overlay (`publication/components/*.json.in`).
    #[arg(long, value_name = "DECLARATION")]
    overlay_declaration: PathBuf,

    /// Overlay main component.
    #[arg(long, value_name = "COMPONENT")]
    overlay: PathBuf,

    /// Component declaration of a base. Pair each one with a `--base`, in order.
    #[arg(long = "base-declaration", value_name = "DECLARATION", required = true)]
    base_declarations: Vec<PathBuf>,

    /// Base component. Pair each one with a `--base-declaration`, in order.
    #[arg(long = "base", value_name = "COMPONENT", required = true)]
    bases: Vec<PathBuf>,

    /// Participant component that a base calls before commit.
    #[arg(long = "participant", value_name = "COMPONENT")]
    participants: Vec<PathBuf>,

    /// Destination for the composed component bytes.
    #[arg(long, value_name = "COMPONENT")]
    output: PathBuf,
}

/// One base member: its component declaration and its component bytes.
#[derive(Debug)]
struct Base {
    declaration: Value,
    bytes: Vec<u8>,
}

/// One registered and instantiated member of the composition.
#[derive(Debug, Clone, Copy)]
struct Member {
    package: PackageId,
    instance: NodeId,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    ensure!(
        args.base_declarations.len() == args.bases.len(),
        "each --base-declaration needs one --base: {} declarations, {} components",
        args.base_declarations.len(),
        args.bases.len()
    );
    let overlay_declaration = read_declaration(&args.overlay_declaration)?;
    let overlay = read_component(&args.overlay)?;
    let bases = args
        .base_declarations
        .iter()
        .zip(&args.bases)
        .map(|(declaration, component)| {
            Ok(Base {
                declaration: read_declaration(declaration)?,
                bytes: read_component(component)?,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let participants = args
        .participants
        .iter()
        .map(|path| read_component(path))
        .collect::<anyhow::Result<Vec<_>>>()?;

    let composed = compose(&overlay_declaration, overlay, bases, participants)?;
    std::fs::write(&args.output, composed)
        .with_context(|| format!("write composed component {}", args.output.display()))
}

fn read_declaration(path: &Path) -> anyhow::Result<Value> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("read component declaration {}", path.display()))?;
    serde_json::from_slice(&bytes)
        .with_context(|| format!("parse component declaration {}", path.display()))
}

fn read_component(path: &Path) -> anyhow::Result<Vec<u8>> {
    std::fs::read(path).with_context(|| format!("read component {}", path.display()))
}

/// Composes the overlay, its bases and its participants into one component.
fn compose(
    overlay_declaration: &Value,
    overlay: Vec<u8>,
    bases: Vec<Base>,
    participants: Vec<Vec<u8>>,
) -> anyhow::Result<Vec<u8>> {
    let mut graph = CompositionGraph::new();
    let mut members = Vec::with_capacity(1 + bases.len() + participants.len());

    let overlay = instantiate(&mut graph, "wamn-member:overlay", overlay)?;

    let mut base_members = Vec::with_capacity(bases.len());
    for (index, base) in bases.into_iter().enumerate() {
        let package = text(&base.declaration, "/scope/package-id", "base declaration")?;
        let version = text(
            &base.declaration,
            "/scope/package-version",
            "base declaration",
        )?;
        let member = instantiate(&mut graph, &format!("wamn-member:base-{index}"), base.bytes)?;
        members.push(member);
        base_members.push((package, version, base.declaration, member));
    }

    let mut participant_members = Vec::with_capacity(participants.len());
    for (index, bytes) in participants.into_iter().enumerate() {
        let member = instantiate(
            &mut graph,
            &format!("wamn-member:participant-{index}"),
            bytes,
        )?;
        members.push(member);
        participant_members.push(member);
    }
    // Overlay interfaces use base types. An exported interface may only use
    // types that an earlier import or export names, so the overlay exports last.
    members.push(overlay);

    let operations = overlay_declaration
        .get("operations")
        .and_then(Value::as_object)
        .context("the overlay declaration has no operations object")?;
    for (operation, entry) in operations {
        let dependencies = entry
            .get("dependencies")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        for dependency in dependencies {
            let context = format!("dependency of overlay operation {operation}");
            let package = text(dependency, "/package", &context)?;
            let version = text(dependency, "/version", &context)?;
            let base_operation = text(dependency, "/operation", &context)?;
            let Some((_, _, base_declaration, base)) = base_members
                .iter()
                .find(|(id, release, _, _)| *id == package && *release == version)
            else {
                bail!(
                    "{operation} depends on {package}@{version}, but no base declaration names it"
                );
            };
            let Some(base_entry) = base_declaration
                .get("operations")
                .and_then(|operations| operations.get(&base_operation))
            else {
                bail!(
                    "{operation} depends on {base_operation}, which {package}@{version} does not declare"
                );
            };

            let export = graph
                .alias_instance_export(base.instance, &base_operation)
                .with_context(|| format!("{package}@{version} does not export {base_operation}"))?;
            graph
                .set_instantiation_argument(overlay.instance, &base_operation, export)
                .with_context(|| format!("plug {base_operation} into the overlay"))?;

            let Some(participant) = dependency.get("participant") else {
                continue;
            };
            let participant = participant
                .as_str()
                .with_context(|| format!("the participant of {context} is not text"))?;
            let pre_commit = base_entry
                .get("pre-commit")
                .and_then(Value::as_str)
                .with_context(|| {
                    format!(
                        "{operation} names participant {participant}, but {base_operation} declares no pre-commit"
                    )
                })?;
            let providers = participant_members
                .iter()
                .filter(|member| {
                    graph.types()[graph[member.package].ty()]
                        .exports
                        .contains_key(participant)
                })
                .collect::<Vec<_>>();
            let [provider] = providers.as_slice() else {
                bail!(
                    "exactly one participant component must export {participant}; {} do",
                    providers.len()
                );
            };
            let export = graph
                .alias_instance_export(provider.instance, participant)
                .with_context(|| format!("alias participant export {participant}"))?;
            graph
                .set_instantiation_argument(base.instance, pre_commit, export)
                .with_context(|| format!("plug {participant} into {pre_commit}"))?;
        }
    }

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

    graph
        .encode(EncodeOptions {
            define_components: true,
            validate: true,
            processor: None,
        })
        .context("encode the composed component")
}

fn instantiate(graph: &mut CompositionGraph, name: &str, bytes: Vec<u8>) -> anyhow::Result<Member> {
    let package = Package::from_bytes(name, None, bytes, graph.types_mut())
        .with_context(|| format!("read member component {name}"))?;
    let package = graph
        .register_package(package)
        .with_context(|| format!("register member component {name}"))?;
    let instance = graph.instantiate(package);
    Ok(Member { package, instance })
}

fn text(value: &Value, pointer: &str, context: &str) -> anyhow::Result<String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .with_context(|| format!("{context} has no text at {pointer}"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::json;
    use sha2::{Digest, Sha256};
    use wac_graph::types::{Package, Types};
    use wasmparser::{Parser, Payload};

    use super::{Base, compose};

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
        component(
            &format!(
                "{HOST_LOG}{}",
                operation_import("test:base/pre-commit@1.0.0")
            ),
            "test:base/op@1.0.0",
        )
    }

    fn participant(export: &str) -> Vec<u8> {
        component(HOST_LOG, export)
    }

    fn overlay_declaration() -> serde_json::Value {
        json!({"operations": {"test:overlay/main@1.0.0": {"dependencies": [{
            "package": "test_base",
            "version": "1.0.0",
            "digest": "__BASE_DIGEST__",
            "operation": "test:base/op@1.0.0",
            "participant": "test:overlay/participant@1.0.0"
        }]}}})
    }

    fn base_declaration() -> serde_json::Value {
        json!({
            "scope": {"package-id": "test_base", "package-version": "1.0.0"},
            "operations": {"test:base/op@1.0.0": {"pre-commit": "test:base/pre-commit@1.0.0"}}
        })
    }

    fn nested_component_digests(bytes: &[u8]) -> BTreeSet<Vec<u8>> {
        Parser::new(0)
            .parse_all(bytes)
            .filter_map(|payload| match payload.expect("the composed bytes parse") {
                Payload::ComponentSection {
                    unchecked_range, ..
                } => Some(Sha256::digest(&bytes[unchecked_range]).to_vec()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn composition_embeds_each_member_unchanged_and_plugs_by_declaration() {
        let overlay = overlay();
        let base = base();
        let participant = participant("test:overlay/participant@1.0.0");
        let members = [&overlay, &base, &participant];

        let composed = compose(
            &overlay_declaration(),
            overlay.clone(),
            vec![Base {
                declaration: base_declaration(),
                bytes: base.clone(),
            }],
            vec![participant.clone()],
        )
        .expect("the members compose");

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
            BTreeSet::from([
                "test:base/op@1.0.0",
                "test:overlay/main@1.0.0",
                "test:overlay/participant@1.0.0",
            ]),
            "every member export is a composed export"
        );
    }

    #[test]
    fn a_participant_that_no_member_exports_is_refused() {
        let error = compose(
            &overlay_declaration(),
            overlay(),
            vec![Base {
                declaration: base_declaration(),
                bytes: base(),
            }],
            vec![participant("test:overlay/other@1.0.0")],
        )
        .expect_err("the named participant has no provider");
        assert!(
            error.to_string().contains(
                "exactly one participant component must export test:overlay/participant@1.0.0"
            ),
            "{error:#}"
        );
    }
}
