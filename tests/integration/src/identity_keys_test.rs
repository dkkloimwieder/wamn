//! Read-only public-key evidence from the actual deployed identity HTTPS service.
//!
//! This foundation test does not authenticate a session token or exercise a
//! host admission boundary. Expiry and outage timing remain covered by the
//! deterministic cache suite; this short deployed observer claims neither.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{anyhow, ensure};
use clap::Args;
use wamn_platform_identity::session_keys::{PublicSessionKey, SessionJwks, decode_public_key};
use wamn_runtime::session_keys::{IssuerKeys, IssuerKeysConfig};

const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
const TEST_TIMEOUT: Duration = Duration::from_secs(90);
const MAX_BODY_BYTES: usize = 65_536;
const UNKNOWN_KID: &str = "identity-jwks-proof-unknown-key";

/// Public inputs only: exact issuer, endpoint, certificate roots, and expected IDs.
#[derive(Debug, Args)]
pub struct IdentityKeysTestArgs {
    #[arg(long, env = "WAMN_IDENTITY_ISSUER")]
    pub issuer: String,
    #[arg(long, env = "WAMN_IDENTITY_JWKS_URL")]
    pub jwks_url: String,
    #[arg(long, env = "WAMN_IDENTITY_CA_FILE")]
    pub ca_file: PathBuf,
    #[arg(long, env = "WAMN_IDENTITY_WRONG_CA_FILE")]
    pub wrong_ca_file: PathBuf,
    /// Exact JSON array of published IDs; [] explicitly shows the empty stage.
    #[arg(long, env = "WAMN_IDENTITY_EXPECTED_KIDS")]
    pub expected_kids: String,
}

/// Require every applicable deployed foundation case before emitting a verdict.
pub async fn run(args: IdentityKeysTestArgs) -> anyhow::Result<()> {
    tokio::time::timeout(TEST_TIMEOUT, exercise(args))
        .await
        .map_err(|_| anyhow!("IDENTITY-JWKS proof exceeded ninety seconds"))?
}

async fn exercise(args: IdentityKeysTestArgs) -> anyhow::Result<()> {
    ensure!(
        args.expected_kids.len() <= MAX_BODY_BYTES,
        "expected key input is oversized"
    );
    let expected: Vec<String> = serde_json::from_str(&args.expected_kids)
        .map_err(|_| anyhow!("expected kids must be an explicit JSON string array"))?;
    let expected_set = expected.iter().cloned().collect::<BTreeSet<_>>();
    ensure!(
        expected_set.len() == expected.len() && expected.iter().all(|kid| !kid.is_empty()),
        "expected IDs must be unique and nonempty"
    );
    ensure!(
        !expected_set.contains(UNKNOWN_KID),
        "unknown-key control must be absent from expected keys"
    );
    let ca = tokio::fs::read(&args.ca_file)
        .await
        .map_err(|_| anyhow!("read public issuer CA"))?;
    let wrong_ca = tokio::fs::read(&args.wrong_ca_file)
        .await
        .map_err(|_| anyhow!("read independent wrong-CA control"))?;
    ensure!(
        ca != wrong_ca,
        "wrong-CA control must differ from the trusted CA"
    );
    let config = IssuerKeysConfig::new(&args.issuer, &args.jwks_url, &ca)
        .map_err(|_| anyhow!("configured HTTPS issuer refused"))?;
    let mut plaintext =
        reqwest::Url::parse(&args.jwks_url).map_err(|_| anyhow!("configured JWKS URL refused"))?;
    let tls_port = plaintext
        .port_or_known_default()
        .ok_or_else(|| anyhow!("configured JWKS listener has no port"))?;
    plaintext
        .set_scheme("http")
        .map_err(|_| anyhow!("construct plaintext control"))?;
    plaintext
        .set_port(Some(tls_port))
        .map_err(|_| anyhow!("preserve TLS listener port for plaintext control"))?;
    ensure!(
        IssuerKeysConfig::new(&args.issuer, plaintext.as_str(), &ca).is_err(),
        "public cache admitted plaintext configuration"
    );
    pass("configuration_https");

    let http = client(&ca, true)?;
    let actual = public_set(&http, &args.jwks_url).await?;
    ensure!(
        actual.keys().cloned().collect::<BTreeSet<_>>() == expected_set,
        "deployed JWKS differs from the exact expected published set"
    );
    pass("public_set");

    let caches = [IssuerKeys::new(config.clone()), IssuerKeys::new(config)]
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| anyhow!("construct independent public caches"))?;
    ensure!(
        caches.iter().all(|cache| cache.issuer() == args.issuer),
        "cache lost its exact configured issuer attribution"
    );
    pass("issuer_binding");

    // These are independent cache instances, not two deployed token-verifying
    // hosts. Retained evidence is never relabelled as session admission test.
    let mut held = Vec::new();
    for cache in &caches {
        for kid in &expected {
            let evidence = cache
                .key(kid)
                .await
                .map_err(|_| anyhow!("expected public key was refused by production cache"))?;
            // Internal dispatch is later than the pre-call observation, so
            // only post-return time supplies this observer's upper bound.
            // Deterministic cache tests show exact request-start aging.
            let observed = Instant::now();
            ensure!(
                evidence.issuer() == args.issuer,
                "key evidence lost exact issuer attribution"
            );
            ensure!(
                Some(evidence.public_key()) == actual.get(kid),
                "cache returned a different public key"
            );
            ensure!(
                evidence.is_fresh() && evidence.deadline() <= observed + Duration::from_secs(300),
                "key evidence exceeded its bounded freshness window"
            );
            let deadline = evidence.deadline();
            let hit = cache
                .key(kid)
                .await
                .map_err(|_| anyhow!("fresh known-key hit failed"))?;
            ensure!(
                hit.deadline() == deadline,
                "a known-key hit extended evidence freshness"
            );
            held.push((evidence, deadline));
        }
    }
    if !expected.is_empty() {
        pass("two_cache_evidence");
    }
    for cache in &caches {
        ensure!(
            cache.key(UNKNOWN_KID).await.is_err(),
            "unknown key ID was admitted"
        );
    }
    // An unknown ID may cause a successful authoritative whole-set refresh.
    // That is new evidence, not permission to re-age an already held result.
    ensure!(
        held.iter()
            .all(|(evidence, deadline)| evidence.deadline() == *deadline),
        "unknown-key traffic re-aged retained evidence"
    );
    pass("unknown_kid");

    ensure!(
        client(&wrong_ca, true)?
            .get(&args.jwks_url)
            .send()
            .await
            .is_err(),
        "deployed endpoint was accepted with an unrelated CA"
    );
    pass("wrong_ca");
    ensure!(
        client(&ca, false)?.get(plaintext).send().await.is_err(),
        "plaintext reached the deployed TLS listener"
    );
    pass("plaintext");

    let mut endpoint =
        reqwest::Url::parse(&args.jwks_url).map_err(|_| anyhow!("configured endpoint refused"))?;
    endpoint.set_query(None);
    for path in ["/session", "/authoring", "/authoring/effective-release"] {
        endpoint.set_path(path);
        for method in [reqwest::Method::GET, reqwest::Method::POST] {
            let response = http
                .request(method, endpoint.clone())
                .send()
                .await
                .map_err(|_| anyhow!("request absent identity authority route"))?;
            ensure!(
                response.status() == reqwest::StatusCode::NOT_FOUND,
                "identity exposed a session or authoring authority route"
            );
        }
    }
    pass("absent_authority_routes");
    println!(
        "IDENTITY_JWKS result=pass expected_keys={} token_admission=not_proven expiry_outage=not_proven",
        expected.len()
    );
    Ok(())
}

