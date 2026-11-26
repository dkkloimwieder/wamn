//! NATS resolver publication and the permission templates owned by identity.

use std::{path::PathBuf, time::Duration};

use async_nats::{Client, ConnectOptions, ServerAddr};
use http_body_util::Full;
use hyper::{Response, StatusCode, body::Bytes};
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use tokio::sync::OnceCell;
use wamn_identity::identity::nats as key_store;
use wamn_identity::nats::{self, Authority, LimitsPerProject};
use wamn_identity::workload_identity;
use wamn_identity_protocol::nats::{
    Account, ENSURE_ACCOUNT_PATH, EnsureAccount, SIGN_USER_PATH, SignHostUser, SignUser, SignedUser,
};
use wamn_kubernetes::client::KubernetesApi;
use wamn_registry::{OrgId, name};

use crate::{IdentityServiceError, Inner, response, unavailable};

const PLATFORM_NAMESPACE: &str = "platform";
const OPERATOR_SECRET: &str = "wamn-nats-operator";
const RUNTIME_OPERATOR_SECRET: &str = "wamn-nats-runtime-operator";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Settings {
    url: String,
    tls_ca: PathBuf,
    max_projects: i64,
    limits_per_project: LimitsPerProject,
}

pub(super) struct State {
    authority: Authority,
    server: ServerAddr,
    options: ConnectOptions,
    client: OnceCell<Client>,
}

impl State {
    async fn client(&self) -> Result<&Client, async_nats::ConnectError> {
        self.client
            .get_or_try_init(|| self.options.clone().connect(self.server.clone()))
            .await
    }

    async fn lookup_account(&self, account: &str) -> Result<Bytes, IdentityServiceError> {
        let subject = nats::LOOKUP_SUBJECT.replacen('*', account, 1);
        tokio::time::timeout(REQUEST_TIMEOUT, async {
            let client = self
                .client()
                .await
                .map_err(|_| IdentityServiceError::new("connect to NATS resolver failed"))?;
            client
                .request(subject, Bytes::new())
                .await
                .map(|message| message.payload)
                .map_err(|_| IdentityServiceError::new("request NATS account lookup failed"))
        })
        .await
        .map_err(|_| IdentityServiceError::new("NATS account lookup timed out"))?
    }

    async fn resolver_request(
        &self,
        subject: &'static str,
        account: Option<&str>,
        jwt: String,
    ) -> Result<(), IdentityServiceError> {
        let message = tokio::time::timeout(REQUEST_TIMEOUT, async {
            let client = self
                .client()
                .await
                .map_err(|_| IdentityServiceError::new("connect to NATS resolver failed"))?;
            client
                .request(subject, jwt.into())
                .await
                .map_err(|_| IdentityServiceError::new("request NATS account update failed"))
        })
        .await
        .map_err(|_| IdentityServiceError::new("NATS account update timed out"))??;
        let reply: UpdateReply = serde_json::from_slice(&message.payload)
            .map_err(|_| IdentityServiceError::new("decode NATS account update reply failed"))?;
        if reply.data.is_some_and(|data| {
            data.code == 200 && account.is_none_or(|account| data.account == account)
        }) && reply.error.is_none()
        {
            Ok(())
        } else {
            Err(IdentityServiceError::new(
                "NATS resolver refused account update",
            ))
        }
    }
}

#[derive(Deserialize)]
struct UpdateReply {
    data: Option<UpdateStatus>,
    error: Option<serde::de::IgnoredAny>,
}

#[derive(Deserialize)]
struct UpdateStatus {
    #[serde(default)]
    account: String,
    code: u16,
}

pub(super) async fn start(
    db: &mut PgConnection,
    issuer: &str,
    envelope: &wamn_envelope::encryption::Envelope,
    api: Option<&KubernetesApi>,
    settings: Option<Settings>,
) -> Result<Option<State>, IdentityServiceError> {
    let Some(Settings {
        url,
        tls_ca,
        max_projects,
        limits_per_project,
    }) = settings
    else {
        return Ok(None);
    };
    if max_projects < 1 {
        return Err(IdentityServiceError::new(
            "NATS project limit must be positive",
        ));
    }
    let server: ServerAddr = url
        .parse()
        .map_err(|_| IdentityServiceError::new("invalid identity NATS URL"))?;
    if server.username().is_some() || server.password().is_some() {
        return Err(IdentityServiceError::new(
            "identity NATS URL must omit credentials",
        ));
    }
    let api = api.ok_or_else(|| IdentityServiceError::new("NATS bootstrap requires Kubernetes"))?;
    if api.namespace != PLATFORM_NAMESPACE {
        return Err(IdentityServiceError::new(
            "identity NATS bootstrap requires the platform namespace",
        ));
    }
    let bootstrap = key_store::bootstrap(db, issuer, envelope, max_projects, limits_per_project)
        .await
        .map_err(|error| {
            tracing::error!(%error, "NATS bootstrap failed");
            IdentityServiceError::new("bootstrap NATS authority failed")
        })?;
    api.patch_secret_data(
        RUNTIME_OPERATOR_SECRET,
        "operator.creds",
        bootstrap.runtime_operator_creds.as_bytes(),
    )
    .await
    .map_err(|error| {
        tracing::error!(%error, "write runtime-operator NATS Secret failed");
        IdentityServiceError::new("write runtime-operator NATS Secret failed")
    })?;
    api.patch_secret_data(
        OPERATOR_SECRET,
        "operator.conf",
        bootstrap.operator_include.as_bytes(),
    )
    .await
    .map_err(|error| {
        tracing::error!(%error, "write NATS operator Secret failed");
        IdentityServiceError::new("write NATS operator Secret failed")
    })?;
    let options = ConnectOptions::new()
        .require_tls(true)
        .add_root_certificates(tls_ca)
        .custom_inbox_prefix(bootstrap.system_inbox)
        .request_timeout(Some(REQUEST_TIMEOUT))
        .max_reconnects(None)
        .retry_on_initial_connect()
        .credentials(&bootstrap.system_creds)
        .map_err(|_| IdentityServiceError::new("configure identity NATS credentials failed"))?;
    Ok(Some(State {
        authority: bootstrap.authority,
        server,
        options,
        client: OnceCell::new(),
    }))
}

