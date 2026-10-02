//! The app shell in Chrome, through an application host and a control host
//! (docs/plan/platform-ui.md §4.8, wamn-a40n.11).
//!
//! The test serves the application set of the host route fixture release and
//! the control set of the control fixture, each through the shipped
//! flow-http component, as `control_client_live` does. A stand-in for the
//! identity service answers the four password paths for a cookie session,
//! with tokens that the fixture keys sign. A Vite dev server gives the test
//! page of `web/shell/test/browser` one origin, and `journey.mjs` drives it
//! in the machine's Chrome through Playwright.

use std::collections::HashMap;
use std::net::TcpListener as StdListener;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use bytes::Bytes;
use http_body_util::{BodyExt as _, Full};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tokio::net::TcpListener;
use wamn_control_provision::PlatformComponent;
use wamn_engine::engine::build_engine;
use wamn_engine::flow_http_routing::{FlowHttpRouting, RouteInFlightLimit};
use wamn_engine::release_manifest::LoadedRelease;
use wamn_engine::router_delivery::RouteDelivery;
use wamn_execution_host::{HostRouteDelivery, HostRouteHandlers};
use wamn_platform_identity::create_or_reuse_user;
use wamn_platform_identity::org::{MemberGrants, invite_member};
use wamn_runtime::plugins::route_authentication::ControlRouteAuthenticator;
use wash_runtime::wasmtime::component::Component;

use crate::local_application::serve;

use super::{application_fixture, control_fixture, session_fixture};
use application_fixture::{ApplicationHost, MEMBER, PROJECT, application_host};
use session_fixture::{AUDIENCE, ISSUER, ORG, Server, claims, header, signed};

const SESSION_COOKIE: &str = "__Host-wamn-session";
const CSRF_COOKIE: &str = "__Host-wamn-csrf";
const RENEWAL_COOKIE: &str = "__Secure-wamn-renewal";

/// One audience a persona reaches, and the roles its session carries there.
#[derive(Clone)]
struct Reach {
    aud: String,
    roles: Vec<String>,
}

/// The persona of each email: its principal and the audiences it reaches.
type Personas = Arc<HashMap<String, (String, Vec<Reach>)>>;

/// The minted session of one persona on one audience, as its cookies.
fn session_cookies(principal: &str, reach: &Reach) -> Vec<String> {
    let csrf = format!("csrf-{principal}");
    let mut session = claims();
    session["sub"] = json!(principal);
    session["aud"] = json!(reach.aud);
    session["roles"] = json!(reach.roles);
    session["authority"] = json!({"login": principal});
    session["csrf"] = json!(hex::encode(Sha256::digest(csrf.as_bytes())));
    let token = signed(&header(), &session);
    vec![
        format!("{SESSION_COOKIE}={token}; Path=/; Secure; HttpOnly; SameSite=Strict"),
        format!("{CSRF_COOKIE}={csrf}; Path=/; Secure; SameSite=Strict"),
        format!(
            "{RENEWAL_COOKIE}={principal}|{}; Path=/; Secure; HttpOnly; SameSite=Strict",
            reach.aud
        ),
    ]
}

fn times() -> Value {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("a clock after 1970")
        .as_secs();
    json!({"expires_at": now + 3600, "login_expires_at": now + 86_400})
}

fn reply(status: StatusCode, body: &Value, cookies: &[String]) -> Response<Full<Bytes>> {
    let mut response = Response::builder()
        .status(status)
        .header("content-type", "application/json");
    for cookie in cookies {
        response = response.header("set-cookie", cookie);
    }
    response
        .body(Full::new(Bytes::from(body.to_string())))
        .expect("a response")
}

/// The value of one request cookie.
fn cookie<'a>(request: &'a Request<hyper::body::Incoming>, name: &str) -> Option<&'a str> {
    request
        .headers()
        .get_all(hyper::header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find_map(|(key, value)| (key == name).then_some(value))
}

