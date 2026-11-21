//! Host contracts through product deployment: probes, the host's NATS user, the stop
//! and status path, and the grant cache lifetime.

mod common;

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::{Context as _, ensure};
use common::host::{
    FIXTURE, Forward, application_get, application_objects, control, declare, fixture, host_object,
    identity_post, member, ready_host_pod, reapply, until, workload_count,
};
use common::{Application, Org};
use k8s_openapi::api::authentication::v1::{BoundObjectReference, TokenRequest, TokenRequestSpec};
use k8s_openapi::api::core::v1::{ConfigMap, ServiceAccount};
use k8s_openapi::jiff::SignedDuration;
use kube::Api;
use kube::api::{ListParams, PostParams};
use serde_json::json;
use wamn_identity_client::nats::{SIGN_HOST_USER_PATH, SignHostUser, SignedUser};
use wamn_test_infrastructure::scratch::ScratchRoot;

/// A second application with the fixture package, for the stop and status path.
const SECOND: &str = "second_fixture";
/// How long one host pod waits to become the only Ready one after `wamn env apply`.
const HOST_DEADLINE: Duration = Duration::from_secs(300);
/// How long the operator takes to see a Workload change on the Host object.
const RUNTIME_DEADLINE: Duration = Duration::from_secs(180);
/// The grant cache lifetime that the grant cache test applies.
const REVOCATION_SECONDS: u64 = 5;
/// The time a call takes, past the cache lifetime, to reach the host and be refused.
const REVOCATION_MARGIN: Duration = Duration::from_secs(10);

/// The host pod becomes Ready through the startup and readiness probes that its pod spec
/// declares, within the time those probes allow, and then serves a call through web.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: cluster, bin:helm"]
async fn the_host_pod_becomes_ready_through_its_probes() -> anyhow::Result<()> {
    let application = Application::prepare(&fixture()).await?;
    let pods = common::host::host_pods(&application.environment).await?;
    let pod = pods.first().context("the environment has no host pod")?;
    let container = pod
        .spec
        .iter()
        .flat_map(|spec| &spec.containers)
        .find(|container| container.startup_probe.is_some())
        .context("no host container declares a startup probe")?;
    let budget = |probe: Option<&k8s_openapi::api::core::v1::Probe>| {
        let probe = probe?;
        let period = i64::from(probe.period_seconds.unwrap_or(10));
        let failures = i64::from(probe.failure_threshold.unwrap_or(3));
        Some(i64::from(probe.initial_delay_seconds.unwrap_or(0)) + period * failures)
    };
    let startup = budget(container.startup_probe.as_ref()).context("startup probe")?;
    let readiness = budget(container.readiness_probe.as_ref())
        .context("the host container declares no readiness probe")?;
    let allowed = startup + readiness;

    let pod = ready_host_pod(
        &application.environment,
        Duration::from_secs(allowed.unsigned_abs()),
    )
    .await?;
    let status = pod.status.as_ref().context("host pod status")?;
    let started = status.start_time.as_ref().context("host pod start time")?;
    let became_ready = status
        .conditions
        .iter()
        .flatten()
        .find(|condition| condition.type_ == "Ready")
        .and_then(|condition| condition.last_transition_time.as_ref())
        .context("the Ready condition has no transition time")?;
    let took = became_ready.0.duration_since(started.0);
    ensure!(
        took <= SignedDuration::from_secs(allowed),
        "the host pod took {took} to become Ready; its probes allow {allowed}s"
    );
    ensure!(
        status
            .container_statuses
            .iter()
            .flatten()
            .any(|state| state.name == container.name
                && state.started == Some(true)
                && state.ready),
        "the host container is not started and ready: {status:?}"
    );

    let id = uuid::Uuid::now_v7().to_string();
    application
        .routes
        .create("probe-call", "probe-call", &id, "standard")
        .await?;
    Ok(())
}

