//! Exact synchronization declarations carried by the package migration stream.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context as _, Result, ensure};
use tokio_postgres::Client;
use wamn_schema_introspection::ir::postgres_type;
use wamn_schema_introspection::migration_policy::{
    StageRelation, inspect_stage_synchronization_declaration, validate_stage_trigger_body,
};
use wamn_schema_introspection::postgres::StageSynchronizationExpectation;

use crate::{PackageManifest, RecordHistoryColumn, TombstoneColumn};

pub(super) async fn expectations(
    client: &Client,
    package_root: &Path,
    manifest: &PackageManifest,
) -> Result<Vec<StageSynchronizationExpectation>> {
    let directory = package_root.join("migrations");
    if !directory.try_exists()? {
        return Ok(Vec::new());
    }
    let mut paths = std::fs::read_dir(&directory)?
        .map(|entry| {
            let entry = entry?;
            ensure!(
                entry.file_type()?.is_file(),
                "package migration entry is not a file: {}",
                entry.path().display()
            );
            Ok(entry.path())
        })
        .collect::<Result<Vec<_>>>()?;
    paths.sort();
    let mut expected = Vec::new();
    for path in paths {
        let bytes = std::fs::read(&path)?;
        // Ordinary migrations keep their existing catalog admission. An unrecognized
        // routine is never excluded: the catalog reader still refuses it.
        let Ok(definition) = inspect_stage_synchronization_declaration(&path, &bytes) else {
            continue;
        };
        let model = manifest
            .models
            .values()
            .find(|model| model.schema == definition.schema && model.table == definition.relation)
            .with_context(|| {
                format!(
                    "synchronization {}.{} has no declared model",
                    definition.schema, definition.trigger
                )
            })?;
        ensure!(
            model.owner == manifest.package.id,
            "synchronization {}.{} targets a relation owned by {}",
            definition.schema,
            definition.trigger,
            model.owner
        );
        let rows = client.query(
            "SELECT a.attname, pg_catalog.pg_get_userbyid(c.relowner) AS owner_role, \
             pg_catalog.format_type(a.atttypid,NULL) AS scalar_type, \
             a.attgenerated='' AND a.attidentity='' \
               AND a.attname NOT IN ('id','tenant_id','row_version') \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_index i WHERE i.indrelid=c.oid AND i.indisprimary AND a.attnum=ANY(i.indkey)) AS writable \
             FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace \
             JOIN pg_catalog.pg_attribute a ON a.attrelid=c.oid \
             WHERE n.nspname=$1 AND c.relname=$2 AND c.relkind='r' \
               AND a.attnum>0 AND NOT a.attisdropped \
             ORDER BY a.attnum",
            &[&definition.schema, &definition.relation],
        ).await.context("read synchronization relation and writable field identities")?;
        let owner_role = rows
            .first()
            .with_context(|| {
                format!(
                    "synchronization relation {}.{} has no writable fields",
                    definition.schema, definition.relation
                )
            })?
            .try_get("owner_role")?;
        let reserved = RecordHistoryColumn::ALL
            .map(RecordHistoryColumn::as_str)
            .into_iter()
            .chain(TombstoneColumn::ALL.map(TombstoneColumn::as_str))
            .collect::<Vec<_>>();
        let mut writable_columns = Vec::new();
        let mut owned = StageRelation {
            schema: definition.schema.clone(),
            name: definition.relation.clone(),
            columns: BTreeMap::new(),
        };
        for row in rows {
            let field: String = row.try_get("attname")?;
            if model.field_owners.get(&field).unwrap_or(&model.owner) != &manifest.package.id {
                continue;
            }
            owned.columns.insert(
                field.clone(),
                postgres_type(&row.try_get::<_, String>("scalar_type")?)?,
            );
            if row.try_get::<_, bool>("writable")?
                && !model.server_owned_fields.contains(&field)
                && !reserved.contains(&field.as_str())
            {
                writable_columns.push(field);
            }
        }
        validate_stage_trigger_body(&path, &definition.body, &owned, &writable_columns)?;
        expected.push(StageSynchronizationExpectation {
            definition,
            writable_columns,
            owner_role,
        });
    }
    Ok(expected)
}
