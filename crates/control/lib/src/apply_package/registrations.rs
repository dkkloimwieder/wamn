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
            condition: registration.condition.clone(),
        };
        declarations.insert(operation_key.clone(), declaration);
    }
    // A workflow's registration id is its workflow id; the release names the
    // wiring it starts.
    for (workflow_id, workflow) in &manifest.workflows {
        let Some(registration) = &workflow.registration else {
            continue;
        };
        declarations.insert(
            workflow_id.clone(),
            EventRegistration {
                schema_version: wamn_event_reg::SCHEMA_VERSION.to_owned(),
                registration_id: workflow_id.clone(),
                package_id: manifest.package.id.clone(),
                source_package_id: registration.source_package.clone(),
                entity: registration.entity.clone(),
                ops: registration.ops.clone(),
                input: RegistrationInput::Event,
                condition: registration.condition.clone(),
            },
        );
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

/// Replace the package's schedule projection in the same transaction as registrations.
pub(super) async fn reconcile_package_schedules(
    tx: &Transaction<'_>,
    tenant: &str,
    manifest: &wamn_schema_generator::PackageManifest,
) -> anyhow::Result<bool> {
    let mut retained = Vec::new();
    let mut changed = false;
    for (workflow_id, workflow) in &manifest.workflows {
        let Some(schedule) = &workflow.schedule else {
            continue;
        };
        retained.push(workflow_id.as_str());
        changed |= tx.execute(
            "INSERT INTO catalog.workflow_schedules AS current \
             (tenant_id, package_id, workflow_id, schedule) VALUES ($1, $2, $3, $4) \
             ON CONFLICT (tenant_id, package_id, workflow_id) DO UPDATE \
             SET schedule = EXCLUDED.schedule WHERE current.schedule IS DISTINCT FROM EXCLUDED.schedule",
            &[&tenant, &manifest.package.id, &workflow_id, &schedule],
        ).await.with_context(|| format!("reconcile schedule {workflow_id:?}"))? > 0;
    }
    changed |= tx
        .execute(
            "DELETE FROM catalog.workflow_schedules WHERE tenant_id = $1 AND package_id = $2 \
         AND workflow_id <> ALL($3::text[])",
            &[&tenant, &manifest.package.id, &retained],
        )
        .await
        .context("delete stale package schedules")?
        > 0;
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_registration_projection_does_not_require_redeclaring_the_base_entity() {
        let manifest: wamn_schema_generator::PackageManifest = serde_json::from_str(include_str!(
            "../../tests/fixtures/apply_package/overlay/wamn.json"
        ))
        .expect("the overlay fixture manifest parses");
        wamn_schema_generator::validate_operation_vocabulary(&manifest)
            .expect("the overlay operation vocabulary is valid without base model restatement");

        let declarations = derive_catalog_registrations(&manifest);
        let registration = &declarations["quality.create_inspection"];
        assert_eq!(registration.registration_id, "quality.create_inspection");
        assert_eq!(registration.package_id, "client_overlay_inventory");
        assert_eq!(registration.source_package_id, "wamn_inventory");
        assert_eq!(registration.entity, "rack");
        assert_eq!(registration.ops, [wamn_event_reg::Op::Insert]);
    }

    /// The observer fixture with one workflow merged into its `workflows`.
    fn observer_with_workflow(id: &str, workflow: serde_json::Value) -> serde_json::Value {
        let mut document: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/observer_package/wamn.json"
        ))
        .expect("the observer fixture manifest parses");
        document["workflows"] = serde_json::json!({});
        document["workflows"][id] = workflow;
        document
    }

    fn parsed(document: serde_json::Value) -> wamn_schema_generator::PackageManifest {
        serde_json::from_value(document).expect("the workflow manifest parses")
    }

    #[test]
    fn a_workflow_is_a_package_registration_named_by_its_workflow_id() {
        let manifest = parsed(observer_with_workflow(
            "item_label",
            serde_json::json!({"wiring": "item_label",
                "registration": {"source_package": "source_fixture", "entity": "item", "ops": ["insert"]}}),
        ));
        wamn_schema_generator::validate_operation_vocabulary(&manifest)
            .expect("a workflow on an installed source package is valid");

        let declarations = derive_catalog_registrations(&manifest);
        let registration = &declarations["item_label"];
        assert_eq!(registration.registration_id, "item_label");
        assert_eq!(registration.package_id, "observer_fixture");
        assert_eq!(registration.source_package_id, "source_fixture");
        assert_eq!(registration.entity, "item");
        assert_eq!(registration.ops, [wamn_event_reg::Op::Insert]);
        assert!(
            declarations.contains_key("audit.observe"),
            "the event handler keeps its own registration"
        );
    }

    #[test]
    fn schedule_requires_exactly_one_valid_five_field_trigger() {
        let registration =
            serde_json::json!({"source_package":"source_fixture","entity":"item","ops":["insert"]});
        for workflow in [
            serde_json::json!({"wiring":"item_label"}),
            serde_json::json!({"wiring":"item_label","schedule":"* * * * *","registration":registration}),
        ] {
            let manifest = parsed(observer_with_workflow("item_label", workflow));
            let error =
                wamn_schema_generator::validate_operation_vocabulary(&manifest).unwrap_err();
            assert!(error.to_string().contains("workflow-trigger-exclusive"));
        }
        for cron in [
            "",
            "@daily",
            "* * * *",
            "* * * * * *",
            "0 0 * * * 2027",
            "61 * * * *",
            "bad * * * *",
        ] {
            let manifest = parsed(observer_with_workflow(
                "item_label",
                serde_json::json!({"wiring":"item_label","schedule":cron}),
            ));
            let error =
                wamn_schema_generator::validate_operation_vocabulary(&manifest).unwrap_err();
            assert!(
                error.to_string().contains("workflow-schedule-invalid"),
                "{cron}: {error}"
            );
        }
        let manifest = parsed(observer_with_workflow(
            "item_label",
            serde_json::json!({"wiring":"item_label","schedule":"*/5 * * * *"}),
        ));
        wamn_schema_generator::validate_operation_vocabulary(&manifest).unwrap();
        assert!(!derive_catalog_registrations(&manifest).contains_key("item_label"));
    }

    #[test]
    fn a_workflow_declaration_refuses_by_name() {
        for (id, workflow, fact) in [
            (
                "audit.observe",
                serde_json::json!({"wiring": "item_label", "registration":
                    {"source_package": "source_fixture", "entity": "item", "ops": ["insert"]}}),
                "workflow id `audit.observe` must be singular snake_case",
            ),
            (
                "item_label",
                serde_json::json!({"wiring": "item_label", "registration":
                    {"source_package": "absent_fixture", "entity": "item", "ops": ["insert"]}}),
                "workflow item_label source package absent_fixture is not installed",
            ),
            (
                "item_label",
                serde_json::json!({"wiring": "item_label", "registration":
                    {"source_package": "source_fixture", "entity": "item", "ops": []}}),
                "workflow item_label registration must declare at least one op",
            ),
        ] {
            let error = wamn_schema_generator::validate_operation_vocabulary(&parsed(
                observer_with_workflow(id, workflow),
            ))
            .expect_err("an invalid workflow declaration was accepted");
            assert!(
                error.to_string().contains(fact),
                "{error} does not name {fact:?}"
            );
        }
    }
}
