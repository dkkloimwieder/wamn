//! Request path contracts through product deployment: credential isolation between
//! environments, and session identity on the host's pooled connections.

mod common;

use anyhow::{Context as _, ensure};
use common::request::{self, OtherEnv, wamn};
use common::{App, Org};
use k8s_openapi::api::authorization::v1::{
    ResourceAttributes, SubjectAccessReview, SubjectAccessReviewSpec,
};
use kube::Api;
use kube::api::PostParams;
use serde_json::{Value, json};
use wamn_registry::{Triple, name};
use wamn_test_infrastructure::scratch::ScratchRoot;

const CONNECTION: &str = "upstream";

common::journey! {
    /// The control command removes the selected connection's Secret entries.
    async fn deactivation_removes_only_the_connections_secret_versions() -> anyhow::Result<()> {
        use common::host::{control, control_reply};
        use k8s_openapi::api::core::v1::Secret;

        let org = Org::create().await?;
        let project = OtherEnv::apply(&org, "credentials", "platform_fixture").await?;
        let target = |name: &str| json!({
            "project":project.triple.project,"env":project.triple.env,"name":name
        });
        let parameters = json!({
            "primary-authority":"api.example.invalid",
            "tls-verification":"verify-authority",
            "auth":{"in":"header","name":"X-Test-Key"}
        });
        for name in ["retiring", "unrelated"] {
            let mut input = target(name);
            input["integration_type"] = json!("http");
            input["auth_type"] = json!("api_key");
            input["parameters"] = parameters.clone();
            input["credential"] = json!({"key":name}).to_string().into();
            control(&org, "connection.write", input).await?;
        }
        let mut rotation = target("retiring");
        rotation["credential"] = json!({"key":"second"}).to_string().into();
        control(&org, "connection.set_credential", rotation).await?;
        let mut observed = common::admit_observer(&org.cluster, &project.triple).await?;
        let versions: Vec<(String, String)> = sqlx::query_as(
            "SELECT c.name, k.version FROM platform.credential k
             JOIN platform.connection c ON c.id=k.connection_id",
        )
        .fetch_all(&mut observed.connection)
        .await?;
        let secrets = Api::<Secret>::namespaced(org.cluster.client.clone(), org.name.as_str());
        let secret_name = name::credential_secret_name(
            &org.name, &project.triple.project, &project.triple.env,
        );
        let before = secrets.get(&secret_name).await?.data.context("credential entries")?;
        for (_, version) in &versions {
            assert!(before.contains_key(version));
        }

        control(&org, "connection.deactivate", target("retiring")).await?;
        let after = secrets.get(&secret_name).await?.data.unwrap_or_default();
        for (name, version) in &versions {
            if name == "retiring" {
                assert!(!after.contains_key(version));
            } else {
                assert_eq!(after.get(version), before.get(version));
            }
        }

        let mut candidate = target("retiring");
        candidate["parameters"] = parameters;
        candidate["credential"] = json!({"key":"candidate"}).to_string().into();
        let answer = control_reply(&org, "connection.test", candidate).await?;
        assert_eq!(answer["error"]["code"], "connection_inactive");
        Ok(())
    }
}

