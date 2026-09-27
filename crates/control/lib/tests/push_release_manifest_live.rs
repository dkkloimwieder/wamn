//! Order of `push_release_manifest`: bytes never sit in the registry without
//! their attestation, and a refused push writes no attestation.

use std::io::ErrorKind;
use std::net::TcpListener;
use std::path::PathBuf;

use tokio_postgres::NoTls;
use wamn_control::push_release_manifest::{
    PushReleaseManifestRequest, publish_and_attest, push_release_manifest,
};

const CANONICAL_MANIFEST: &[u8] = br#"{"attachments":{},"components":[{"component":"http-request","digest":"sha256:1111111111111111111111111111111111111111111111111111111111111111","interface-version":"0.1","operations":{"wamn:node/handler@0.1.0":{}},"package-id":"orders"}],"format-version":3,"release":{"effective-release-id":3,"environment":"prod","packages":[{"package-id":"orders","package-version":"1.0.0"}],"tenant-id":"tenant-a"},"routes":[],"workflow":{"wirings":[{"graph-hash":"sha256:3333333333333333333333333333333333333333333333333333333333333333","package-id":"orders","wiring-id":"orders","wiring-version":1}]}}"#;

/// A port with no listener: connecting to it is refused.
fn closed_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A registry credential file for `registry`, in a private temporary file.
fn auth_file(registry: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "push-release-manifest-live-{}-{}.json",
        std::process::id(),
        registry.replace([':', '.'], "-")
    ));
    std::fs::write(
        &path,
        format!(r#"{{"auths":{{"{registry}":{{"username":"user","password":"secret"}}}}}}"#),
    )
    .unwrap();
    path
}

fn request(registry: &str, control_database_url: String) -> PushReleaseManifestRequest {
    PushReleaseManifestRequest {
        database_url: format!("postgresql://postgres@127.0.0.1:{}/release", closed_port()),
        org: "acme".to_owned(),
        project: "billing".to_owned(),
        tenant: "tenant-a".to_owned(),
        effective_release_id: 3,
        artifact_base: format!("{registry}/wamn/releases"),
        registry_auth_file: auth_file(registry),
        insecure_registry: true,
        oci_ca_paths: Vec::new(),
        control_database_url,
    }
}

#[tokio::test]
async fn an_unreachable_control_database_refuses_before_any_push() {
    let registry = TcpListener::bind("127.0.0.1:0").unwrap();
    registry.set_nonblocking(true).unwrap();
    let authority = registry.local_addr().unwrap().to_string();
    let control = format!(
        "postgresql://postgres@127.0.0.1:{}/wamn_system",
        closed_port()
    );

    let error = push_release_manifest(&request(&authority, control), None)
        .await
        .expect_err("an unreachable control database refuses");

    assert!(
        format!("{error:#}").contains("connect to the control database"),
        "{error:#}"
    );
    assert_eq!(
        registry.accept().map(|_| ()).unwrap_err().kind(),
        ErrorKind::WouldBlock,
        "the registry was contacted"
    );
}

#[tokio::test]
async fn a_refused_push_writes_no_attestation() {
    let control = wamn_control_provision::test_database::system();
    let (mut client, connection) = tokio_postgres::connect(control.url(), NoTls)
        .await
        .expect("connect to the disposable control database");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
        .batch_execute(
            "SELECT set_config('app.tenant', 'tenant-a', false); \
             INSERT INTO catalog.packages (tenant_id, package_id, package_version, manifest_sha256) \
               VALUES ('tenant-a', 'orders', '1.0.0', 'sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'); \
             INSERT INTO catalog.effective_releases \
               (tenant_id, effective_release_id, environment, verified_publisher_principal) \
               VALUES ('tenant-a', 3, 'prod', 'publisher');",
        )
        .await
        .expect("register the release that the fixture bytes name");
    // A registry that accepts each connection and closes it at once.
    let registry = TcpListener::bind("127.0.0.1:0").unwrap();
    let authority = registry.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        for stream in registry.incoming() {
            drop(stream);
        }
    });

    let error = publish_and_attest(
        &mut client,
        &request(&authority, control.url().to_owned()),
        CANONICAL_MANIFEST,
        None,
    )
    .await
    .expect_err("a registry that closes every connection refuses the push");

    assert!(
        !format!("{error:#}").contains("nothing was pushed"),
        "the release read refused instead of the push: {error:#}"
    );
    let attestations: i64 = client
        .query_one("SELECT count(*) FROM catalog.deployment_attestations", &[])
        .await
        .expect("count the attestations")
        .get(0);
    assert_eq!(attestations, 0);
}
