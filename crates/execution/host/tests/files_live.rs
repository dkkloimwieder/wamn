//! Native file transfers against owned PostgreSQL and MinIO fixtures.
//! Root runs these cases after the three C4 lanes are integrated.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use bytes::Bytes;
use http_body_util::{BodyExt as _, Full, StreamBody};
use hyper::body::Frame;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use uuid::Uuid;
use wamn_catalog::HostRouteSet;
use wamn_engine::flow_http_routing::{AuthenticatedCaller, CredentialType};
use wamn_engine::release_manifest::{LoadedRelease, ReleaseScope};
use wamn_engine::router_delivery::{DeliveryOutcome, DeliveryRequest, RouteDelivery as _, Source};
use wamn_execution_host::file_route::{FileRefusal, FileRoutes, RegistryFileConnections};
use wamn_execution_host::{HostRouteDelivery, HostRouteHandlers};
use wamn_platform_identity::connection_changes::ConnectionChanges;
use wamn_runtime::connection_credentials::ConnectionCredentials;
use wamn_runtime::connection_facts::ConnectionFacts;
use wamn_runtime::connection_memory::ConnectionMemory;
use wamn_runtime::plugins::connection_http::{
    ConnectionExecutionClosure, ConnectionInvocation, ConnectionOrigin, InvocationEntry,
};
use wamn_runtime::plugins::wamn_blobstore::plugin::WamnBlobstore;
use wasmtime_wasi_http::WasiBody;

#[path = "support/application_fixture.rs"]
#[expect(
    dead_code,
    reason = "Other host tests use the remaining application fixture helpers."
)]
mod application_fixture;
#[path = "support/file_delete_proxy.rs"]
mod file_delete_proxy;
#[path = "../../../control/lib/tests/support/files.rs"]
#[expect(
    dead_code,
    reason = "The shared file fixture also supports schema and registry tests."
)]
mod files;
#[path = "../../../platform/runtime/tests/support/session_fixture.rs"]
#[expect(
    dead_code,
    reason = "The shared session fixture also supports verifier barriers."
)]
mod session_fixture;
#[path = "../../../control/lib/tests/support/file_package/signed_read.rs"]
mod signed_read;
use application_fixture::{ApplicationHost, ENV_KEY, MEMBER, PROJECT, READ};
use session_fixture::{ORG, claims, header, signed};

fn attachment(action: &str) -> String {
    HostRouteSet::Application
        .attachments()
        .find(|(_, a)| a.reference == format!("wamn-control:file/{action}"))
        .unwrap_or_else(|| panic!("file.{action} is a granted host operation"))
        .0
        .to_owned()
}

fn release() -> anyhow::Result<Arc<LoadedRelease>> {
    let original = application_fixture::load_release()?;
    let mut manifest = serde_json::to_value(original.manifest())?;
    manifest["release"]["packages"]
        .as_array_mut()
        .unwrap()
        .push(json!({"package-id":"other_fixture","package-version":"1.0.0"}));
    manifest["components"][0]["descriptor"]["connections"] = json!([
        {"store-alias":"files","requirement-type":"blobstore"},
        {"store-alias":"files_read","requirement-type":"blobstore"},
        {"store-alias":"files_no_cdn","requirement-type":"blobstore"}
    ]);
    let manifest: wamn_catalog::ServingManifest = serde_json::from_value(manifest)?;
    Ok(Arc::new(LoadedRelease::load_canonical_bytes(
        &wamn_execution_contract::canonical_json_bytes(&serde_json::to_value(manifest)?),
        "native files fixture",
        ReleaseScope::new(ENV_KEY, "dev").with_applications(serde_json::from_value(json!({
            "files":{"packages":["session_test"],"connections":{}},
            "other":{"packages":["other_fixture"],"connections":{}}
        }))?),
    )?))
}

struct FileHost {
    administration_url: String,
    postgres: Arc<wamn_runtime::plugins::wamn_postgres::WamnPostgres>,
    writer: tokio_postgres::Client,
    routes: Arc<FileRoutes>,
    delivery: HostRouteDelivery,
    blobstore: Arc<WamnBlobstore>,
    registry: files::Registry,
    minio: files::Minio,
    // Keep the database server alive until its scoped clients and pools drop.
    host: ApplicationHost,
}

impl FileHost {
    async fn start(name: &str) -> anyhow::Result<Self> {
        Self::start_with_release(name, release()?).await
    }

    async fn start_with_release(name: &str, release: Arc<LoadedRelease>) -> anyhow::Result<Self> {
        let minio = files::Minio::start().await?;
        let definition = minio.definition();
        Self::start_on_store(name, release, minio, definition).await
    }