common::journey! {
    /// `connection copy` from A into B keeps B's active Secret version, and
    /// B's host ServiceAccount may get its own credential Secret but not A's. The outside
    /// call with the credential is a service test (Epic 15 ruling Q-B).
    async fn another_envs_credential_is_refused() -> anyhow::Result<()> {
        let org = Org::create().await?;
        let a = OtherEnv::apply(&org, "alpha", "platform_fixture").await?;
        let b = OtherEnv::apply(&org, "beta", "platform_fixture").await?;
        let scratch = ScratchRoot::create()?;
        let file = |name: String, contents: String| -> anyhow::Result<String> {
            let path = scratch.path().join(name);
            std::fs::write(&path, contents)?;
            Ok(path.to_str().context("scratch path")?.to_owned())
        };
        let target =
            |triple: &Triple| json!({"project": triple.project, "env": triple.env, "name": CONNECTION});

        let mut beta_credential = String::new();
        for (env, header) in [(&a, "X-Alpha-Key"), (&b, "X-Beta-Key")] {
            let mut request = target(&env.triple);
            request["integration_type"] = json!("http");
            request["auth_type"] = json!("api_key");
            request["parameters"] = json!({
                "primary-authority": "api.example.com",
                "tls-verification": "verify-authority",
                "auth": {"in": "header", "name": header},
            });
            let env = env.triple.env.as_str();
            let credential =
                json!({"key": format!("{env}-{}", uuid::Uuid::new_v4().simple())}).to_string();
            if env == "beta" {
                beta_credential.push_str(&credential);
            }
            let request = file(format!("{env}-write.json"), request.to_string())?;
            let credential = file(format!("{env}-credential.json"), credential)?;
            wamn(
                &org,
                &[
                    "connection",
                    "write",
                    "--file",
                    &request,
                    "--credential-file",
                    &credential,
                ],
            )
            .await?;
        }
        let get = async |triple: &Triple| -> anyhow::Result<Value> {
            let request = file(
                format!("{}-get.json", triple.env),
                target(triple).to_string(),
            )?;
            Ok(serde_json::from_str(
                &wamn(&org, &["connection", "get", "--file", &request]).await?,
            )?)
        };
        let alpha = get(&a.triple).await?;

        let mut copy = target(&b.triple);
        copy["source_env"] = json!(a.triple.env);
        let copy = file("copy.json".to_owned(), copy.to_string())?;
        wamn(&org, &["connection", "copy", "--file", &copy]).await?;
        let copied = get(&b.triple).await?;
        ensure!(
            copied["parameters"] == alpha["parameters"],
            "a copy takes alpha's parameters: {copied}"
        );

        let client = &org.cluster.client;
        let mut observed = common::admit_observer(&org.cluster, &b.triple).await?;
        let (reference, version): (String, String) = sqlx::query_as(
            "SELECT k.ref,k.version FROM platform.connection c
             JOIN platform.connection_generation g ON (g.connection_id,g.generation)=(c.id,c.active_generation)
             JOIN platform.credential k ON k.id=g.credential_id WHERE c.name=$1",
        )
        .bind(CONNECTION)
        .fetch_one(&mut observed.connection)
        .await?;
        let secrets = Api::<k8s_openapi::api::core::v1::Secret>::namespaced(
            client.clone(), org.name.as_str(),
        );
        let mut entries = secrets.get(&reference).await?.data.context("credential entries")?;
        let credential = entries.remove(&version).context("active credential version")?;
        ensure!(
            credential.0 == beta_credential.as_bytes(),
            "copy must retain beta's credential"
        );
        let account = name::env_service_account(&org.name, &b.triple.env);
        for (owner, allowed) in [(&a.triple, false), (&b.triple, true)] {
            let secret = name::credential_secret_name(&org.name, &owner.project, &owner.env);
            let review = Api::<SubjectAccessReview>::all(client.clone())
                .create(
                    &PostParams::default(),
                    &SubjectAccessReview {
                        spec: SubjectAccessReviewSpec {
                            user: Some(format!("system:serviceaccount:{}:{account}", org.name)),
                            resource_attributes: Some(ResourceAttributes {
                                namespace: Some(org.name.to_string()),
                                verb: Some("get".to_owned()),
                                resource: Some("secrets".to_owned()),
                                name: Some(secret.clone()),
                                ..ResourceAttributes::default()
                            }),
                            ..SubjectAccessReviewSpec::default()
                        },
                        ..SubjectAccessReview::default()
                    },
                )
                .await?;
            let answer = review.status.context("the review has no status")?.allowed;
            ensure!(
                answer == allowed,
                "{account} get secret {secret}: allowed {answer}, expected {allowed}"
            );
        }
        drop((b, a));
        Ok(())
    }
}

