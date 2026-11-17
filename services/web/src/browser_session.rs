//! Own the browser credentials at the web proxy boundary.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, ensure};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::header::{
    AUTHORIZATION, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE, COOKIE, HOST, HeaderValue, ORIGIN,
    SET_COOKIE,
};
use hyper::http::uri::Authority;
use hyper::{HeaderMap, Method, Request, Response, StatusCode};
use ring::{hmac, rand};
use serde::{Deserialize, Serialize};

use crate::{ProxyBody, boxed, remove_hop_headers};

const SESSION: &str = "__Host-wamn-session";
const RENEWAL: &str = "__Host-wamn-renewal";
const CSRF: &str = "__Host-wamn-web-csrf";
const CSRF_HEADER: &str = "x-wamn-csrf";
const NONCE_BYTES: usize = 32;
const MAX_PASSWORD_BODY_BYTES: usize = 8192;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PasswordRoute {
    Login,
    Bootstrap,
    Renew,
    Logout,
}

pub(super) fn password_route(request: &Request<Incoming>) -> Option<PasswordRoute> {
    if request.method() != Method::POST || request.uri().query().is_some() {
        return None;
    }
    match request.uri().path() {
        "/password/session" => Some(PasswordRoute::Login),
        "/password/enroll" | "/password/recover" | "/password/reset" => {
            Some(PasswordRoute::Bootstrap)
        }
        "/password/renew" => Some(PasswordRoute::Renew),
        "/password/logout" => Some(PasswordRoute::Logout),
        _ => None,
    }
}

#[derive(Default)]
struct Cookies {
    session: Option<String>,
    renewal: Option<String>,
    csrf: Option<String>,
}

fn take_cookies(headers: &mut HeaderMap) -> Result<Cookies, StatusCode> {
    let mut found = Cookies::default();
    let mut retained = String::new();
    for header in headers.get_all(COOKIE) {
        let pairs = header.to_str().map_err(|_| StatusCode::BAD_REQUEST)?;
        for (name, value) in pairs
            .split(';')
            .map(str::trim)
            .filter_map(|pair| pair.split_once('='))
        {
            let field = match name.trim_end() {
                SESSION => &mut found.session,
                RENEWAL => &mut found.renewal,
                CSRF => &mut found.csrf,
                _ => {
                    if !retained.is_empty() {
                        retained.push_str("; ");
                    }
                    retained.push_str(name);
                    retained.push('=');
                    retained.push_str(value);
                    continue;
                }
            };
            if field.is_some() {
                return Err(StatusCode::UNAUTHORIZED);
            }
            *field = Some(value.to_owned());
        }
    }
    headers.remove(COOKIE);
    if !retained.is_empty() {
        let value = HeaderValue::try_from(retained).map_err(|_| StatusCode::BAD_REQUEST)?;
        headers.insert(COOKIE, value);
    }
    Ok(found)
}

fn same_origin(headers: &HeaderMap) -> bool {
    let mut hosts = headers.get_all(HOST).iter();
    let mut origins = headers.get_all(ORIGIN).iter();
    let host = hosts
        .next()
        .and_then(|value| value.to_str().ok())
        .and_then(|authority| authority.parse::<Authority>().ok());
    let origin = origins
        .next()
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("https://"))
        .and_then(|authority| authority.parse::<Authority>().ok());
    hosts.next().is_none()
        && origins.next().is_none()
        && host.zip(origin).is_some_and(|(host, origin)| {
            let host = host.as_str();
            let origin = origin.as_str();
            !host.contains('@')
                && !origin.contains('@')
                && origin
                    .strip_suffix(":443")
                    .unwrap_or(origin)
                    .eq_ignore_ascii_case(host.strip_suffix(":443").unwrap_or(host))
        })
}

#[derive(Deserialize)]
struct Claims<'a> {
    jti: &'a str,
}

