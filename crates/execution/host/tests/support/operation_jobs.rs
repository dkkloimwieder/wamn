//! Real control authority with a recorded Kubernetes transport.

use super::*;
use anyhow::Context as _;
use std::collections::BTreeMap;
use wamn_catalog::HostHandler;
use wamn_kubernetes::kubernetes_api::KubernetesApi;

struct Api {
    client: KubernetesApi,
    requests: mpsc::UnboundedReceiver<(String, Value)>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Api {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn api(writer: Arc<tokio::sync::Mutex<Client>>) -> anyhow::Result<Api> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}", listener.local_addr()?);
    let (sender, requests) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        let mut jobs = BTreeMap::<String, Value>::new();
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut bytes = Vec::new();
            let mut buffer = [0_u8; 4096];
            let end = loop {
                let size = stream.read(&mut buffer).await.unwrap();
                if size == 0 {
                    return;
                }
                bytes.extend_from_slice(&buffer[..size]);
                if let Some(index) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                    break index + 4;
                }
            };
            let headers = String::from_utf8(bytes[..end].to_vec()).unwrap();
            let length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            while bytes.len() < end + length {
                let size = stream.read(&mut buffer).await.unwrap();
                assert!(size > 0);
                bytes.extend_from_slice(&buffer[..size]);
            }
            assert!(
                writer.try_lock().is_ok(),
                "Kubernetes call holds the control writer"
            );
            let path = headers.lines().next().unwrap().to_owned();
            let mut value: Value = if length == 0 {
                Value::Null
            } else {
                serde_json::from_slice(&bytes[end..end + length]).unwrap()
            };
            sender.send((path.clone(), value.clone())).unwrap();
            let mut status = "200 OK";
            let body = if path.starts_with("POST /apis/batch/") {
                value["metadata"]["uid"] = "00000000-0000-4000-8000-000000000001".into();
                value["status"]["conditions"] = json!([{"type":"Complete","status":"True"}]);
                jobs.insert(
                    value["metadata"]["name"].as_str().unwrap().into(),
                    value.clone(),
                );
                value.to_string()
            } else if path.contains("/configmaps/wamn-operation-org-a HTTP") {
                json!({"data":{"inputs":"wamn-operation-org-a-selected"}}).to_string()
            } else if path.contains("/configmaps/wamn-operation-") {
                json!({"data":{"image":format!("registry/ctl@sha256:{}","a".repeat(64))}})
                    .to_string()
            } else if path.contains("/namespaces/hosts/configmaps/") {
                json!({"data":{"environment.json":json!({"org":ORG,"project":"billing","env":"dev","applications":{"billing_app":{}}}).to_string()}}).to_string()
            } else if path.contains("/configmaps/") {
                json!({"data":{"cluster":"wamn-pg"}}).to_string()
            } else if path.contains("/jobs/") {
                let name = path
                    .split_whitespace()
                    .nth(1)
                    .unwrap()
                    .rsplit('/')
                    .next()
                    .unwrap();
                if let Some(job) = jobs.get(name) {
                    job.to_string()
                } else {
                    status = "404 Not Found";
                    json!({}).to_string()
                }
            } else if path.contains("/log?") {
                "a".repeat(40 * 1024)
            } else if path.contains("/pods?") {
                json!({"items":[{"metadata":{"name":"operation-pod","ownerReferences":[{"kind":"Job","uid":"00000000-0000-4000-8000-000000000001"}]},"status":{"containerStatuses":[{"name":"operation","state":{"terminated":{"exitCode":0}}}]}}]}).to_string()
            } else {
                panic!("unexpected Kubernetes request {path}");
            };
            stream.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        }
    });
    Ok(Api {
        client: KubernetesApi::with_client(
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .build(
                    hyper_rustls::HttpsConnectorBuilder::new()
                        .with_tls_config(
                            rustls::ClientConfig::builder_with_provider(Arc::new(
                                rustls::crypto::aws_lc_rs::default_provider(),
                            ))
                            .with_safe_default_protocol_versions()?
                            .with_root_certificates(rustls::RootCertStore::empty())
                            .with_no_client_auth(),
                        )
                        .https_or_http()
                        .enable_http1()
                        .build(),
                ),
            endpoint,
            "protected-token".into(),
            "hosts".into(),
        ),
        requests,
        task,
    })
}