/// Identity signs a NATS user for a token bound to the host pod. The NATS server refuses
/// that user's publish outside its subjects, and identity refuses another account's token.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: cluster, bin:helm"]
async fn the_host_nats_user_is_refused_outside_its_subjects() -> anyhow::Result<()> {
    let environment = Org::create().await?.apply(FIXTURE).await?;
    let client = &environment.org.cluster.client;
    let triple = &environment.triple;
    let namespace = triple.org.as_str();
    let pod = ready_host_pod(&environment, HOST_DEADLINE).await?;
    let accounts = Api::<ServiceAccount>::namespaced(client.clone(), namespace);
    let host_account = wamn_registry::name::environment_service_account(&triple.org, &triple.env);
    let bound = token(
        &accounts,
        &host_account,
        Some(BoundObjectReference {
            api_version: Some("v1".into()),
            kind: Some("Pod".into()),
            name: pod.metadata.name.clone(),
            uid: pod.metadata.uid.clone(),
        }),
    )
    .await?;

    let key = nkeys::KeyPair::new_user();
    let request = SignHostUser {
        public_key: key.public_key(),
    };
    let (status, body) = identity_post(client, SIGN_HOST_USER_PATH, &bound, &request).await?;
    ensure!(
        status == hyper::StatusCode::OK,
        "identity answered {status} for the host pod token: {body}"
    );
    let signed: SignedUser = serde_json::from_str(&body)?;

    let other = token(&accounts, "default", None).await?;
    let (status, body) = identity_post(client, SIGN_HOST_USER_PATH, &other, &request).await?;
    ensure!(
        status == hyper::StatusCode::FORBIDDEN,
        "identity answered {status} for another account's token: {body}"
    );

    let ca = Api::<ConfigMap>::namespaced(client.clone(), namespace)
        .get("nats-ca")
        .await?
        .data
        .and_then(|mut data| data.remove("ca.crt"))
        .context("the NATS CA has no ca.crt")?;
    let scratch = ScratchRoot::create()?;
    let ca_file = scratch.path().join("nats-ca.crt");
    std::fs::write(&ca_file, ca)?;
    let forward = Forward::start(client, "platform", "wamn-nats-0", 4222).await?;
    let refusal = Arc::new(Mutex::new(None::<String>));
    let seen = Arc::clone(&refusal);
    let nats = async_nats::ConnectOptions::new()
        .jwt(signed.jwt, move |nonce| {
            std::future::ready(key.sign(&nonce).map_err(async_nats::AuthError::new))
        })
        .custom_inbox_prefix(signed.inbox_prefix)
        .ignore_discovered_servers()
        .require_tls(true)
        .add_root_certificates(ca_file)
        .event_callback(move |event| {
            let seen = Arc::clone(&seen);
            async move {
                if let async_nats::Event::ServerError(async_nats::ServerError::Other(text)) = event
                {
                    *seen.lock().unwrap_or_else(PoisonError::into_inner) = Some(text);
                }
            }
        })
        // The server certificate names 127.0.0.1, the port-forward's address.
        .connect(forward.address().to_string())
        .await
        .context("connect to NATS as the host user")?;
    let outside = "wamn.journey.outside";
    nats.publish(outside, "refused".into()).await?;
    nats.flush().await?;
    let text = until(
        Duration::from_secs(30),
        "a NATS permission error",
        || async {
            Ok(refusal
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone())
        },
    )
    .await?;
    ensure!(
        text.to_lowercase()
            .contains("permissions violation for publish")
            && text.contains(outside),
        "the server refused another error: {text}"
    );
    Ok(())
}

/// A pod-bound or unbound token for `account` with the identity audience.
async fn token(
    accounts: &Api<ServiceAccount>,
    account: &str,
    bound_object_ref: Option<BoundObjectReference>,
) -> anyhow::Result<String> {
    let request = TokenRequest {
        spec: TokenRequestSpec {
            audiences: vec![wamn_identity_client::TOKEN_AUDIENCE.into()],
            bound_object_ref,
            expiration_seconds: Some(600),
        },
        ..TokenRequest::default()
    };
    Ok(accounts
        .create_token_request(account, &PostParams::default(), &request)
        .await?
        .status
        .with_context(|| format!("the TokenRequest for {account} has no status"))?
        .token)
}

