//! Check the public edge, or the cluster Service on kind, after installation.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use wamn_package_artifact::PackageRegistry;

use super::document::{ClusterDocument, Provider};
use super::job::{self, JobRequest, SecretMount};
use crate::{kubectl, release_chart::Target};

const MARKER: &str = "wamn-install-platform-https-check";
// Reuse the pinned curl image previously used by the edge certificate Job.
const CURL: &str =
    "curlimages/curl@sha256:58adaa4e8dca9c988bae2aba4ab3434a0bb2da16bbe3f92dec39ec7785166777";

pub(super) async fn apply(
    target: &Target,
    document: &ClusterDocument,
    environments: &Path,
    directory: &Path,
    registry: &PackageRegistry,
    mut progress: impl FnMut(&str) -> anyhow::Result<()>,
) -> anyhow::Result<bool> {
    let environments = super::broker::documents(environments)?;
    let mut routes = vec![(
        document.route_hosts.control.clone(),
        format!("c-{}", document.org),
    )];
    for (_, environment) in &environments {
        routes.push((
            environment.route_host.clone(),
            crate::release_chart::release_name(
                &environment.org,
                &environment.project,
                &environment.env,
            )?,
        ));
    }
    // Read only the public certificate, never the TLS private key.
    let certificate = kubectl::kubectl(
        &target.kubeconfig,
        &target.context,
        "edge",
        &[
            "get",
            "secret",
            "wamn-edge-tls",
            "-o",
            "jsonpath={.data.tls\\.crt}",
        ],
        None,
    )
    .await?;
    let pem = STANDARD
        .decode(certificate.trim())
        .context("decode edge/wamn-edge-tls certificate")?;
    let expected_serial = pem_serial(std::str::from_utf8(&pem)?)?;
    let mut releases = Vec::new();
    for (_, name) in &routes {
        let release: Value = kubectl::get(
            &target.kubeconfig,
            &target.context,
            "hosts",
            "configmap",
            name,
        )
        .await?
        .with_context(|| format!("missing release ConfigMap hosts/{name}"))?;
        releases.push(release["data"].clone());
    }
    let proxy: Value = kubectl::get(
        &target.kubeconfig,
        &target.context,
        "edge",
        "deployment",
        "wamn-edge",
    )
    .await?
    .context("missing edge/wamn-edge Deployment")?;
    let digest = job::input_digest(&json!({
        "document": document, "releases": releases, "certificate": certificate,
        "proxy": proxy["spec"], "proxy_uid": proxy["metadata"]["uid"],
        "check": 1,
    }));
    let completed = kubectl::get(
        &target.kubeconfig,
        &target.context,
        "platform",
        "configmap",
        MARKER,
    )
    .await?;
    if document.provider == Provider::Kind {
        progress("4.21 Public load balancer: skipped: kind")?;
    }
    if document.buckets.web.is_empty() {
        progress("4.21 Web / and /config.json: skipped: no bucket")?;
    }
    if job::skip(completed.as_ref(), &digest) {
        return Ok(false);
    }
    let source = wamn_runtime::release_manifest_source::ReleaseManifestSource::new(
        &super::packages::artifact_base(registry, "releases")?,
        registry.insecure_registry,
        &registry.registry_auth_file,
    )?
    .with_ca_paths(&registry.oci_ca_paths)?;
    let mut apis = vec![(
        "GET".to_owned(),
        "/api/wamn_control/control/mine".to_owned(),
    )];
    for (_, environment) in &environments {
        let bytes = source
            .pull_verified(&environment.release.to_string())
            .await?;
        let (manifest, _) = wamn_catalog::ServingManifest::from_canonical_bytes(&bytes)?;
        apis.push(
            api_route(&manifest)
                .with_context(|| format!("4.21 API check for {}", environment.route_host))?,
        );
    }
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .tls_info(true)
        .build()?;
    let mut results = Vec::new();
    for ((host, _), (method, api)) in routes.iter().zip(apis) {
        let mut checks = vec![(method.as_str(), api.as_str(), 401)];
        if !document.buckets.web.is_empty() {
            checks.extend([("GET", "/", 200), ("GET", "/config.json", 200)]);
        }
        for (method, path, expected) in checks {
            let hint = if document.provider == Provider::Gcp {
                "; likely missing step: tofu apply deploy/gcp"
            } else {
                ""
            };
            let context = format!("4.21 {method} {path} on {host}{hint}");
            let (status, serial) = if document.provider == Provider::Kind {
                kind_check(target, directory, host, method, path)
                    .await
                    .with_context(|| context.clone())?
            } else {
                let response = http
                    .request(method.parse()?, format!("https://{host}{path}"))
                    .send()
                    .await
                    .with_context(|| context.clone())?;
                let der = response
                    .extensions()
                    .get::<reqwest::tls::TlsInfo>()
                    .and_then(reqwest::tls::TlsInfo::peer_certificate)
                    .with_context(|| format!("4.21 certificate on {host}: no peer certificate"))?;
                (
                    response.status().as_u16(),
                    serial(der).with_context(|| context.clone())?,
                )
            };
            ensure!(
                status == expected,
                "{context}: answered {status}, expected {expected}"
            );
            // On GCP the load balancer presents its Google-managed
            // certificate, which the client already validated for the host
            // against public roots. Only kind serves edge/wamn-edge-tls.
            ensure!(
                document.provider != Provider::Kind || serial == expected_serial,
                "4.21 certificate on {host}: served serial {serial} differs from edge/wamn-edge-tls {expected_serial}"
            );
            results.push(json!({"host": host, "method": method, "path": path, "status": status, "serial": serial}));
        }
    }
    super::install::apply_object(target, &json!({
        "apiVersion": "v1", "kind": "ConfigMap", "metadata": {"name": MARKER, "namespace": "platform"},
        "data": {"digest": digest, "results": serde_json::to_string(&results)?,
            "load_balancer": if document.provider == Provider::Kind {"skipped: kind"} else {"checked"},
            "web": if document.buckets.web.is_empty() {"skipped: no bucket"} else {"checked"}},
    })).await?;
    Ok(true)
}