#[tokio::test]
#[ignore = "requires: env:WAMN_NATIVE_C_NATS_BIN"]
async fn operations_authorize_before_jobs_and_bound_status_to_the_target() -> anyhow::Result<()> {
    wamn_test_postgres::require_ignored_prerequisites("requires: env:WAMN_NATIVE_C_NATS_BIN");
    let mut postgres = wamn_test_postgres::start(&[])?;
    let database = postgres.create_database("operation_routes")?;
    let mut admin = connect(database.url()).await?;
    let control_url = install(&mut admin, database.url()).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!(
            "SET app.user_id='{}'",
            PlatformComponent::Provisioning.principal_id()
        ))
            .to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)?;
    let owner = create_or_reuse_user(&mut admin, "job-owner@example.test", "Owner")
        .await?
        .principal_id;
    let other = create_or_reuse_user(&mut admin, "job-other@example.test", "Other")
        .await?
        .principal_id;
    for (principal, project) in [(&owner, "billing"), (&other, "shop")] {
        invite_member(
            &mut admin,
            principal,
            ORG,
            &MemberGrants {
                project_admins: vec![project.into()],
                ..MemberGrants::default()
            },
        )
        .await?;
    }
    let (_server, nats) = wamn_runtime::auth_cache::test_util::nats().await;
    let writer = Arc::new(tokio::sync::Mutex::new(connect(&control_url).await?));
    let mut api = api(Arc::clone(&writer)).await?;
    let delivery = HostRouteDelivery::new(
        Arc::new(LoadedRelease::control_root()),
        HostRouteHandlers::Control {
            status: Arc::new(wamn_execution_host::environment_status::KubernetesSource),
            administration: None,
            control: wamn_postgres::pool(&control_url, 4).await?,
            writer,
            identity: None,
            org: ORG.into(),
            changes: Publisher::new(nats),
        },
        None,
    )
    .with_operation_jobs(api.client.clone());
    let mut started = Vec::new();
    for (_, attachment) in HostRouteSet::Control
        .attachments()
        .filter(|(_, attachment)| attachment.route.handler == HostHandler::OperationJob)
    {
        let route = attachment.route.operation;
        let path = route.replace('.', "/");
        let mut value = json!({"project":"billing","env":"dev"});
        match route {
            "application.deploy" => {
                value["application"] = "billing_app".into();
                value["release"] = format!("sha256:{}", "a".repeat(64)).into();
            }
            "application.rollback" | "schema.apply" | "schema.diff" => {
                value["application"] = "billing_app".into();
            }
            "operation.status" => {
                value["job"] = "op-00000000-0000-4000-8000-000000000000".into();
            }
            _ => {}
        }
        for caller in [
            "00000000-0000-4000-8000-000000000099",
            other.to_string().as_str(),
        ] {
            assert!(
                call(&delivery, &path, caller, value.clone())
                    .await
                    .unwrap_err()
                    .starts_with("permission denied "),
                "{route}"
            );
        }
        for field in [
            "org",
            "image",
            "command",
            "namespace",
            "actor",
            "target",
            "secret",
            "chart_path",
        ] {
            let mut injected = value.clone();
            injected[field] = "untrusted".into();
            assert!(
                call(&delivery, &path, owner.to_string().as_str(), injected)
                    .await
                    .is_err(),
                "{route}: {field}"
            );
        }
        let mut wrong = value.clone();
        wrong["env"] = "absent".into();
        assert!(
            call(&delivery, &path, owner.to_string().as_str(), wrong)
                .await
                .unwrap_err()
                .starts_with("environment_not_found ")
        );
        assert!(
            api.requests.try_recv().is_err(),
            "a refused request reached Kubernetes: {route}"
        );
        if route == "operation.status" {
            continue;
        }
        let result = call(&delivery, &path, owner.to_string().as_str(), value)
            .await
            .unwrap();
        started.push(result["job"].as_str().unwrap().to_owned());
        let mut created = None;
        while let Ok((request, body)) = api.requests.try_recv() {
            if request.starts_with("POST ") {
                assert!(created.replace(body).is_none());
            }
        }
        let job = created.context("start creates one Job")?;
        assert_eq!(job["metadata"]["name"], result["job"]);
        let actor = job["spec"]["template"]["spec"]["containers"][0]["env"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["name"] == "WAMN_DEPLOY_ACTOR")
            .unwrap();
        assert_eq!(actor["value"], owner.to_string().as_str());
    }
    assert!(
        call(
            &delivery,
            "operation/status",
            owner.to_string().as_str(),
            json!({"project":"billing","env":"dev","job":"op-00000000-0000-4000-8000-000000000000"})
        )
        .await
        .unwrap_err()
        .starts_with("not_found ")
    );
    assert!(api.requests.try_recv()?.0.contains("/jobs/"));
    assert!(api.requests.try_recv().is_err());
    for job in started {
        let value = json!({"project":"billing","env":"dev","job":job});
        let result = call(
            &delivery,
            "operation/status",
            owner.to_string().as_str(),
            value.clone(),
        )
        .await
        .unwrap();
        assert_eq!(result["state"], "succeeded");
        assert_eq!(result["log"].as_str().unwrap().len(), 32 * 1024);
        while let Ok((path, _)) = api.requests.try_recv() {
            if path.contains("/log?") {
                assert!(path.contains("tailLines=100&limitBytes=32768"));
            }
            assert!(!path.starts_with("POST "));
        }
        let mut wrong = value;
        wrong["project"] = "shop".into();
        assert!(
            call(
                &delivery,
                "operation/status",
                other.to_string().as_str(),
                wrong
            )
            .await
            .unwrap_err()
            .starts_with("not_found ")
        );
        let (path, _) = api.requests.try_recv()?;
        assert!(path.contains("/jobs/"));
        assert!(
            api.requests.try_recv().is_err(),
            "wrong target reached pod logs"
        );
    }
    assert!(
        !HostRouteSet::Control
            .attachments()
            .any(|(_, attachment)| attachment.route.operation.starts_with("system_schema."))
    );
    Ok(())
}
