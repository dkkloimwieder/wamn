use super::*;
use serde_json::json;

#[tokio::test]
async fn stage_evidence_is_strict_and_unimplemented_execution_preserves_data() {
    let mut fixture = fixture("stage-format", false).await;
    let original = state(&mut fixture.source).await;
    let result = qualify_upgrade_with_observer(
        request(&fixture, "additive.json"),
        WorkloadObserver::Captured(fixture.serving.clone()),
    )
    .await
    .unwrap();
    let (mut evidence, additive_bytes, _) = read_qualification(&result.result).unwrap();
    assert_eq!(evidence.format_version, 1);
    assert!(evidence.upgrade_stage.is_none());
    let additive_value: serde_json::Value = serde_json::from_slice(&additive_bytes).unwrap();
    assert!(additive_value.get("upgrade_stage").is_none());

    let declaration = json!({
        "phase": "backfill",
        "preconditions": {"expanded": "SELECT true"},
        "postconditions": {"complete": "SELECT true"},
        "backfill": {
            "sql": "SELECT $1::jsonb AS next_cursor, true AS complete WHERE $2::int4 > 0",
            "batch_size": 100,
            "initial_cursor": null
        }
    });
    evidence.format_version = 3;
    evidence.upgrade_stage = Some(serde_json::from_value(declaration.clone()).unwrap());
    let staged = serde_json::to_value(&evidence).unwrap();
    let stage_bytes = wamn_execution_contract::canonical_json_bytes(&staged);
    assert_eq!(decode_qualification(&stage_bytes).unwrap(), evidence);
    assert_ne!(bytes_digest(&stage_bytes), bytes_digest(&additive_bytes));

    for (path, replacement) in [
        ("/format_version", json!(1)),
        ("/upgrade_stage/phase", json!("manual")),
        ("/upgrade_stage/backfill/batch_size", json!(0)),
        ("/upgrade_stage/preconditions", json!({})),
        ("/upgrade_stage/postconditions", json!({})),
        ("/candidate_package/predecessor_version", json!("wrong")),
    ] {
        let mut changed = staged.clone();
        *changed.pointer_mut(path).unwrap() = replacement;
        let bytes = wamn_execution_contract::canonical_json_bytes(&changed);
        assert!(decode_qualification(&bytes).is_err(), "accepted {path}");
    }
    let mut missing_cursor = staged.clone();
    missing_cursor["upgrade_stage"]["backfill"]
        .as_object_mut()
        .unwrap()
        .remove("initial_cursor");
    assert!(
        decode_qualification(&wamn_execution_contract::canonical_json_bytes(
            &missing_cursor
        ))
        .is_err()
    );

    let candidate = fixture.root.join("candidate");
    let manifest_path = wamn_schema_generator::package_manifest_path(&candidate);
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["upgrade_stage"] = declaration;
    fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let declared = identity_from_directory(&read_package_directory(&candidate).unwrap()).unwrap();
    assert_ne!(
        declared.manifest_sha256,
        evidence.candidate_package.manifest_sha256
    );

    let refused = request(&fixture, "stage.json");
    let output = refused.result.clone();
    let error =
        qualify_upgrade_with_observer(refused, WorkloadObserver::Captured(fixture.serving.clone()))
            .await
            .unwrap_err();
    assert!(
        format!("{error:#}").contains("stage executor is not available"),
        "{error:#}"
    );
    assert!(!output.exists());
    let error = crate::apply_package::apply_package(apply_request(
        fixture.source_database.url(),
        &candidate,
    ))
    .await
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("stage executor is not available"),
        "{error:#}"
    );
    assert_eq!(state(&mut fixture.source).await, original);
    fixture.server.stop().unwrap();
}
