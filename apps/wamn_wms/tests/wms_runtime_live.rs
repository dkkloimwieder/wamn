//! Shared fixture and its local tests.

include!("wms_runtime_live_fixture.rs");
#[cfg(test)]
mod shape {
    use super::*;

    fn runtime() -> RuntimePhase {
        RuntimePhase {
            packaging_id: "00000000-0000-0000-0000-000000000301".to_owned(),
            to_location_id: "00000000-0000-0000-0000-000000000202".to_owned(),
        }
    }

    #[test]
    fn a_move_body_is_the_array_envelope_the_route_admits() {
        let body = move_body("r", "k", &runtime(), 1);
        let request_elements = body.as_array().expect("array envelope");
        assert_eq!(request_elements.len(), 1);
        assert_eq!(request_elements[0]["request_id"], "r");
        let value = &request_elements[0]["value"];
        for key in [
            "idempotency_key",
            "packaging_id",
            "to_location_id",
            "expected_row_version",
            "occurred_at",
        ] {
            assert!(value.get(key).is_some(), "value carries {key}");
        }
        assert_eq!(value["expected_row_version"], 1);
        assert_eq!(
            value.as_object().expect("object").len(),
            5,
            "exactly the declared fields, nothing extra"
        );
    }

    #[test]
    fn a_request_element_must_answer_the_request_it_was_asked_with() {
        let answer = json!([{"request_id": "other", "value": {}}]);
        assert!(request_element(&answer, "mine").is_err());
        let answer =
            json!([{"request_id": "mine", "value": {}}, {"request_id": "mine", "value": {}}]);
        assert!(
            request_element(&answer, "mine").is_err(),
            "two request elements for one request is refused"
        );
        let answer = json!([{"request_id": "mine", "value": {}}]);
        assert!(request_element(&answer, "mine").is_ok());
    }

    #[test]
    fn a_path_is_appended_to_the_endpoint_without_a_double_slash() {
        let route = Route {
            endpoint: "http://10.0.0.2:30999/".trim_end_matches('/').to_owned(),
            host: "wms.localhost".to_owned(),
            bearer: "t".to_owned(),
        };
        assert_eq!(
            format!("{}{}", route.endpoint, "/inventory/move"),
            "http://10.0.0.2:30999/inventory/move"
        );
    }
}
