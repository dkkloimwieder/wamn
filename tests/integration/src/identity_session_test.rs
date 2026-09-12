//! Deployed HTTPS session exchange evidence and disposable credential fixtures.
//!
//! The observer holds PAT cases and public keys, not database credentials. Its
//! timing check brackets client dispatch with the approved clock tolerance;
//! exact server validation-start aging belongs to the live issuer test.
//! Neither path claims that a host admits session tokens.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, ensure};
use clap::Args;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt as _;
use wamn_control_provision::identity_issuer::validate_identity_issuer;
use wamn_platform_identity::session_keys::{PublicSessionKey, SessionJwks, decode_public_key};
use wamn_platform_identity::session_token::{
    MAXIMUM_LIFETIME, SessionScope, TOLERANCE, session_key_id, verify_session_token,
};
use wamn_platform_identity::{create_human, create_service, issue_pat};

const IO_TIMEOUT: Duration = Duration::from_secs(5);
const TEST_TIMEOUT: Duration = Duration::from_secs(90);
const MAX_BYTES: usize = 65_536;

/// HTTPS observer inputs; the cases file is a protected Secret mount.
#[derive(Debug, Args)]
pub struct IdentitySessionTestArgs {
    #[arg(long, env = "WAMN_IDENTITY_ISSUER")]
    pub issuer: String,
    #[arg(long, env = "WAMN_IDENTITY_CA_FILE")]
    pub ca_file: PathBuf,
    #[arg(long, env = "WAMN_IDENTITY_SESSION_CASES_FILE")]
    pub cases_file: PathBuf,
}

/// Explicit private output only; database authority is accepted from Secret env.
#[derive(Debug, Args)]
pub struct IdentitySessionFixtureArgs {
    #[arg(long)]
    pub output: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    name: String,
    pat: String,
    audience: String,
    expected_status: u16,
    subject: String,
    org: String,
    roles: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExchangeResponse {
    access_token: String,
    token_type: String,
    expires_at: i64,
}

fn cases(bytes: &[u8]) -> anyhow::Result<Vec<Case>> {
    ensure!(
        bytes.len() <= MAX_BYTES,
        "identity session cases exceed the size bound"
    );
    let cases: Vec<Case> = serde_json::from_slice(bytes)
        .map_err(|_| anyhow!("identity session cases document refused"))?;
    ensure!(
        !cases.is_empty() && cases.len() <= 64,
        "identity session case count refused"
    );
    let mut names = BTreeSet::new();
    for case in &cases {
        ensure!(
            !case.name.is_empty()
                && case.name.len() <= 64
                && case.name.bytes().all(|byte| byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'_' | b'-'))
                && names.insert(case.name.as_str()),
            "identity session case name refused"
        );
        ensure!(
            matches!(case.expected_status, 200 | 401),
            "identity session expected status refused"
        );
        ensure!(
            !case.pat.is_empty() && !case.audience.is_empty(),
            "identity session request case is incomplete"
        );
        if case.expected_status == 200 {
            ensure!(
                !case.subject.is_empty() && !case.org.is_empty() && !case.roles.is_empty(),
                "identity session success expectation is incomplete"
            );
            let unique: BTreeSet<_> = case.roles.iter().collect();
            ensure!(
                unique.len() == case.roles.len() && case.roles.iter().all(|role| !role.is_empty()),
                "identity session role expectation refused"
            );
        }
    }
    Ok(cases)
}

async fn read_file(path: &Path) -> anyhow::Result<Vec<u8>> {
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|_| anyhow!("read identity proof input failed"))?;
    let mut body = Vec::new();
    file.take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut body)
        .await
        .map_err(|_| anyhow!("read identity proof input failed"))?;
    ensure!(
        body.len() <= MAX_BYTES,
        "identity proof input exceeds the size bound"
    );
    Ok(body)
}

async fn read_body(mut response: reqwest::Response) -> anyhow::Result<Vec<u8>> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| anyhow!("read identity HTTPS response failed"))?
    {
        ensure!(
            chunk.len() <= MAX_BYTES - body.len(),
            "identity HTTPS response exceeds the size bound"
        );
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn exact_header(
    headers: &reqwest::header::HeaderMap,
    name: reqwest::header::HeaderName,
    expected: &str,
) -> bool {
    let mut values = headers.get_all(name).iter();
    values.next().is_some_and(|value| value == expected) && values.next().is_none()
}

fn unix_seconds() -> anyhow::Result<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| anyhow!("identity proof clock refused"))?
        .as_secs()
        .try_into()
        .map_err(|_| anyhow!("identity proof clock refused"))
}

