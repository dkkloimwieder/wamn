//! Focused contracts of retained platform resources.

use serde_json::json;
use std::path::Path;
use wamn_environment_control::platform::{identity, resources};

#[test]
fn identity_chart_mounts_only_the_selected_credentials() -> anyhow::Result<()> {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    for mode in [
        identity::Mode::Foundation,
        identity::Mode::Session {
            targets: vec!["fixture-target".into()],
            nats: None,
        },
    ] {
        let foundation = matches!(mode, identity::Mode::Foundation);
        let entry = identity::chart_input(&identity::ChartInput {
            chart: &repository.join("deploy/platform/identity"),
            name: "fixture-identity",
            namespace: "fixture-identity",
            image: "wamn-identity:fixture",
            pull_policy: "Never",
            issuer: "https://fixture-identity.fixture-identity.svc.cluster.local",
            database_secret: "fixture-db",
            tls_secret: "fixture-tls",
            operator_ca_secret: "",
            resend_secret: "",
            resend_from: "",
            invite_url: "",
            mode,
        })?;
        let rendered = resources::helm_template(&entry)?;
        assert_eq!(entry["values"]["authNats"]["url"], "");
        assert_eq!(
            entry["values"]["sessionTargetSecrets"],
            if foundation {
                json!([])
            } else {
                json!(["fixture-target"])
            }
        );
        let deployment = rendered
            .iter()
            .find(|object| object["kind"] == "Deployment")
            .unwrap();
        let volumes = deployment["spec"]["template"]["spec"]["volumes"]
            .as_array()
            .unwrap();
        let variables = deployment["spec"]["template"]["spec"]["containers"][0]["env"]
            .as_array()
            .unwrap();
        assert!(
            variables
                .iter()
                .any(|variable| variable["name"] == "WAMN_IDENTITY_DATABASE_URL"
                    && variable["valueFrom"]["secretKeyRef"]["name"] == "fixture-db")
        );
        assert!(
            volumes
                .iter()
                .any(|volume| volume["secret"]["secretName"] == "fixture-tls")
        );
        assert_eq!(
            volumes
                .iter()
                .any(|volume| volume["secret"]["secretName"] == "fixture-target"),
            !foundation
        );
    }
    Ok(())
}
