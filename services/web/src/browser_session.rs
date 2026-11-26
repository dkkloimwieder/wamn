//! Own the browser credentials at the web proxy boundary.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, ensure};
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::header::{
    AUTHORIZATION, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE, COOKIE, HOST, HeaderValue, ORIGIN,
    SET_COOKIE,
};
use hyper::http::uri::Authority;
use hyper::{HeaderMap, Method, Request, Response, StatusCode};
use serde::{Deserialize, Serialize};

use crate::{ProxyBody, boxed, remove_hop_headers};

const SESSION: &str = "__Host-wamn-session";
const RENEWAL: &str = "__Host-wamn-renewal";
const SEC_FETCH_SITE: &str = "sec-fetch-site";
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

fn allows_session_write(headers: &HeaderMap) -> bool {
    let mut sites = headers.get_all(SEC_FETCH_SITE).iter();
    let Some(site) = sites.next() else {
        return same_origin(headers);
    };
    sites.next().is_none() && matches!(site.as_bytes(), b"same-origin" | b"none")
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
        && cookies.session.is_some()
        && !allows_session_write(&parts.headers)
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
    age: u64,
) -> anyhow::Result<()> {
    for (name, token) in [(SESSION, session), (RENEWAL, renewal)] {
        ensure!(
            token
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte)),
            "invalid identity cookie value for {name}"
        );
        let mut value = HeaderValue::try_from(format!(
            "{name}={token}; Path=/; Max-Age={age}; Secure; SameSite=Strict; HttpOnly"
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
                    && !issued.access_token.is_empty()
                    && !issued.renewal_token.is_empty(),
                "identity returned an unusable session"
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
                issued.login_expires_at - now,
            )?;
            Ok(boxed(reply))
        }
        (Some(PasswordRoute::Logout), StatusCode::NO_CONTENT) => {
            set_cookies(response.headers_mut(), "", "", 0)?;
            Ok(response.map(BodyExt::boxed_unsync))
        }
        _ => Ok(response.map(BodyExt::boxed_unsync)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;
    use std::convert::Infallible;

    const HOST_NAME: &str = "shop--acme.example.test";
    const ACCESS_TOKEN: &str = "header.payload.signature";

    /// Pass an HTTP request through Hyper without opening a socket.
    async fn exchange<F, A>(request: hyper::http::request::Builder, answer: F) -> Response<Incoming>
    where
        F: Fn(Request<Incoming>) -> A + Send + 'static,
        A: std::future::Future<Output = Response<Full<bytes::Bytes>>> + Send + 'static,
    {
        let (client, server) = tokio::io::duplex(4096);
        let service = service_fn(move |request| {
            let reply = answer(request);
            async move { Ok::<_, Infallible>(reply.await) }
        });
        tokio::spawn(async move {
            hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(server), service)
                .await
                .unwrap();
        });
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(client))
            .await
            .unwrap();
        tokio::spawn(connection);
        let request = request
            .header(HOST, HOST_NAME)
            .body(Full::new(bytes::Bytes::new()))
            .unwrap();
        sender.send_request(request).await.unwrap()
    }

    /// The headers that web forwards for `request` to `path`, or its refusal.
    async fn forwarded(
        path: &'static str,
        request: hyper::http::request::Builder,
    ) -> Result<HeaderMap, StatusCode> {
        let (sender, mut received) = tokio::sync::mpsc::unbounded_channel();
        exchange(request.uri(path), move |request| {
            let sender = sender.clone();
            async move {
                let route = password_route(&request);
                let password = path.starts_with("/password/");
                let forwarded = forward(request, route, password)
                    .await
                    .map(|request| request.into_parts().0.headers);
                sender.send(forwarded).unwrap();
                Response::new(Full::default())
            }
        })
        .await;
        received.recv().await.unwrap()
    }

    /// The browser reply to a successful identity response for `route`.
    async fn session_reply(route: PasswordRoute) -> Response<ProxyBody> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let (status, body) = if route == PasswordRoute::Logout {
            (StatusCode::NO_CONTENT, String::new())
        } else {
            let body = serde_json::json!({
                "sub": "subject", "expires_at": now + 900, "login_expires_at": now + 3600,
                "access_token": ACCESS_TOKEN, "renewal_token": "renewal",
            });
            (StatusCode::OK, body.to_string())
        };
        let response = exchange(Request::builder().method(Method::POST), move |_| {
            let body = body.clone();
            async move {
                Response::builder()
                    .status(status)
                    .body(Full::new(body.into()))
                    .unwrap()
            }
        })
        .await;
        respond(Some(route), response).await.unwrap()
    }

    fn set_cookie<'a>(reply: &'a Response<ProxyBody>, name: &str) -> &'a str {
        reply
            .headers()
            .get_all(SET_COOKIE)
            .iter()
            .map(|value| value.to_str().unwrap())
            .find(|cookie| cookie.starts_with(&format!("{name}=")))
            .unwrap()
    }

    #[tokio::test]
    async fn login_and_renewal_set_two_secure_cookies_and_logout_clears_them() {
        for route in [
            PasswordRoute::Login,
            PasswordRoute::Renew,
            PasswordRoute::Logout,
        ] {
            let reply = session_reply(route).await;
            assert_eq!(reply.headers().get_all(SET_COOKIE).iter().count(), 2);
            for (name, token) in [
                ("__Host-wamn-session", ACCESS_TOKEN),
                ("__Host-wamn-renewal", "renewal"),
            ] {
                let cookie = set_cookie(&reply, name);
                let (pair, attributes) = cookie.split_once("; ").unwrap();
                let attributes: Vec<_> = attributes.split("; ").collect();
                for attribute in ["Secure", "Path=/", "HttpOnly", "SameSite=Strict"] {
                    assert!(attributes.contains(&attribute), "{cookie}");
                }
                if route == PasswordRoute::Logout {
                    assert_eq!(pair, format!("{name}="));
                    assert!(attributes.contains(&"Max-Age=0"), "{cookie}");
                } else {
                    assert_eq!(pair, format!("{name}={token}"));
                }
            }
            let body = reply.into_body().collect().await.unwrap().to_bytes();
            assert!(!String::from_utf8_lossy(&body).contains(ACCESS_TOKEN));
        }
    }

    #[tokio::test]
    async fn unsafe_session_requests_use_fetch_site_or_fall_back_to_origin() {
        let origin = format!("https://{HOST_NAME}");
        let other_origin = "https://other.example.test";
        for (sites, origin, allowed) in [
            (&["same-origin"][..], None, true),
            (&["none"][..], Some(other_origin), true),
            (&["same-site"][..], Some(origin.as_str()), false),
            (&["cross-site"][..], None, false),
            (&["unknown"][..], Some(origin.as_str()), false),
            (
                &["same-origin", "same-origin"][..],
                Some(origin.as_str()),
                false,
            ),
            (&["same-origin, none"][..], Some(origin.as_str()), false),
            (&[][..], Some(origin.as_str()), true),
            (&[][..], Some(other_origin), false),
            (&[][..], None, false),
        ] {
            let mut request = Request::builder()
                .method(Method::POST)
                .header(COOKIE, format!("{SESSION}={ACCESS_TOKEN}; other=1"));
            for site in sites {
                request = request.header("Sec-Fetch-Site", *site);
            }
            if let Some(origin) = origin {
                request = request.header(ORIGIN, origin);
            }
            let result = forwarded("/api/shop/x", request).await;
            if allowed {
                let headers = result.unwrap();
                assert_eq!(
                    headers[AUTHORIZATION],
                    format!("Bearer {ACCESS_TOKEN}").as_str()
                );
                assert_eq!(headers[COOKIE], "other=1");
            } else {
                assert_eq!(
                    result.unwrap_err(),
                    StatusCode::FORBIDDEN,
                    "{sites:?} {origin:?}"
                );
            }
        }
    }

    #[tokio::test]
    async fn safe_session_and_bearer_only_requests_need_no_origin_headers() {
        let headers = forwarded(
            "/api/shop/x",
            Request::builder()
                .method(Method::GET)
                .header(COOKIE, format!("{SESSION}={ACCESS_TOKEN}")),
        )
        .await
        .unwrap();
        assert_eq!(
            headers[AUTHORIZATION],
            format!("Bearer {ACCESS_TOKEN}").as_str()
        );
        let headers = forwarded(
            "/api/shop/x",
            Request::builder()
                .method(Method::POST)
                .header(AUTHORIZATION, "Bearer caller-token"),
        )
        .await
        .unwrap();
        assert_eq!(headers[AUTHORIZATION], "Bearer caller-token");

        let mixed = forwarded(
            "/api/shop/x",
            Request::builder()
                .method(Method::POST)
                .header(COOKIE, format!("{SESSION}={ACCESS_TOKEN}"))
                .header(AUTHORIZATION, "Bearer caller-token"),
        )
        .await;
        assert_eq!(mixed.unwrap_err(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn login_and_bootstrap_routes_require_a_same_origin_origin() {
        for path in [
            "/password/session",
            "/password/enroll",
            "/password/recover",
            "/password/reset",
        ] {
            let request = |origin: Option<String>| {
                let request = Request::builder()
                    .method(Method::POST)
                    .header(COOKIE, format!("{SESSION}={ACCESS_TOKEN}"))
                    .header("Sec-Fetch-Site", "same-origin");
                match origin {
                    Some(origin) => request.header(ORIGIN, origin),
                    None => request,
                }
            };
            forwarded(path, request(Some(format!("https://{HOST_NAME}"))))
                .await
                .unwrap();
            for origin in [
                None,
                Some(format!("http://{HOST_NAME}")),
                Some("https://other.example.test".to_owned()),
            ] {
                assert_eq!(
                    forwarded(path, request(origin.clone())).await.unwrap_err(),
                    StatusCode::FORBIDDEN,
                    "{path} {origin:?}"
                );
            }
        }
    }
}