fn csrf_message(session: &str, nonce: &[u8; NONCE_BYTES]) -> Option<Vec<u8>> {
    let (header, rest) = session.split_once('.')?;
    let (payload, signature) = rest.split_once('.')?;
    if header.is_empty() || signature.is_empty() || signature.contains('.') {
        return None;
    }
    let decoded = URL_SAFE_NO_PAD.decode(payload).ok()?;
    // The JWT payload supplies only a binding for the CSRF MAC. The upstream
    // verifier owns access-token admission.
    let Claims { jti } = serde_json::from_slice(&decoded).ok()?;
    if jti.is_empty() {
        return None;
    }
    let mut message = Vec::with_capacity(jti.len() + nonce.len());
    message.extend_from_slice(jti.as_bytes());
    message.extend_from_slice(nonce);
    Some(message)
}

fn valid_csrf(headers: &HeaderMap, key: &hmac::Key, session: &str, cookie: Option<&str>) -> bool {
    let mut values = headers.get_all(CSRF_HEADER).iter();
    let Some((cookie, header)) = cookie.zip(values.next()) else {
        return false;
    };
    if values.next().is_some() || header.as_bytes() != cookie.as_bytes() {
        return false;
    }
    let Some((encoded_nonce, encoded_mac)) = cookie.split_once('.') else {
        return false;
    };
    let mut nonce = [0; NONCE_BYTES];
    let mut mac = [0; 32];
    matches!(
        URL_SAFE_NO_PAD.decode_slice(encoded_nonce, &mut nonce),
        Ok(NONCE_BYTES)
    ) && matches!(URL_SAFE_NO_PAD.decode_slice(encoded_mac, &mut mac), Ok(32))
        && csrf_message(session, &nonce)
            .is_some_and(|message| hmac::verify(key, &message, &mac).is_ok())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserRenewal<'a> {
    aud: &'a str,
}

#[derive(Serialize)]
struct IdentityRenewal<'a> {
    aud: &'a str,
    renewal_token: &'a str,
}

pub(super) async fn forward(
    request: Request<Incoming>,
    key: &hmac::Key,
    route: Option<PasswordRoute>,
    password_request: bool,
) -> Result<Request<ProxyBody>, StatusCode> {
    let (mut parts, body) = request.into_parts();
    let cookies = take_cookies(&mut parts.headers)?;
    if cookies.session.is_some() && parts.headers.contains_key(AUTHORIZATION) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    if matches!(route, Some(PasswordRoute::Login | PasswordRoute::Bootstrap)) {
        if !same_origin(&parts.headers) {
            return Err(StatusCode::FORBIDDEN);
        }
    } else if !parts.method.is_safe()
        && let Some(session) = &cookies.session
        && !valid_csrf(&parts.headers, key, session, cookies.csrf.as_deref())
    {
        return Err(StatusCode::FORBIDDEN);
    }
    let renewal = if matches!(route, Some(PasswordRoute::Renew | PasswordRoute::Logout)) {
        match (cookies.session.as_ref(), cookies.renewal.as_deref()) {
            (Some(_), Some(renewal)) => Some(renewal),
            (None, None) if route == Some(PasswordRoute::Logout) => {
                return Err(StatusCode::NO_CONTENT);
            }
            _ => return Err(StatusCode::UNAUTHORIZED),
        }
    } else {
        None
    };

    remove_hop_headers(&mut parts.headers);
    parts.headers.remove(CSRF_HEADER);
    if password_request {
        parts.headers.remove(COOKIE);
    }
    if let Some(session) = &cookies.session {
        let mut value = HeaderValue::try_from(format!("Bearer {session}"))
            .map_err(|_| StatusCode::UNAUTHORIZED)?;
        value.set_sensitive(true);
        parts.headers.insert(AUTHORIZATION, value);
    }

    let body = if let Some(renewal_token) = renewal {
        let bytes = Limited::new(body, MAX_PASSWORD_BODY_BYTES)
            .collect()
            .await
            .map_err(|_| StatusCode::BAD_REQUEST)?
            .to_bytes();
        let BrowserRenewal { aud } =
            serde_json::from_slice(&bytes).map_err(|_| StatusCode::BAD_REQUEST)?;
        let body = serde_json::to_vec(&IdentityRenewal { aud, renewal_token })
            .map_err(|_| StatusCode::BAD_REQUEST)?;
        parts.headers.remove(CONTENT_LENGTH);
        parts
            .headers
            .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        Full::new(body.into())
            .map_err(|never| match never {})
            .boxed_unsync()
    } else {
        body.boxed_unsync()
    };
    Ok(Request::from_parts(parts, body))
}

