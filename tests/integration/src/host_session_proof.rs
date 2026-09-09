//! Observe one actual session token on two deployed hosts before and after key removal.
//!
//! The runner owns key removal and service shutdown after the flushed warm marker.
//! This observer holds a human PAT and public keys, without database or signing authority.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, ensure};
use clap::Args;
use serde::Deserialize;
use serde_json::Value;
use tokio::io::AsyncReadExt as _;
use wamn_control_provision::identity_issuer::validate_identity_issuer;
use wamn_control_provision::session_target::{session_audience, validate_session_tenant_id};
use wamn_platform_identity::session_keys::{PublicSessionKey, SessionJwks, decode_public_key};
use wamn_platform_identity::session_token::{
    MAXIMUM_LIFETIME, SessionScope, TOLERANCE, session_key_id, verify_session_token,
};

const MAX_BYTES: usize = 65_536;
const IO_TIMEOUT: Duration = Duration::from_secs(5);
const WARM_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const PROOF_TIMEOUT: Duration = Duration::from_secs(420);
const KEY_MAX_AGE: Duration = Duration::from_secs(300);
const ROUTE_PATH: &str = "/purchase_order/get";

/// Public endpoint configuration and the path to the protected credential fixture.
#[derive(Debug, Args)]
pub struct HostSessionProofArgs {
    #[arg(long, env = "WAMN_IDENTITY_ISSUER", hide_env_values = true)]
    pub issuer: String,
    #[arg(long, env = "WAMN_IDENTITY_CA_FILE")]
    pub ca_file: PathBuf,
    #[arg(long, env = "WAMN_HOST_SESSION_FIXTURE_FILE")]
    pub fixture_file: PathBuf,
    #[arg(long, env = "WAMN_HOST_SESSION_ENDPOINTS", hide_env_values = true)]
    pub endpoints: String,
    #[arg(long, env = "WAMN_HOST_SESSION_JWKS_AVAILABLE", action = clap::ArgAction::Set, required = true)]
    pub jwks_available: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    manifest_digest: String,
    human_pat: String,
    human_id: String,
    audience: String,
    org: String,
    project: String,
    environment: String,
    tenant: String,
    instance_suffix: String,
    roles: Vec<String>,
    request_body: Value,
    expected_response: Value,
    route_path: String,
    route_host: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExchangeResponse {
    access_token: String,
    token_type: String,
    expires_at: i64,
}

fn endpoints(value: &str, route_path: &str) -> anyhow::Result<[reqwest::Url; 2]> {
    ensure!(
        value.len() <= MAX_BYTES,
        "host endpoints exceed the size bound"
    );
    let parsed = value
        .split(',')
        .map(|value| {
            let url =
                reqwest::Url::parse(value).map_err(|_| anyhow!("host endpoint URL refused"))?;
            ensure!(
                matches!(url.scheme(), "http" | "https")
                    && url.host_str().is_some()
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.query().is_none()
                    && url.fragment().is_none()
                    && !value.chars().any(char::is_whitespace)
                    && url.path() == route_path,
                "host endpoint must name the exact route without credentials, query, or fragment"
            );
            Ok(url)
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let parsed: [reqwest::Url; 2] = parsed
        .try_into()
        .map_err(|_| anyhow!("exactly two host endpoints are required"))?;
    ensure!(
        parsed[0].origin() != parsed[1].origin(),
        "the two host origins must differ"
    );
    Ok(parsed)
}

fn fixture(bytes: &[u8]) -> anyhow::Result<Fixture> {
    ensure!(
        bytes.len() <= MAX_BYTES,
        "host fixture exceeds the size bound"
    );
    let fixture: Fixture =
        serde_json::from_slice(bytes).map_err(|_| anyhow!("host fixture document refused"))?;
    let triple = wamn_control_registry::Triple::new(
        &fixture.org,
        &fixture.project,
        fixture.environment.as_str(),
    );
    ensure!(
        session_audience(&triple, &fixture.instance_suffix)
            .is_ok_and(|audience| audience == fixture.audience),
        "host fixture audience differs from its scope"
    );
    validate_session_tenant_id(&fixture.tenant)
        .map_err(|_| anyhow!("host fixture tenant refused"))?;
    wamn_catalog::ManifestDigest::parse(&fixture.manifest_digest)
        .map_err(|_| anyhow!("host fixture manifest digest refused"))?;
    let principal = fixture
        .human_id
        .parse::<wamn_platform_identity::PrincipalId>()
        .map_err(|_| anyhow!("host fixture principal refused"))?;
    ensure!(
        principal.as_str() == fixture.human_id
            && fixture
                .human_pat
                .starts_with(wamn_platform_identity::PAT_TOKEN_PREFIX),
        "host fixture human credential refused"
    );
    let roles = fixture.roles.iter().collect::<BTreeSet<_>>();
    ensure!(
        !roles.is_empty()
            && roles.len() == fixture.roles.len()
            && fixture.roles.iter().all(|role| !role.is_empty()),
        "host fixture roles refused"
    );
    ensure!(
        fixture.route_path == ROUTE_PATH
            && !fixture.route_host.is_empty()
            && reqwest::header::HeaderValue::from_str(&fixture.route_host).is_ok(),
        "host fixture route refused"
    );
    let request = unit_item(&fixture.request_body)?;
    let expected = unit_item(&fixture.expected_response)?;
    let value = expected
        .get("value")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("host fixture needs a successful GET value"))?;
    ensure!(
        request.len() == 2
            && expected.len() == 2
            && request
                .get("request_id")
                .and_then(Value::as_str)
                .is_some_and(|id| !id.is_empty())
            && request.get("request_id") == expected.get("request_id")
            && request.get("id") == value.get("id")
            && value.len() == 7
            && [
                "id",
                "created_at",
                "updated_at",
                "purchase_order_number",
                "supplier_id",
                "status",
                "row_version"
            ]
            .iter()
            .all(|name| value
                .get(*name)
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty())),
        "host fixture request or complete GET response refused"
    );
    Ok(fixture)
}

fn unit_item(value: &Value) -> anyhow::Result<&serde_json::Map<String, Value>> {
    value
        .as_array()
        .filter(|items| items.len() == 1)
        .and_then(|items| items[0].as_object())
        .ok_or_else(|| anyhow!("host fixture requires a one-item batch envelope"))
}

async fn read_file(path: &Path, private: bool) -> anyhow::Result<Vec<u8>> {
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|_| anyhow!("read host proof input failed"))?;
    let metadata = file
        .metadata()
        .await
        .map_err(|_| anyhow!("read host proof input metadata failed"))?;
    ensure!(
        metadata.is_file() && (!private || metadata.permissions().mode() & 0o077 == 0),
        "host proof credential input must be an owner-only regular file"
    );
    let mut body = Vec::new();
    file.take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut body)
        .await
        .map_err(|_| anyhow!("read host proof input failed"))?;
    ensure!(
        body.len() <= MAX_BYTES,
        "host proof input exceeds the size bound"
    );
    Ok(body)
}

