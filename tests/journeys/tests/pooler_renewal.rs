//! C11 checks the TLS certificate served by the control Pooler. Run this target alone.

mod common;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use common::{Org, PortForward, host, request::until, secret_key};
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{Pod, Service};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
use k8s_openapi::jiff::Timestamp;
use kube::Api;
use kube::api::{
    ApiResource, DeleteParams, DynamicObject, GroupVersionKind, ListParams, Patch, PatchParams,
    Preconditions,
};
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, ServerName};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio::time::{sleep, timeout};
use tokio_rustls::TlsConnector;

const PLATFORM: &str = "platform";
const POOLER: &str = "wamn-pg-control";
const TLS_SECRET: &str = "wamn-pg-control-tls";
const OPERATOR: &str = "cnpg-controller-manager";
const POSTGRES_PORT: u16 = 5432;
// PostgreSQL SSLRequest length (8) and code (80877103) in network byte order.
const SSL_REQUEST: [u8; 8] = [0, 0, 0, 8, 4, 210, 22, 47];
const SETUP: Duration = Duration::from_mins(20);
const API: Duration = Duration::from_secs(30);
const HANDSHAKE: Duration = Duration::from_secs(10);
const RECONCILE: Duration = Duration::from_secs(180);

type Fingerprint = [u8; 32];

common::journey! {
    async fn the_control_pooler_serves_its_renewed_certificate() -> anyhow::Result<()> {
        let env = timeout(SETUP, async {
            Org::create().await?.apply(host::FIXTURE).await
        })
        .await
        .context("create the renewal test environment")??;
        let client = &env.org.cluster.client;
        let pods = Api::namespaced(client.clone(), PLATFORM);
        let (selector, tls, old_secret) = timeout(API, async {
            let service = Api::<Service>::namespaced(client.clone(), PLATFORM)
                .get(POOLER)
                .await
                .context("read the control Pooler Service")?;
            let labels = service
                .spec
                .and_then(|spec| spec.selector)
                .filter(|labels| !labels.is_empty())
                .context("the control Pooler Service has no pod selector")?;
            let selector = ListParams::default().labels(
                &labels
                    .into_iter()
                    .map(|(key, value)| format!("{key}={value}"))
                    .collect::<Vec<_>>()
                    .join(","),
            );
            let ca = secret_key(client, PLATFORM, "wamn-pg-server-tls", "ca.crt").await?;
            anyhow::Ok((selector, connector(&ca)?, secret_fingerprint(client).await?))
        })
        .await
        .context("read the Pooler Service and certificate inputs")??;

        let old_served = timeout(HANDSHAKE, served_fingerprint(&pods, &selector, &tls))
            .await
            .context("read the baseline Pooler certificate")??;
        ensure!(
            old_served == old_secret,
            "Pooler serves {old_served:02x?}, but {TLS_SECRET} contains {old_secret:02x?}"
        );

        timeout(API, request_renewal(client))
            .await
            .context("request Pooler certificate renewal")??;
        let renewed = timeout(
            RECONCILE,
            until(RECONCILE, "the Pooler Secret changes", || async {
                let fingerprint = secret_fingerprint(client).await?;
                Ok((fingerprint != old_secret).then_some(fingerprint))
            }),
        )
        .await
        .with_context(|| format!("{TLS_SECRET} still held {old_secret:02x?} after {RECONCILE:?}"))??;

        timeout(RECONCILE, restart_operator(client))
            .await
            .with_context(|| {
                format!("a replacement CloudNativePG operator is Ready within {RECONCILE:?}")
            })??;

        let mut last_served = old_served;
        let mut last_error = None;
        timeout(RECONCILE, async {
            loop {
                match timeout(HANDSHAKE, served_fingerprint(&pods, &selector, &tls)).await {
                    Ok(Ok(fingerprint)) if fingerprint == renewed => return,
                    Ok(Ok(fingerprint)) => {
                        last_served = fingerprint;
                        last_error = None;
                    }
                    Ok(Err(error)) => last_error = Some(format!("{error:#}")),
                    Err(error) => last_error = Some(error.to_string()),
                }
                sleep(Duration::from_secs(1)).await;
            }
        })
        .await
        .with_context(|| format!(
            "within {RECONCILE:?}, wanted {renewed:02x?}, last served {last_served:02x?}, last probe error {last_error:?}"
        ))?;

        // The renewed certificate is valid. Each probe closes its own port-forward.
        Ok(())
    }
}

fn connector(ca: &[u8]) -> anyhow::Result<TlsConnector> {
    let mut roots = rustls::RootCertStore::empty();
    for der in CertificateDer::pem_slice_iter(ca) {
        roots
            .add(der.context("decode Pooler CA PEM")?)
            .context("trust Pooler CA")?;
    }
    ensure!(
        !roots.is_empty(),
        "wamn-pg-server-tls has no CA certificate"
    );
    let mut config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_root_certificates(roots)
    .with_no_client_auth();
    config.resumption = rustls::client::Resumption::disabled();
    Ok(TlsConnector::from(Arc::new(config)))
}

async fn secret_fingerprint(client: &kube::Client) -> anyhow::Result<Fingerprint> {
    let pem = secret_key(client, PLATFORM, TLS_SECRET, "tls.crt").await?;
    let leaf = CertificateDer::from_pem_slice(&pem).context("decode the Pooler Secret PEM")?;
    Ok(Sha256::digest(leaf.as_ref()).into())
}

