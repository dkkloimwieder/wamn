use std::time::Duration;

use serde_json::json;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio_postgres::{Client, NoTls};
use wamn_catalog::{PackageCoordinate, ServingManifest};

use crate::print_release_env::{ReleaseCarrier, ReleaseSnapshot};
use crate::push_release_manifest::PushReleaseManifestRequest;

use super::{
    SELECT_RELEASE, argument, authenticated_interaction, claim, require_compatible_schema,
    require_released_route, require_selected, require_supplied_fields,
};

mod vector {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../crates/catalog/model/tests/fixtures/release_manifest_mint_vector.rs"
    ));
}

#[test]
fn native_inputs_admit_kubernetes_defaults_but_refuse_changed_qualified_fields() {
    let qualified = json!([{"name":"host","image":"host@sha256:abc","args":["--release-manifest-digest=sha256:123"]}]);
    let mut actual = qualified.clone();
    actual[0]["imagePullPolicy"] = json!("IfNotPresent");
    require_supplied_fields(&qualified, &actual).unwrap();
    actual[0]["image"] = json!("host:latest");
    assert!(require_supplied_fields(&qualified, &actual).is_err());
    actual = qualified.clone();
    actual
        .as_array_mut()
        .unwrap()
        .push(json!({"name":"unqualified-sidecar"}));
    assert!(require_supplied_fields(&qualified, &actual).is_err());
    let repeated = json!([
        "--release-manifest-digest=one",
        "--release-manifest-digest",
        "two"
    ]);
    assert!(argument(repeated.as_array().unwrap(), "--release-manifest-digest").is_err());
}

#[test]
fn acceptance_route_requires_the_selected_pat_operation() {
    let (mut manifest, digest) =
        ServingManifest::from_canonical_bytes(vector::CANONICAL_BYTES).unwrap();
    assert_eq!(digest.as_str(), vector::DIGEST);
    let attachment = manifest
        .workflow
        .attachments
        .get_mut("orders-http")
        .unwrap();
    attachment.definition["route"] =
        json!({"path":"/orders","method":"POST","host":"fixture.localhost"});
    assert_eq!(
        require_released_route(
            &manifest,
            "http://127.0.0.1:1234/orders",
            "fixture.localhost",
        )
        .unwrap(),
        "POST",
        "a wiring is a POST"
    );
    manifest
        .workflow
        .attachments
        .get_mut("orders-http")
        .unwrap()
        .definition["route"]["method"] = json!("GET");
    assert!(
        require_released_route(
            &manifest,
            "http://127.0.0.1:1234/orders",
            "fixture.localhost"
        )
        .is_err(),
        "a method that the kind does not give refuses"
    );
    manifest
        .workflow
        .attachments
        .get_mut("orders-http")
        .unwrap()
        .definition["route"]["method"] = json!("POST");
    assert!(
        require_released_route(
            &manifest,
            "http://127.0.0.1:1234/other",
            "fixture.localhost"
        )
        .is_err()
    );
    assert!(
        require_released_route(
            &manifest,
            "http://127.0.0.1:1234/orders",
            "another.localhost"
        )
        .is_err()
    );
    manifest
        .workflow
        .attachments
        .get_mut("orders-http")
        .unwrap()
        .auth_policy = json!({"modes":["none"]});
    assert!(
        require_released_route(
            &manifest,
            "http://127.0.0.1:1234/orders",
            "fixture.localhost"
        )
        .is_err()
    );
}

#[tokio::test]
async fn authenticated_acceptance_refuses_denial_redirect_and_wrong_result() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/orders", listener.local_addr().unwrap());
    let serving = tokio::spawn(async move {
        for (status, body, line) in [
            (401, "{}", "post /orders "),
            (302, "{\"ok\":true}", "post /orders "),
            (200, "{\"ok\":false}", "post /orders "),
            (200, "{\"ok\":true}", "post /orders "),
            (200, "{\"ok\":true}", "get /orders?id=%22a%22 "),
        ] {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 8192];
            let read = stream.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..read]).to_ascii_lowercase();
            assert!(request.starts_with(line), "{request}");
            assert!(request.contains("authorization: bearer test-token"));
            assert!(request.contains("host: fixture.localhost"));
            stream.write_all(format!("HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        }
    });
    for _ in 0..3 {
        assert!(
            authenticated_interaction(
                &url,
                "fixture.localhost",
                "test-token",
                "POST",
                b"{}".to_vec(),
                json!({"ok":true})
            )
            .await
            .is_err()
        );
    }
    authenticated_interaction(
        &url,
        "fixture.localhost",
        "test-token",
        "POST",
        b"{}".to_vec(),
        json!({"ok":true}),
    )
    .await
    .unwrap();
    authenticated_interaction(
        &url,
        "fixture.localhost",
        "test-token",
        "GET",
        br#"[{"id":"a"}]"#.to_vec(),
        json!({"ok":true}),
    )
    .await
    .unwrap();
    serving.await.unwrap();
}

