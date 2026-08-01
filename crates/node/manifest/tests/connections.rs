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
