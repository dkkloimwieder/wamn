//! Candidate tests through the real control route, TLS and Modbus stores.

use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering};

use super::*;
use wamn_package_artifact::{
    PackageRegistry,
    registry_credentials::{RegistryCredentialSource, read_registry_credentials},
};
use wamn_runtime::connection_health::ConnectionHealth;
use wamn_runtime::plugins::connection_http::transport::HttpTransport;

#[path = "../../../../control/environment/tests/support/local_registry.rs"]
mod local_registry;

const SECRET: &str = "candidate-private-api-key";

#[derive(Debug, Default)]
struct HealthStatus(tokio::sync::Mutex<Option<String>>);

#[async_trait::async_trait]
impl wamn_execution_host::environment_status::StatusSource for HealthStatus {
    async fn read(&self, _: &str, _: &str, _: &str) -> anyhow::Result<Value> {
        Ok(json!({"digest":self.0.lock().await.clone()}))
    }
}

fn private_ip() -> anyhow::Result<std::net::IpAddr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0")?;
    socket.connect("192.0.2.1:9")?;
    Ok(socket.local_addr()?.ip())
}

async fn counts(client: &tokio_postgres::Client) -> anyhow::Result<(i64, i64, i64)> {
    let row = client.query_one("SELECT (SELECT count(*) FROM registry.connection_generation),(SELECT count(*) FROM identity.credential),(SELECT count(*) FROM registry.connection)", &[]).await?;
    Ok((row.get(0), row.get(1), row.get(2)))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires: bin:/usr/lib/postgresql/18/bin/postgres, env:WAMN_NATIVE_C_NATS_BIN, bin:docker, bin:rustc"]
async fn candidate_http_and_modbus_health_commit_nothing() -> anyhow::Result<()> {
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: bin:/usr/lib/postgresql/18/bin/postgres, env:WAMN_NATIVE_C_NATS_BIN, bin:docker, bin:rustc",
    );
    let mut postgres = wamn_test_postgres::start(&[])?;
    let database = postgres.create_database("candidate_health")?;
    let admin = connect(database.url()).await?;
    let control_url = install(&admin, database.url()).await?;
    let (logins, billing, _shop) = environments(&admin, database.url(), "candidate-health").await?;
    admin.batch_execute(&format!("BEGIN; {} UPDATE registry.meta SET platform_domain='example.test'; UPDATE registry.project_envs SET target_id=(SELECT id FROM registry.target WHERE name='cloud'); COMMIT",wamn_control_provision::bind_platform_principal_sql(PlatformComponent::Provisioning))).await?;
    let boss = create_or_reuse_user(&admin, "health-admin@example.test", "Admin")
        .await?
        .principal_id;
    invite_member(
        &admin,
        &boss,
        ORG,
        &MemberGrants {
            org_admin: true,
            ..MemberGrants::default()
        },
    )
    .await?;
    let ip = private_ip()?;
    let key = KeyPair::generate()?;
    let certificate = CertificateParams::new(vec![ip.to_string()])?.self_signed(&key)?;
    let mut roots = rustls::RootCertStore::empty();
    roots.add(certificate.der().clone())?;
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let tls = rustls::ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    let server_tls = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(
            vec![certificate.der().clone()],
            rustls::pki_types::PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
        )?;
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_tls));
    let listener = tokio::net::TcpListener::bind((ip, 0)).await?;
    let authority = listener.local_addr()?;
    let status = Arc::new(AtomicU16::new(200));
    let requests = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&requests);
    let response_status = Arc::clone(&status);
    let server = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let stream = acceptor.accept(stream).await.unwrap();
            let counter = Arc::clone(&counter);
            let status = Arc::clone(&response_status);
            tokio::spawn(async move {
                hyper::server::conn::http1::Builder::new()
                    .serve_connection(
                        hyper_util::rt::TokioIo::new(stream),
                        hyper::service::service_fn(
                            move |request: hyper::Request<hyper::body::Incoming>| {
                                assert_eq!(request.method(), hyper::Method::HEAD);
                                assert_eq!(request.uri().path(), "/base/");
                                assert!(
                                    request
                                        .headers()
                                        .get("authorization")
                                        .and_then(|value| value.to_str().ok())
                                        .is_some_and(|value| value == SECRET)
                                );
                                counter.fetch_add(1, Ordering::SeqCst);
                                let status = status.load(Ordering::SeqCst);
                                async move {
                                    if status == 0 {
                                        tokio::time::sleep(std::time::Duration::from_secs(12))
                                            .await;
                                    }
                                    let response = hyper::Response::builder()
                                        .status(if status == 0 { 200 } else { status })
                                        .body(http_body_util::Full::new(bytes::Bytes::new()))
                                        .unwrap();
                                    Ok::<_, std::convert::Infallible>(response)
                                }
                            },
                        ),
                    )
                    .await
                    .ok();
            });
        }
    });
    let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create()?;
    let registry = local_registry::LocalRegistry::start("candidate-health");
    registry.ready().await;
    let auth_file = scratch.path().join("auth.json");
    std::fs::write(
        &auth_file,
        json!({"auths":{&registry.address:{"username":"wamn","password":"wamn"}}}).to_string(),
    )?;
    let configuration = PackageRegistry {
        artifact_base: format!("{}/wamn/components", registry.address),
        credentials: None,
        registry_auth_file: auth_file.clone(),
        insecure_registry: true,
        oci_ca_paths: vec![],
    };
    let credentials =
        RegistryCredentialSource::Fixed(read_registry_credentials(&auth_file, &registry.address)?);
    let health = ConnectionHealth::new(Arc::new(HttpTransport::with_tls_config(tls)))
        .with_registry(configuration.clone(), credentials);
    let (_nats, nats) = wamn_runtime::auth_cache::test_util::nats().await;
    let live = Arc::new(HealthStatus::default());
    let delivery = HostRouteDelivery::new(
        Arc::new(LoadedRelease::control_root()),
        HostRouteHandlers::Control {
            status: Arc::<HealthStatus>::clone(&live),
            administration: Some(logins),
            control: Arc::new(connect(&control_url).await?),
            writer: Arc::new(tokio::sync::Mutex::new(connect(&control_url).await?)),
            identity: None,
            org: ORG.into(),
            changes: AuthChanges::new(nats),
        },
        None,
    )
    .with_connection_health(Arc::new(health));
    call(
        &delivery,
        "integration/write",
        boss.as_str(),
        json!({"project":"billing","name":"api","integration_type":"http","auth_type":"api_key"}),
    )
    .await
    .unwrap();
    let mut request = json!({"project":"billing","env":"dev","name":"api","parameters":{"primary-authority":format!("https://{authority}/base/"),"tls-verification":"verify-authority","auth":{"in":"header","name":"Authorization"}},"credential":json!({"key":SECRET}).to_string()});
    let before = counts(&admin).await?;
    assert!(
        call(
            &delivery,
            "connection/test",
            "00000000-0000-4000-8000-000000000099",
            request.clone()
        )
        .await
        .unwrap_err()
        .starts_with("permission denied")
    );
    assert_eq!(requests.load(Ordering::SeqCst), 0);
    assert_eq!(
        call(&delivery, "connection/test", boss.as_str(), request.clone())
            .await
            .unwrap(),
        json!({"ok":true})
    );
    status.store(401, Ordering::SeqCst);
    let refused = call(&delivery, "connection/test", boss.as_str(), request.clone())
        .await
        .unwrap();
    assert_eq!(refused["ok"], false);
    assert_eq!(refused["error"], "401");
    assert!(!refused.to_string().contains(SECRET));
    assert_eq!(requests.load(Ordering::SeqCst), 2);
    status.store(0, Ordering::SeqCst);
    let timed_out = call(&delivery, "connection/test", boss.as_str(), request.clone())
        .await
        .unwrap();
    assert_eq!(timed_out["ok"], false);
    assert!(timed_out["error"].as_str().unwrap().contains("timed out"));
    assert!(!timed_out.to_string().contains(SECRET));
    request["parameters"]["unknown"] = true.into();
    assert_eq!(
        call(&delivery, "connection/test", boss.as_str(), request)
            .await
            .unwrap()["ok"],
        false
    );
    assert_eq!(counts(&admin).await?, before);
    server.abort();

    call(
        &delivery,
        "integration/write",
        boss.as_str(),
        json!({"project":"billing","name":"meter","integration_type":"modbus","auth_type":"none"}),
    )
    .await
    .unwrap();
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()?;
    let built = wamn_package_control::integration::build(&repository, "modbus").await?;
    let schema = json!({"type":"object","properties":{"authority":{"type":"string","minLength":1,"immutable":true}},"required":["authority"]});
    wamn_package_control::integration::publish(
        &built,
        &configuration,
        database.url(),
        &wamn_package_control::integration::Publication {
            version: 2,
            parameter_schema: schema.clone(),
            operations: "wamn:modbus/client@1.0.0".into(),
            write_interfaces: vec!["wamn:modbus/write@1.0.0".into()],
        },
    )
    .await?;
    let listener = tokio::net::TcpListener::bind((ip, 0)).await?;
    let address = listener.local_addr()?;
    let gateway = tokio::spawn(async move {
        loop {
            let _ = listener.accept().await.unwrap();
        }
    });
    let request = json!({"project":"billing","env":"dev","name":"meter","parameters":{"authority":address.to_string()}});
    let before = counts(&admin).await?;
    assert_eq!(
        call(&delivery, "connection/test", boss.as_str(), request.clone())
            .await
            .unwrap(),
        json!({"ok":true})
    );
    let unused = tokio::net::TcpListener::bind((ip, 0)).await?;
    let mut bad = request.clone();
    bad["parameters"]["authority"] = unused.local_addr()?.to_string().into();
    drop(unused);
    assert_eq!(
        call(&delivery, "connection/test", boss.as_str(), bad)
            .await
            .unwrap()["ok"],
        false
    );
    assert_eq!(counts(&admin).await?, before);
    // Native versions use the same component health ABI with their own artifact identity.
    let mut native_config: Value =
        serde_json::from_slice(&std::fs::read(built.directory.join("config.json"))?)?;
    native_config["code"] = "native-meter".into();
    native_config["runtime_type"] = "native".into();
    let native_config = wamn_digest::canonical_json_bytes(&native_config);
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(built.directory.join("manifest.json"))?)?;
    manifest["config"]["digest"] = wamn_digest::bytes_sha256(&native_config).into();
    manifest["config"]["size"] = native_config.len().into();
    let manifest = wamn_digest::canonical_json_bytes(&manifest);
    let native_digest = wamn_digest::bytes_sha256(&manifest);
    let native_repository =
        wamn_package_artifact::Repository::open(&configuration, &native_digest)?;
    native_repository.manifest().await?;
    native_repository
        .client
        .push_blob(
            &native_repository.reference,
            native_config.clone(),
            &wamn_digest::bytes_sha256(&native_config),
        )
        .await?;
    native_repository
        .client
        .push_manifest_raw(
            &native_repository.reference,
            manifest,
            reqwest::header::HeaderValue::from_static("application/vnd.oci.image.manifest.v1+json"),
        )
        .await?;
    admin.batch_execute("INSERT INTO registry.integration_type(code,runtime_type,exclusive) VALUES('native-meter','native',false); INSERT INTO registry.integration_type_auth(integration_type,auth_type) VALUES('native-meter','none')").await?;
    admin.execute("SELECT registry.publish_integration('native-meter',1,$1,$2,'wamn:modbus/client@1.0.0',$3,$4)",&[&native_digest,&schema,&vec!["wamn:modbus/read@1.0.0","wamn:modbus/write@1.0.0","wamn:integration/health@0.1.0"],&vec!["wamn:modbus/write@1.0.0"]]).await?;
    call(&delivery,"integration/write",boss.as_str(),json!({"project":"billing","name":"native-meter","integration_type":"native-meter","auth_type":"none"})).await.unwrap();
    let mut native_request = request.clone();
    native_request["name"] = "native-meter".into();
    assert_eq!(
        call(&delivery, "connection/test", boss.as_str(), native_request)
            .await
            .unwrap(),
        json!({"ok":true})
    );
    assert_eq!(counts(&admin).await?, before);
    // The newest version defaults, while an explicit version still selects its exact code.
    admin.execute("SELECT registry.publish_integration('modbus',3,$1,$2,'wamn:modbus/client@1.0.0',$3,$4)",&[&format!("sha256:{}","f".repeat(64)),&schema,&vec!["wamn:modbus/read@1.0.0","wamn:modbus/write@1.0.0","wamn:integration/health@0.1.0"],&vec!["wamn:modbus/write@1.0.0"]]).await?;
    assert_eq!(
        call(&delivery, "connection/test", boss.as_str(), request.clone())
            .await
            .unwrap()["ok"],
        false
    );
    let mut explicit = request.clone();
    explicit["version"] = 2.into();
    assert_eq!(
        call(&delivery, "connection/test", boss.as_str(), explicit)
            .await
            .unwrap(),
        json!({"ok":true})
    );
    assert_eq!(counts(&admin).await?, before);
    // A live release takes precedence over the newest published version.
    let connection = call(
        &delivery,
        "connection/write",
        boss.as_str(),
        request.clone(),
    )
    .await
    .unwrap();
    let canonical = b"{}".as_slice();
    let digest: String = billing.query_one("INSERT INTO catalog.releases(env_key,manifest_digest,canonical_bytes) VALUES($1,'sha256:'||encode(sha256($2::bytea),'hex'),$2) RETURNING manifest_digest",&[&wamn_control_registry::project_env_key(ORG,"billing","dev"),&canonical]).await?.get(0);
    billing.execute("INSERT INTO catalog.release_connection(env_key,manifest_digest,connection_id,version) VALUES($1,$2,$3::text::uuid,2)",&[&wamn_control_registry::project_env_key(ORG,"billing","dev"),&digest,&connection["id"].as_str().unwrap()]).await?;
    *live.0.lock().await = Some(digest);
    let before = counts(&admin).await?;
    assert_eq!(
        call(&delivery, "connection/test", boss.as_str(), request)
            .await
            .unwrap(),
        json!({"ok":true})
    );
    assert_eq!(counts(&admin).await?, before);
    let wasm = std::fs::read(built.directory.join("integration.wasm"))?;
    assert_eq!(
        wamn_engine::integration::compile_count(&wamn_digest::bytes_sha256(&wasm)),
        1
    );
    gateway.abort();
    Ok(())
}
