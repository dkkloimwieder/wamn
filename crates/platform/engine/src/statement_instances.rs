//! Per-member `wamn:postgres/statements` instances of a composed component.
//!
//! The composer gives each member that imports the statements interface an
//! outer import of its own, `sql-<position>`, labelled `(implements
//! "wamn:postgres/statements@0.4.0")`, and records the digest of that member
//! beside the name in the custom section
//! [`wamn_component_policy::STATEMENT_INSTANCES_SECTION`]. Two outer imports
//! share one interface type, so a valid composition could wire one member to
//! the import meant for another. [`statement_instances`] reads the composed
//! component's instantiation graph and accepts the record only when each named
//! import is the statements argument of exactly the member whose digest the
//! record names (docs/architecture/components.md#grouping-and-publication). A
//! route wrapper can forward an import through a nested application's verified
//! record to that same member. Admission refuses a component that fails it, and
//! the host binds each import to that member's statements.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context as _, bail, ensure};
use wasmparser::{
    ComponentAlias, ComponentExternalKind, ComponentInstance, ComponentOuterAliasKind,
    ComponentTypeRef, Parser, Payload,
};

use wamn_component_policy::{STATEMENT_INSTANCES_SECTION, STATEMENTS_INTERFACE};

use crate::component_admission::component_digest;

/// One component index: a nested member and its digest, or anything else.
enum ComponentEntry {
    Member {
        digest: String,
        statements: BTreeMap<String, String>,
    },
    Other,
}

/// One component instance index: a named statements import, or anything else.
#[derive(Clone)]
enum InstanceEntry {
    Statements(String),
    Other,
}

