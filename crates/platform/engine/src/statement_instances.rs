//! Per-member `wamn:postgres/statements` instances of a composed component.
//!
//! The composer gives each member that imports the statements interface an
//! outer import of its own, `sql-<position>`, labelled `(implements
//! "wamn:postgres/statements@0.3.0")`, and records the digest of that member
//! beside the name in the custom section
//! [`wamn_component_policy::STATEMENT_INSTANCES_SECTION`].
//! Two outer imports share one interface type, so a valid composition could
//! wire one member to the import meant for another. [`statement_instances`]
//! reads the composed component's instantiation graph and accepts the record
//! only when each named import is the statements argument of exactly the
//! member whose digest the record names (`docs/plan/trx-pkg-plan.md`,
//! Per-member import instances). Admission refuses a component that fails it,
//! and the host binds each import to that member's statements.

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
    Member(String),
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
                    components.push(ComponentEntry::Member(component_digest(member)));
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
        let digest = match components.get(usize::try_from(component_index)?) {
            Some(ComponentEntry::Member(digest)) => Some(digest),
            Some(ComponentEntry::Other) => None,
            None => bail!("an instantiation names component {component_index}, which is undefined"),
        };
        for (arg, index) in args {
            let entry = instances.get(usize::try_from(index)?).with_context(|| {
                format!("argument {arg:?} names instance {index}, which is undefined")
            })?;
            let InstanceEntry::Statements(import) = entry else {
                ensure!(
                    arg != STATEMENTS_INTERFACE,
                    "a member receives {STATEMENTS_INTERFACE} from no named import"
                );
                continue;
            };
            ensure!(
                arg == STATEMENTS_INTERFACE,
                "statements import {import:?} reaches a member as {arg:?}"
            );
            let member = digest.with_context(|| {
                format!("statements import {import:?} reaches a component that is not a member")
            })?;
            ensure!(
                record.get(import) == Some(member),
                "statements import {import:?} reaches member {member}, but the record names {:?}",
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
