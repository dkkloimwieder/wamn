//! Project authority and the public HTTP callback on the control login.

use super::*;
use base64::Engine as _;
use http_body_util::{BodyExt as _, Full};
use sha2::{Digest as _, Sha256};
use wash_runtime::host::http::{
    HostHandler as _, Ingress, IngressRoute, NormalizedPathPrefix, RouteScope, ServiceHttpJob,
};

#[tokio::test]
async fn project_admin_authorize_and_public_pkce_callback() -> anyhow::Result<()> {
    let _lock = wamn_test_postgres::lock();
    let mut postgres = wamn_test_postgres::start(&[])?;
    let database = postgres.create_database("control_oauth_authorization")?;
    let mut admin = connect(database.url()).await?;
    let control_url = install(&admin, database.url()).await?;
    let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create()?;
    let key = scratch.path().join("oauth.key");
    std::fs::write(&key, [9_u8; 32])?;
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600))?;
    }
    let store = Arc::new(wamn_test_infrastructure::credentials::LocalCredentials::create()?);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let token_url = format!("http://{}/token", listener.local_addr()?);
    let challenge = Arc::new(std::sync::Mutex::new(None::<String>));
    let expected = Arc::clone(&challenge);
    let provider = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let expected = Arc::clone(&expected);
            hyper::server::conn::http1::Builder::new().serve_connection(hyper_util::rt::TokioIo::new(stream),hyper::service::service_fn(move |request:hyper::Request<hyper::body::Incoming>| {
                let expected = Arc::clone(&expected);
                async move {
                    assert_eq!(request.uri().path(),"/token");
                    let body = request.into_body().collect().await.unwrap().to_bytes();
                    let form:std::collections::HashMap<_,_> = url::form_urlencoded::parse(&body).into_owned().collect();
                    assert_eq!(form["grant_type"],"authorization_code");
                    assert_eq!(form["code"],"accepted-code");
                    assert_eq!(form["client_id"],"admin-client");
                    assert_eq!(form["client_secret"],"private-secret");
                    let actual = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(form["code_verifier"].as_bytes()));
                    assert_eq!(expected.lock().unwrap().as_deref(),Some(actual.as_str()));
                    Ok::<_,std::convert::Infallible>(hyper::Response::new(Full::new(bytes::Bytes::from_static(br#"{"access_token":"stored-access","refresh_token":"stored-refresh","token_type":"Bearer","expires_in":3600}"#))))
                }
            })).await.unwrap();
        }
    });
    admin.batch_execute(&format!("SET app.user_id='{}'; INSERT INTO registry.target(name,type,status) VALUES('oauth-fixture','local','active'); UPDATE registry.project_envs SET target_id=(SELECT id FROM registry.target WHERE name='oauth-fixture')",PlatformComponent::Provisioning.principal_id())).await?;
    let tx = admin.transaction().await?;
    wamn_test_infrastructure::credentials::define(&tx,ORG,&json!({"project":"billing","env":"dev","name":"oauth","integration_type":"http","auth_type":"oauth2_authorization_code","parameters":{"primary-authority":token_url,"tls-verification":"disabled","auth":{"client_id":"admin-client","authorize_url":"https://provider.example/authorize?existing=a%2Bb","token_url":token_url,"scopes":["read+","write"]}},"credential":json!({"client_secret":"private-secret"}).to_string()}),store.as_ref()).await?;
    tx.commit().await?;
    let boss = create_or_reuse_user(&admin, "oauth-admin@example.test", "OAuth admin")
        .await?
        .principal_id;
    invite_member(
        &admin,
        &boss,
        ORG,
        &MemberGrants {
            project_admins: vec!["billing".to_owned()],
            ..MemberGrants::default()
        },
    )
    .await?;
    let outsider = create_or_reuse_user(&admin, "oauth-other@example.test", "Other admin")
        .await?
        .principal_id;
    invite_member(
        &admin,
        &outsider,
        ORG,
        &MemberGrants {
            project_admins: vec!["shop".to_owned()],
            ..MemberGrants::default()
        },
    )
    .await?;
    let (_, stopping) = tokio::sync::watch::channel(false);
    let ingress = Ingress::builder(
        wamn_engine::expected_router::expected_host_router(None, stopping),
        "127.0.0.1:0".parse()?,
    )
    .build()
    .await?;
    let oauth = Arc::new(
        wamn_runtime::oauth_authorization::Authorization::new(
            &format!("http://{}", ingress.addr()),
            Arc::new(wamn_runtime::oauth_encryption::Envelope::from_file(&key)?),
        )?
        .with_store(store),
    );
    let writer = Arc::new(tokio::sync::Mutex::new(connect(&control_url).await?));
    let (sender, mut receiver) = tokio::sync::mpsc::channel::<ServiceHttpJob>(16);
    ingress
        .on_service_http_resolved(
            "oauth-fixture",
            &[IngressRoute {
                host: "127.0.0.1".into(),
                path_prefix: NormalizedPathPrefix::new("/oauth/callback"),
                scope: RouteScope::Ingress,
            }],
            sender,
        )
        .await?;
    let callback_oauth = Arc::clone(&oauth);
    let callback_writer = Arc::clone(&writer);
    let callback = tokio::spawn(async move {
        while let Some(job) = receiver.recv().await {
            let response = wamn_execution_host::oauth_callback::response(
                &callback_oauth,
                &callback_writer,
                ORG,
                job.req,
            )
            .await;
            let _ = job.resp_tx.send(Ok(response));
        }
    });
    ingress.start().await?;
    let (_nats_server, nats) = wamn_runtime::auth_cache::test_util::nats().await;
    let delivery = HostRouteDelivery::new(
        Arc::new(LoadedRelease::control_root()),
        HostRouteHandlers::Control {
            control: Arc::new(connect(&control_url).await?),
            writer,
            identity: None,
            administration: None,
            status: Arc::new(wamn_execution_host::environment_status::KubernetesSource),
            org: ORG.to_owned(),
            changes: AuthChanges::new(nats),
        },
        None,
    )
    .with_oauth(Some(Arc::clone(&oauth)));
    let selection = json!({"project":"billing","env":"dev","name":"oauth"});
    assert!(
        call(
            &delivery,
            "connection/authorize",
            outsider.as_str(),
            selection.clone()
        )
        .await
        .unwrap_err()
        .starts_with("permission denied")
    );
    assert_eq!(
        admin
            .query_one("SELECT count(*) FROM identity.oauth_authorization", &[])
            .await?
            .get::<_, i64>(0),
        0
    );
    let metadata = call(
        &delivery,
        "connection/get",
        boss.as_str(),
        selection.clone(),
    )
    .await
    .unwrap();
    assert_eq!(metadata["redirect_uri"], oauth.redirect_uri());
    assert!(metadata.get("credential").is_none());
    let answer = call(&delivery, "connection/authorize", boss.as_str(), selection)
        .await
        .unwrap();
    let url = url::Url::parse(answer["authorization_url"].as_str().unwrap())?;
    let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(query["existing"], "a+b");
    assert_eq!(query["redirect_uri"], oauth.redirect_uri());
    assert_eq!(query["scope"], "read+ write");
    *challenge.lock().unwrap() = Some(query["code_challenge"].clone());
    let state = &query["state"];
    let row = admin.query_one("SELECT created_by,expires_at>clock_timestamp()+interval '9 minutes' AS recent,code_verifier FROM identity.oauth_authorization WHERE state=$1",&[state]).await?;
    assert_eq!(
        row.get::<_, uuid::Uuid>("created_by").to_string(),
        boss.as_str()
    );
    assert!(row.get::<_, bool>("recent"));
    assert_ne!(row.get::<_, Vec<u8>>("code_verifier").len(), 43);
    let client = reqwest::Client::new();
    let callback_url = format!("{}?state={state}&code=accepted-code", oauth.redirect_uri());
    assert_eq!(client.post(&callback_url).send().await?.status(), 405);
    let refused = client
        .get(format!(
            "{}?state={state}&error=%3Cscript%3E%26%22%27",
            oauth.redirect_uri()
        ))
        .send()
        .await?;
    assert_eq!(refused.status(), 400);
    assert!(
        refused
            .text()
            .await?
            .contains("&lt;script&gt;&amp;&quot;&#39;")
    );
    let success = client.get(&callback_url).send().await?;
    assert_eq!(success.status(), 200);
    assert!(success.text().await?.contains("Authorization succeeded"));
    let replay = client.get(&callback_url).send().await?;
    assert_eq!(replay.status(), 400);
    assert!(replay.text().await?.contains("replayed state"));
    assert_eq!(
        admin
            .query_one("SELECT count(*) FROM identity.oauth_authorization", &[])
            .await?
            .get::<_, i64>(0),
        0
    );
    let row = admin
        .query_one(
            "SELECT ciphertext,wrapped_key,nonce,key_id,refresh_status FROM identity.oauth_token",
            &[],
        )
        .await?;
    assert_eq!(row.get::<_, String>("refresh_status"), "ok");
    let envelope = wamn_runtime::oauth_encryption::Envelope::from_file(&key)?;
    let plaintext = envelope
        .decrypt(&wamn_runtime::oauth_encryption::Encrypted {
            ciphertext: row.get("ciphertext"),
            wrapped_key: row.get("wrapped_key"),
            nonce: row.get("nonce"),
            key_id: row.get("key_id"),
        })
        .await?;
    let token: Value = serde_json::from_slice(plaintext.expose())?;
    assert_eq!(token["access_token"], "stored-access");
    assert_eq!(token["refresh_token"], "stored-refresh");
    ingress.stop().await?;
    callback.abort();
    provider.abort();
    Ok(())
}