/// An application removed from the environment file leaves the host at the next
/// `wamn env apply`: its Workload objects go, and the Host object counts fewer workloads.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: cluster, bin:helm"]
async fn a_removed_application_leaves_its_host() -> anyhow::Result<()> {
    let environment = Org::create().await?.apply(FIXTURE).await?;
    reapply(&environment, |file| declare(file, SECOND, "second")).await?;
    environment.deploy(&fixture(), FIXTURE).await?;
    environment.deploy(&fixture(), SECOND).await?;
    let client = &environment.org.cluster.client;
    let pod = ready_host_pod(&environment, HOST_DEADLINE).await?;

    let removed: Vec<String> = application_objects(&environment, "Workload", "workloads", SECOND)
        .await?
        .into_iter()
        .filter_map(|workload| workload.metadata.name)
        .collect();
    ensure!(!removed.is_empty(), "{SECOND} has no Workload objects");
    let workloads = common::host::runtime(
        client,
        environment.triple.org.as_str(),
        "Workload",
        "workloads",
    );
    let placed = workloads.list(&ListParams::default()).await?.items.len();
    let before = until(
        RUNTIME_DEADLINE,
        "the Host object counts every workload",
        || async {
            let count = host_object(client, &pod)
                .await?
                .as_ref()
                .map(workload_count);
            Ok(count.filter(|count| usize::try_from(*count).is_ok_and(|count| count == placed)))
        },
    )
    .await?;

    reapply(&environment, |file| {
        file["projects"][0]["applications"]
            .as_object_mut()
            .map(|applications| applications.remove(SECOND));
    })
    .await?;
    until(
        RUNTIME_DEADLINE,
        "the removed Workload objects go",
        || async {
            let left = workloads.list(&ListParams::default()).await?.items;
            Ok((!left.iter().any(|workload| {
                workload
                    .metadata
                    .name
                    .as_ref()
                    .is_some_and(|name| removed.contains(name))
            }))
            .then_some(()))
        },
    )
    .await?;
    until(
        RUNTIME_DEADLINE,
        "the Host object's workloadCount drops",
        || async {
            let count = host_object(client, &pod)
                .await?
                .as_ref()
                .map(workload_count);
            Ok(count.filter(|count| *count < before))
        },
    )
    .await?;
    Ok(())
}

/// A project admin grant reaches the host at once, because the host caches only positive
/// grants. A revoke reaches it within `revocation_seconds`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: cluster, bin:helm"]
async fn a_revoked_grant_is_refused_within_the_revocation_lifetime() -> anyhow::Result<()> {
    let environment = Org::create().await?.apply(FIXTURE).await?;
    reapply(&environment, |file| {
        file["revocation_seconds"] = REVOCATION_SECONDS.into();
    })
    .await?;
    environment.deploy(&fixture(), FIXTURE).await?;
    let org = &environment.org;
    let (principal, token) = member(org, &format!("member-{}@example.com", org.name)).await?;
    let call = || application_get(&environment, FIXTURE, "role.list", &token);
    let grant = || json!({"project": environment.triple.project, "principal_id": principal});

    // A member with no grant is no principal of the application yet, so either refusal holds.
    let refused = |status| {
        matches!(
            status,
            hyper::StatusCode::UNAUTHORIZED | hyper::StatusCode::FORBIDDEN
        )
    };
    let status = call().await?;
    ensure!(
        refused(status),
        "the call before the grant answered {status}"
    );
    control(org, "project_admin.grant", grant()).await?;
    let status = call().await?;
    ensure!(
        status == hyper::StatusCode::OK,
        "the first call after the grant answered {status}"
    );
    control(org, "project_admin.revoke", grant()).await?;
    until(
        Duration::from_secs(REVOCATION_SECONDS) + REVOCATION_MARGIN,
        "the refusal after the revoke",
        || async { Ok(refused(call().await?).then_some(())) },
    )
    .await
}
