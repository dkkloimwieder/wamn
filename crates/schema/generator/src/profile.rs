//! The effect profile of one release (docs/plan/trx-pkg-plan.md, Spike and
//! acceptance B): per operation, per export it reaches, the statements those
//! bindings reach and the tables and columns each reads, writes or locks.
//!
//! Reachability is the release's component composition. An export reaches
//! its own declared statements. A composed component also reaches each base
//! member through that member's named `statements` instance, and nothing in
//! the bytes says which base export it calls, so the reach is the member's
//! whole statement set: the union of that base's exports, marked imprecise.
//!
//! The relations come from each statement's SQL in the release, so an
//! SQL-only change changes the profile with no component rebuild. They are the
//! relation access that generation derives for a custom operation
//! ([`crate::sql_lex::relation_access`]), over the relation columns the caller
//! reads from PostgreSQL. The lexer admits a closed SQL subset; a statement
//! outside it carries the lexer's refusal instead of relations.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use wamn_catalog::{ComponentSqlStatement, ServingComponent};

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
    /// Why the lexer derived no relations from this SQL, or `None`.
    pub refused: Option<String>,
}

/// What one statement does to one table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RelationProfile {
    pub table: String,
    pub reads: Vec<String>,
    /// The columns an INSERT or UPDATE writes.
    pub writes: Vec<String>,
    pub deletes: bool,
    pub locks: bool,
}

