use boon::{Compiler, Schemas};
use serde_json::Value;
use wamn_node_manifest::{
    CONNECTION_DESCRIPTOR_VERSION, ConnectionTypeDescriptor,
    PORTABLE_CONNECTION_REQUIREMENT_VERSION, PortableConnectionRequirement, PortableRecoveryClaim,
};

fn stable_requirement() -> PortableConnectionRequirement {
    PortableConnectionRequirement::stable_key_dedup_v1(
        ConnectionTypeDescriptor::http_v1(),
        86_400_000,
    )
}

fn published_schema_accepts(document: &Value) -> bool {
    let mut schemas = Schemas::new();
    let mut compiler = Compiler::new();
    let schema = serde_json::from_str(include_str!(
        "../../../../docs/contracts/wamn-connection-contract.schema.json"
    ))
    .expect("published connection contract schema parses");
    compiler
        .add_resource("connection-contract-schema", schema)
        .expect("connection contract schema resource");
    let schema_index = compiler
        .compile("connection-contract-schema", &mut schemas)
        .expect("connection contract schema compiles");
    schemas.validate(document, schema_index).is_ok()
}

#[test]
fn descriptor_and_requirement_have_versioned_canonical_round_trips() {
    let descriptor = ConnectionTypeDescriptor::http_v1();
    assert_eq!(descriptor.descriptor_version, CONNECTION_DESCRIPTOR_VERSION);
    let descriptor_bytes = descriptor.identity_bytes();
    let decoded_descriptor: ConnectionTypeDescriptor =
        serde_json::from_slice(&descriptor_bytes).expect("canonical descriptor decodes");
    assert_eq!(decoded_descriptor, descriptor);
    assert_eq!(decoded_descriptor.identity_bytes(), descriptor_bytes);

    let requirement = stable_requirement();
    assert_eq!(
        requirement.requirement_version,
        PORTABLE_CONNECTION_REQUIREMENT_VERSION
    );
    assert_eq!(
        requirement.recovery,
        PortableRecoveryClaim::StableKeyDedupV1 {
            minimum_retention_ms: 86_400_000
        }
    );
    let requirement_bytes = requirement.identity_bytes();
    let decoded_requirement: PortableConnectionRequirement =
        serde_json::from_slice(&requirement_bytes).expect("canonical requirement decodes");
    assert_eq!(decoded_requirement, requirement);
    assert_eq!(decoded_requirement.identity_bytes(), requirement_bytes);
}

#[test]
fn unknown_claims_parameters_and_descriptor_fields_fail_closed() {
    let requirement = stable_requirement();
    let mut value = serde_json::to_value(&requirement).expect("requirement serializes");
    value["recovery"]["claim"] = Value::String("receiver-is-safe".to_string());
    assert!(serde_json::from_value::<PortableConnectionRequirement>(value).is_err());

    let mut value = serde_json::to_value(&requirement).expect("requirement serializes");
    value["recovery"]["evidence"] = Value::String("operator-said-so".to_string());
    assert!(serde_json::from_value::<PortableConnectionRequirement>(value).is_err());

    let descriptor = ConnectionTypeDescriptor::http_v1();
    let mut value = serde_json::to_value(&descriptor).expect("descriptor serializes");
    value["authority-model"] = Value::String("guest-url".to_string());
    assert!(serde_json::from_value::<ConnectionTypeDescriptor>(value).is_err());

    let mut value = serde_json::to_value(&descriptor).expect("descriptor serializes");
    value["claims-are-trusted"] = Value::Bool(true);
    assert!(serde_json::from_value::<ConnectionTypeDescriptor>(value).is_err());
}

#[test]
fn environment_instance_values_are_unrepresentable_in_portable_forms() {
    let requirement = stable_requirement();
    let bytes = requirement.identity_bytes();
    for forbidden in [
        b"https://prod.example".as_slice(),
        b"environment-id".as_slice(),
        b"instance-generation".as_slice(),
        b"evidence-reference".as_slice(),
        b"credential-generation".as_slice(),
    ] {
        assert!(
            !bytes
                .windows(forbidden.len())
                .any(|window| window == forbidden),
            "portable bytes contain forbidden environment value {:?}",
            String::from_utf8_lossy(forbidden)
        );
    }

    for field in [
        "endpoint",
        "environment-id",
        "instance-generation",
        "evidence-reference",
        "credential-generation",
    ] {
        let mut value = serde_json::to_value(&requirement).expect("requirement serializes");
        value[field] = Value::String("forbidden".to_string());
        assert!(
            serde_json::from_value::<PortableConnectionRequirement>(value).is_err(),
            "portable requirement admitted environment field {field:?}"
        );
    }
}

#[test]
fn published_connection_contract_schema_accepts_canonical_forms() {
    let descriptor = ConnectionTypeDescriptor::http_v1();
    let never_replay = PortableConnectionRequirement::never_replay(descriptor.clone());
    let stable_key_dedup =
        PortableConnectionRequirement::stable_key_dedup_v1(descriptor.clone(), 86_400_000);

    for document in [
        serde_json::to_value(descriptor).expect("descriptor serializes"),
        serde_json::to_value(never_replay).expect("never-replay requirement serializes"),
        serde_json::to_value(stable_key_dedup).expect("stable-key requirement serializes"),
    ] {
        assert!(
            published_schema_accepts(&document),
            "published schema rejected canonical document: {document}"
        );
    }
}

#[test]
fn published_connection_contract_schema_rejects_unknown_surface() {
    let requirement = stable_requirement();

    let mut unknown_field = serde_json::to_value(&requirement).expect("requirement serializes");
    unknown_field["unexpected"] = Value::Bool(true);
    assert!(!published_schema_accepts(&unknown_field));

    let mut unknown_descriptor_field =
        serde_json::to_value(ConnectionTypeDescriptor::http_v1()).expect("descriptor serializes");
    unknown_descriptor_field["environment-default"] = Value::Bool(true);
    assert!(!published_schema_accepts(&unknown_descriptor_field));

    let mut unknown_claim = serde_json::to_value(&requirement).expect("requirement serializes");
    unknown_claim["recovery"]["claim"] = Value::String("receiver-is-safe".to_string());
    assert!(!published_schema_accepts(&unknown_claim));

    let mut unknown_parameter = serde_json::to_value(&requirement).expect("requirement serializes");
    unknown_parameter["recovery"]["retention_ms"] = Value::Number(86_400_000.into());
    assert!(!published_schema_accepts(&unknown_parameter));

    for field in ["endpoint", "generation", "evidence", "environment-id"] {
        let mut environment_field =
            serde_json::to_value(&requirement).expect("requirement serializes");
        environment_field[field] = Value::String("forbidden".to_string());
        assert!(
            !published_schema_accepts(&environment_field),
            "published schema admitted environment field {field:?}"
        );
    }
}

#[test]
fn connection_contract_schema_drift() {
    let committed = include_str!("../../../../docs/contracts/wamn-connection-contract.schema.json");
    assert_eq!(
        committed,
        wamn_node_manifest::connection_contract_json_schema_string(),
        "docs/contracts/wamn-connection-contract.schema.json is out of sync with the types; \
         regenerate: cargo run -p wamn-node-manifest --example print-connection-contract-schema > \
         docs/contracts/wamn-connection-contract.schema.json"
    );
}
