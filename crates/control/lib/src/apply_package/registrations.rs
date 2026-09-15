use std::collections::BTreeMap;

use anyhow::Context as _;
use tokio_postgres::Transaction;
use wamn_event_reg::{
    DELETE_STALE_CATALOG_REGISTRATIONS_SQL, EventRegistration, RegistrationInput,
    UPSERT_CATALOG_REGISTRATION_SQL, project_catalog_registrations,
};

pub(super) fn derive_catalog_registrations(
    manifest: &wamn_schema_generator::PackageManifest,
) -> BTreeMap<String, EventRegistration> {
    let mut declarations = BTreeMap::new();
    for (operation_key, operation) in &manifest.custom_operations {
        let Some(registration) = operation.registration() else {
            continue;
        };
        let declaration = EventRegistration {
            schema_version: wamn_event_reg::SCHEMA_VERSION.to_owned(),
            registration_id: operation_key.clone(),
            package_id: manifest.package.id.clone(),
            source_package_id: registration.source_package.clone(),
            entity: registration.entity.clone(),
            ops: registration.ops.clone(),
            input: RegistrationInput::Event,
            condition: None,
        };
        declarations.insert(operation_key.clone(), declaration);
    }
    declarations
}

pub(super) async fn reconcile_package_registrations(
    tx: &Transaction<'_>,
    tenant: &str,
    package_id: &str,
    declarations: &BTreeMap<String, EventRegistration>,
) -> anyhow::Result<bool> {
    let projection = project_catalog_registrations(package_id, declarations)
        .context("derive exact package registration rows")?;
    let mut changed = false;
    for row in &projection.rows {
        changed |= tx
            .execute(
                UPSERT_CATALOG_REGISTRATION_SQL,
                &[
                    &tenant,
                    &projection.package_id,
                    &row.registration_id,
                    &row.entity_id,
                    &row.registration_json,
                ],
            )
            .await
            .with_context(|| format!("reconcile registration {:?}", row.registration_id))?
            > 0;
    }
    changed |= tx
        .execute(
            DELETE_STALE_CATALOG_REGISTRATIONS_SQL,
            &[
                &tenant,
                &projection.package_id,
                &projection.retained_registration_ids,
            ],
        )
        .await
        .context("delete stale package registrations")?
        > 0;
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_registration_projection_does_not_require_redeclaring_the_base_entity() {
        let manifest: wamn_schema_generator::PackageManifest = serde_json::from_str(include_str!(
            "../../../../../apps/client_acme_receiving/wamn.json"
        ))
        .expect("the repository overlay manifest parses");
        wamn_schema_generator::validate_operation_vocabulary(&manifest)
            .expect("the overlay operation vocabulary is valid without base model restatement");

        let declarations = derive_catalog_registrations(&manifest);
        let registration = &declarations["quality.create_inspection"];
        assert_eq!(registration.registration_id, "quality.create_inspection");
        assert_eq!(registration.package_id, "client_acme_receiving");
        assert_eq!(registration.source_package_id, "wamn_receiving");
        assert_eq!(registration.entity, "receipt");
        assert_eq!(registration.ops, [wamn_event_reg::Op::Insert]);
    }
}