async fn read_body(mut response: reqwest::Response) -> anyhow::Result<Vec<u8>> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| anyhow!("read host proof HTTP body failed"))?
    {
        ensure!(
            chunk.len() <= MAX_BYTES - body.len(),
            "host proof HTTP body exceeds the size bound"
        );
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn unix_seconds() -> anyhow::Result<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| anyhow!("host proof clock refused"))?
        .as_secs()
        .try_into()
        .map_err(|_| anyhow!("host proof clock refused"))
}

async fn public_keys(
    http: &reqwest::Client,
    endpoint: &reqwest::Url,
) -> anyhow::Result<BTreeMap<String, PublicSessionKey>> {
    let response = http
        .get(endpoint.clone())
        .send()
        .await
        .map_err(|_| anyhow!("host proof JWKS request failed"))?;
    ensure!(
        response.status() == reqwest::StatusCode::OK,
        "host proof JWKS must return HTTP 200"
    );
    let jwks: SessionJwks = serde_json::from_slice(&read_body(response).await?)
        .map_err(|_| anyhow!("host proof JWKS document refused"))?;
    let mut keys = BTreeMap::new();
    for key in jwks.keys {
        decode_public_key(&key).map_err(|_| anyhow!("host proof public key profile refused"))?;
        ensure!(
            keys.insert(key.kid.clone(), key).is_none(),
            "host proof JWKS key IDs repeat"
        );
    }
    Ok(keys)
}

async fn host_request(
    http: &reqwest::Client,
    endpoint: &reqwest::Url,
    fixture: &Fixture,
    token: &str,
    timeout: Duration,
) -> anyhow::Result<(reqwest::StatusCode, Vec<u8>)> {
    let response = http
        .post(endpoint.clone())
        .timeout(timeout)
        .header(reqwest::header::HOST, &fixture.route_host)
        .bearer_auth(token)
        .json(&fixture.request_body)
        .send()
        .await
        .map_err(|_| anyhow!("deployed host request failed"))?;
    let status = response.status();
    Ok((status, read_body(response).await?))
}

