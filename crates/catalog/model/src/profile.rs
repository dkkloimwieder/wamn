//! The effect profile of one release
//! (docs/architecture/execution.md#host-run-routes): per operation, per export
//! it reaches, the statements those bindings reach and the tables and columns
//! each reads, writes or locks.
//!
//! Reachability is the release's component composition. An export reaches
//! its own declared statements. A composed component also reaches each base
//! member through that member's named `statements` instance, and nothing in
//! the bytes says which base export it calls, so the reach is the member's
//! whole statement set: the union of that base's exports, marked imprecise.
//!
//! The compiler records statement relation facts. Profiling expands them with
//! the recorded view dependencies and never reads SQL syntax.

use std::collections::{BTreeMap, BTreeSet};

use crate::{ComponentSqlStatement, ServingComponent};
use serde::{Deserialize, Serialize};

/// The profile of every operation of a release's components.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReleaseProfile {
    pub operations: Vec<OperationProfile>,
}

/// One export of one release component.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OperationProfile {
    pub package: String,
    pub component: String,
    pub operation: String,
    pub reaches: Vec<Reach>,
}

/// The statements one operation reaches through one member.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Reach {
    pub package: String,
    /// The exports whose statements this reach holds: the operation itself,
    /// or every export of a base member.
    pub operations: Vec<String>,
    /// Whether the reach is a base member's whole statement set rather than
    /// the statements of the export the operation calls.
    pub imprecise: bool,
    pub statements: Vec<StatementProfile>,
}

/// One statement and the relations its SQL reaches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StatementProfile {
    pub path: String,
    pub relations: Vec<RelationProfile>,
}

/// What one statement does to one relation, including a view's base tables.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RelationProfile {
    pub table: String,
    pub reads: Vec<String>,
    /// The columns an INSERT or UPDATE writes.
    pub writes: Vec<String>,
    pub deletes: bool,
    pub locks: bool,
}

/// One view's recorded base table dependencies.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ViewRelation {
    /// Each base table and the columns that the view reads from it.
    /// An empty column set retains a table dependency such as `count(*)`.
    pub base_tables: BTreeMap<String, BTreeSet<String>>,
}

/// Derive the profile of `components`, the components of one release.
///
/// `views` holds the recorded view dependencies under their relation names.
pub fn release_profile<'a>(
    components: impl IntoIterator<Item = &'a ServingComponent>,
    views: &BTreeMap<String, ViewRelation>,
) -> ReleaseProfile {
    let components = components.into_iter().collect::<Vec<_>>();
    let mut operations = Vec::new();
    for component in &components {
        let bases = component
            .descriptor
            .statement_instances
            .iter()
            .filter_map(|(name, digest)| {
                let base = components
                    .iter()
                    .find(|base| &base.descriptor.component_digest == digest)?;
                Some((base, component.statement_instances.get(name)?))
            })
            .collect::<Vec<_>>();
        for (name, operation) in &component.descriptor.operations {
            let mut reaches = vec![Reach {
                package: component.package_id.clone(),
                operations: vec![name.clone()],
                imprecise: false,
                statements: statements(&operation.statements, views),
            }];
            reaches.extend(bases.iter().map(|(base, set)| Reach {
                package: base.package_id.clone(),
                operations: base.descriptor.operations.keys().cloned().collect(),
                imprecise: true,
                statements: statements(set, views),
            }));
            operations.push(OperationProfile {
                package: component.package_id.clone(),
                component: component.descriptor.component.clone(),
                operation: name.clone(),
                reaches,
            });
        }
    }
    ReleaseProfile { operations }
}

fn statements(
    statements: &BTreeMap<String, ComponentSqlStatement>,
    views: &BTreeMap<String, ViewRelation>,
) -> Vec<StatementProfile> {
    statements
        .iter()
        .map(|(path, statement)| {
            let mut relations = statement
                .relations
                .iter()
                .map(|relation| (relation.table.clone(), relation.clone()))
                .collect::<BTreeMap<_, _>>();
            for view in statement
                .relations
                .iter()
                .filter_map(|relation| views.get(&relation.table))
            {
                for (table, columns) in &view.base_tables {
                    let relation =
                        relations
                            .entry(table.clone())
                            .or_insert_with(|| RelationProfile {
                                table: table.clone(),
                                reads: Vec::new(),
                                writes: Vec::new(),
                                deletes: false,
                                locks: false,
                            });
                    relation.reads.extend(columns.iter().cloned());
                    relation.reads.sort();
                    relation.reads.dedup();
                }
            }
            StatementProfile {
                path: path.clone(),
                relations: relations.into_values().collect(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AdmittedComponentOperation, ComponentDescriptor};

    #[test]
    fn profile_reads_compiled_facts_and_expands_view_dependencies() {
        let relation = RelationProfile {
            table: "report".into(),
            reads: vec!["id".into()],
            writes: Vec::new(),
            deletes: false,
            locks: true,
        };
        let statement = ComponentSqlStatement {
            name: "read".into(),
            path: "read.sql".into(),
            sql: "not parsed by profiling".into(),
            binds: Vec::new(),
            columns: Vec::new(),
            relations: vec![relation.clone()],
            transactional: true,
        };
        let mut operation = AdmittedComponentOperation {
            connections: Vec::new(),
            registered_operation: None,
            committed_result_schema: None,
            host_transaction: false,
            input_ports: Vec::new(),
            output_ports: Vec::new(),
            parameters: Vec::new(),
            statements: BTreeMap::from([("read.sql".into(), statement)]),
        };
        let views = BTreeMap::from([(
            "report".into(),
            ViewRelation {
                base_tables: BTreeMap::from([("source".into(), BTreeSet::from(["id".into()]))]),
            },
        )]);
        let component = |operation| ServingComponent {
            runtime_schema: None,
            package_id: "fixture".into(),
            descriptor: ComponentDescriptor {
                component: "fixture".into(),
                interface_version: "1.0.0".into(),
                component_digest: "sha256:fixture".into(),
                operations: BTreeMap::from([("read".into(), operation)]),
                imports: Vec::new(),
                statement_instances: BTreeMap::new(),
                imports_fingerprint: String::new(),
                effects: Vec::new(),
                connections: Vec::new(),
            },
            statement_instances: BTreeMap::new(),
        };
        let profile = release_profile([&component(operation.clone())], &views);
        let facts = &profile.operations[0].reaches[0].statements[0].relations;
        assert_eq!(
            facts
                .iter()
                .find(|fact| fact.table == relation.table)
                .unwrap(),
            &relation
        );
        for (table, columns) in &views["report"].base_tables {
            assert_eq!(
                facts
                    .iter()
                    .find(|fact| &fact.table == table)
                    .unwrap()
                    .reads
                    .iter()
                    .collect::<BTreeSet<_>>(),
                columns.iter().collect()
            );
        }
        operation
            .statements
            .values_mut()
            .next()
            .unwrap()
            .sql
            .clear();
        assert_eq!(release_profile([&component(operation)], &views), profile);
    }
}