pub(super) async fn respond(
    inner: &Inner,
    client: &mut PgConnection,
    org: &OrgId,
    route: &str,
    body: &[u8],
) -> Response<Full<Bytes>> {
    let Some(state) = &inner.nats else {
        return unavailable();
    };
    match route {
        ENSURE_ACCOUNT_PATH => ensure_account(inner, state, client, org, body).await,
        SIGN_USER_PATH => sign_user(inner, client, org, body).await,
        _ => unreachable!("only NATS routes enter NATS request handling"),
    }
}

async fn ensure_account(
    inner: &Inner,
    state: &State,
    client: &mut PgConnection,
    org: &OrgId,
    body: &[u8],
) -> Response<Full<Bytes>> {
    let Ok(request) = serde_json::from_slice::<EnsureAccount>(body) else {
        return invalid();
    };
    if &request.org != org {
        return super::users::forbidden();
    }
    let Ok(count) = i32::try_from(request.project_count) else {
        return invalid();
    };
    if count == 0 {
        let deleted =
            match key_store::retire_env(&state.authority, client, &inner.issuer, org, &request.env)
                .await
            {
                Ok(None) => {
                    return json(&Account {
                        public_key: String::new(),
                    });
                }
                Ok(Some(deleted)) => deleted,
                Err(error) => return signing_error(&error),
            };
        let (public_key, jwt) = deleted;
        if let Err(error) = state
            .resolver_request(nats::DELETE_SUBJECT, None, jwt)
            .await
        {
            tracing::warn!(%error, "retire NATS environment account failed");
            return unavailable();
        }
        return json(&Account { public_key });
    }
    let (public_key, jwt) = match key_store::ensure_env(
        &state.authority,
        client,
        &inner.envelope,
        &inner.issuer,
        (org, &request.env),
        count,
        async |account| state.lookup_account(account).await,
    )
    .await
    {
        Ok(value) => value,
        Err(error) if error.is_project_limit() => {
            return response(
                StatusCode::CONFLICT,
                "application/json",
                br#"{"error":"platform NATS project limit exceeded"}"#.to_vec(),
            );
        }
        Err(error) => return signing_error(&error),
    };
    if let Err(error) = state
        .resolver_request(nats::UPDATE_SUBJECT, Some(&public_key), jwt)
        .await
    {
        tracing::warn!(%error, "publish NATS environment account failed");
        return unavailable();
    }
    json(&Account { public_key })
}

async fn sign_user(
    inner: &Inner,
    client: &mut PgConnection,
    org: &OrgId,
    body: &[u8],
) -> Response<Full<Bytes>> {
    let Ok(request) = serde_json::from_slice::<SignUser>(body) else {
        return invalid();
    };
    if &request.org != org {
        return super::users::forbidden();
    }
    if !nats::is_user_public_key(&request.public_key) {
        return invalid();
    }
    let inbox_prefix = name::nats_user_inbox(&request.public_key);
    let (name, permissions) = workload_identity::env_permissions(&request, &inbox_prefix);
    let user = nats::User {
        public_key: &request.public_key,
        name: &name,
        permissions: &permissions,
    };
    let signed = key_store::env_user(
        client,
        &inner.envelope,
        &inner.issuer,
        org,
        &request.env,
        &user,
    )
    .await;
    match signed {
        Ok(Some(jwt)) => json(&SignedUser { jwt, inbox_prefix }),
        Ok(None) => response(
            StatusCode::NOT_FOUND,
            "application/json",
            br#"{"error":"NATS environment account is absent"}"#.to_vec(),
        ),
        Err(error) => signing_error(&error),
    }
}

pub(super) fn sign_host_user(inner: &Inner, pod: &str, body: &[u8]) -> Response<Full<Bytes>> {
    let Some(state) = &inner.nats else {
        return unavailable();
    };
    let Ok(request) = serde_json::from_slice::<SignHostUser>(body) else {
        return invalid();
    };
    if !nats::is_user_public_key(&request.public_key) {
        return invalid();
    }
    let inbox_prefix = name::nats_user_inbox(&request.public_key);
    let permissions = workload_identity::host_permissions(pod, &inbox_prefix);
    let user = nats::User {
        public_key: &request.public_key,
        name: pod,
        permissions: &permissions,
    };
    match state.authority.platform_user(&user) {
        Ok(jwt) => json(&SignedUser { jwt, inbox_prefix }),
        Err(error) => signing_error(&error),
    }
}

fn signing_error(error: &nats::Error) -> Response<Full<Bytes>> {
    if error.is_invalid_user_key() {
        return invalid();
    }
    tracing::error!(%error, "sign NATS claims failed");
    unavailable()
}

fn invalid() -> Response<Full<Bytes>> {
    response(
        StatusCode::BAD_REQUEST,
        "application/json",
        br#"{"error":"invalid NATS signing request"}"#.to_vec(),
    )
}

fn json(body: &impl Serialize) -> Response<Full<Bytes>> {
    serde_json::to_vec(body).map_or_else(
        |_| unavailable(),
        |body| response(StatusCode::OK, "application/json", body),
    )
}
