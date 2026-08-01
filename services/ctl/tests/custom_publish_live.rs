//! Live publication proof for verified custom-node artifacts (`wamn-5wd1.67`).
//!
//! The test is inert unless the throwaway PostgreSQL URL and all three real
//! POC component paths are supplied by the canonical build recipe.

use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};
use tokio_postgres::NoTls;
use wamn_ctl::publish_catalog::{self, PublishCatalogArgs};

const COMPONENTS: [(&str, &str, &str); 3] = [
    (
        "normalize-receipt",
        "WAMN_CF_NORMALIZE_RECEIPT_WASM",
        "Normalize Receipt",
    ),
    (
        "evaluate-specs",
        "WAMN_CF_EVALUATE_SPECS_WASM",
        "Evaluate Receipt Specifications",
    ),
    (
        "disposition-recommendation",
        "WAMN_CF_DISPOSITION_NODE_WASM",
        "Disposition Recommendation",
    ),
];

fn digest(bytes: &[u8]) -> String {
    let value = Sha256::digest(bytes);
    let hex = value
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("sha256:{hex}")
}

fn write_inputs(directory: &Path) -> Vec<PathBuf> {
    COMPONENTS
        .iter()
        .map(|(node_type, environment, name)| {
            let component = PathBuf::from(
                std::env::var(environment)
                    .unwrap_or_else(|_| panic!("{environment} must name the real component")),
            );
            let bytes = std::fs::read(&component).unwrap();
            let manifest = directory.join(format!("{node_type}.manifest.json"));
            std::fs::write(
                &manifest,
                serde_json::json!({
                    "schema-version": "0.1",
                    "node-type": node_type,
                    "name": name,
                    "version": "0.1.0",
                    "contract": "0.1.0",
                    "ordering": ["unordered"],
                    "purity": "pure"
                })
                .to_string(),
            )
            .unwrap();
            let descriptor = directory.join(format!("{node_type}.component.json"));
            std::fs::write(
                &descriptor,
                serde_json::json!({
                    "node-type": node_type,
                    "component": component,
                    "manifest": manifest,
                    "component-digest": digest(&bytes),
                })
                .to_string(),
            )
            .unwrap();
            descriptor
        })
        .collect()
}

fn publish_args(
    url: &str,
    tenant: &str,
    catalog: &Path,
    flow: &Path,
    custom_node: Vec<PathBuf>,
) -> PublishCatalogArgs {
    PublishCatalogArgs {
        catalog: catalog.to_path_buf(),
        admin_database_url: Some(url.to_string()),
        tenant: tenant.to_string(),
        project_config: None,
        schema: "public".to_string(),
        provision: false,
        runstate: false,
        seed_dataset: None,
        flow: vec![flow.to_path_buf()],
        custom_node,
        exposure: None,
        skip_reconcile_replica_identity: true,
    }
}