    async fn start_on_store(
        name: &str,
        release: Arc<LoadedRelease>,
        minio: files::Minio,
        definition: Value,
    ) -> anyhow::Result<Self> {
        let registry = files::registry(ORG, PROJECT, definition).await?;
        let (host, postgres, administration_url) =
            application_fixture::application_host_with_release(name, Arc::clone(&release)).await?;
        let writer = application_fixture::connect(&administration_url).await?;
        writer.execute("SELECT set_config('app.user_id',$1,false),set_config('app.operation','admin:file-host-fixture',false)", &[&host.first]).await?;
        for action in ["put", "get", "delete", "expose"] {
            writer.execute("INSERT INTO platform.permission(role_name,permission) VALUES ('purchase-reader',$1)",
                &[&format!("wamn-control:file/{action}")]).await?;
        }
        for name in [files::STORE_NAME, "files-no-cdn"] {
            let row = registry
                .owner
                .query_one(
                    "SELECT c.id::text,i.id::text,i.integration_type,i.name,i.status,CASE WHEN i.status='active' AND c.status='active' THEN 'active' ELSE 'disabled' END FROM registry.connection c JOIN registry.integration i ON i.id=c.integration_id WHERE c.org=$1 AND c.project=$3 AND c.env='dev' AND i.name=$2",
                    &[&ORG, &name, &PROJECT],
                )
                .await?;
            writer.execute("INSERT INTO platform.integration(id,integration_type,name,status) VALUES ($1::text::uuid,$2,$3,$4)",
                &[&row.get::<_,String>(1),&row.get::<_,String>(2),&row.get::<_,String>(3),&row.get::<_,String>(4)]).await?;
            writer.execute("INSERT INTO platform.connection(id,integration_id,integration_type,status) VALUES ($1::text::uuid,$2::text::uuid,$3,$4)",
                &[&row.get::<_,String>(0),&row.get::<_,String>(1),&row.get::<_,String>(2),&row.get::<_,String>(5)]).await?;
        }
        host.admin.execute("INSERT INTO catalog.releases(env_key,manifest_digest,canonical_bytes) VALUES($1,$2,$3)",&[&ENV_KEY,&release.release().manifest_digest.as_str(),&release.manifest().canonical_bytes()]).await?;
        let component = release
            .manifest()
            .components
            .first()
            .context("file fixture component")?;
        for (alias, name, mode) in [
            ("files", files::STORE_NAME, "write"),
            ("files_read", files::STORE_NAME, "read"),
            ("files_no_cdn", "files-no-cdn", "write"),
        ] {
            let id: String=host.admin.query_one("SELECT c.id::text FROM platform.connection c JOIN platform.integration i ON i.id=c.integration_id WHERE i.name=$1",&[&name]).await?.get(0);
            host.admin.execute("INSERT INTO catalog.connection_requirement(env_key,component_digest,package_id,name,interfaces,mode) VALUES($1,$2,$3,$4,ARRAY['wasmcloud:blobstore/blobstore@0.1.0'],$5)",&[&ENV_KEY,&component.descriptor.component_digest,&component.package_id,&alias,&mode]).await?;
            host.admin.execute("INSERT INTO catalog.release_connection(env_key,manifest_digest,connection_id,version) VALUES($1,$2,$3::text::uuid,1) ON CONFLICT DO NOTHING",&[&ENV_KEY,&release.release().manifest_digest.as_str(),&id]).await?;
            host.admin.execute("INSERT INTO catalog.connection_binding(env_key,manifest_digest,component_digest,package_id,name,connection_id) VALUES($1,$2,$3,$4,$5,$6::text::uuid)",&[&ENV_KEY,&release.release().manifest_digest.as_str(),&component.descriptor.component_digest,&component.package_id,&alias,&id]).await?;
        }
        let credentials = Arc::new(
            ConnectionCredentials::from_database(Arc::clone(&registry.reader), ORG)
                .with_store(registry.credentials.clone()),
        );
        let facts = Arc::new(ConnectionFacts::new(
            Arc::new(ConnectionMemory::default()),
            ConnectionChanges::default(),
            credentials,
            Arc::clone(&postgres),
            ENV_KEY,
            PROJECT,
            "dev",
        ));
        let blobstore = Arc::new(WamnBlobstore::new(
            Arc::clone(&postgres),
            Arc::clone(&facts),
            ENV_KEY,
            PROJECT,
            Some(Arc::clone(&release)),
        ));
        let routes = Arc::new(FileRoutes::new(
            Arc::clone(&release),
            Arc::clone(&postgres),
            Arc::clone(&blobstore),
            Arc::new(RegistryFileConnections {
                client: Some(Arc::clone(&registry.reader)),
                release: Arc::clone(&release),
                facts: Arc::clone(&facts),
                org: ORG.into(),
                project: PROJECT.into(),
                environment: "dev".into(),
            }),
            PROJECT.into(),
            Some(administration_url.clone()),
        ));
        let delivery = HostRouteDelivery::new(
            Arc::clone(&release),
            HostRouteHandlers::Application {
                postgres: Arc::clone(&postgres),
                project: PROJECT.into(),
                identity: None,
                changes: wamn_platform_identity::auth_changes::AuthChanges::default(),
            },
            None,
        )
        .with_files(Some(Arc::clone(&routes)));
        Ok(Self {
            administration_url,
            postgres,
            writer,
            routes,
            delivery,
            blobstore,
            registry,
            minio,
            host,
        })
    }

    fn token(&self, admin: bool) -> String {
        let mut body = claims();
        if admin {
            body["sub"] = json!(self.host.first);
            body["authority"] = json!({"login":self.host.first});
            body["roles"] = json!(["admin"]);
        } else {
            body["sub"] = json!(MEMBER);
            body["authority"] = json!({"login":MEMBER});
            body["roles"] = json!(["purchase-reader"]);
        }
        format!("Bearer {}", signed(&header(), &body))
    }

    async fn caller(&self, action: &str) -> AuthenticatedCaller {
        self.host
            .routing
            .authenticate_authorization_for_test(&attachment(action), Some(&self.token(false)))
            .await
            .expect("valid fixture session")
            .expect("authenticated caller")
    }