fn client(ca: &[u8], https_only: bool) -> anyhow::Result<reqwest::Client> {
    let roots = reqwest::Certificate::from_pem_bundle(ca)
        .map_err(|_| anyhow!("parse public CA control"))?;
    ensure!(!roots.is_empty(), "CA control is empty");
    reqwest::Client::builder()
        .https_only(https_only)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .timeout(FETCH_TIMEOUT)
        .tls_backend_rustls()
        .tls_certs_only(roots)
        .build()
        .map_err(|_| anyhow!("construct read-only public HTTPS observer"))
}

async fn public_set(
    http: &reqwest::Client,
    endpoint: &str,
) -> anyhow::Result<BTreeMap<String, PublicSessionKey>> {
    let mut response = http
        .get(endpoint)
        .send()
        .await
        .map_err(|_| anyhow!("fetch deployed public JWKS"))?;
    ensure!(
        response.status() == reqwest::StatusCode::OK,
        "deployed JWKS is not HTTP 200"
    );
    ensure!(
        response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .is_some_and(|value| value == "application/json"),
        "deployed JWKS does not declare application/json"
    );
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| anyhow!("read deployed public JWKS"))?
    {
        ensure!(
            chunk.len() <= MAX_BODY_BYTES - body.len(),
            "deployed public JWKS exceeds 65536 bytes"
        );
        body.extend_from_slice(&chunk);
    }
    // Both structs reject unknown fields, including private JWK parameters.
    let jwks: SessionJwks = serde_json::from_slice(&body)
        .map_err(|_| anyhow!("deployed JWKS is not the exact public-only shape"))?;
    let mut keys = BTreeMap::new();
    for key in jwks.keys {
        decode_public_key(&key)
            .map_err(|_| anyhow!("deployed key is not the exact Ed25519 public profile"))?;
        ensure!(
            keys.insert(key.kid.clone(), key).is_none(),
            "deployed JWKS repeats a key ID"
        );
    }
    Ok(keys)
}

fn pass(case: &str) {
    println!("IDENTITY_JWKS case={case} result=pass");
}