#[tokio::test]
async fn real_f1_f2_components_publish_retry_and_conflict_by_exact_bytes() {
    let Ok(url) = std::env::var("WAMN_MIGRATE_PG_URL") else {
        return;
    };
    if COMPONENTS
        .iter()
        .any(|(_, environment, _)| std::env::var(environment).is_err())
    {
        return;
    }
    let suffix = std::process::id();
    let tenant = format!("custom-publish-{suffix}");
    let catalog_id = format!("custom-publish-{suffix}");
    let directory = std::env::temp_dir().join(&tenant);
    std::fs::create_dir_all(&directory).unwrap();
    let catalog = directory.join("catalog.json");
    std::fs::write(
        &catalog,
        format!(
            r#"{{"schema-version":"0.1","catalog-id":"{catalog_id}","version":1,"entities":[]}}"#
        ),
    )
    .unwrap();
    let flow = directory.join("flow.json");
    std::fs::write(
        &flow,
        r#"{
          "schema-version":"0.1","flow-id":"custom-proof","version":1,
          "nodes":[
            {"id":"request","type":"request","config":{"input-schema":{
              "$schema":"https://json-schema.org/draft/2020-12/schema","type":"object"
            }}},
            {"id":"normalize","type":"normalize-receipt"},
            {"id":"evaluate","type":"evaluate-specs"},
            {"id":"recommend","type":"disposition-recommendation"},
            {"id":"respond","type":"respond","config":{"status":200}}
          ],
          "edges":[
            {"from":"request","to":"normalize"},
            {"from":"normalize","to":"evaluate"},
            {"from":"evaluate","to":"recommend"},
            {"from":"recommend","to":"respond"}
          ]
        }"#,
    )
    .unwrap();
    let descriptors = write_inputs(&directory);

    publish_catalog::run(publish_args(
        &url,
        &tenant,
        &catalog,
        &flow,
        descriptors.clone(),
    ))
    .await
    .expect("publish exact real component inputs");

    let (client, connection) = tokio_postgres::connect(&url, NoTls).await.unwrap();
    let connection_task = tokio::spawn(connection);
    let row = client
        .query_one(
            "SELECT interface_bundle_json::text, component_digests::text, \
                    occurrence_recovery_json, occurrence_recovery_hash, artifact_hash \
             FROM catalog.flow_artifacts \
             WHERE tenant_id = $1 AND flow_id = 'custom-proof' AND flow_version = 1",
            &[&tenant],
        )
        .await
        .unwrap();
    let interfaces: serde_json::Value =
        serde_json::from_str(row.get::<_, String>(0).as_str()).unwrap();
    let components: serde_json::Value =
        serde_json::from_str(row.get::<_, String>(1).as_str()).unwrap();
    assert_eq!(interfaces.as_array().unwrap().len(), 3);
    assert_eq!(components.as_array().unwrap().len(), 3);
    for (node_type, environment, _) in COMPONENTS {
        let interface = interfaces
            .as_array()
            .unwrap()
            .iter()
            .find(|value| value["node-type"] == node_type)
            .unwrap();
        assert_eq!(interface["output-ports"], serde_json::json!(["main"]));
        assert_eq!(interface["purity"], "pure");
        assert_eq!(interface["recovery-class"], "replay");
        let expected = digest(&std::fs::read(std::env::var(environment).unwrap()).unwrap());
        let component = components
            .as_array()
            .unwrap()
            .iter()
            .find(|value| value["interface"]["node-type"] == node_type)
            .unwrap();
        assert_eq!(component["component-digest"], expected);
    }
    let occurrence_recovery_json: String = row.get(2);
    let occurrence_recovery: serde_json::Value =
        serde_json::from_str(&occurrence_recovery_json).unwrap();
    assert_eq!(occurrence_recovery.as_array().unwrap().len(), 3);
    assert_eq!(
        row.get::<_, String>(3),
        digest(occurrence_recovery_json.as_bytes())
    );
    let artifact_hash: String = row.get(4);

    publish_catalog::run(publish_args(
        &url,
        &tenant,
        &catalog,
        &flow,
        descriptors.clone(),
    ))
    .await
    .expect("exact retry converges");
    let retried_hash: String = client
        .query_one(
            "SELECT artifact_hash FROM catalog.flow_artifacts \
             WHERE tenant_id = $1 AND flow_id = 'custom-proof' AND flow_version = 1",
            &[&tenant],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(retried_hash, artifact_hash);

    let first_descriptor: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&descriptors[0]).unwrap()).unwrap();
    let original = PathBuf::from(first_descriptor["component"].as_str().unwrap());
    let mut changed = std::fs::read(original).unwrap();
    changed.push(0);
    let changed_component = directory.join("normalize-receipt.changed.wasm");
    std::fs::write(&changed_component, &changed).unwrap();
    let changed_descriptor = directory.join("normalize-receipt.changed.component.json");
    let mut changed_input = first_descriptor;
    changed_input["component"] = serde_json::Value::String(changed_component.display().to_string());
    changed_input["component-digest"] = serde_json::Value::String(digest(&changed));
    std::fs::write(&changed_descriptor, changed_input.to_string()).unwrap();
    let mut conflicting = descriptors;
    conflicting[0] = changed_descriptor;
    let error = publish_catalog::run(publish_args(&url, &tenant, &catalog, &flow, conflicting))
        .await
        .expect_err("changed component bytes at one flow version conflict");
    assert!(
        format!("{error:#}").contains("content-conflict"),
        "{error:#}"
    );

    drop(client);
    let _ = connection_task.await;
}
