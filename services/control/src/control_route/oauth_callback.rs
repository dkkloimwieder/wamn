//! Public control callback pages. The pending state supplies all authority.

use sqlx::Connection as _;
use wamn_http::body::{HttpBody, oauth_callback_page as page};

use super::oauth_authorization::{Authorization, callback_target};

/// Answer only the exact callback path and GET method, without a session.
pub async fn response(
    oauth: &Authorization,
    logins: &crate::project_login::ProjectLogins,
    request: hyper::Request<HttpBody>,
) -> hyper::Response<HttpBody> {
    if request.uri().path() != "/oauth/callback" {
        return page(404, "OAuth callback not found");
    }
    if request.method() != hyper::Method::GET {
        return page(405, "OAuth callback requires GET");
    }
    let query = request.uri().query().unwrap_or_default();
    let (project, env) = match callback_target(query) {
        Ok(target) => target,
        Err(error) => return page(400, &error.reason),
    };
    let Ok(login) = logins.options(&project, &env).await else {
        return page(503, "callback database unavailable");
    };
    let Ok(namespace) = logins.namespace(&env).await else {
        return page(503, "callback database unavailable");
    };
    let local;
    let envelope = if let Some(envelope) = oauth.shared_envelope() {
        envelope
    } else {
        let Ok(key) = logins.oauth_key(&namespace).await else {
            return page(503, "callback key unavailable");
        };
        local = key;
        &local
    };
    let Ok(database) = sqlx::PgConnection::connect_with(&login).await else {
        return page(503, "callback database unavailable");
    };
    let result = oauth
        .callback(&login, database, envelope, &namespace, query)
        .await;
    match result {
        Ok(()) => page(200, "Authorization succeeded. You can close this page."),
        Err(error) => page(400, &error.reason),
    }
}
