use std::collections::BTreeSet;

use anyhow::{Context as _, bail, ensure};
use tokio_postgres::Transaction;
use wamn_schema_generator::PackageManifest;

pub(super) async fn reconcile_entity_maps(
    tx: &Transaction<'_>,
    plan: &wamn_schema_control::PackageMigrationPlan,
    manifest: &PackageManifest,
) -> anyhow::Result<()> {
    let schemas = plan
        .models
        .iter()
        .map(|model| model.schema.as_str())
        .chain(
            plan.cdc_excluded_relations
                .iter()
                .map(|relation| relation.schema.as_str()),
        )
        .collect::<BTreeSet<_>>();
    for schema in schemas {
        tx.batch_execute(&wamn_control_provision::sql::ensure_entity_map_sql(schema))
            .await
            .with_context(|| format!("ensure {schema}.wamn_entities"))?;
        tx.batch_execute(&wamn_control_provision::sql::ensure_cdc_exclusion_map_sql(
            schema,
        ))
        .await
        .with_context(|| format!("ensure {schema}.wamn_cdc_exclusions"))?;
    }

    for model in &plan.models {
        let relation_owner = &manifest
            .models
            .get(&model.model_id)
            .expect("the migration plan preserves every manifest model")
            .owner;
        let schema = wamn_schema_control::BareSchemaName::new(&model.schema)
            .context("validate manifest model schema for entity-map query")?;
        let mapping_sql = format!(
            "SELECT mapped.package_id, mapped.entity_id, mapped.table_name, \
                    excluded.relation_oid IS NOT NULL AS cdc_excluded \
               FROM pg_catalog.pg_class AS relation \
               JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
               LEFT JOIN {}.wamn_entities AS mapped ON mapped.relation_oid = relation.oid \
               LEFT JOIN {}.wamn_cdc_exclusions AS excluded ON excluded.relation_oid = relation.oid \
              WHERE namespace.nspname = $1 AND relation.relname = $2 \
                AND relation.relkind = 'r'",
            schema.quoted(),
            schema.quoted()
        );
        let Some(mapping) = tx
            .query_opt(&mapping_sql, &[&model.schema, &model.table])
            .await
            .with_context(|| {
                format!(
                    "read entity map {}.{} ({})",
                    model.schema, model.table, model.model_id
                )
            })?
        else {
            bail!(
                "package-model-relation-missing: {} maps {}.{} but the migration stream did not create that table",
                model.model_id,
                model.schema,
                model.table
            );
        };
        let mapped_package = mapping.get::<_, Option<String>>(0);
        let mapped_entity = mapping.get::<_, Option<String>>(1);
        let mapped_table = mapping.get::<_, Option<String>>(2);
        ensure!(
            !mapping.get::<_, bool>(3),
            "package-relation-classification-conflict: {}.{} is already CDC-excluded and cannot also map to model {}",
            model.schema,
            model.table,
            model.model_id
        );
        if let (Some(mapped_package), Some(mapped_entity)) =
            (mapped_package.as_deref(), mapped_entity.as_deref())
        {
            if mapped_package != relation_owner || mapped_entity != model.model_id.as_str() {
                bail!(
                    "package-entity-oid-rebind-refused: {}.{} is already mapped to {mapped_package}/{mapped_entity}; cannot rebind it to {}/{}",
                    model.schema,
                    model.table,
                    relation_owner,
                    model.model_id
                );
            }
            if mapped_table.as_deref() == Some(model.table.as_str()) {
                continue;
            }
        }
        let mapped = tx
            .execute(
                &wamn_control_provision::sql::upsert_entity_map_sql(&model.schema),
                &[relation_owner, &model.model_id, &model.table],
            )
            .await
            .with_context(|| {
                format!(
                    "upsert entity map {}.{} ({})",
                    model.schema, model.table, model.model_id
                )
            })?;
        ensure!(mapped == 1, "package-entity-map-write-refused");
    }

    for excluded in &plan.cdc_excluded_relations {
        let schema = wamn_schema_control::BareSchemaName::new(&excluded.schema)
            .context("validate internal relation schema for CDC-exclusion query")?;
        let mapping_sql = format!(
            "SELECT mapped.package_id, mapped.relation_id, mapped.table_name, \
                    entity.relation_oid IS NOT NULL AS entity_mapped \
               FROM pg_catalog.pg_class AS relation \
               JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
               LEFT JOIN {}.wamn_cdc_exclusions AS mapped ON mapped.relation_oid = relation.oid \
               LEFT JOIN {}.wamn_entities AS entity ON entity.relation_oid = relation.oid \
              WHERE namespace.nspname = $1 AND relation.relname = $2 \
                AND relation.relkind = 'r'",
            schema.quoted(),
            schema.quoted()
        );
        let Some(mapping) = tx
            .query_opt(&mapping_sql, &[&excluded.schema, &excluded.table])
            .await
            .with_context(|| {
                format!(
                    "read CDC exclusion map {}.{} ({})",
                    excluded.schema, excluded.table, excluded.relation_id
                )
            })?
        else {
            bail!(
                "cdc-excluded-relation-missing: {} maps {}.{}, but the migration stream did not create that table",
                excluded.relation_id,
                excluded.schema,
                excluded.table
            );
        };
        ensure!(
            !mapping.get::<_, bool>(3),
            "package-relation-classification-conflict: {}.{} is already entity-mapped and cannot also be CDC-excluded",
            excluded.schema,
            excluded.table
        );
        let mapped_package = mapping.get::<_, Option<String>>(0);
        let mapped_relation = mapping.get::<_, Option<String>>(1);
        let mapped_table = mapping.get::<_, Option<String>>(2);
        if let (Some(mapped_package), Some(mapped_relation)) =
            (mapped_package.as_deref(), mapped_relation.as_deref())
        {
            if mapped_package != manifest.package.id
                || mapped_relation != excluded.relation_id.as_str()
            {
                bail!(
                    "package-cdc-exclusion-oid-rebind-refused: {}.{} is already mapped to {mapped_package}/{mapped_relation}; cannot rebind it to {}/{}",
                    excluded.schema,
                    excluded.table,
                    manifest.package.id,
                    excluded.relation_id
                );
            }
            if mapped_table.as_deref() == Some(excluded.table.as_str()) {
                continue;
            }
        }
        let mapped = tx
            .execute(
                &wamn_control_provision::sql::upsert_cdc_exclusion_map_sql(&excluded.schema),
                &[&manifest.package.id, &excluded.relation_id, &excluded.table],
            )
            .await
            .with_context(|| {
                format!(
                    "upsert CDC exclusion {}.{} ({})",
                    excluded.schema, excluded.table, excluded.relation_id
                )
            })?;
        ensure!(mapped == 1, "package-cdc-exclusion-map-write-refused");
    }
    Ok(())
}