/// The identity stand-in: the password paths of a cookie session. Every
/// password signs in, because the test is about the shell, not the password.
async fn identity(
    personas: Personas,
    request: Request<hyper::body::Incoming>,
) -> Response<Full<Bytes>> {
    let path = request.uri().path().to_owned();
    let renewal = cookie(&request, RENEWAL_COOKIE).map(str::to_owned);
    let body: Value = request
        .into_body()
        .collect()
        .await
        .ok()
        .and_then(|body| serde_json::from_slice(&body.to_bytes()).ok())
        .unwrap_or(Value::Null);
    let unauthorized = || {
        reply(
            StatusCode::UNAUTHORIZED,
            &json!({"error": "unauthorized"}),
            &[],
        )
    };
    let persona = body["email"].as_str().and_then(|email| personas.get(email));
    match path.as_str() {
        "/password/environments" => {
            let listed: Vec<Value> = persona
                .map(|(_, reaches)| reaches.as_slice())
                .unwrap_or_default()
                .iter()
                .map(|reach| {
                    if reach.aud == AUDIENCE {
                        json!({"aud": reach.aud, "org": ORG, "project": PROJECT, "env": "dev"})
                    } else {
                        json!({"aud": reach.aud, "org": ORG})
                    }
                })
                .collect();
            reply(StatusCode::OK, &json!({"environments": listed}), &[])
        }
        "/password/session" => {
            let Some((principal, reaches)) = persona else {
                return unauthorized();
            };
            let Some(reach) = reaches.iter().find(|reach| body["aud"] == reach.aud) else {
                return unauthorized();
            };
            reply(StatusCode::OK, &times(), &session_cookies(principal, reach))
        }
        "/password/renew" => {
            let Some((principal, aud)) = renewal.as_deref().and_then(|value| value.split_once('|'))
            else {
                return unauthorized();
            };
            // `nobody` and the member share a principal, so look through
            // every persona of the principal.
            let reach = personas
                .values()
                .filter(|(id, _)| id == principal)
                .find_map(|(_, reaches)| reaches.iter().find(|reach| reach.aud == aud));
            match reach {
                Some(reach) if body["aud"] == aud => {
                    reply(StatusCode::OK, &times(), &session_cookies(principal, reach))
                }
                _ => unauthorized(),
            }
        }
        "/password/logout" => {
            let cleared: Vec<String> = [SESSION_COOKIE, CSRF_COOKIE, RENEWAL_COOKIE]
                .iter()
                .map(|name| format!("{name}=; Path=/; Secure; Max-Age=0"))
                .collect();
            let mut response = reply(StatusCode::NO_CONTENT, &Value::Null, &cleared);
            *response.body_mut() = Full::new(Bytes::new());
            response
        }
        _ => reply(StatusCode::NOT_FOUND, &Value::Null, &[]),
    }
}

/// Serve the identity stand-in on a loopback port.
async fn serve_identity(
    personas: Personas,
) -> anyhow::Result<(String, tokio::task::JoinHandle<()>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let task = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let personas = Arc::clone(&personas);
            tokio::spawn(async move {
                let service = service_fn(move |request| {
                    let personas = Arc::clone(&personas);
                    async move { Ok::<_, std::convert::Infallible>(identity(personas, request).await) }
                });
                let _ = http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service)
                    .await;
            });
        }
    });
    Ok((format!("http://{address}"), task))
}