/// Each named statements import of `component_bytes` and its member's digest.
///
/// Empty when the component has no named statements import and no record: an
/// ordinary component, or a composition whose members share one import.
///
/// # Errors
///
/// Refuses the component when the record and the instantiation graph differ:
/// a named import without a record or a record without an import, a member
/// that receives another member's import or no named import, an import that
/// reaches a member under another name or more than once, a shared statements
/// import beside named ones, or an alias of a function or instance out of a
/// named import.
pub fn statement_instances(component_bytes: &[u8]) -> anyhow::Result<BTreeMap<String, String>> {
    let mut components = Vec::new();
    let mut instances = Vec::new();
    let mut named = BTreeSet::new();
    let mut shared_import = false;
    let mut record = None;
    let mut instantiations = Vec::new();
    let mut depth = 0_usize;
    for payload in Parser::new(0).parse_all(component_bytes) {
        let payload = payload?;
        match payload {
            Payload::ModuleSection { .. } => depth += 1,
            Payload::ComponentSection {
                unchecked_range, ..
            } => {
                if depth == 0 {
                    let start = usize::try_from(unchecked_range.start)?;
                    let end = usize::try_from(unchecked_range.end)?;
                    let member = component_bytes
                        .get(start..end)
                        .context("a nested component section is out of range")?;
                    components.push(ComponentEntry::Member {
                        digest: component_digest(member),
                        statements: statement_instances(member)
                            .context("validate nested member statement instances")?,
                    });
                }
                depth += 1;
            }
            Payload::End(_) => depth = depth.saturating_sub(1),
            _ if depth > 0 => {}
            Payload::CustomSection(reader) if reader.name() == STATEMENT_INSTANCES_SECTION => {
                ensure!(record.is_none(), "the statement instance record repeats");
                let parsed: BTreeMap<String, String> = serde_json::from_slice(reader.data())
                    .context("the statement instance record is not a JSON object of names")?;
                record = Some(parsed);
            }
            Payload::ComponentImportSection(reader) => {
                for import in reader {
                    let import = import?;
                    match import.ty {
                        ComponentTypeRef::Instance(_) => {
                            if import.name.implements == Some(STATEMENTS_INTERFACE) {
                                let name = import.name.name.to_owned();
                                named.insert(name.clone());
                                instances.push(InstanceEntry::Statements(name));
                            } else {
                                shared_import |= import.name.name == STATEMENTS_INTERFACE;
                                instances.push(InstanceEntry::Other);
                            }
                        }
                        ComponentTypeRef::Component(_) => components.push(ComponentEntry::Other),
                        _ => {}
                    }
                }
            }
            Payload::ComponentInstanceSection(reader) => {
                for instance in reader {
                    if let ComponentInstance::Instantiate {
                        component_index,
                        args,
                    } = instance?
                    {
                        let args = args
                            .iter()
                            .filter(|arg| arg.kind == ComponentExternalKind::Instance)
                            .map(|arg| (arg.name.to_owned(), arg.index))
                            .collect::<Vec<_>>();
                        instantiations.push((component_index, args));
                    }
                    instances.push(InstanceEntry::Other);
                }
            }
            Payload::ComponentAliasSection(reader) => {
                for alias in reader {
                    match alias? {
                        ComponentAlias::InstanceExport {
                            kind,
                            instance_index,
                            name,
                        } => {
                            if let Some(InstanceEntry::Statements(import)) =
                                instances.get(usize::try_from(instance_index)?)
                                && matches!(
                                    kind,
                                    ComponentExternalKind::Func | ComponentExternalKind::Instance
                                )
                            {
                                bail!(
                                    "the composition aliases {name:?} out of statements import {import:?}"
                                );
                            }
                            match kind {
                                ComponentExternalKind::Instance => {
                                    instances.push(InstanceEntry::Other);
                                }
                                ComponentExternalKind::Component => {
                                    components.push(ComponentEntry::Other);
                                }
                                _ => {}
                            }
                        }
                        ComponentAlias::Outer {
                            kind: ComponentOuterAliasKind::Component,
                            ..
                        } => components.push(ComponentEntry::Other),
                        ComponentAlias::Outer { .. }
                        | ComponentAlias::CoreInstanceExport { .. } => {}
                    }
                }
            }
            Payload::ComponentExportSection(reader) => {
                for export in reader {
                    match export?.kind {
                        ComponentExternalKind::Instance => instances.push(InstanceEntry::Other),
                        ComponentExternalKind::Component => components.push(ComponentEntry::Other),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    let record = record.unwrap_or_default();
    if named.is_empty() && record.is_empty() {
        return Ok(record);
    }
    ensure!(
        !shared_import,
        "the composition imports {STATEMENTS_INTERFACE} unnamed beside named instances"
    );
    let recorded = record.keys().cloned().collect::<BTreeSet<_>>();
    ensure!(
        named == recorded,
        "the named statements imports {named:?} differ from the record {recorded:?}"
    );
    let mut wired = BTreeSet::new();
    for (component_index, args) in instantiations {
        let member = match components.get(usize::try_from(component_index)?) {
            Some(ComponentEntry::Member { digest, statements }) => Some((digest, statements)),
            Some(ComponentEntry::Other) => None,
            None => bail!("an instantiation names component {component_index}, which is undefined"),
        };
        for (arg, index) in args {
            let entry = instances.get(usize::try_from(index)?).with_context(|| {
                format!("argument {arg:?} names instance {index}, which is undefined")
            })?;
            let InstanceEntry::Statements(import) = entry else {
                ensure!(
                    arg != STATEMENTS_INTERFACE
                        && !member.is_some_and(|(_, statements)| statements.contains_key(&arg)),
                    "a member receives {STATEMENTS_INTERFACE} from no named import"
                );
                continue;
            };
            let (digest, statements) = member.with_context(|| {
                format!("statements import {import:?} reaches a component that is not a member")
            })?;
            // A wrapper forwards a named import through the already verified
            // record of its nested application. The final owner remains the
            // leaf member, not the wrapper's digest.
            let owner = if arg == STATEMENTS_INTERFACE {
                digest
            } else {
                statements.get(&arg).with_context(|| {
                    format!("statements import {import:?} reaches a member as {arg:?}")
                })?
            };
            ensure!(
                record.get(import) == Some(owner),
                "statements import {import:?} reaches member {owner}, but the record names {:?}",
                record.get(import)
            );
            ensure!(
                wired.insert(import.clone()),
                "statements import {import:?} reaches more than one member"
            );
        }
    }
    ensure!(
        wired == named,
        "statements imports {:?} reach no member",
        named.difference(&wired).collect::<Vec<_>>()
    );
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAMED: &str =
        r#"(import "sql-0" (implements "wamn:postgres/statements@0.4.0") (instance))"#;

    fn member(import: &str, label: &str) -> String {
        format!(r#"(component (import "{import}" (instance)) (@custom "member" "{label}"))"#)
    }

    fn record(member: &str) -> String {
        let digest = component_digest(&wat::parse_str(member).unwrap());
        let json = serde_json::to_string(&BTreeMap::from([("sql-0", digest)])).unwrap();
        format!(
            r#"(@custom "{STATEMENT_INSTANCES_SECTION}" {})"#,
            serde_json::to_string(&json).unwrap()
        )
    }

    fn inspect(parts: &str) -> anyhow::Result<BTreeMap<String, String>> {
        statement_instances(&wat::parse_str(format!("(component {parts})")).unwrap())
    }

    fn refuses(parts: &str, reason: &str) {
        let error = inspect(parts).unwrap_err();
        assert!(error.to_string().contains(reason), "{error:#}");
    }

    #[test]
    fn named_statement_imports_and_the_record_must_agree() {
        let leaf = member(STATEMENTS_INTERFACE, "leaf");
        refuses(NAMED, "differ from the record");
        refuses(&record(&leaf), "differ from the record");
        assert!(inspect("").unwrap().is_empty());
    }

    #[test]
    fn a_statement_import_must_reach_its_recorded_member() {
        let leaf = member(STATEMENTS_INTERFACE, "leaf");
        let other = member(STATEMENTS_INTERFACE, "other");
        let wiring = format!(
            r#"{NAMED} {leaf}
            (instance (instantiate 0 (with "{STATEMENTS_INTERFACE}" (instance 0)))) {}"#,
            record(&leaf)
        );
        assert_eq!(inspect(&wiring).unwrap().len(), 1);
        refuses(
            &format!(
                r#"{NAMED} {other}
            (instance (instantiate 0 (with "{STATEMENTS_INTERFACE}" (instance 0)))) {}"#,
                record(&leaf)
            ),
            "but the record names",
        );
    }

    #[test]
    fn a_member_cannot_receive_statements_from_an_unnamed_import() {
        let leaf = member(STATEMENTS_INTERFACE, "leaf");
        refuses(
            &format!(
                r#"{NAMED} (import "other" (instance)) {leaf}
            (instance (instantiate 0 (with "{STATEMENTS_INTERFACE}" (instance 1)))) {}"#,
                record(&leaf)
            ),
            "from no named import",
        );
    }

    #[test]
    fn a_statement_import_cannot_reach_a_member_under_another_name() {
        let leaf = member("renamed", "leaf");
        refuses(
            &format!(
                r#"{NAMED} {leaf}
            (instance (instantiate 0 (with "renamed" (instance 0)))) {}"#,
                record(&leaf)
            ),
            "reaches a member as",
        );
    }

    #[test]
    fn a_statement_import_must_reach_exactly_one_member() {
        let leaf = member(STATEMENTS_INTERFACE, "leaf");
        refuses(
            &format!("{NAMED} {leaf} {}", record(&leaf)),
            "reach no member",
        );
        refuses(
            &format!(
                r#"{NAMED} {leaf}
            (instance (instantiate 0 (with "{STATEMENTS_INTERFACE}" (instance 0))))
            (instance (instantiate 0 (with "{STATEMENTS_INTERFACE}" (instance 0)))) {}"#,
                record(&leaf)
            ),
            "reaches more than one member",
        );
    }

    #[test]
    fn shared_and_named_statement_imports_cannot_coexist() {
        let leaf = member(STATEMENTS_INTERFACE, "leaf");
        refuses(
            &format!(
                r#"{NAMED} (import "{STATEMENTS_INTERFACE}" (instance)) {}"#,
                record(&leaf)
            ),
            "unnamed beside named instances",
        );
    }

    #[test]
    fn functions_and_instances_cannot_be_aliased_out_of_a_named_import() {
        let import = r#"(import "sql-0" (implements "wamn:postgres/statements@0.4.0")
            (instance (export "run" (func)) (export "nested" (instance))))"#;
        for (name, kind) in [("run", "func"), ("nested", "instance")] {
            refuses(
                &format!(r#"{import} (alias export 0 "{name}" ({kind}))"#),
                "composition aliases",
            );
        }
    }
}