/// Observe warm acceptance and bounded refusal without changing any deployed authority.
pub async fn run(args: HostSessionProofArgs) -> anyhow::Result<()> {
    tokio::time::timeout(PROOF_TIMEOUT, observe(args))
        .await
        .map_err(|_| anyhow!("host session proof exceeded 420 seconds"))?
}

async fn observe(args: HostSessionProofArgs) -> anyhow::Result<()> {
    ensure!(
        args.issuer.len() <= MAX_BYTES,
        "host proof issuer exceeds the size bound"
    );
    validate_identity_issuer(&args.issuer)
        .map_err(|_| anyhow!("host proof HTTPS issuer refused"))?;
    let fixture = fixture(&read_file(&args.fixture_file, true).await?)?;
    let endpoints = endpoints(&args.endpoints, &fixture.route_path)?;
    let roots = reqwest::Certificate::from_pem_bundle(&read_file(&args.ca_file, false).await?)
        .map_err(|_| anyhow!("host proof CA bundle refused"))?;
    ensure!(!roots.is_empty(), "host proof CA bundle is empty");
    let http = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .timeout(IO_TIMEOUT)
        .tls_backend_rustls()
        .tls_certs_only(roots)
        .build()
        .map_err(|_| anyhow!("construct host proof HTTP client failed"))?;
    let mut jwks_url =
        reqwest::Url::parse(&args.issuer).map_err(|_| anyhow!("host proof issuer URL refused"))?;
    jwks_url.set_path("/.well-known/jwks.json");
    let keys = public_keys(&http, &jwks_url).await?;
    let mut exchange_url = jwks_url.clone();
    exchange_url.set_path("/session");
    let started_at = unix_seconds()?;
    let response = http
        .post(exchange_url)
        .bearer_auth(&fixture.human_pat)
        .json(&serde_json::json!({"aud": fixture.audience}))
        .send()
        .await
        .map_err(|_| anyhow!("host proof session exchange failed"))?;
    ensure!(
        response.status() == reqwest::StatusCode::OK,
        "host proof session exchange must return HTTP 200"
    );
    ensure!(
        response
            .headers()
            .get(reqwest::header::CACHE_CONTROL)
            .is_some_and(|value| value == "no-store"),
        "host proof session exchange must prohibit storage"
    );
    let exchange: ExchangeResponse = serde_json::from_slice(&read_body(response).await?)
        .map_err(|_| anyhow!("host proof session exchange document refused"))?;
    ensure!(
        exchange.token_type == "Bearer",
        "host proof session token type refused"
    );
    let kid = session_key_id(&exchange.access_token)
        .map_err(|_| anyhow!("host proof signed header refused"))?;
    let key = keys
        .get(&kid)
        .ok_or_else(|| anyhow!("host proof signing key is absent from configured JWKS"))?;
    let scope = SessionScope {
        issuer: &args.issuer,
        org: &fixture.org,
        audience: &fixture.audience,
    };
    let received_at = unix_seconds()?;
    let claims = verify_session_token(&exchange.access_token, key, scope, received_at)
        .map_err(|_| anyhow!("host proof session signature, scope, or age refused"))?;
    let expected_roles = fixture.roles.iter().collect::<BTreeSet<_>>();
    let actual_roles = claims.roles.iter().collect::<BTreeSet<_>>();
    ensure!(
        claims.sub == fixture.human_id
            && claims.org == fixture.org
            && claims.aud == fixture.audience
            && actual_roles == expected_roles
            && actual_roles.len() == claims.roles.len()
            && claims.exp == exchange.expires_at,
        "host proof signed identity or expiry differs from its fixture"
    );
    let expiry_bound = started_at
        .checked_add(MAXIMUM_LIFETIME)
        .and_then(|value| value.checked_add(TOLERANCE))
        .ok_or_else(|| anyhow!("host proof exchange age bound overflowed"))?;
    ensure!(
        claims.exp <= expiry_bound
            && claims.iat >= started_at - TOLERANCE
            && claims.iat <= received_at + TOLERANCE,
        "host proof exchange exceeds its observed evidence-age bound"
    );
    for endpoint in &endpoints {
        let (status, body) = host_request(
            &http,
            endpoint,
            &fixture,
            &exchange.access_token,
            WARM_REQUEST_TIMEOUT,
        )
        .await?;
        ensure!(
            status == reqwest::StatusCode::OK,
            "deployed host did not accept the session token"
        );
        let response: Value = serde_json::from_slice(&body)
            .map_err(|_| anyhow!("deployed host success body is not JSON"))?;
        ensure!(
            response == fixture.expected_response,
            "deployed host GET response differs from the complete expected result"
        );
    }
    let warm_completed = tokio::time::Instant::now();
    ensure!(
        claims.exp
            > unix_seconds()?
                .checked_add(KEY_MAX_AGE.as_secs() as i64)
                .ok_or_else(|| anyhow!("host proof remaining lifetime overflowed"))?,
        "host proof token cannot outlive the key-freshness observation"
    );
    println!("HOST_SESSION_WARM hosts=2");
    std::io::stdout()
        .flush()
        .map_err(|_| anyhow!("flush host proof warm marker failed"))?;
    tokio::time::sleep_until(warm_completed + KEY_MAX_AGE).await;
    ensure!(
        unix_seconds()? < claims.exp,
        "host proof token expired before key-removal refusal"
    );
    for _ in 0..2 {
        for endpoint in &endpoints {
            let (status, _) = host_request(
                &http,
                endpoint,
                &fixture,
                &exchange.access_token,
                IO_TIMEOUT,
            )
            .await?;
            ensure!(
                status == reqwest::StatusCode::UNAUTHORIZED,
                "deployed host did not refuse the removed key at its freshness deadline"
            );
        }
    }
    ensure!(
        unix_seconds()? < claims.exp,
        "host proof refusal cannot be attributed to token expiry"
    );
    verify_session_token(&exchange.access_token, key, scope, unix_seconds()?)
        .map_err(|_| anyhow!("host proof token no longer satisfies its signed age profile"))?;
    if args.jwks_available {
        ensure!(
            !public_keys(&http, &jwks_url).await?.contains_key(&kid),
            "removed key remains present in reachable JWKS"
        );
    } else {
        match http.get(jwks_url).send().await {
            Err(error) if error.is_connect() || error.is_timeout() => {}
            _ => {
                return Err(anyhow!(
                    "unavailable JWKS must fail to connect or time out, not return HTTP"
                ));
            }
        }
    }
    println!(
        "HOST_SESSION_PROOF result=pass hosts=2 jwks_available={}",
        args.jwks_available
    );
    std::io::stdout()
        .flush()
        .map_err(|_| anyhow!("flush host proof result failed"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{HostSessionProofArgs, ROUTE_PATH, endpoints};
    use clap::{CommandFactory as _, Parser};

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        args: HostSessionProofArgs,
    }

    #[test]
    fn endpoint_pair_requires_distinct_origins_and_the_exact_route() {
        let pair = endpoints(
            "http://10.1.0.1:8000/purchase_order/get,http://10.1.0.2:8000/purchase_order/get",
            ROUTE_PATH,
        )
        .expect("two explicit host routes");
        assert_ne!(pair[0].origin(), pair[1].origin());
        for bad in [
            "http://one/purchase_order/get",
            "http://one/purchase_order/get,http://one:80/purchase_order/get",
            "http://one/purchase_order/get,http://two/other",
            "http://one/purchase_order/get,http://user:secret@two/purchase_order/get",
            "http://one/purchase_order/get,http://two/purchase_order/get?override=1",
            "http://one/purchase_order/get,http://two/purchase_order/get#fragment",
            "http://one/purchase_order/get, http://two/purchase_order/get",
            "http://one/purchase_order/get,file:///purchase_order/get",
            "http://one/purchase_order/get,http://two/purchase_order/get,http://three/purchase_order/get",
        ] {
            assert!(endpoints(bad, ROUTE_PATH).is_err());
        }
    }

    #[test]
    fn jwks_availability_is_an_explicit_boolean_argument() {
        let command = Cli::command();
        let available = command
            .get_arguments()
            .find(|argument| argument.get_id() == "jwks_available")
            .unwrap();
        assert!(available.is_required_set());
        for value in ["true", "false"] {
            let parsed = Cli::try_parse_from([
                "host-session-proof",
                "--issuer",
                "https://identity.example.test",
                "--ca-file",
                "/fixture/ca.pem",
                "--fixture-file",
                "/fixture/input.json",
                "--endpoints",
                "http://one/purchase_order/get,http://two/purchase_order/get",
                "--jwks-available",
                value,
            ])
            .unwrap();
            assert_eq!(parsed.args.jwks_available, value == "true");
        }
    }
}
