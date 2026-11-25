//! Request path contracts through product deployment: credential isolation between
//! environments, and session identity on the host's pooled connections.

mod common;

use anyhow::{Context as _, ensure};
use common::request::{self, OtherEnvironment, wamn};
use common::{Application, Org};
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
    /// Each environment keeps its own credential: `connection get` shows its own hint,
    /// `connection copy` from A into B copies A's parameters and keeps B's credential, and
    /// B's host ServiceAccount may get its own credential Secret but not A's. The outside
    /// call with the credential is a service test (Epic 15 ruling Q-B).
    async fn another_environments_credential_is_refused() -> anyhow::Result<()> {
        let org = Org::create().await?;
        let a = OtherEnvironment::apply(&org, "alpha", "platform_fixture").await?;
        let b = OtherEnvironment::apply(&org, "beta", "platform_fixture").await?;
        let scratch = ScratchRoot::create()?;
        let file = |name: String, contents: String| -> anyhow::Result<String> {
            let path = scratch.path().join(name);
            std::fs::write(&path, contents)?;
            Ok(path.to_str().context("scratch path")?.to_owned())
        };
        let target =
            |triple: &Triple| json!({"project": triple.project, "env": triple.env, "name": CONNECTION});

        let mut hints = Vec::new();
        for (environment, header) in [(&a, "X-Alpha-Key"), (&b, "X-Beta-Key")] {
            let mut request = target(&environment.triple);
            request["integration_type"] = json!("http");
            request["auth_type"] = json!("api_key");
            request["parameters"] = json!({
                "primary-authority": "api.example.com",
                "tls-verification": "verify-authority",
                "auth": {"in": "header", "name": header},
            });
            let env = environment.triple.env.as_str();
            let credential =
                json!({"key": format!("{env}-{}", uuid::Uuid::new_v4().simple())}).to_string();
            // The product hint is the last four characters of the credential value.
            let hint: String = credential
                .chars()
                .skip(credential.chars().count() - 4)
                .collect();
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
            hints.push(hint);
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
        let (alpha, beta) = (get(&a.triple).await?, get(&b.triple).await?);
        ensure!(
            alpha["hint"] == hints[0].as_str(),
            "alpha shows its own hint: {alpha}"
        );
        ensure!(
            beta["hint"] == hints[1].as_str(),
            "beta shows its own hint: {beta}"
        );

        let mut copy = target(&b.triple);
        copy["source_env"] = json!(a.triple.env);
        let copy = file("copy.json".to_owned(), copy.to_string())?;
        wamn(&org, &["connection", "copy", "--file", &copy]).await?;
        let copied = get(&b.triple).await?;
        ensure!(
            copied["parameters"] == alpha["parameters"] && copied["hint"] == hints[1].as_str(),
            "a copy takes alpha's parameters and keeps beta's credential: {copied}"
        );

        let client = &org.cluster.client;
        let account = name::environment_service_account(&org.name, &b.triple.env);
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
        let application = Application::prepare(&request::fixture("platform_fixture")).await?;
        let client = &application.environment.org.cluster.client;
        let environment = &application.environment;

        let old = request::delete_host(client, environment, Some(0)).await?;
        request::wait_host_restarted(client, environment, &old).await?;
        let (status, answer) = request::read(&application.routes, "/widget/list", &json!({})).await?;
        ensure!(
            status == 200,
            "the first read after a restart answers {status}: {answer}"
        );

        // `widget.code` is unique and takes two values, so the concurrent writes create
        // counters, which keep a history, and numbered labels, which carry actor stamps.
        let (second, other) = request::caller(&application, "second").await?;
        let callers = [
            (application.caller().to_string(), &application.routes),
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

        let mut database = environment.observe().await?;
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

        let counters = "SELECT s.application, c.sequence_id::text, c.counter, c.key_value, c.value \
                        FROM platform.number_counter c \
                        JOIN platform.number_sequence s ON s.id = c.sequence_id";
        let before: Vec<(String, String, String, String, i64)> = sqlx::query_as(counters)
            .fetch_all(&mut database.connection)
            .await?;
        application
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