async fn served_fingerprint(
    pods: &Api<Pod>,
    selector: &ListParams,
    tls: &TlsConnector,
) -> anyhow::Result<Fingerprint> {
    let pod = ready_pod(pods, selector, &[])
        .await?
        .context("the control Pooler Service has no Ready pod")?;
    let name = pod.metadata.name.as_deref().context("Pooler pod name")?;
    let forward = PortForward::start(pods.clone(), name, POSTGRES_PORT).await?;
    let mut stream = TcpStream::connect(forward.address)
        .await
        .context("connect to the Pooler port-forward")?;
    stream
        .write_all(&SSL_REQUEST)
        .await
        .context("send SSLRequest")?;
    let response = stream.read_u8().await.context("read SSLRequest reply")?;
    ensure!(
        response == b'S',
        "Pooler SSLRequest reply was {response:#04x}"
    );
    let dns = ServerName::try_from(format!("{POOLER}.{PLATFORM}.svc.cluster.local"))
        .context("the control Pooler Service DNS name")?;
    let stream = tls
        .connect(dns, stream)
        .await
        .context("verify Pooler TLS")?;
    let leaf = stream
        .get_ref()
        .1
        .peer_certificates()
        .and_then(|chain| chain.first())
        .context("the Pooler served no TLS certificate")?;
    Ok(Sha256::digest(leaf.as_ref()).into())
}

async fn request_renewal(client: &kube::Client) -> anyhow::Result<()> {
    let resource = ApiResource::from_gvk_with_plural(
        &GroupVersionKind::gvk("cert-manager.io", "v1", "Certificate"),
        "certificates",
    );
    let certificates = Api::<DynamicObject>::namespaced_with(client.clone(), PLATFORM, &resource);
    let mut certificate = certificates
        .get(TLS_SECRET)
        .await
        .context("read Pooler Certificate")?;
    ensure!(
        certificate.data["spec"]["secretName"] == TLS_SECRET,
        "Pooler Certificate points to a different Secret"
    );
    let generation = certificate
        .metadata
        .generation
        .context("Pooler Certificate generation")?;
    let version = certificate
        .metadata
        .resource_version
        .context("Pooler Certificate revision")?;
    let mut conditions: Vec<Value> =
        serde_json::from_value(certificate.data["status"]["conditions"].take())
            .context("read Pooler Certificate conditions")?;
    ensure!(
        conditions
            .iter()
            .any(|condition| condition["type"] == "Ready" && condition["status"] == "True"),
        "Pooler Certificate must be Ready before renewal"
    );
    ensure!(
        conditions
            .iter()
            .all(|condition| condition["type"] != "Issuing" || condition["status"] != "True"),
        "Pooler Certificate is already renewing"
    );
    // Match cmctl renew and preserve the other status conditions.
    conditions.retain(|condition| condition["type"] != "Issuing");
    conditions.push(json!({
        "type": "Issuing",
        "status": "True",
        "reason": "ManuallyTriggered",
        "message": "Certificate re-issuance manually triggered",
        "observedGeneration": generation,
        "lastTransitionTime": Time(Timestamp::now()),
    }));
    certificates
        .patch_status(
            TLS_SECRET,
            &PatchParams::default(),
            &Patch::Merge(json!({
                "metadata": {"resourceVersion": version},
                "status": {"conditions": conditions},
            })),
        )
        .await
        .context("set Issuing=True on the Pooler Certificate")?;
    Ok(())
}

async fn restart_operator(client: &kube::Client) -> anyhow::Result<()> {
    // The infrastructure release puts the CloudNativePG Deployment in platform.
    let deployment = Api::<Deployment>::namespaced(client.clone(), PLATFORM)
        .get(OPERATOR)
        .await
        .context("read the CloudNativePG Deployment")?;
    let labels = deployment
        .spec
        .and_then(|spec| spec.selector.match_labels)
        .filter(|labels| !labels.is_empty())
        .context("the CloudNativePG Deployment has no pod selector")?;
    let selector = ListParams::default().labels(
        &labels
            .into_iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join(","),
    );
    let pods = Api::namespaced(client.clone(), PLATFORM);
    let before = pods
        .list(&selector)
        .await
        .context("list CloudNativePG pods")?
        .items;
    let old = before
        .iter()
        .find(|pod| active_and_ready(pod))
        .context("the CloudNativePG Deployment has no Ready pod")?;
    let name = old
        .metadata
        .name
        .as_deref()
        .context("CloudNativePG pod name")?;
    let uid = old
        .metadata
        .uid
        .as_deref()
        .context("CloudNativePG pod UID")?;
    pods.delete(
        name,
        &DeleteParams {
            preconditions: Some(Preconditions {
                uid: Some(uid.to_owned()),
                resource_version: None,
            }),
            ..DeleteParams::default()
        },
    )
    .await
    .with_context(|| format!("delete CloudNativePG pod {name}"))?;
    until(RECONCILE, "a different CloudNativePG pod is Ready", || {
        ready_pod(&pods, &selector, &before)
    })
    .await?;
    Ok(())
}

async fn ready_pod(
    pods: &Api<Pod>,
    selector: &ListParams,
    previous: &[Pod],
) -> anyhow::Result<Option<Pod>> {
    Ok(pods
        .list(selector)
        .await
        .context("list pod candidates")?
        .items
        .into_iter()
        .find(|pod| {
            active_and_ready(pod)
                && pod.metadata.uid.is_some()
                && previous
                    .iter()
                    .all(|old| old.metadata.uid != pod.metadata.uid)
        }))
}

fn active_and_ready(pod: &Pod) -> bool {
    pod.metadata.deletion_timestamp.is_none() && host::ready(pod)
}
