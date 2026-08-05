//! Static contract guards for the connection-backed credential proof fixtures.

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    const FLOW_JSON: &str = include_str!("../../../deploy/cred/notify.flow.json");
    const FLOW_ID: &str = "cred-notify";
    const DENY_FLOW_JSON: &str = include_str!("../../../deploy/cred/deny.flow.json");
    const DENY_FLOW_ID: &str = "egress-deny";
    const DEMO_TOKEN: &str = "wamn-cred-proof-7f3a9b2e41d05c68";
    const DEMO_SECRET: &str = "Bearer wamn-cred-proof-7f3a9b2e41d05c68";
    const CREDENTIAL_NAME: &str = "notify-token";
    const CONNECTION_NAME: &str = "notify-endpoint";

    fn expected_connection_requirement() -> wamn_flow::FlowConnectionRequirement {
        wamn_flow::FlowConnectionRequirement {
            name: CONNECTION_NAME.to_string(),
            requirement: wamn_node_manifest::PortableConnectionRequirement::never_replay(
                wamn_node_manifest::ConnectionTypeDescriptor::http_v1(),
            ),
        }
    }

    fn resolved_interfaces() -> wamn_flow::ResolvedInterfaces {
        std::collections::BTreeMap::from([
            ("http-request".to_string(), vec!["main".to_string()]),
            ("transform".to_string(), vec!["main".to_string()]),
        ])
    }

    #[test]
    fn credential_fixtures_use_exactly_one_rev18_request_entry() {
        for (name, source) in [(FLOW_ID, FLOW_JSON), (DENY_FLOW_ID, DENY_FLOW_JSON)] {
            let value: Value = serde_json::from_str(source).expect("fixture parses");
            assert!(value.get("trigger").is_none());
            assert!(value.get("entry").is_none());

            let entries: Vec<&Value> = value["nodes"]
                .as_array()
                .expect("nodes array")
                .iter()
                .filter(|node| {
                    node["type"]
                        .as_str()
                        .is_some_and(|kind| matches!(kind, "request" | "cron" | "event"))
                })
                .collect();
            assert_eq!(entries.len(), 1, "{name} must have exactly one entry");
            assert_eq!(entries[0]["id"], json!("in"));
            assert_eq!(entries[0]["type"], json!("request"));

            let flow = wamn_flow::Flow::from_json(source).expect("fixture is a wamn-flow");
            flow.validate(&resolved_interfaces())
                .expect("fixture validates");
        }
    }

    #[test]
    fn positive_fixture_uses_only_a_portable_connection() {
        let value: Value = serde_json::from_str(FLOW_JSON).expect("fixture parses");
        let flow = wamn_flow::Flow::from_json(FLOW_JSON).expect("fixture is a wamn-flow");
        flow.validate(&resolved_interfaces())
            .expect("fixture validates");

        assert_eq!(flow.flow_id, FLOW_ID);
        assert_eq!(
            flow.connection_requirements,
            vec![expected_connection_requirement()]
        );
        assert!(flow.credentials.is_empty());
        assert!(flow.allowed_hosts.is_empty());

        let nodes = value["nodes"].as_array().expect("nodes array");
        assert_eq!(nodes.len(), 4, "in -> notify -> status -> out");
        assert_eq!(nodes[1]["id"], json!("notify"));
        assert_eq!(nodes[1]["type"], json!("http-request"));
        assert_eq!(nodes[1]["connection"], json!(CONNECTION_NAME));
        assert!(nodes[1].get("credential").is_none());
        assert!(nodes[1]["config"].get("url").is_none());
        assert_eq!(nodes[1]["config"]["path-and-query"], json!("/credproof"));
        assert_eq!(nodes[2]["config"]["expression"], json!("status"));
        assert_eq!(nodes[3]["type"], json!("respond"));
        assert!(!FLOW_JSON.contains(DEMO_SECRET));
        assert!(!FLOW_JSON.contains("serve-echo:8091"));
        assert!(!FLOW_JSON.contains("127.0.0.1:8093"));
    }

    #[test]
    fn deny_fixture_has_no_environment_binding_material() {
        let value: Value = serde_json::from_str(DENY_FLOW_JSON).expect("fixture parses");
        let flow = wamn_flow::Flow::from_json(DENY_FLOW_JSON).expect("fixture is a wamn-flow");
        flow.validate(&resolved_interfaces())
            .expect("fixture validates");

        assert_eq!(flow.flow_id, DENY_FLOW_ID);
        assert_eq!(
            flow.connection_requirements,
            vec![expected_connection_requirement()]
        );
        assert!(flow.allowed_hosts.is_empty());
        assert!(flow.credentials.is_empty());
        assert_eq!(value["nodes"][1]["connection"], json!(CONNECTION_NAME));
        assert_eq!(
            value["nodes"][1]["config"]["path-and-query"],
            json!("/credproof")
        );
        assert!(value["nodes"][1]["config"].get("url").is_none());
        assert!(value["nodes"][1].get("credential").is_none());
    }

    #[test]
    fn example_runner_secret_matches_the_demo_mapping() {
        let manifest = include_str!("../../../deploy/platform/runner-credentials.example.yaml");
        assert!(manifest.contains(CREDENTIAL_NAME));
        assert!(manifest.contains(DEMO_TOKEN));
        assert!(manifest.contains(DEMO_SECRET));
        assert!(manifest.contains(r#"{\"headers\":{\"authorization\":\"Bearer "#));
        assert!(manifest.contains("\"default\""));
    }
}