/// Observe every protected case over the actual configured HTTPS service.
pub async fn run(args: IdentitySessionTestArgs) -> anyhow::Result<()> {
    tokio::time::timeout(TEST_TIMEOUT, observe(args))
        .await
        .map_err(|_| anyhow!("identity session proof exceeded ninety seconds"))?
}

async fn observe(args: IdentitySessionTestArgs) -> anyhow::Result<()> {
    validate_identity_issuer(&args.issuer)
        .map_err(|_| anyhow!("identity session HTTPS issuer refused"))?;
    let cases = cases(&read_file(&args.cases_file).await?)?;
    let ca = read_file(&args.ca_file).await?;
    let roots = reqwest::Certificate::from_pem_bundle(&ca)
        .map_err(|_| anyhow!("identity session CA refused"))?;
    ensure!(!roots.is_empty(), "identity session CA is empty");
    let http = reqwest::Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .timeout(IO_TIMEOUT)
        .tls_backend_rustls()
        .tls_certs_only(roots)
        .build()
        .map_err(|_| anyhow!("construct identity session HTTPS observer failed"))?;
    let mut endpoint = reqwest::Url::parse(&args.issuer)
        .map_err(|_| anyhow!("identity session endpoint refused"))?;
    endpoint.set_path("/.well-known/jwks.json");
    let response = http
        .get(endpoint.clone())
        .send()
        .await
        .map_err(|_| anyhow!("fetch identity session public keys failed"))?;
    ensure!(
        response.status() == reqwest::StatusCode::OK,
        "identity session public keys status refused"
    );
    ensure!(
        exact_header(
            response.headers(),
            reqwest::header::CONTENT_TYPE,
            "application/json"
        ),
        "identity session public keys content type refused"
    );
    let jwks: SessionJwks = serde_json::from_slice(&read_body(response).await?)
        .map_err(|_| anyhow!("identity session public key document refused"))?;
    let mut keys: BTreeMap<String, PublicSessionKey> = BTreeMap::new();
    for key in jwks.keys {
        decode_public_key(&key)
            .map_err(|_| anyhow!("identity session public key profile refused"))?;
        ensure!(
            keys.insert(key.kid.clone(), key).is_none(),
            "identity session public key IDs repeat"
        );
    }
    ensure!(!keys.is_empty(), "identity session public key set is empty");
    endpoint.set_path("/session");
    for case in &cases {
        let started_at = unix_seconds()?;
        let response = http
            .post(endpoint.clone())
            .bearer_auth(&case.pat)
            .json(&serde_json::json!({"aud": case.audience}))
            .send()
            .await
            .map_err(|_| anyhow!("identity session HTTPS request failed"))?;
        ensure!(
            response.status().as_u16() == case.expected_status,
            "identity session HTTP status differs from expectation"
        );
        ensure!(
            exact_header(
                response.headers(),
                reqwest::header::CONTENT_TYPE,
                "application/json"
            ),
            "identity session response content type refused"
        );
        ensure!(
            exact_header(
                response.headers(),
                reqwest::header::CACHE_CONTROL,
                "no-store"
            ),
            "identity session response must prohibit storage"
        );
        if case.expected_status == 401 {
            ensure!(
                exact_header(
                    response.headers(),
                    reqwest::header::WWW_AUTHENTICATE,
                    "Bearer"
                ),
                "identity session refusal challenge differs"
            );
            let body = read_body(response).await?;
            ensure!(
                body.as_slice() == b"{\"error\":\"unauthorized\"}",
                "identity session refusal body differs"
            );
        } else {
            let response: ExchangeResponse = serde_json::from_slice(&read_body(response).await?)
                .map_err(|_| anyhow!("identity session response fields refused"))?;
            ensure!(
                response.token_type == "Bearer",
                "identity session token type refused"
            );
            let kid = session_key_id(&response.access_token)
                .map_err(|_| anyhow!("identity session signed header refused"))?;
            let key = keys
                .get(&kid)
                .ok_or_else(|| anyhow!("identity session signing key is absent"))?;
            let received_at = unix_seconds()?;
            let claims = verify_session_token(
                &response.access_token,
                key,
                SessionScope {
                    issuer: &args.issuer,
                    org: &case.org,
                    audience: &case.audience,
                },
                received_at,
            )
            .map_err(|_| anyhow!("identity session signature or scope refused"))?;
            let expected_roles = case.roles.iter().collect::<BTreeSet<_>>();
            let actual_roles = claims.roles.iter().collect::<BTreeSet<_>>();
            ensure!(
                claims.sub == case.subject
                    && claims.org == case.org
                    && claims.aud == case.audience
                    && actual_roles == expected_roles
                    && actual_roles.len() == claims.roles.len(),
                "identity session principal or environment roles differ"
            );
            let expiry_bound = started_at
                .checked_add(MAXIMUM_LIFETIME)
                .and_then(|value| value.checked_add(TOLERANCE))
                .ok_or_else(|| anyhow!("identity session request-start bound overflowed"))?;
            ensure!(
                claims.exp == response.expires_at
                    && claims.exp <= expiry_bound
                    && claims.iat >= started_at - TOLERANCE
                    && claims.iat <= received_at + TOLERANCE,
                "identity session age exceeds the client-observed request-start bound"
            );
        }
        println!("IDENTITY_SESSION case={} result=pass", case.name);
    }
    println!(
        "IDENTITY_SESSION result=pass cases={} host_admission=not_tested",
        cases.len()
    );
    Ok(())
}