common::journey! {
    /// After the host pod restarts, the first read parses its statements cold and answers.
    /// Concurrent writes of two API services on the host's pooled connections each carry
    /// their own caller: the actor stamps, and the history operation and changer. And
    /// `number.issue` advances only this application's counters.
    async fn each_session_carries_its_own_identity() -> anyhow::Result<()> {
        let app = App::prepare(&request::fixture("platform_fixture")).await?;
        let client = &app.env.org.cluster.client;
        let env = &app.env;

        public_roots_arrive_with_their_certificates(&env.org).await?;
        let old = request::delete_host(client, env, Some(0)).await?;
        request::wait_host_restarted(client, env, &old).await?;
        let (status, answer) = request::read(&app.routes, "/widget/list", &json!({})).await?;
        ensure!(
            status == 200,
            "the first read after a restart answers {status}: {answer}"
        );

        // `widget.code` is unique and takes two values, so the concurrent writes create
        // counters, which keep a history, and numbered labels, which carry actor stamps.
        let (second, other) = request::caller(&app, "second").await?;
        let callers = [
            (app.caller().to_string(), &app.routes),
            (second.to_string(), &other),
        ];
        let calls = callers.iter().flat_map(|(caller, routes)| {
            (0..20).map(move |index| async move {
                let key = format!("{caller}-{index}");
                let (path, element) = if index % 2 == 0 {
                    (
                        "/counter/create",
                        json!({"request_id": key, "idempotency_key": key, "label": key}),
                    )
                } else {
                    let id = uuid::Uuid::now_v7().to_string();
                    (
                        "/numbered_label/create",
                        json!({"request_id": key, "idempotency_key": key, "id": id, "stock_id": "s"}),
                    )
                };
                let answer = routes.post(path, element).await?;
                let id = &answer["value"]["id"];
                let id = id
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| id.as_i64().map(|id| id.to_string()))
                    .with_context(|| format!("{path} answers an id: {answer}"))?;
                anyhow::Ok((path, id, caller))
            })
        });
        let written = futures_util::future::try_join_all(calls).await?;

        let mut database = env.observe().await?;
        for (path, id, caller) in &written {
            if *path == "/counter/create" {
                let (operation, changed_by): (String, String) = sqlx::query_as(
                    "SELECT operation, changed_by::text FROM inventory.counter_history \
                     WHERE type = 'insert' AND row_key = jsonb_build_object('id', $1::bigint)",
                )
                .bind(id.parse::<i64>()?)
                .fetch_one(&mut database.connection)
                .await?;
                ensure!(
                    operation.starts_with("platform-fixture:counter/create") && changed_by == **caller,
                    "counter {id} of {caller} logs {operation} by {changed_by}"
                );
            } else {
                let (created_by, updated_by): (String, String) = sqlx::query_as(
                    "SELECT created_by::text, updated_by::text FROM inventory.numbered_label \
                     WHERE id = $1::uuid",
                )
                .bind(id)
                .fetch_one(&mut database.connection)
                .await?;
                ensure!(
                    created_by == **caller && updated_by == **caller,
                    "numbered label {id} of {caller} is stamped {created_by} and {updated_by}"
                );
            }
        }

        let counters = "SELECT s.app, c.sequence_id::text, c.counter, c.key_value, c.value \
                        FROM platform.number_counter c \
                        JOIN platform.number_sequence s ON s.id = c.sequence_id";
        let before: Vec<(String, String, String, String, i64)> = sqlx::query_as(counters)
            .fetch_all(&mut database.connection)
            .await?;
        app
            .routes
            .post(
                "/number/issue",
                json!({"request_id": "issue", "value": {"idempotency_key": "issue", "batch_id": "a"}}),
            )
            .await?;
        let after: Vec<(String, String, String, String, i64)> = sqlx::query_as(counters)
            .fetch_all(&mut database.connection)
            .await?;
        let advanced: Vec<_> = after.iter().filter(|row| !before.contains(row)).collect();
        ensure!(
            !advanced.is_empty() && advanced.iter().all(|row| row.0 == "platform_fixture"),
            "number.issue advances only this application's counters: {advanced:?}"
        );
        Ok(())
    }
}