fn api_route(manifest: &wamn_catalog::ServingManifest) -> anyhow::Result<(String, String)> {
    for attachment in manifest.attachments.values() {
        let Some(path) = attachment.definition["route"]["path"].as_str() else {
            continue;
        };
        let modes = attachment.auth_policy["modes"].as_array();
        if modes.is_none_or(|modes| modes.is_empty() || modes.iter().any(|mode| mode == "none")) {
            continue;
        }
        // Parameter-free routes can be checked without inventing domain inputs.
        if path.contains('{') || path.contains(':') {
            continue;
        }
        let method = attachment.definition["route"]["method"]
            .as_str()
            .or_else(|| {
                manifest
                    .routes
                    .iter()
                    .find(|route| route.operation == attachment.operation)
                    .map(|route| route.type_.http_method())
            })
            .context("HTTP attachment has no method")?;
        return Ok((method.to_owned(), format!("/api{path}")));
    }
    anyhow::bail!("release has no protected HTTP route without path parameters")
}

async fn kind_check(
    target: &Target,
    directory: &Path,
    host: &str,
    method: &str,
    path: &str,
) -> anyhow::Result<(u16, String)> {
    let key = job::input_digest(&json!([host, method, path]));
    let request = JobRequest {
        name: format!("platform-https-{}", &key[..16]),
        image: CURL.into(),
        command: vec!["curl".into()],
        capture_output: true,
        refresh: true,
        arguments: vec![
            "--silent".into(),
            "--show-error".into(),
            "--max-time".into(),
            "30".into(),
            "--noproxy".into(),
            "*".into(),
            "--cacert".into(),
            "/etc/edge/tls.crt".into(),
            "--connect-to".into(),
            format!("{host}:443:wamn-edge.edge.svc.cluster.local:443"),
            "--request".into(),
            method.into(),
            "--output".into(),
            "/dev/null".into(),
            "--write-out".into(),
            "%{json}".into(),
            format!("https://{host}{path}"),
        ],
        secret_mounts: vec![SecretMount {
            name: "wamn-edge-tls".into(),
            path: "/etc/edge".into(),
            keys: vec!["tls.crt".into()],
        }],
        run_as_user: Some(0),
        ..JobRequest::default()
    };
    let edge = Target {
        namespace: "edge".into(),
        ..target.clone()
    };
    job::run(&edge, directory, &request, &json!({}), &[]).await?;
    let output: Value = serde_json::from_str(&job::output(&edge, &request).await?)?;
    let status = output["http_code"]
        .as_u64()
        .and_then(|status| u16::try_from(status).ok())
        .context("curl result has no HTTP status")?;
    let certificate = output["certs"]
        .as_str()
        .context("curl result has no certificate")?;
    Ok((status, pem_serial(certificate)?))
}