/// One free loopback port for the page.
fn free_port() -> anyhow::Result<u16> {
    Ok(StdListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires: WAMN_FLOW_HTTP_COMPONENT and Chrome"]
async fn the_shell_signs_in_and_shows_only_what_each_caller_holds() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&["WAMN_FLOW_HTTP_COMPONENT"]);
    let flow_http_wasm = std::env::var("WAMN_FLOW_HTTP_COMPONENT")?;
    let engine = Arc::new(build_engine(&[])?);
    let flow_http = Component::new(engine.inner(), std::fs::read(&flow_http_wasm)?)?;

    // The application host: `first` holds admin, and the member holds the
    // read operation only.
    let mut host = application_host("shell_browser").await?;
    let ApplicationHost {
        routing,
        delivery,
        first,
        ..
    } = &host;
    let (application, application_task) = serve(
        Arc::clone(&engine),
        flow_http.clone(),
        Arc::clone(routing),
        Arc::clone(delivery) as Arc<dyn RouteDelivery>,
    )
    .await?;

    // The control host: Boss holds org-admin, and Cat project-admin of billing.
    let mut postgres = wamn_test_postgres::start(&[])?;
    let control_database = postgres.create_database("shell_browser_control")?;
    let admin_url = control_database.url();
    let admin = control_fixture::connect(admin_url).await?;
    let control_url = control_fixture::install(&admin, admin_url).await?;
    let control_audience = wamn_platform_identity::control::control_audience(ORG)?;
    admin
        .execute(
            "SELECT set_config('app.user_id', $1, false)",
            &[&PlatformComponent::Provisioning.principal_id().to_string()],
        )
        .await?;
    let boss = create_or_reuse_user(&admin, "boss@example.test", "Boss")
        .await?
        .principal_id;
    let cat = create_or_reuse_user(&admin, "cat@example.test", "Cat")
        .await?
        .principal_id;
    let boss_grants = MemberGrants {
        org_admin: true,
        ..MemberGrants::default()
    };
    let cat_grants = MemberGrants {
        project_admins: vec!["billing".to_owned()],
        ..MemberGrants::default()
    };
    invite_member(&admin, &boss, ORG, &boss_grants).await?;
    invite_member(&admin, &cat, ORG, &cat_grants).await?;
    for principal in [&boss, &cat] {
        admin
            .execute(
                "INSERT INTO identity.password_logins \
                   (id, principal_id, issuer, audience, authenticated_at, expires_at, renewal_expires_at) \
                 VALUES ($1::text::uuid, $1::text::uuid, $2, $3, now(), now() + interval '8 hours', \
                   now() + interval '30 minutes')",
                &[&principal.as_str(), &ISSUER, &control_audience],
            )
            .await?;
    }
    let control_client = Arc::new(control_fixture::connect(&control_url).await?);
    let writer = Arc::new(tokio::sync::Mutex::new(
        control_fixture::connect(&control_url).await?,
    ));
    let mut keys_server = Server::start().await;
    let (keys, _key_clock) = keys_server.cache();
    let (verifier, _token_clock) = wamn_session::verifier::SessionVerifier::with_test_clock(
        keys,
        ORG,
        &control_audience,
        1000,
    )?;
    let release = Arc::new(LoadedRelease::control_root());
    let control_routing = Arc::new(
        FlowHttpRouting::new(Some(Arc::clone(&release)), RouteInFlightLimit::default())
            .with_authenticator(Arc::new(ControlRouteAuthenticator::new(
                verifier,
                Arc::clone(&control_client),
            ))),
    );
    let control_delivery = Arc::new(HostRouteDelivery::new(
        release,
        HostRouteHandlers::Control {
            administration: None,
            control: control_client,
            writer,
            identity: None,
            org: ORG.to_owned(),
        },
        None,
    ));
    let (control, control_task) = serve(
        engine,
        flow_http,
        control_routing,
        control_delivery as Arc<dyn RouteDelivery>,
    )
    .await?;

    let reach = |aud: &str, roles: &[&str]| Reach {
        aud: aud.to_owned(),
        roles: roles.iter().map(|role| (*role).to_owned()).collect(),
    };
    let personas: Personas = Arc::new(HashMap::from([
        (
            "nobody@example.test".to_owned(),
            (MEMBER.to_owned(), vec![]),
        ),
        (
            "member@example.test".to_owned(),
            (
                MEMBER.to_owned(),
                vec![reach(AUDIENCE, &["purchase-reader"])],
            ),
        ),
        (
            "first@example.test".to_owned(),
            (first.clone(), vec![reach(AUDIENCE, &["admin"])]),
        ),
        (
            "boss@example.test".to_owned(),
            (
                boss.as_str().to_owned(),
                vec![reach(&control_audience, &[])],
            ),
        ),
        (
            "cat@example.test".to_owned(),
            (cat.as_str().to_owned(), vec![reach(&control_audience, &[])]),
        ),
    ]));
    let (identity_url, identity_task) = serve_identity(personas).await?;

    // The page, on one origin, and the journey in Chrome.
    let shell = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/shell");
    let port = free_port()?;
    let mut vite = tokio::process::Command::new("pnpm")
        .current_dir(&shell)
        .args([
            "exec",
            "vite",
            "--config",
            "test/browser/vite.config.ts",
            "--port",
        ])
        .arg(port.to_string())
        .env("WAMN_BROWSER_IDENTITY_URL", &identity_url)
        .env("WAMN_BROWSER_APPLICATION_URL", &application)
        .env("WAMN_BROWSER_CONTROL_URL", &control)
        .env("WAMN_BROWSER_ORG", ORG)
        .env("WAMN_BROWSER_PROJECT", PROJECT)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("start the page's dev server")?;
    let url = format!("http://localhost:{port}");
    let mut ready = false;
    for _ in 0..120 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    anyhow::ensure!(ready, "the page's dev server did not listen on {port}");
    let output = tokio::process::Command::new("node")
        .current_dir(&shell)
        .arg("test/browser/journey.mjs")
        .env("WAMN_BROWSER_URL", &url)
        .env("WAMN_BROWSER_AUDIENCE", AUDIENCE)
        .output()
        .await
        .context("run the browser journey")?;
    vite.kill().await.ok();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.matches("ok: ").count() == 5,
        "the browser journey passes every step:\n{stdout}\n{stderr}"
    );

    identity_task.abort();
    control_task.abort();
    application_task.abort();
    keys_server.stop().await;
    host.server.stop().await;
    Ok(())
}