/// Check the controller-produced targets of the installed Bundles, using their
/// source certificate Secrets and the cluster's actual namespace labels.
async fn public_roots_arrive_with_their_certificates(org: &Org) -> anyhow::Result<()> {
    use k8s_openapi::api::core::v1::{ConfigMap, Namespace, Secret};
    use kube::api::{ApiResource, DynamicObject, GroupVersionKind, ListParams};
    use rustls::pki_types::{CertificateDer, pem::PemObject as _};

    let client = &org.cluster.client;
    let namespaces = Api::<Namespace>::all(client.clone());
    let (hosts, tenant) =
        tokio::try_join!(namespaces.get("hosts"), namespaces.get(org.name.as_str()))?;
    let resource = ApiResource::from_gvk_with_plural(
        &GroupVersionKind::gvk("trust.cert-manager.io", "v1alpha1", "Bundle"),
        "bundles",
    );
    let bundles = Api::<DynamicObject>::all_with(client.clone(), &resource);
    let sources = Api::<Secret>::namespaced(client.clone(), "platform");
    let records = latest_trust_release_records(org).await?;
    for (release, record) in &records {
        ensure!(
            !record.contains("PRIVATE KEY-----"),
            "{release} contains a PEM private key"
        );
    }
    for bundle in bundles.list(&ListParams::default()).await? {
        let name = bundle.metadata.name.as_deref().context("Bundle name")?;
        let spec = &bundle.data["spec"];
        let selector = spec["target"]["namespaceSelector"]["matchLabels"]
            .as_object()
            .context("Bundle namespace selector")?;
        let key = spec["target"]["configMap"]["key"]
            .as_str()
            .context("target PEM key")?;
        let source = &spec["sources"][0]["secret"];
        let secret = sources
            .get(source["name"].as_str().context("source Secret name")?)
            .await?;
        let source_data = secret.data.context("source Secret data")?;
        let private_key =
            serde_json::to_value(source_data.get("tls.key").context("source signing key")?)?;
        let encoded_key = private_key.as_str().context("encoded source signing key")?;
        for (release, record) in &records {
            ensure!(
                !record.contains(encoded_key),
                "{release} contains the signing key for Bundle {name}"
            );
        }
        let pem = &source_data
            .get(source["key"].as_str().context("source PEM key")?)
            .context("source PEM")?
            .0;
        let certificate = CertificateDer::from_pem_slice(pem)?;
        for namespace in [&hosts, &tenant] {
            if !selector.iter().all(|(key, value)| {
                namespace
                    .metadata
                    .labels
                    .as_ref()
                    .and_then(|labels| labels.get(key))
                    .map(String::as_str)
                    == value.as_str()
            }) {
                continue;
            }
            let ns = namespace
                .metadata
                .name
                .as_deref()
                .context("namespace name")?;
            let map = Api::<ConfigMap>::namespaced(client.clone(), ns)
                .get(name)
                .await?;
            let data = map.data.context("root bundle data")?;
            let pem = data.get(key).context("public PEM")?.as_bytes();
            let certificates =
                CertificateDer::pem_slice_iter(pem).collect::<Result<Vec<_>, _>>()?;
            ensure!(
                certificates.as_slice() == std::slice::from_ref(&certificate)
                    && !pem.windows(11).any(|bytes| bytes == b"PRIVATE KEY"),
                "Bundle {name} in {ns} must contain only the source certificate"
            );
        }
    }
    Ok(())
}

async fn latest_trust_release_records(org: &Org) -> anyhow::Result<Vec<(&'static str, String)>> {
    let outputs = &org.cluster.outputs;
    let storage = format!("wamn-helm-{}", org.name);
    let mut records = Vec::new();
    for (namespace, release) in [
        ("platform", "wamn-infrastructure"),
        ("platform", "wamn"),
        (storage.as_str(), "wamn-org"),
    ] {
        let output = tokio::process::Command::new("helm")
            .args([
                "get",
                "all",
                release,
                "-n",
                namespace,
                "--kube-context",
                &outputs.context,
            ])
            .arg("--kubeconfig")
            .arg(&outputs.kubeconfig)
            .output()
            .await?;
        ensure!(
            output.status.success(),
            "read the latest Helm release {namespace}/{release}"
        );
        records.push((release, String::from_utf8(output.stdout)?));
    }
    Ok(records)
}