#[derive(Serialize)]
struct FixtureDocument<'a> {
    human_id: &'a str,
    human_pat: &'a str,
    service_id: &'a str,
    service_pat: &'a str,
}

/// Mint disposable fixtures only after explicit arming; never print their values.
pub async fn fixture(args: IdentitySessionFixtureArgs) -> anyhow::Result<()> {
    ensure!(
        std::env::var("WAMN_SESSION_FIXTURE_ALLOW").as_deref() == Ok("1"),
        "identity session fixture is not explicitly armed"
    );
    ensure!(
        args.output.is_absolute(),
        "identity session fixture output must be an explicit absolute path"
    );
    let raw = std::env::var("WAMN_SYSTEM_ADMIN_URL")
        .map_err(|_| anyhow!("identity session fixture database Secret is required"))?;
    let parsed = reqwest::Url::parse(&raw)
        .map_err(|_| anyhow!("identity session fixture database configuration refused"))?;
    ensure!(
        matches!(parsed.scheme(), "postgres" | "postgresql")
            && parsed.path() == "/wamn_system"
            && parsed.host_str().is_some_and(|host| !host.is_empty())
            && parsed.query().is_none()
            && parsed.fragment().is_none(),
        "identity session fixture must address the owned system database"
    );
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&args.output)
        .map_err(|_| anyhow!("create private identity fixture output failed"))?;
    let bytes = tokio::time::timeout(Duration::from_secs(30), create_fixture(&raw))
        .await
        .map_err(|_| anyhow!("identity session fixture exceeded thirty seconds"))??;
    output
        .write_all(&bytes)
        .map_err(|_| anyhow!("write private identity fixture output failed"))?;
    output
        .sync_all()
        .map_err(|_| anyhow!("flush private identity fixture output failed"))?;
    println!("IDENTITY_SESSION_FIXTURE result=pass principals=2");
    Ok(())
}