    async fn http(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: WasiBody,
    ) -> anyhow::Result<(u16, hyper::HeaderMap, Vec<u8>)> {
        let mut request = hyper::Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "text/plain");
        if let Some(token) = token {
            request = request.header("authorization", token);
        }
        let response = self
            .routes
            .http(&self.host.routing, request.body(body)?)
            .await
            .context("native file route")?;
        let (parts, body) = response.into_parts();
        Ok((
            parts.status.as_u16(),
            parts.headers,
            body.collect().await?.to_bytes().to_vec(),
        ))
    }

    async fn upload(&self, binding: &str, bytes: &[u8]) -> anyhow::Result<Uuid> {
        let result = self
            .http(
                "POST",
                &format!("/api/files/files?binding={binding}&name=fixture.txt"),
                Some(&self.token(false)),
                body(bytes),
            )
            .await?;
        anyhow::ensure!(
            result.0 == 201,
            "native upload status {}: {}",
            result.0,
            String::from_utf8_lossy(&result.2)
        );
        Ok(serde_json::from_slice::<Value>(&result.2)?["file_id"]
            .as_str()
            .context("file id")?
            .parse()?)
    }

    async fn read(
        &self,
        application: &str,
        id: Uuid,
        token: Option<&str>,
    ) -> anyhow::Result<(u16, hyper::HeaderMap, Vec<u8>)> {
        self.http(
            "GET",
            &format!("/api/{application}/files/{id}"),
            token,
            body(&[]),
        )
        .await
    }

    async fn mutate(
        &self,
        action: &str,
        id: Uuid,
        binding: &str,
        exposure: Option<&str>,
    ) -> anyhow::Result<Value> {
        let mut input = json!({"application":"files","file_id":id,"binding":binding});
        if let Some(exposure) = exposure {
            input["exposure"] = json!(exposure);
        }
        let outcome = self
            .delivery
            .deliver(
                DeliveryRequest {
                    source: Source::Attachment(attachment(action)),
                    delivery_id: Uuid::new_v4().to_string(),
                    payload: json!([{"request_id":Uuid::new_v4().to_string(),"value":input}])
                        .to_string(),
                    caller: None,
                    trace: None,
                    parent_causation: None,
                    if_none_match: None,
                },
                Some(self.caller(action).await),
            )
            .await
            .outcome
            .map_err(|error| anyhow::anyhow!("file operation delivery: {error:?}"))?;
        let DeliveryOutcome::Respond(response) = outcome else {
            anyhow::bail!("file operation did not respond: {outcome:?}");
        };
        let [request_element]: [Value; 1] = serde_json::from_str(&response)?;
        Ok(request_element)
    }

    fn bind_operation(&self) -> anyhow::Result<AuthenticatedCaller> {
        let digest = format!("sha256:{}", "a".repeat(64));
        self.blobstore.bind_invocation(
            "file-operation",
            ConnectionInvocation {
                origin: ConnectionOrigin {
                    package_id: "session_test".into(),
                    component_digest: digest.clone(),
                    component: "purchase".into(),
                    interface_version: "0.1.0".into(),
                    operation: READ.into(),
                },
                entry: InvocationEntry::Route,
                package_id: "session_test".into(),
                component_digest: digest,
                component: "purchase".into(),
                operation: READ.into(),
                closure: ConnectionExecutionClosure::Released,
                effects: None,
            },
        )?;
        Ok(AuthenticatedCaller::new(
            "file-operation",
            MEMBER,
            CredentialType::Session,
            false,
            [wamn_catalog::sealed_operation_reference(READ).to_owned()].into(),
            None,
        ))
    }
}