/// Derive the profile of `components`, the components of one release.
///
/// `relations` maps each table name to its columns, as the statements name
/// them unqualified under the host's search path.
pub fn release_profile<'a>(
    components: impl IntoIterator<Item = &'a ServingComponent>,
    relations: &BTreeMap<String, BTreeSet<String>>,
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
        for (name, operation) in &component.operations {
            let mut reaches = vec![Reach {
                package: component.package_id.clone(),
                operations: vec![name.clone()],
                imprecise: false,
                statements: statements(&operation.statements, relations),
            }];
            reaches.extend(bases.iter().map(|(base, set)| Reach {
                package: base.package_id.clone(),
                operations: base.operations.keys().cloned().collect(),
                imprecise: true,
                statements: statements(set, relations),
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
    relations: &BTreeMap<String, BTreeSet<String>>,
) -> Vec<StatementProfile> {
    statements
        .iter()
        .map(|(path, statement)| {
            match crate::sql_lex::relation_access(statement.sql.as_bytes(), relations) {
                Ok(access) => StatementProfile {
                    path: path.clone(),
                    relations: access
                        .into_iter()
                        .map(|(table, access)| RelationProfile {
                            table,
                            reads: access.select_fields.into_iter().collect(),
                            writes: access
                                .insert_fields
                                .union(&access.update_fields)
                                .cloned()
                                .collect(),
                            deletes: access.delete,
                            locks: access.lock,
                        })
                        .collect(),
                    refused: None,
                },
                Err(detail) => StatementProfile {
                    path: path.clone(),
                    relations: Vec::new(),
                    refused: Some(detail.to_owned()),
                },
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use wamn_catalog::{ComponentDescriptor, ServingComponentOperation};

    fn statement(path: &str, sql: &str) -> (String, ComponentSqlStatement) {
        (
            path.to_owned(),
            ComponentSqlStatement {
                name: path.to_owned(),
                path: path.to_owned(),
                sql: sql.to_owned(),
                binds: Vec::new(),
                columns: Vec::new(),
                transactional: true,
            },
        )
    }

    fn component(
        package: &str,
        digest: &str,
        operations: BTreeMap<String, BTreeMap<String, ComponentSqlStatement>>,
    ) -> ServingComponent {
        ServingComponent {
            package_id: package.to_owned(),
            descriptor: ComponentDescriptor {
                component: package.to_owned(),
                interface_version: "1.0.0".to_owned(),
                component_digest: digest.to_owned(),
                operations: BTreeMap::new(),
                imports: Vec::new(),
                statement_instances: BTreeMap::new(),
                imports_fingerprint: String::new(),
                effects: Vec::new(),
                connections: Vec::new(),
            },
            operations: operations
                .into_iter()
                .map(|(name, statements)| {
                    (
                        name,
                        ServingComponentOperation {
                            registered_operation: None,
                            permissions: BTreeSet::new(),
                            committed_result_schema: None,
                            statements,
                        },
                    )
                })
                .collect(),
            statement_instances: BTreeMap::new(),
        }
    }

    /// Acme's record_receipt composed over Receiving, as one release holds
    /// them, with Receiving's lock statement written as `lock`.
    fn release(lock: &str) -> Vec<ServingComponent> {
        let receiving_statements = BTreeMap::from([
            statement("record_receipt/lock_purchase_order.sql", lock),
            statement(
                "record_receipt/insert_receipt.sql",
                "INSERT INTO receipt (purchase_order_id, occurred_at) VALUES ($1, $2) RETURNING id",
            ),
        ]);
        let receiving = component(
            "wamn_receiving",
            "sha256:receiving",
            BTreeMap::from([("record_receipt".to_owned(), receiving_statements.clone())]),
        );
        let acme_statements = BTreeMap::from([statement(
            "record_receipt/record_inspection.sql",
            "INSERT INTO quality_inspection (receipt_id, status) VALUES ($1, 'approved')",
        )]);
        let mut acme = component(
            "client_acme_receiving",
            "sha256:acme",
            BTreeMap::from([("record_receipt".to_owned(), acme_statements.clone())]),
        );
        acme.descriptor.statement_instances = BTreeMap::from([
            ("sql-0".to_owned(), "sha256:receiving".to_owned()),
            ("sql-1".to_owned(), "sha256:acme-member".to_owned()),
        ]);
        acme.statement_instances = BTreeMap::from([
            ("sql-0".to_owned(), receiving_statements),
            ("sql-1".to_owned(), acme_statements),
        ]);
        vec![receiving, acme]
    }

    fn relations() -> BTreeMap<String, BTreeSet<String>> {
        let columns = |names: &[&str]| names.iter().map(|name| (*name).to_owned()).collect();
        BTreeMap::from([
            (
                "purchase_order".to_owned(),
                columns(&["id", "status", "cost"]),
            ),
            (
                "receipt".to_owned(),
                columns(&["id", "purchase_order_id", "occurred_at"]),
            ),
            (
                "quality_inspection".to_owned(),
                columns(&["receipt_id", "status"]),
            ),
        ])
    }

    fn lock_statement(profile: &ReleaseProfile) -> &StatementProfile {
        let acme = profile
            .operations
            .iter()
            .find(|operation| operation.package == "client_acme_receiving")
            .expect("the release profiles acme.record_receipt");
        let receiving = acme
            .reaches
            .iter()
            .find(|reach| reach.package == "wamn_receiving")
            .expect("acme.record_receipt reaches Receiving");
        assert!(receiving.imprecise, "a base member reach is its union");
        assert_eq!(receiving.operations, ["record_receipt"]);
        receiving
            .statements
            .iter()
            .find(|statement| statement.path == "record_receipt/lock_purchase_order.sql")
            .expect("the reach holds the lock statement")
    }

    #[test]
    fn an_sql_only_change_to_one_statement_changes_the_profile_with_no_component_rebuild() {
        let before = release("SELECT status FROM purchase_order WHERE id = $1 FOR UPDATE");
        let after = release("SELECT status, cost FROM purchase_order WHERE id = $1");
        let digests = |release: &[ServingComponent]| {
            release
                .iter()
                .map(|component| component.descriptor.component_digest.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(digests(&before), digests(&after), "no component is rebuilt");

        let (before, after) = (
            release_profile(&before, &relations()),
            release_profile(&after, &relations()),
        );
        assert_eq!(
            lock_statement(&before).relations,
            [RelationProfile {
                table: "purchase_order".to_owned(),
                reads: vec!["id".to_owned(), "status".to_owned()],
                writes: Vec::new(),
                deletes: false,
                locks: true,
            }]
        );
        assert_eq!(
            lock_statement(&after).relations,
            [RelationProfile {
                table: "purchase_order".to_owned(),
                reads: vec!["cost".to_owned(), "id".to_owned(), "status".to_owned()],
                writes: Vec::new(),
                deletes: false,
                locks: false,
            }]
        );
        let own = &before.operations[0].reaches[0];
        assert!(!own.imprecise, "an export's own statements are precise");
    }
}