async fn create_fixture(url: &str) -> anyhow::Result<Vec<u8>> {
    let (mut client, connection) = tokio::time::timeout(
        IO_TIMEOUT,
        tokio_postgres::connect(url, tokio_postgres::NoTls),
    )
    .await
    .map_err(|_| anyhow!("identity session fixture database connection timed out"))?
    .map_err(|_| anyhow!("identity session fixture database connection failed"))?;
    let driver = tokio::spawn(async move {
        let _ = connection.await;
    });
    let result = async {
        client.batch_execute("SET statement_timeout = '5s'; SET lock_timeout = '5s'").await
            .map_err(|_| anyhow!("bound identity session fixture database operations failed"))?;
        let row = client.query_one("SELECT current_database() = 'wamn_system' AND current_setting('server_version_num')::int >= 180000 AND current_setting('server_version_num')::int < 190000", &[]).await
            .map_err(|_| anyhow!("check identity session fixture database failed"))?;
        ensure!(row.get::<_, bool>(0), "identity session fixture requires owned PostgreSQL eighteen system database");
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH)
            .map_err(|_| anyhow!("identity session fixture clock refused"))?.as_nanos();
        let transaction = client.transaction().await
            .map_err(|_| anyhow!("begin identity session fixture transaction failed"))?;
        let human = create_human(&transaction, &format!("session-fixture-human-{suffix}-{}", std::process::id()), "Disposable session proof human").await
            .map_err(|_| anyhow!("create identity session fixture human failed"))?;
        let service = create_service(&transaction, &format!("session-fixture-service-{suffix}-{}", std::process::id()), "Disposable session proof service").await
            .map_err(|_| anyhow!("create identity session fixture service failed"))?;
        let human_pat = issue_pat(&transaction, human.id(), "Disposable session proof", Duration::from_secs(3600)).await
            .map_err(|_| anyhow!("issue identity session fixture human PAT failed"))?;
        let service_pat = issue_pat(&transaction, service.id(), "Disposable session proof", Duration::from_secs(3600)).await
            .map_err(|_| anyhow!("issue identity session fixture service PAT failed"))?;
        let bytes = serde_json::to_vec(&FixtureDocument {
            human_id: human.id().as_str(), human_pat: human_pat.token(),
            service_id: service.id().as_str(), service_pat: service_pat.token(),
        }).map_err(|_| anyhow!("encode private identity session fixture failed"))?;
        transaction.commit().await.map_err(|_| anyhow!("commit identity session fixture failed"))?;
        Ok(bytes)
    }.await;
    drop(client);
    driver.abort();
    result
}

#[cfg(test)]
mod tests {
    use super::{Case, ExchangeResponse, cases, exact_header};
    use serde_json::json;

    fn valid() -> serde_json::Value {
        json!([{"name":"dev_allowed","pat":"fixture-secret","audience":"environment",
            "expected_status":200,"subject":"human","org":"acme","roles":["receiver"]}])
    }

    #[test]
    fn observer_cases_require_exact_fields_unique_safe_names_and_explicit_status() {
        assert!(cases(&serde_json::to_vec(&valid()).unwrap()).is_ok());
        let mut denied = valid();
        denied[0]["expected_status"] = json!(401);
        denied[0]["roles"] = json!([]);
        assert!(cases(&serde_json::to_vec(&denied).unwrap()).is_ok());
        for (field, value) in [
            ("name", json!("forged\nIDENTITY_SESSION result=pass")),
            ("name", json!("")),
            ("expected_status", json!(403)),
            ("roles", json!(["receiver", "receiver"])),
            ("database_url", json!("fixture-secret")),
        ] {
            let mut malformed = valid();
            malformed[0][field] = value;
            let error = cases(&serde_json::to_vec(&malformed).unwrap())
                .err()
                .expect("case must refuse");
            assert!(!format!("{error:?} {error}").contains("fixture-secret"));
        }
        let original = valid();
        assert!(cases(&serde_json::to_vec(&json!([original[0], original[0]])).unwrap()).is_err());
        assert!(cases(b"[]").is_err());
        assert!(serde_json::from_str::<Case>(r#"{"name":"a","name":"b"}"#).is_err());
    }

    #[test]
    fn success_response_accepts_only_the_exact_three_field_shape() {
        assert!(
            serde_json::from_str::<ExchangeResponse>(
                r#"{"token_type":"Bearer","access_token":"fixture-secret","expires_at":123}"#
            )
            .is_ok()
        );
        for body in [
            r#"{"token_type":"Bearer","access_token":"fixture-secret"}"#,
            r#"{"token_type":"Bearer","access_token":"fixture-secret","expires_at":123,"refresh_token":"extra"}"#,
            r#"{"token_type":"Bearer","access_token":"fixture-secret","expires_at":123,"expires_at":124}"#,
        ] {
            assert!(serde_json::from_str::<ExchangeResponse>(body).is_err());
        }
    }

    #[test]
    fn headers_refuse_missing_wrong_and_duplicate_values() {
        use reqwest::header::{HeaderMap, HeaderValue, WWW_AUTHENTICATE};

        let mut headers = HeaderMap::new();
        assert!(!exact_header(&headers, WWW_AUTHENTICATE, "Bearer"));
        headers.insert(WWW_AUTHENTICATE, HeaderValue::from_static("Basic"));
        assert!(!exact_header(&headers, WWW_AUTHENTICATE, "Bearer"));
        headers.insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        assert!(exact_header(&headers, WWW_AUTHENTICATE, "Bearer"));
        headers.append(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        assert!(!exact_header(&headers, WWW_AUTHENTICATE, "Bearer"));
    }
}