fn body(bytes: &[u8]) -> WasiBody {
    Full::new(Bytes::copy_from_slice(bytes))
        .map_err(|never| match never {})
        .boxed_unsync()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn orphan_collector_retries_failed_deletes_and_protects_rows_young_and_unknown_keys()
-> anyhow::Result<()> {
    use object_store::{
        ObjectStoreExt as _,
        aws::AmazonS3Builder,
        path::Path,
        signer::{Method, Signer as _},
    };
    use std::sync::atomic::Ordering;

    let minio = files::Minio::start().await?;
    let proxy = file_delete_proxy::DeleteProxy::start(minio.endpoint.clone()).await?;
    let mut definition = minio.definition();
    definition["endpoint"] = json!(proxy.endpoint);
    let mut fixture =
        FileHost::start_on_store("file_orphan_collector", release()?, minio, definition).await?;
    let generations_sql =
        "SELECT connection_id::text, generation FROM registry.read_file_connection($1,$2,$3)";
    assert!(
        !fixture
            .registry
            .reader
            .query(generations_sql, &[&ORG, &PROJECT, &"dev"])
            .await?
            .is_empty()
    );
    for reader in [&fixture.registry.unmapped, &fixture.registry.unenabled] {
        assert!(
            reader
                .query(generations_sql, &[&ORG, &PROJECT, &"dev"])
                .await?
                .is_empty()
        );
    }
    for scope in [
        ("forged-org", PROJECT, "dev"),
        (ORG, "forged-project", "dev"),
        (ORG, PROJECT, "forged-env"),
    ] {
        assert!(
            fixture
                .registry
                .reader
                .query(generations_sql, &[&scope.0, &scope.1, &scope.2])
                .await?
                .is_empty()
        );
    }
    let orphan = fixture.upload("files", b"orphan bytes").await?;
    let kept = fixture.upload("files", b"kept bytes").await?;
    fixture
        .mutate("expose", orphan, "files", Some("public"))
        .await?;
    fixture
        .mutate("expose", kept, "files", Some("public"))
        .await?;
    let backend = AmazonS3Builder::new()
        .with_endpoint(&fixture.minio.endpoint)
        .with_bucket_name("files-a")
        .with_region("us-east-1")
        .with_access_key_id("file-fixture")
        .with_secret_access_key(files::STORE_PASSWORD)
        .with_allow_http(true)
        .build()?;
    let client = fixture.minio.client()?;
    let malformed = [
        "dev/_wamn_files/live/not-a-uuid".to_owned(),
        format!("dev/public/_wamn_files/live/{kept}/extra"),
        "dev/_wamn_files/live/AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA".to_owned(),
    ];
    let outside = [
        format!("_wamn_files/live/{}", Uuid::new_v4()),
        format!("public/_wamn_files/live/{}", Uuid::new_v4()),
        format!("prod/_wamn_files/live/{}", Uuid::new_v4()),
        format!("prod/public/_wamn_files/live/{}", Uuid::new_v4()),
    ];
    let mut aged: Vec<(String, &[u8])> = malformed
        .iter()
        .chain(&outside)
        .map(|key| (key.clone(), b"unknown bytes".as_slice()))
        .collect();
    let batch_orphans: Vec<_> = (0..101)
        .map(|_| format!("dev/_wamn_files/live/{}", Uuid::new_v4()))
        .collect();
    for key in &batch_orphans {
        aged.push((key.clone(), b"batch orphan".as_slice()));
    }
    for (id, bytes) in [
        (orphan, b"orphan bytes".as_slice()),
        (kept, b"kept bytes".as_slice()),
    ] {
        aged.push((format!("dev/_wamn_files/live/{id}"), bytes));
        aged.push((format!("dev/public/_wamn_files/live/{id}"), bytes));
    }
    for (key, bytes) in &aged {
        let path = Path::from(format!("generation-one/{key}"));
        let response = client
            .put(
                backend
                    .signed_url(Method::PUT, &path, Duration::from_secs(60))
                    .await?,
            )
            .header("x-minio-source-mtime", "2000-01-01T00:00:00Z")
            .body(bytes.to_vec())
            .send()
            .await?;
        anyhow::ensure!(
            response.status().is_success(),
            "seed aged object: {}",
            response.text().await?
        );
        anyhow::ensure!(
            std::time::SystemTime::from(backend.head(&path).await?.last_modified)
                < std::time::SystemTime::now()
                    - wamn_execution_host::file_collector::MIN_OBJECT_AGE,
            "MinIO stores the actual aged timestamp"
        );
    }
    proxy.refuse.store(true, Ordering::SeqCst);
    let deleted = fixture.mutate("delete", orphan, "files", None).await?;
    assert_eq!(deleted["value"]["file_id"], json!(orphan));
    assert_eq!(
        fixture
            .writer
            .query_one(
                "SELECT count(*) FROM platform.file WHERE id=$1::text::uuid",
                &[&orphan.to_string()]
            )
            .await?
            .get::<_, i64>(0),
        0
    );
    backend
        .head(&Path::from(format!(
            "generation-one/dev/_wamn_files/live/{orphan}"
        )))
        .await?;
    backend
        .head(&Path::from(format!(
            "generation-one/dev/public/_wamn_files/live/{orphan}"
        )))
        .await?;
    proxy.refuse.store(false, Ordering::SeqCst);

    // A credential rotation keeps the same physical store and its existing files.
    let equivalent = fixture.registry.definition.clone();
    fixture.registry.move_store(&equivalent).await?;
    fixture.registry.configure("connection.set_credential",json!({"name":files::STORE_NAME,"credential":json!({"ACCESS_KEY_ID":"file-fixture","ACCESS_SECRET_KEY":files::STORE_PASSWORD}).to_string()})).await?;
    let mut moved = fixture.registry.definition.clone();
    moved["container"] = json!("files-b");
    moved["prefix"] = json!("generation-two/");
    assert!(fixture.registry.move_store(&moved).await.is_err());
    fixture
        .registry
        .define_store(PROJECT, "other-files", &moved)
        .await?;
    let moved_backend = AmazonS3Builder::new()
        .with_endpoint(&fixture.minio.endpoint)
        .with_bucket_name("files-b")
        .with_region("us-east-1")
        .with_access_key_id("file-fixture")
        .with_secret_access_key(files::STORE_PASSWORD)
        .with_allow_http(true)
        .build()?;
    let moved_key = Path::from(format!("generation-two/dev/_wamn_files/live/{kept}"));
    let response = client
        .put(
            moved_backend
                .signed_url(Method::PUT, &moved_key, Duration::from_secs(60))
                .await?,
        )
        .header("x-minio-source-mtime", "2000-01-01T00:00:00Z")
        .body("orphan in the moved store")
        .send()
        .await?;
    anyhow::ensure!(response.status().is_success(), "seed moved-store orphan");

    // An independent project uses the same storage through its own authority.
    fixture
        .registry
        .define_store("unenabled", files::STORE_NAME, &equivalent)
        .await?;
    let source = fixture.registry.control.transaction().await?;
    let mut administration = application_fixture::connect(&fixture.administration_url).await?;
    let target = administration.transaction().await?;
    target
        .execute(
            "SELECT set_config('app.user_id',$1,true), \
             set_config('app.operation','admin:file-host-fixture',true)",
            &[&fixture.host.first],
        )
        .await?;
    let _ = wamn_platform_identity::connection_projection::project(
        &source, &target, ORG, PROJECT, "dev",
    )
    .await?;
    target.commit().await?;
    source.commit().await?;

    // Hold an uncommitted row while its fresh object exists. The collector's
    // separate SELECT-only connection cannot see this transaction's row.
    let fresh = Uuid::new_v4();
    let store = fixture
        .blobstore
        .file_store(files::STORE_NAME, false)
        .await?;
    let staged = store.stage(fresh, &b"pending row"[..]).await?;
    fixture.writer.batch_execute("BEGIN").await?;
    fixture.writer.execute("INSERT INTO platform.file(id,application,connection_id,name,media_type,size,sha256,blob_key,exposure) VALUES($1::text::uuid,'files',$2::text::uuid,'pending.txt','text/plain',$3,$4,$5,'application')", &[&fresh.to_string(), &fixture.registry.connection_id, &i64::try_from(staged.size)?, &staged.sha256, &staged.blob_key]).await?;
    store.finish(fresh).await?;
    assert_eq!(
        fixture
            .host
            .admin
            .query_one(
                "SELECT count(*) FROM platform.file WHERE id=$1::text::uuid",
                &[&fresh.to_string()]
            )
            .await?
            .get::<_, i64>(0),
        0
    );

    // A second host must skip a pass while the first host holds the lease.
    let lease = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(lease) = fixture.postgres.file_collection(PROJECT, ENV_KEY).await? {
                return anyhow::Ok(lease);
            }
            tokio::task::yield_now().await;
        }
    })
    .await??;
    assert_eq!(fixture.routes.collect_orphan_files().await?, 0);
    drop(lease);
    // Wait for PostgreSQL to observe the closed session before the next pass.
    let deleted = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let deleted = fixture.routes.collect_orphan_files().await?;
            if deleted != 0 {
                return anyhow::Ok(deleted);
            }
            tokio::task::yield_now().await;
        }
    })
    .await??;
    assert_eq!(deleted, 104);
    assert!(matches!(
        moved_backend.head(&moved_key).await,
        Err(object_store::Error::NotFound { .. })
    ));
    assert_eq!(fixture.routes.collect_orphan_files().await?, 0);
    fixture.writer.batch_execute("ROLLBACK").await?;
    for (key, _) in &aged {
        let path = Path::from(format!("generation-one/{key}"));
        if key.ends_with(&orphan.to_string()) || batch_orphans.contains(key) {
            assert!(matches!(
                backend.head(&path).await,
                Err(object_store::Error::NotFound { .. })
            ));
        } else {
            backend.head(&path).await?;
        }
    }
    assert_eq!(
        store.get(&staged.blob_key).await?.bytes().await?.as_ref(),
        b"pending row"
    );
    let read = fixture
        .read("files", kept, Some(&fixture.token(false)))
        .await?;
    assert_eq!(read.0, 200);
    assert_eq!(read.2, b"kept bytes");
    assert_eq!(
        fixture
            .writer
            .query_one(
                "SELECT count(*) FROM platform.file WHERE id=$1::text::uuid",
                &[&kept.to_string()]
            )
            .await?
            .get::<_, i64>(0),
        1
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_upload_streams_more_than_64_mib_and_hashes_the_stored_bytes() -> anyhow::Result<()>
{
    let fixture = FileHost::start("file_transfer").await?;
    let size = 65usize * 1024 * 1024 + 17;
    let stream = futures_util::stream::unfold(size, |remaining| async move {
        if remaining == 0 {
            None
        } else {
            let count = remaining.min(64 * 1024);
            Some((
                Ok::<_, wasmtime_wasi_http::Error>(Frame::data(Bytes::from(vec![42; count]))),
                remaining - count,
            ))
        }
    });
    let uploaded = fixture
        .http(
            "POST",
            "/api/files/files?binding=files&name=stream.bin",
            Some(&fixture.token(false)),
            StreamBody::new(stream).boxed_unsync(),
        )
        .await?;
    assert_eq!(uploaded.0, 201, "{}", String::from_utf8_lossy(&uploaded.2));
    let id: Uuid = serde_json::from_slice::<Value>(&uploaded.2)?["file_id"]
        .as_str()
        .unwrap()
        .parse()?;
    let row = fixture
        .host
        .admin
        .query_one(
            "SELECT size,sha256,blob_key FROM platform.file WHERE id=$1::text::uuid",
            &[&id.to_string()],
        )
        .await?;
    assert_eq!(row.get::<_, i64>(0), i64::try_from(size)?);
    let mut expected = Sha256::new();
    for count in std::iter::repeat_n(64 * 1024, size / (64 * 1024)).chain([size % (64 * 1024)]) {
        expected.update(vec![42; count]);
    }
    assert_eq!(row.get::<_, String>(1), hex::encode(expected.finalize()));
    assert_eq!(
        row.get::<_, String>(2),
        format!("dev/_wamn_files/live/{id}")
    );
    let store = fixture
        .blobstore
        .file_store(files::STORE_NAME, false)
        .await?;
    let stored = store.get(&row.get::<_, String>(2)).await?.bytes().await?;
    assert_eq!(stored.len(), size);
    assert!(stored.iter().all(|byte| *byte == 42));
    assert_eq!(
        hex::encode(Sha256::digest(&stored)),
        row.get::<_, String>(1)
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_unsegmented_file_still_reads_signs_exposes_and_deletes() -> anyhow::Result<()> {
    let fixture = FileHost::start("file_legacy_key").await?;
    let id = Uuid::new_v4();
    let key = format!("_wamn_files/live/{id}");
    fixture
        .minio
        .mc(&format!(
            "printf 'legacy bytes' | mc pipe file/files-a/generation-one/{key}"
        ))
        .await?;
    fixture.writer.execute(
        "INSERT INTO platform.file(id,application,connection_id,name,media_type,size,sha256,blob_key) \
         VALUES ($1::text::uuid,'files',$2::text::uuid,'legacy.txt','text/plain',12,$3,$4)",
        &[&id.to_string(), &fixture.registry.connection_id,
          &hex::encode(Sha256::digest(b"legacy bytes")), &key],
    ).await?;
    let read = fixture
        .read("files", id, Some(&fixture.token(false)))
        .await?;
    assert_eq!(read.0, 200);
    assert_eq!(read.2, b"legacy bytes");
    let caller = fixture.bind_operation()?;
    let signed = fixture
        .routes
        .signed_url(
            &fixture.blobstore,
            "file-operation",
            &caller,
            id,
            Duration::from_secs(60),
        )
        .await?;
    assert_eq!(
        url::Url::parse(&signed)?.path(),
        format!("/files-a/generation-one/{key}")
    );
    let client = fixture.minio.client()?;
    assert_eq!(
        client.get(signed).send().await?.bytes().await?.as_ref(),
        b"legacy bytes"
    );
    let exposed = fixture
        .mutate("expose", id, "files", Some("public"))
        .await?;
    let url = exposed["value"]["url"]
        .as_str()
        .context("legacy public URL")?;
    assert_eq!(
        url::Url::parse(url)?.path(),
        format!("/generation-one/public/{key}")
    );
    assert_eq!(
        client.get(url).send().await?.bytes().await?.as_ref(),
        b"legacy bytes"
    );
    fixture
        .mutate("expose", id, "files", Some("application"))
        .await?;
    assert_eq!(client.get(url).send().await?.status(), 404);
    fixture
        .mutate("expose", id, "files", Some("public"))
        .await?;
    assert_eq!(
        fixture
            .writer
            .query_one(
                "SELECT blob_key FROM platform.file WHERE id=$1::text::uuid",
                &[&id.to_string()],
            )
            .await?
            .get::<_, String>(0),
        key
    );
    let deleted = fixture.mutate("delete", id, "files", None).await?;
    assert_eq!(deleted["value"]["file_id"], json!(id));
    assert_eq!(
        fixture
            .read("files", id, Some(&fixture.token(false)))
            .await?
            .0,
        404
    );
    assert_eq!(client.get(url).send().await?.status(), 404);
    assert!(
        fixture
            .minio
            .mc(&format!("mc stat file/files-a/generation-one/{key}"))
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn application_file_route_requires_its_grant_and_enforces_exposure_and_owner()
-> anyhow::Result<()> {
    let fixture = FileHost::start("file_route_access").await?;
    let id = fixture.upload("files", b"route bytes").await?;
    let token = fixture.token(false);
    let read = fixture.read("files", id, Some(&token)).await?;
    assert_eq!(read.0, 200);
    assert_eq!(read.1["content-type"], "text/plain");
    assert_eq!(read.2, b"route bytes");
    assert_eq!(fixture.read("files", id, None).await?.0, 401);
    fixture.writer.execute("DELETE FROM platform.permission WHERE role_name='purchase-reader' AND permission='wamn-control:file/get'",&[]).await?;
    assert_eq!(fixture.read("files", id, Some(&token)).await?.0, 403);
    fixture.writer.execute("INSERT INTO platform.permission(role_name,permission) VALUES ('purchase-reader','wamn-control:file/get')",&[]).await?;
    assert_eq!(fixture.read("other", id, Some(&token)).await?.0, 404);
    assert!(
        fixture
            .mutate("expose", id, "files", Some("private"))
            .await?
            .get("value")
            .is_some()
    );
    assert_eq!(fixture.read("files", id, Some(&token)).await?.0, 403);
    assert_eq!(
        fixture
            .read("files", id, Some(&fixture.token(true)))
            .await?
            .0,
        403
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn file_id_reads_survive_binding_removal_until_connection_disablement() -> anyhow::Result<()>
{
    let mut fixture = FileHost::start("file_binding_removal").await?;
    let id = fixture.upload("files", b"retained bytes").await?;
    let token = fixture.token(false);
    let read = fixture.read("files", id, Some(&token)).await?;
    assert_eq!(read.0, 200);
    assert_eq!(read.2, b"retained bytes");

    fixture
        .host
        .admin
        .batch_execute(
            "DELETE FROM catalog.connection_binding; DELETE FROM catalog.release_connection",
        )
        .await?;
    let read = fixture.read("files", id, Some(&token)).await?;
    assert_eq!(read.0, 200);
    assert_eq!(read.2, b"retained bytes");
    let upload = fixture
        .http(
            "POST",
            "/api/files/files?binding=files&name=denied.txt",
            Some(&token),
            body(b"refused bytes"),
        )
        .await?;
    assert_eq!(upload.0, 403);
    assert_eq!(
        serde_json::from_slice::<Value>(&upload.2)?["error"]["code"],
        "binding_refused"
    );

    let tx = fixture.registry.control.transaction().await?;
    wamn_platform_identity::connections::administration::execute(
        &tx,
        ORG,
        "connection.disable",
        &json!({"project":PROJECT,"env":"dev","name":files::STORE_NAME}).to_string(),
        &mut wamn_platform_identity::auth_changes::AuthDrops::default(),
        &mut wamn_platform_identity::connection_changes::ConnectionDrops::default(),
    )
    .await?;
    tx.commit().await?;
    let read = fixture.read("files", id, Some(&token)).await?;
    assert_eq!(read.0, 403);
    assert_eq!(
        serde_json::from_slice::<Value>(&read.2)?["error"]["code"],
        "binding_refused"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn committed_staging_bytes_repair_a_missing_live_object_on_native_get() -> anyhow::Result<()>
{
    let fixture = FileHost::start("file_crash_repair").await?;
    let store = fixture
        .blobstore
        .file_store(files::STORE_NAME, false)
        .await?;
    let id = Uuid::new_v4();
    let staged = store.stage(id, &b"committed staging"[..]).await?;
    fixture.writer.execute(
        "INSERT INTO platform.file(id,application,connection_id,name,media_type,size,sha256,blob_key) \
         VALUES ($1::text::uuid,'files',$2::text::uuid,'crash.txt','text/plain',$3,$4,$5)",
        &[&id.to_string(),&fixture.registry.connection_id,&i64::try_from(staged.size)?,&staged.sha256,&staged.blob_key],
    ).await?;
    assert_eq!(
        fixture
            .host
            .admin
            .query_one(
                "SELECT count(*) FROM platform.file WHERE id=$1::text::uuid",
                &[&id.to_string()]
            )
            .await?
            .get::<_, i64>(0),
        1
    );
    assert!(
        fixture
            .minio
            .mc(&format!(
                "mc stat file/files-a/generation-one/dev/_wamn_files/live/{id}"
            ))
            .await
            .is_err()
    );
    fixture
        .minio
        .mc(&format!(
            "mc stat file/files-a/generation-one/dev/_wamn_files/staging/{id}"
        ))
        .await?;
    let response = fixture
        .read("files", id, Some(&fixture.token(false)))
        .await?;
    assert_eq!(response.0, 200);
    assert_eq!(response.2, b"committed staging");
    fixture
        .minio
        .mc(&format!(
            "mc stat file/files-a/generation-one/dev/_wamn_files/live/{id}"
        ))
        .await?;
    assert!(
        fixture
            .minio
            .mc(&format!(
                "mc stat file/files-a/generation-one/dev/_wamn_files/staging/{id}"
            ))
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_operation_signed_url_reads_private_bytes_then_expires_and_caps_lifetime()
-> anyhow::Result<()> {
    let fixture = FileHost::start("file_signed_url").await?;
    let id = fixture.upload("files", b"signed private bytes").await?;
    fixture
        .mutate("expose", id, "files", Some("private"))
        .await?;
    let caller = fixture.bind_operation()?;
    assert!(!caller.permits("wamn-control:file/get"));
    let client = fixture.minio.client()?;
    let url = fixture
        .routes
        .signed_url(
            &fixture.blobstore,
            "file-operation",
            &caller,
            id,
            Duration::from_secs(2),
        )
        .await?;
    let response = client.get(&url).send().await?;
    assert_eq!(response.status(), 200);
    assert_eq!(response.bytes().await?.as_ref(), b"signed private bytes");
    let capped = fixture
        .routes
        .signed_url(
            &fixture.blobstore,
            "file-operation",
            &caller,
            id,
            Duration::from_secs(3600),
        )
        .await?;
    let cap = url::Url::parse(&capped)?
        .query_pairs()
        .find(|(key, _)| key == "X-Amz-Expires")
        .map(|(_, value)| value.into_owned());
    assert_eq!(cap.as_deref(), Some("900"));
    assert!(
        fixture
            .routes
            .signed_url(
                &fixture.blobstore,
                "file-operation",
                &caller,
                id,
                Duration::ZERO
            )
            .await
            .is_err()
    );
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(client.get(&url).send().await?.status(), 403);
    fixture.blobstore.revoke_invocation("file-operation");
    let refused = fixture
        .routes
        .signed_url(
            &fixture.blobstore,
            "file-operation",
            &caller,
            id,
            Duration::from_secs(30),
        )
        .await
        .unwrap_err();
    assert_eq!(refused.refusal, Some(FileRefusal::PermissionDenied));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn public_origin_returns_bytes_then_404_after_idempotent_unexpose() -> anyhow::Result<()> {
    let mut fixture = FileHost::start("file_public").await?;
    let id = fixture.upload("files", b"public bytes").await?;
    let anonymous = fixture.minio.client()?;
    assert_eq!(
        anonymous
            .get(format!(
                "{}/generation-one/dev/_wamn_files/live/{id}",
                fixture.minio.cdn_origin
            ))
            .send()
            .await?
            .status(),
        403,
        "the origin policy serves public copies only"
    );
    let exposed = fixture
        .mutate("expose", id, "files", Some("public"))
        .await?;
    let url = exposed["value"]["url"].as_str().context("public URL")?;
    assert_eq!(
        url::Url::parse(url)?.path(),
        format!("/generation-one/dev/public/_wamn_files/live/{id}")
    );
    let mut next_cdn = fixture.registry.definition.clone();
    next_cdn["cdn_host"] = json!("https://next-cdn.example.invalid");
    fixture.registry.move_store(&next_cdn).await?;
    let repeated = fixture
        .mutate("expose", id, "files", Some("public"))
        .await?;
    assert_eq!(repeated["value"]["url"], url);
    let client = fixture.minio.client()?;
    let response = client.get(url).send().await?;
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "max-age=3600");
    assert_eq!(response.bytes().await?.as_ref(), b"public bytes");
    for _ in 0..2 {
        assert!(
            fixture
                .mutate("expose", id, "files", Some("application"))
                .await?
                .get("value")
                .is_some()
        );
    }
    assert_eq!(client.get(url).send().await?.status(), 404);
    let row = fixture
        .host
        .admin
        .query_one(
            "SELECT exposure,url FROM platform.file WHERE id=$1::text::uuid",
            &[&id.to_string()],
        )
        .await?;
    assert_eq!(row.get::<_, String>(0), "application");
    assert!(row.get::<_, Option<String>>(1).is_none());
    assert_eq!(
        fixture
            .read("files", id, Some(&fixture.token(false)))
            .await?
            .2,
        b"public bytes"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn public_exposure_without_cdn_and_every_read_binding_mutation_refuse() -> anyhow::Result<()>
{
    let fixture = FileHost::start("file_binding_denial").await?;
    let id = fixture.upload("files", b"retained").await?;
    let no_cdn = fixture.upload("files_no_cdn", b"no CDN").await?;
    assert_eq!(
        fixture
            .mutate("expose", no_cdn, "files_no_cdn", Some("public"))
            .await?["error"]["code"],
        "cdn_host_required"
    );
    let upload = fixture
        .http(
            "POST",
            "/api/files/files?binding=files_read&name=denied",
            Some(&fixture.token(false)),
            body(b"denied"),
        )
        .await?;
    assert_eq!(upload.0, 403);
    for exposure in ["private", "application", "public"] {
        assert_eq!(
            fixture
                .mutate("expose", id, "files_read", Some(exposure))
                .await?["error"]["code"],
            "binding_refused"
        );
    }
    assert_eq!(
        fixture.mutate("delete", id, "files_read", None).await?["error"]["code"],
        "binding_refused"
    );
    assert_eq!(
        fixture
            .read("files", id, Some(&fixture.token(false)))
            .await?
            .2,
        b"retained"
    );
    assert_eq!(
        fixture
            .host
            .admin
            .query_one("SELECT count(*) FROM platform.file", &[])
            .await?
            .get::<_, i64>(0),
        2
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn referenced_file_delete_refuses_and_disablement_keeps_the_application_foreign_key()
-> anyhow::Result<()> {
    use wamn_control_provision::{
        CredentialGeneration, WorkloadRoleFamily, WorkloadRoleScope, workload_generation_role,
    };
    let mut fixture = FileHost::start("file_referenced_delete").await?;
    let id = fixture.upload("files", b"referenced bytes").await?;
    fixture.host.admin.batch_execute(files::DOCUMENTS).await?;
    fixture.host.admin.batch_execute("GRANT USAGE ON SCHEMA file_fixture TO wamn_app; GRANT SELECT,INSERT,DELETE ON file_fixture.document TO wamn_app").await?;
    let role = workload_generation_role(
        WorkloadRoleFamily::App,
        WorkloadRoleScope::EnvKey {
            env_key: ENV_KEY,
            database: "file_referenced_delete",
        },
        CredentialGeneration::A,
    )?;
    let guest = application_fixture::connect(&application_fixture::login(
        &fixture.administration_url,
        &role,
    )?)
    .await?;
    guest
        .execute(
            "INSERT INTO file_fixture.document(id,file_id) VALUES ($1::text::uuid,$1::text::uuid)",
            &[&id.to_string()],
        )
        .await?;
    let refusal = fixture.mutate("delete", id, "files", None).await?;
    assert_eq!(refusal["error"]["code"], "file_referenced");
    assert_eq!(
        fixture
            .read("files", id, Some(&fixture.token(false)))
            .await?
            .2,
        b"referenced bytes"
    );
    fixture
        .registry
        .configure("connection.disable", json!({"name":files::STORE_NAME}))
        .await?;
    fixture
        .writer
        .execute(
            "UPDATE platform.connection SET status='disabled' WHERE id=$1::text::uuid",
            &[&fixture.registry.connection_id],
        )
        .await?;
    let row = guest.query_one("SELECT f.id::text,c.status FROM file_fixture.document d JOIN platform.file f ON f.id=d.file_id JOIN platform.connection c ON c.id=f.connection_id",&[]).await?;
    assert_eq!(row.get::<_, String>(0), id.to_string());
    assert_eq!(row.get::<_, String>(1), "disabled");
    assert_eq!(
        fixture
            .read("files", id, Some(&fixture.token(false)))
            .await?
            .0,
        403
    );
    Ok(())
}

#[test]
fn package_signed_read_admits_the_real_node_abi_and_manifest_owned_authority() -> anyhow::Result<()>
{
    let package = signed_read::signed_read_fixture(ENV_KEY)?;
    let serving = package
        .release
        .manifest()
        .components
        .first()
        .context("released file component")?;
    assert!(
        serving.descriptor.connections.iter().any(|connection| {
            connection.store_alias == "files"
                && connection.requirement_type == wamn_catalog::ComponentConnectionType::Blobstore
        }),
        "the release carries the admitted file binding"
    );
    let operation = &package.component.operations[signed_read::OPERATION];
    let reads =
        wamn_catalog::signed_file_reads(operation)?.context("package signed read metadata")?;
    assert_eq!(reads.fields.len(), 1);
    assert_eq!(reads.fields[0].path, "document");
    assert_eq!(
        reads.fields[0].read.lifetime_seconds()?,
        signed_read::LIFETIME
    );
    assert!(!reads.fields[0].nullable);
    assert_eq!(
        operation.registered_operation.as_deref(),
        Some(signed_read::OPERATION)
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn package_operation_returns_a_private_file_signed_url_that_expires() -> anyhow::Result<()> {
    use wamn_engine::artifact_source::LocalComponentSource;
    use wamn_execution_host::{OperationHost, OperationScope, RouterDeliveryBridge};
    use wamn_runtime::plugins::connection_http::transport::HttpTransport;
    use wamn_runtime::plugins::wamn_jetstream::WamnJetstream;
    use wamn_runtime::plugins::wamn_logging::{WamnLogging, WamnLoggingConfig};

    let package = signed_read::signed_read_fixture(ENV_KEY)?;
    let fixture =
        FileHost::start_with_release("package_signed_file", Arc::clone(&package.release)).await?;
    let bytes = b"private bytes returned by the package's signed read";
    let id = fixture.upload("files", bytes).await?;
    let outcome = fixture
        .mutate("expose", id, "files", Some("private"))
        .await?;
    assert!(
        outcome.get("error").is_none(),
        "private exposure: {outcome}"
    );
    // Grant only the package operation to this caller. The upload grants belong
    // to the fixture setup, and are removed before the signed read begins.
    fixture
        .writer
        .execute(
            "DELETE FROM platform.permission WHERE role_name='purchase-reader'",
            &[],
        )
        .await?;
    fixture
        .writer
        .execute(
            "INSERT INTO platform.permission(role_name,permission) VALUES ('purchase-reader',$1)",
            &[&signed_read::OPERATION.split('@').next().unwrap()],
        )
        .await?;
    let caller = fixture
        .host
        .routing
        .authenticate_authorization_for_test(signed_read::ATTACHMENT, Some(&fixture.token(false)))
        .await
        .map_err(|error| anyhow::anyhow!("signed read session authentication: {error:?}"))?
        .context("operation session caller")?;
    assert!(caller.permits(signed_read::OPERATION));
    assert!(!caller.permits("wamn-control:file/get"));
    let operations = Arc::new(OperationHost::new(
        Arc::new(wamn_engine::build_engine(&[])?),
        Arc::clone(&fixture.postgres),
        Arc::new(HttpTransport::new()?),
        Arc::new(ConnectionFacts::new(
            Arc::new(ConnectionMemory::default()),
            ConnectionChanges::default(),
            Arc::new(ConnectionCredentials::from_database(
                Arc::clone(&fixture.registry.reader),
                ORG,
            )),
            Arc::clone(&fixture.postgres),
            ENV_KEY,
            PROJECT,
            "dev",
        )),
        Arc::new(WamnLogging::new(&WamnLoggingConfig::default())?),
        Arc::from([]),
        Arc::clone(&package.release),
        Arc::new(LocalComponentSource::new(package.scratch.path().to_owned())),
        OperationScope {
            project: PROJECT.into(),
            schema: None,
            owner_prefix: "file-signed-read".into(),
            warm_reuse: wamn_engine::warm_reuse::WarmReuse::default(),
        },
    )?);
    operations.set_file_routes(Arc::clone(&fixture.routes))?;
    operations
        .released_application(&Arc::from([package.component.clone()]))
        .await?;
    let bridge =
        RouterDeliveryBridge::new(operations, Arc::new(WamnJetstream::from_env()), PROJECT)?;
    let report = bridge
        .deliver(
            DeliveryRequest {
                source: Source::Attachment(signed_read::ATTACHMENT.into()),
                delivery_id: Uuid::new_v4().to_string(),
                payload: json!([{"value":{"document":id}}]).to_string(),
                caller: None,
                trace: None,
                parent_causation: None,
                if_none_match: None,
            },
            Some(caller),
        )
        .await;
    let outcome = report
        .outcome
        .map_err(|error| anyhow::anyhow!("signed read delivery: {error:?}"))?;
    let DeliveryOutcome::Respond(payload) = outcome else {
        anyhow::bail!("signed read did not return its operation result: {outcome:?}");
    };
    let [request_element]: [Value; 1] = serde_json::from_str(&payload)?;
    let url = request_element["value"]["document"]
        .as_str()
        .context("host replaces the private file id with its signed URL")?;
    let lifetime = url::Url::parse(url)?
        .query_pairs()
        .find(|(key, _)| key == "X-Amz-Expires")
        .map(|(_, value)| value.into_owned());
    assert_eq!(lifetime.as_deref(), Some("3"));
    let client = fixture.minio.client()?;
    let response = client.get(url).send().await?;
    assert_eq!(response.status(), 200);
    assert_eq!(response.bytes().await?.as_ref(), bytes);
    assert_eq!(
        fixture
            .read("files", id, Some(&fixture.token(false)))
            .await?
            .0,
        403
    );
    assert_eq!(
        fixture
            .read("files", id, Some(&fixture.token(true)))
            .await?
            .0,
        403,
        "private exposure refuses direct file.get even for a granted admin"
    );
    tokio::time::sleep(Duration::from_secs(signed_read::LIFETIME + 1)).await;
    assert_eq!(client.get(url).send().await?.status(), 403);
    Ok(())
}