fn pem_serial(pem: &str) -> anyhow::Result<String> {
    let pem = pem
        .split_once("-----BEGIN CERTIFICATE-----")
        .context("missing PEM certificate")?
        .1
        .split_once("-----END CERTIFICATE-----")
        .context("unfinished PEM certificate")?
        .0;
    let encoded: String = pem
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    serial(&STANDARD.decode(encoded).context("decode PEM certificate")?)
}

// An X.509 serial is the INTEGER after the optional version in TBSCertificate.
// Read only those DER envelopes. TLS libraries validate the full served certificate.
fn serial(der: &[u8]) -> anyhow::Result<String> {
    fn field<'a>(bytes: &mut &'a [u8], tag: u8) -> anyhow::Result<&'a [u8]> {
        ensure!(
            bytes.first() == Some(&tag),
            "unexpected certificate DER tag"
        );
        let first = *bytes.get(1).context("truncated certificate DER length")?;
        let (length, header) = if first < 128 {
            (usize::from(first), 2)
        } else {
            let count = usize::from(first & 127);
            ensure!(
                count > 0 && count <= std::mem::size_of::<usize>(),
                "invalid certificate DER length"
            );
            let length = bytes
                .get(2..2 + count)
                .context("truncated certificate DER length")?
                .iter()
                .try_fold(0usize, |length, byte| {
                    length.checked_mul(256)?.checked_add(usize::from(*byte))
                })
                .context("certificate DER length overflow")?;
            (length, 2 + count)
        };
        let end = header
            .checked_add(length)
            .context("certificate DER length overflow")?;
        let value = bytes
            .get(header..end)
            .context("truncated certificate DER value")?;
        *bytes = &bytes[end..];
        Ok(value)
    }
    let mut certificate = field(&mut &*der, 0x30)?;
    let mut body = field(&mut certificate, 0x30)?;
    if body.first() == Some(&0xa0) {
        field(&mut body, 0xa0)?;
    }
    let number = field(&mut body, 0x02)?;
    ensure!(!number.is_empty(), "empty certificate serial");
    let number = number.strip_prefix(&[0]).unwrap_or(number);
    Ok(hex::encode(number))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn certificate_serial_matches_pem_and_der_and_refuses_truncation() {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec!["test.example.com".into()]).unwrap();
        params.serial_number = Some(vec![0x80, 1, 2].into());
        let certificate = params.self_signed(&key).unwrap();
        assert_eq!(serial(certificate.der()).unwrap(), "800102");
        assert_eq!(pem_serial(&certificate.pem()).unwrap(), "800102");
        for length in 0..certificate.der().len() {
            assert!(serial(&certificate.der()[..length]).is_err());
        }
        assert!(pem_serial("no certificate").is_err());
    }
}