async fn seed_package(
    client: &Client,
    version: &str,
    predecessor: Option<&str>,
    migration: char,
    wiring: char,
) {
    client.execute("INSERT INTO catalog.packages (tenant_id,package_id,package_version,manifest_sha256,predecessor_version) VALUES ('delivery','inventory',$1,$2,$3)",
        &[&version, &format!("sha256:{}", migration.to_string().repeat(64)), &predecessor]).await.unwrap();
    client.execute("INSERT INTO catalog.package_migrations (tenant_id,package_id,package_version,ordinal,relative_path,sha256) VALUES ('delivery','inventory',$1,1,'migrations/0001_initial.sql',$2)",
        &[&version, &format!("sha256:{}", migration.to_string().repeat(64))]).await.unwrap();
    client.execute("INSERT INTO catalog.wirings (tenant_id,package_id,package_version,wiring_id,version,graph_json,wiring_hash) VALUES ('delivery','inventory',$1,'orders',1,'{}',$2)",
        &[&version, &format!("sha256:{}", wiring.to_string().repeat(64))]).await.unwrap();
}

/// The tenant and environment every selection in these tests names.
fn release_request() -> PushReleaseManifestRequest {
    PushReleaseManifestRequest {
        database_url: String::new(),
        org: String::new(),
        project: String::new(),
        environment: "test".to_owned(),
        tenant: "delivery".to_owned(),
        manifest_digest: String::new(),
        artifact_base: String::new(),
        registry_auth_file: std::path::PathBuf::new(),
        insecure_registry: false,
        oci_ca_paths: Vec::new(),
        control_database_url: String::new(),
    }
}

async fn seed_release(client: &Client, version: &str, hash: char) -> ReleaseSnapshot {
    let (mut manifest, _) = ServingManifest::from_canonical_bytes(vector::CANONICAL_BYTES).unwrap();
    manifest.release.packages = [PackageCoordinate::new("inventory", version).unwrap()].into();
    let mut wiring = manifest.workflow.wirings.iter().next().unwrap().clone();
    wiring.package_id = "inventory".to_owned();
    wiring.wiring_id = "orders".to_owned();
    wiring.graph_hash =
        wamn_catalog::DefinitionHash::parse(format!("sha256:{}", hash.to_string().repeat(64)))
            .unwrap();
    manifest.workflow.wirings = [wiring].into();
    let manifest_digest = manifest.digest();
    client
        .execute(
            "INSERT INTO catalog.releases (tenant_id,manifest_digest,canonical_bytes) VALUES ('delivery',$1,$2)",
            &[&manifest_digest.as_str(), &manifest.canonical_bytes()],
        )
        .await
        .unwrap();
    ReleaseSnapshot {
        manifest,
        carrier: ReleaseCarrier {
            artifact_base: String::new(),
            manifest_digest,
        },
    }
}

