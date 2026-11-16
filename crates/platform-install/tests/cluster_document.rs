//! Cluster document compilation and refusal through the public interface.

use serde_json::{Value, json};
use wamn_platform_install::platform::document::{ClusterDocument, kcl_value};

fn document() -> Value {
    json!({
        "provider": "gcp", "project_id": "wamn-dev", "zone": "us-central1-a",
        "cluster_name": "wamn",
        "org": "dkk", "owner_email": "dkkloimwieder@gmail.com",
        "registry": "us-central1-docker.pkg.dev/wamn-dev/wamn",
        "build_cloud_cap_usd": 3.0,
        "buckets": {"web": "wamn-dev-web", "otel": "wamn-dev-otel", "state": "wamn-dev-tofu-state"},
        "route_hosts": {"control": "control.wamn.dev"},
        "resend_from": "noreply@wamn.dev"
    })
}

fn compile(source: &str) -> anyhow::Result<ClusterDocument> {
    let directory = tempfile::tempdir()?;
    let file = directory.path().join("cluster.k");
    std::fs::write(&file, source)?;
    ClusterDocument::compile(&file)
}

#[test]
#[ignore = "requires: bin:kcl"]
fn the_kcl_form_compiles_back_to_the_same_document() {
    let parsed = ClusterDocument::from_json(document().to_string().as_bytes()).unwrap();
    let compiled = compile(&parsed.to_kcl()).expect("the document compiles");
    assert_eq!(compiled, parsed);
}

#[test]
#[ignore = "requires: bin:kcl"]
fn kcl_and_json_refuse_fields_the_schema_lacks() {
    for pointer in ["", "/buckets", "/route_hosts"] {
        let mut unknown = document();
        unknown.pointer_mut(pointer).unwrap()["unknown"] = json!("refused");
        let source = format!("import cluster\ncluster.Cluster {}\n", kcl_value(&unknown));
        assert!(compile(&source).is_err(), "KCL accepted {pointer}/unknown");
        let _error = ClusterDocument::from_json(unknown.to_string().as_bytes()).unwrap_err();
    }
}

#[test]
#[ignore = "requires: bin:kcl"]
fn kcl_and_json_refuse_bad_values() {
    for (pointer, value) in [
        ("/provider", json!("aws")),
        ("/build_cloud_cap_usd", json!(-1.0)),
        ("/project_id", json!("bad")),
        ("/zone", json!("us-central1")),
        ("/cluster_name", json!("Bad")),
        ("/org", json!("Bad")),
        ("/owner_email", json!("owner")),
        ("/resend_from", json!("no reply@wamn.dev")),
        ("/registry", json!("https://registry.example")),
        ("/buckets/web", json!("")),
        ("/route_hosts/control", json!("bad..host")),
    ] {
        let mut bad = document();
        *bad.pointer_mut(pointer).unwrap() = value;
        let source = format!("import cluster\ncluster.Cluster {}\n", kcl_value(&bad));
        assert!(compile(&source).is_err(), "KCL accepted {pointer}");
        assert!(
            ClusterDocument::from_json(bad.to_string().as_bytes()).is_err(),
            "JSON accepted {pointer}"
        );
    }
}

#[test]
fn canonical_bytes_do_not_depend_on_key_order() {
    fn reversed(value: &Value) -> String {
        match value {
            Value::Object(fields) => format!(
                "{{{}}}",
                fields
                    .iter()
                    .rev()
                    .map(|(key, value)| {
                        format!(
                            "{}:{}",
                            serde_json::to_string(key).unwrap(),
                            reversed(value)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            Value::Array(items) => format!(
                "[{}]",
                items.iter().map(reversed).collect::<Vec<_>>().join(",")
            ),
            _ => value.to_string(),
        }
    }
    let value = document();
    let ordered = value.to_string();
    let reordered = reversed(&value);
    assert_ne!(ordered, reordered);
    let a = ClusterDocument::from_json(ordered.as_bytes()).unwrap();
    let b = ClusterDocument::from_json(reordered.as_bytes()).unwrap();
    assert_eq!(a.canonical_json().unwrap(), b.canonical_json().unwrap());
}

#[test]
#[ignore = "requires: bin:kcl"]
fn the_local_provider_needs_no_gcp_resources() {
    let mut value = document();
    value["provider"] = json!("kind");
    for field in ["project_id", "zone"] {
        value[field] = json!("");
    }
    value["buckets"] = json!({"web": "", "otel": "", "state": ""});
    value["registry"] = json!("localhost:5000/wamn");
    let parsed = ClusterDocument::from_json(value.to_string().as_bytes()).unwrap();
    let compiled = compile(&parsed.to_kcl()).expect("type compiles without GCP resources");
    assert_eq!(compiled, parsed);
}