#[derive(Serialize)]
struct SessionTimes<'a> {
    sub: &'a str,
    expires_at: u64,
    login_expires_at: u64,
}

#[derive(Deserialize)]
struct IdentitySession<'a> {
    sub: &'a str,
    expires_at: u64,
    login_expires_at: u64,
    access_token: &'a str,
    renewal_token: &'a str,
}

fn set_cookies(
    headers: &mut HeaderMap,
    session: &str,
    renewal: &str,
    csrf: &str,
    age: u64,
) -> anyhow::Result<()> {
    for (name, token, http_only) in [
        (SESSION, session, "; HttpOnly"),
        (RENEWAL, renewal, "; HttpOnly"),
        (CSRF, csrf, ""),
    ] {
        ensure!(
            token
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte)),
            "invalid identity cookie value for {name}"
        );
        let mut value = HeaderValue::try_from(format!(
            "{name}={token}; Path=/; Max-Age={age}; Secure; SameSite=Strict{http_only}"
        ))
        .with_context(|| format!("encode {name} cookie"))?;
        value.set_sensitive(true);
        headers.append(SET_COOKIE, value);
    }
    Ok(())
}

pub(super) async fn respond(
    route: Option<PasswordRoute>,
    mut response: Response<Incoming>,
    key: &hmac::Key,
) -> anyhow::Result<Response<ProxyBody>> {
    response.headers_mut().remove(SET_COOKIE);
    match (route, response.status()) {
        (Some(PasswordRoute::Login | PasswordRoute::Renew), StatusCode::OK) => {
            let bytes = Limited::new(response.into_body(), MAX_PASSWORD_BODY_BYTES)
                .collect()
                .await
                .map_err(anyhow::Error::from_boxed)
                .context("read identity session response")?
                .to_bytes();
            let issued: IdentitySession<'_> =
                serde_json::from_slice(&bytes).context("decode identity session response")?;
            let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
            ensure!(
                issued.login_expires_at > now
                    && issued.login_expires_at >= issued.expires_at
                    && !issued.renewal_token.is_empty(),
                "identity returned an unusable session"
            );
            let mut nonce = [0_u8; NONCE_BYTES];
            rand::SecureRandom::fill(&rand::SystemRandom::new(), &mut nonce)
                .map_err(|_| anyhow::anyhow!("generate browser CSRF nonce"))?;
            let message = csrf_message(issued.access_token, &nonce)
                .context("identity access token has no session ID")?;
            let mac = hmac::sign(key, &message);
            let csrf = format!(
                "{}.{}",
                URL_SAFE_NO_PAD.encode(nonce),
                URL_SAFE_NO_PAD.encode(mac)
            );
            let body = serde_json::to_vec(&SessionTimes {
                sub: issued.sub,
                expires_at: issued.expires_at,
                login_expires_at: issued.login_expires_at,
            })
            .context("encode browser session times")?;
            let mut reply = Response::builder()
                .header(CONTENT_TYPE, "application/json")
                .header(CACHE_CONTROL, "no-store")
                .body(Full::new(body.into()))?;
            set_cookies(
                reply.headers_mut(),
                issued.access_token,
                issued.renewal_token,
                &csrf,
                issued.login_expires_at - now,
            )?;
            Ok(boxed(reply))
        }
        (Some(PasswordRoute::Logout), StatusCode::NO_CONTENT) => {
            set_cookies(response.headers_mut(), "", "", "", 0)?;
            Ok(response.map(BodyExt::boxed_unsync))
        }
        _ => Ok(response.map(BodyExt::boxed_unsync)),
    }
}
