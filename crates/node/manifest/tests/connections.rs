use serde_json::Value;
use wamn_node_manifest::{CONNECTION_DESCRIPTOR_VERSION, ConnectionTypeDescriptor};

#[test]
fn descriptor_has_a_versioned_canonical_round_trip() {
    let descriptor = ConnectionTypeDescriptor::http_v1();
    assert_eq!(descriptor.descriptor_version, CONNECTION_DESCRIPTOR_VERSION);
    let bytes = descriptor.identity_bytes();
    let decoded: ConnectionTypeDescriptor =
        serde_json::from_slice(&bytes).expect("canonical descriptor decodes");
    assert_eq!(decoded, descriptor);
    assert_eq!(decoded.identity_bytes(), bytes);
}

#[test]
fn unknown_authority_and_descriptor_fields_fail_closed() {
    let descriptor = ConnectionTypeDescriptor::http_v1();
    let mut value = serde_json::to_value(&descriptor).expect("descriptor serializes");
    value["authority-model"] = Value::String("guest-url".to_string());
    assert!(serde_json::from_value::<ConnectionTypeDescriptor>(value).is_err());

    let mut value = serde_json::to_value(&descriptor).expect("descriptor serializes");
    value["claims-are-trusted"] = Value::Bool(true);
    assert!(serde_json::from_value::<ConnectionTypeDescriptor>(value).is_err());
}
