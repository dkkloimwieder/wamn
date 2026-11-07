use super::{
    BTreeMap, BTreeSet, CoordinateHashes, DirectAcl, EffectiveAcl, EffectiveDataAccess,
    UndeclaredResidue, bail, ensure,
};
use anyhow::Context as _;
use std::fmt::Write as _;

/// Require one schema contribution for every installed package.
/// Code versions do not determine whether a package schema exists.
pub(super) fn validate_package_set(
    installed: &CoordinateHashes,
    presented: &CoordinateHashes,
) -> anyhow::Result<()> {
    let installed_packages = installed
        .keys()
        .map(|(id, _)| id.as_str())
        .collect::<BTreeSet<_>>();
    let presented_packages = presented
        .keys()
        .map(|(id, _)| id.as_str())
        .collect::<BTreeSet<_>>();
    ensure!(
        installed_packages == presented_packages,
        "package-data-access-installed-set-mismatch: missing-packages={:?}; uninstalled-packages={:?}",
        installed_packages
            .difference(&presented_packages)
            .collect::<Vec<_>>(),
        presented_packages
            .difference(&installed_packages)
            .collect::<Vec<_>>()
    );
    Ok(())
}

/// Refuse a presented root whose bytes moved under an applied coordinate.
///
/// Published reconciliation uses the recorded SQL source, independently of
/// executable selection. A working tree supplies no verified artifact.
pub(super) fn validate_recorded_manifests(
    installed: &CoordinateHashes,
    presented: &CoordinateHashes,
) -> anyhow::Result<()> {
    for ((package_id, package_version), presented_hash) in presented {
        let recorded_hash = installed
            .iter()
            .find_map(|((id, version), digest)| {
                (id == package_id && version == package_version).then_some(digest)
            })
            .with_context(|| format!(
                "package-data-access-source-drift: package={package_id}@{package_version} is not the recorded schema source"
            ))?;
        let Some(recorded_hash) = recorded_hash else {
            bail!(
                "{}: package={package_id}@{package_version} was applied by wamn dev from a working tree; recreate the environment and apply verified artifacts",
                wamn_schema_control::PACKAGE_COORDINATE_WITHOUT_ARTIFACT_REFUSAL
            );
        };
        let Some(presented_hash) = presented_hash else {
            bail!(
                "{}: package={package_id}@{package_version} is a working tree; present its verified artifact",
                wamn_package_control::apply_package::PACKAGE_ARTIFACT_REQUIRED_REFUSAL
            );
        };
        ensure!(
            recorded_hash == presented_hash,
            "package-data-access-source-drift: package={package_id}@{package_version}; recorded-digest={recorded_hash}; presented-digest={presented_hash}"
        );
    }
    Ok(())
}

pub(super) fn desired_acl(effective: &EffectiveDataAccess) -> DirectAcl {
    let mut desired = DirectAcl::default();
    desired
        .schema
        .extend(effective.schemas().iter().map(|schema| {
            (
                schema.clone(),
                effective.role().to_owned(),
                "USAGE".to_owned(),
                false,
            )
        }));
    for relation in effective.relations() {
        for (privilege, fields) in [
            ("SELECT", relation.select_fields()),
            ("INSERT", relation.insert_fields()),
            ("UPDATE", relation.update_fields()),
        ] {
            desired.column.extend(fields.iter().map(|field| {
                (
                    relation.schema().to_owned(),
                    relation.table().to_owned(),
                    field.clone(),
                    effective.role().to_owned(),
                    privilege.to_owned(),
                    false,
                )
            }));
        }
        // DELETE has no column form, so a declared hard delete is the one
        // table-level entry the direct ACL carries.
        if relation.delete() {
            desired.table.insert((
                relation.schema().to_owned(),
                relation.table().to_owned(),
                effective.role().to_owned(),
                "DELETE".to_owned(),
                false,
            ));
        }
    }
    desired
}

pub(super) fn desired_effective_acl(effective: &EffectiveDataAccess) -> EffectiveAcl {
    let mut desired = EffectiveAcl::default();
    desired.schema.extend(
        effective
            .schemas()
            .iter()
            .map(|schema| (schema.clone(), "USAGE".to_owned())),
    );
    for relation in effective.relations() {
        for (privilege, fields) in [
            ("SELECT", relation.select_fields()),
            ("INSERT", relation.insert_fields()),
            ("UPDATE", relation.update_fields()),
        ] {
            desired.column.extend(fields.iter().map(|field| {
                (
                    relation.schema().to_owned(),
                    relation.table().to_owned(),
                    field.clone(),
                    privilege.to_owned(),
                )
            }));
        }
        if relation.delete() {
            desired.table.insert((
                relation.schema().to_owned(),
                relation.table().to_owned(),
                "DELETE".to_owned(),
            ));
        }
    }
    desired
}

/// Render the revocation that clears one residue read.
///
/// Revoking a privilege the role never held is not an error in PostgreSQL, so
/// the render stays safe even when the read and the write disagree. The role
/// and PUBLIC both lose the privilege, because PUBLIC reaches the App role.
pub(super) fn render_undeclared_revocation(role: &str, residue: &UndeclaredResidue) -> String {
    let role = quote_identifier(role);
    let mut targets = BTreeMap::<(&str, &str, &str), BTreeSet<&str>>::new();
    for (schema, table, kind, column, _) in residue {
        let columns = targets
            .entry((schema.as_str(), table.as_str(), kind.as_str()))
            .or_default();
        if let Some(column) = column {
            columns.insert(column.as_str());
        }
    }
    let mut sql = String::new();
    for ((schema, table, kind), columns) in targets {
        let target = format!("{}.{}", quote_identifier(schema), quote_identifier(table));
        // A sequence carries its own privilege vocabulary, and REVOKE refuses to
        // name a sequence as a table.
        let carrier = if kind == "S" { "SEQUENCE" } else { "TABLE" };
        writeln!(
            sql,
            "REVOKE ALL PRIVILEGES ON {carrier} {target} FROM PUBLIC, {role};"
        )
        .expect("writing SQL to a String cannot fail");
        if columns.is_empty() {
            continue;
        }
        // A table-level revocation leaves a direct column grant in place, so the
        // columns the read named lose each column privilege by name.
        let columns = columns
            .into_iter()
            .map(quote_identifier)
            .collect::<Vec<_>>()
            .join(", ");
        for privilege in ["SELECT", "INSERT", "UPDATE", "REFERENCES"] {
            writeln!(
                sql,
                "REVOKE {privilege} ({columns}) ON TABLE {target} FROM PUBLIC, {role};"
            )
            .expect("writing SQL to a String cannot fail");
        }
    }
    sql
}

/// Name each relation a refusal has to report.
pub(super) fn residue_targets(residue: &UndeclaredResidue) -> String {
    residue
        .iter()
        .map(|(schema, table, ..)| format!("{schema}.{table}"))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(",")
}

pub(super) fn quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}
