//! Package-owned catalog projection for shared application databases.

use std::collections::{BTreeMap, BTreeSet};

use wamn_schema_introspection::ir::{CatalogIr, Table};

use crate::PackageManifest;

/// Project installed schema objects to one package and its declared base owners.
///
/// Neighbor-owned relations, fields, constraints, and exclusions stay outside
/// the target package's generated contract. Indexes remain only when all their
/// fields survive projection. `installed` must contain the complete presented
/// package set whose ownership declarations describe the introspected catalog.
///
/// # Errors
/// Refuses any introspected relation without an installed definition owner.
pub fn project_package_catalog(
    catalog: &CatalogIr,
    target: &PackageManifest,
    installed: &[PackageManifest],
) -> anyhow::Result<CatalogIr> {
    let mut relation_owners = BTreeMap::<(String, String), String>::new();
    let mut field_owners = BTreeMap::<(String, String, String), String>::new();
    let mut constraint_owners = BTreeMap::<(String, String, String), String>::new();
    for package in installed {
        for model in package.models.values() {
            relation_owners
                .entry((model.schema.clone(), model.table.clone()))
                .or_insert_with(|| model.owner.clone());
            for (field, owner) in &model.field_owners {
                field_owners.insert(
                    (model.schema.clone(), model.table.clone(), field.clone()),
                    owner.clone(),
                );
            }
            for (constraint, owner) in &model.constraint_owners {
                constraint_owners.insert(
                    (
                        model.schema.clone(),
                        model.table.clone(),
                        constraint.clone(),
                    ),
                    owner.clone(),
                );
            }
        }
        for relation in package.internal_relations.values() {
            relation_owners.insert(
                (relation.schema.clone(), relation.table.clone()),
                package.package.id.clone(),
            );
        }
    }

    let admitted_owners = std::iter::once(target.package.id.as_str())
        .chain(
            target
                .base_dependencies
                .values()
                .map(|dependency| dependency.package.as_str()),
        )
        .collect::<BTreeSet<_>>();
    let mut tables = Vec::new();
    for table in catalog.tables() {
        let coordinate = (table.schema().to_owned(), table.name().to_owned());
        let relation_owner = relation_owners.get(&coordinate).ok_or_else(|| {
            anyhow::anyhow!(
                "{}.{} has no installed package definition owner",
                table.schema(),
                table.name()
            )
        })?;
        if !admitted_owners.contains(relation_owner.as_str()) {
            continue;
        }

        let columns = table
            .columns()
            .iter()
            .filter(|column| {
                let owner = field_owners
                    .get(&(
                        table.schema().to_owned(),
                        table.name().to_owned(),
                        column.name().to_owned(),
                    ))
                    .unwrap_or(relation_owner);
                admitted_owners.contains(owner.as_str())
            })
            .cloned()
            .collect::<Vec<_>>();
        let column_names = columns
            .iter()
            .map(wamn_schema_introspection::ir::Column::name)
            .collect::<BTreeSet<_>>();
        let constraints = table
            .constraints()
            .iter()
            .filter(|constraint| {
                let owner = constraint_owners
                    .get(&(
                        table.schema().to_owned(),
                        table.name().to_owned(),
                        constraint.name().to_owned(),
                    ))
                    .unwrap_or(relation_owner);
                admitted_owners.contains(owner.as_str())
            })
            .cloned()
            .collect::<Vec<_>>();
        let indexes = table
            .indexes()
            .iter()
            .filter(|index| {
                index
                    .columns()
                    .iter()
                    .all(|column| column_names.contains(column.name()))
            })
            .cloned()
            .collect::<Vec<_>>();
        let exclusions = table
            .exclusions()
            .iter()
            .filter(|exclusion| {
                let owner = constraint_owners
                    .get(&(
                        table.schema().to_owned(),
                        table.name().to_owned(),
                        exclusion.name().to_owned(),
                    ))
                    .unwrap_or(relation_owner);
                admitted_owners.contains(owner.as_str())
            })
            .cloned()
            .collect::<Vec<_>>();
        tables.push(
            Table::new(table.schema(), table.name(), columns, constraints, indexes)
                .with_exclusions(exclusions),
        );
    }
    Ok(CatalogIr::new(tables))
}