#[tokio::test]
async fn owned_selection_lock_refuses_late_activation_and_changed_installed_schema() {
    let mut server = wamn_test_infrastructure::postgres::start(&[]).unwrap();
    let database = server.create_database("delivery_order").unwrap();
    let (mut first, first_connection) = tokio_postgres::connect(database.url(), NoTls)
        .await
        .unwrap();
    let first_connection = tokio::spawn(first_connection);
    let (mut second, second_connection) = tokio_postgres::connect(database.url(), NoTls)
        .await
        .unwrap();
    let second_connection = tokio::spawn(second_connection);
    let (observer, observer_connection) = tokio_postgres::connect(database.url(), NoTls)
        .await
        .unwrap();
    let observer_connection = tokio::spawn(observer_connection);
    first
        .batch_execute("CREATE ROLE wamn_app; CREATE ROLE wamn_scenario_author;")
        .await
        .unwrap();
    first
        .batch_execute(wamn_catalog::CATALOG_SCHEMA_SQL)
        .await
        .unwrap();
    seed_package(&first, "0.1.0", None, 'd', 'd').await;
    seed_release(&first, "0.1.0", 'd').await;
    seed_package(&first, "1.0.0", Some("0.1.0"), 'a', 'b').await;
    seed_package(&first, "2.0.0", Some("1.0.0"), 'a', 'c').await;
    let previous = seed_release(&first, "1.0.0", 'b').await;
    let selected = seed_release(&first, "2.0.0", 'c').await;
    first
        .execute(
            SELECT_RELEASE,
            &[
                &"delivery",
                &"test",
                &previous.carrier.manifest_digest.as_str(),
            ],
        )
        .await
        .unwrap();
    let release = release_request();
    let mut transaction = first.transaction().await.unwrap();
    claim(&transaction, &release.tenant).await.unwrap();
    require_selected(&transaction, &previous, &release)
        .await
        .unwrap();
    // Different historical migrations do not matter when the installed leaf
    // and requested package have exactly the same applied migration stream.
    require_compatible_schema(&mut transaction, &previous.manifest, &release)
        .await
        .unwrap();
    let second_pid: i32 = second
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .unwrap()
        .get(0);
    let selected_digest = selected.carrier.manifest_digest.as_str().to_owned();
    let selection = tokio::spawn(async move {
        let transaction = second.transaction().await.unwrap();
        transaction
            .execute(SELECT_RELEASE, &[&"delivery", &"test", &selected_digest])
            .await
            .unwrap();
        transaction.commit().await.unwrap();
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let blocked: bool = observer
                .query_one(
                    "SELECT cardinality(pg_blocking_pids($1)) > 0",
                    &[&second_pid],
                )
                .await
                .unwrap()
                .get(0);
            if blocked {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the concurrent selection waits on the real head row lock");
    assert!(!selection.is_finished());
    transaction.commit().await.unwrap();
    selection.await.unwrap();
    let transaction = first.transaction().await.unwrap();
    claim(&transaction, &release.tenant).await.unwrap();
    assert!(
        require_selected(&transaction, &previous, &release)
            .await
            .unwrap_err()
            .to_string()
            .contains("superseded")
    );
    transaction.rollback().await.unwrap();
    seed_package(&first, "3.0.0", Some("2.0.0"), 'e', 'e').await;
    let mut transaction = first.transaction().await.unwrap();
    claim(&transaction, &release.tenant).await.unwrap();
    require_selected(&transaction, &selected, &release)
        .await
        .unwrap();
    assert!(
        require_compatible_schema(&mut transaction, &selected.manifest, &release)
            .await
            .unwrap_err()
            .to_string()
            .contains("requires persisted immediate-predecessor upgrade evidence")
    );
    transaction.rollback().await.unwrap();
    first_connection.abort();
    second_connection.abort();
    observer_connection.abort();
    drop(first);
    drop(observer);
    server.stop().unwrap();
}

#[test]
fn http_readiness_requires_a_ready_backend_on_the_actual_service_port() {
    let service =
        json!({"metadata":{"name":"flow-http"},"spec":{"ports":[{"name":"http","targetPort":80}]}});
    let mut slices = json!({"items":[{
        "metadata":{"labels":{"kubernetes.io/service-name":"flow-http"}},
        "ports":[{"name":"http","port":80,"protocol":"TCP"}],
        "endpoints":[{"addresses":["10.0.0.2"],"conditions":{"ready":false}}]
    }]});
    assert!(super::ready_http_backends(&service, &slices).is_empty());
    slices["items"][0]["endpoints"][0]["conditions"]["ready"] = json!(true);
    assert_eq!(super::ready_http_backends(&service, &slices).len(), 1);
    slices["items"][0]["endpoints"][0]["conditions"]["terminating"] = json!(true);
    assert!(super::ready_http_backends(&service, &slices).is_empty());
    slices["items"][0]["endpoints"][0]["conditions"]["terminating"] = json!(false);
    slices["items"][0]["metadata"]["labels"]["kubernetes.io/service-name"] = json!("other");
    assert!(super::ready_http_backends(&service, &slices).is_empty());
    slices["items"][0]["metadata"]["labels"]["kubernetes.io/service-name"] = json!("flow-http");
    slices["items"][0]["ports"][0]["port"] = json!(null);
    assert!(super::ready_http_backends(&service, &slices).is_empty());
    assert!(super::ready_http_backends(&service, &json!({"items":[]})).is_empty());
}
