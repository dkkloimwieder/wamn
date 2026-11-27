//! NATS resolver publication and the permission templates owned by identity.

use std::{path::PathBuf, sync::Weak, time::Duration};

use async_nats::{Client, ConnectOptions, ServerAddr};
use http_body_util::Full;
use hyper::{Response, StatusCode, body::Bytes};
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use tokio::sync::OnceCell;
use wamn_identity::authentication_event::{Event, Outcome};
use wamn_identity::identity::nats as key_store;
use wamn_identity::nats::{self, Authority};
use wamn_identity::workload_identity;
use wamn_identity_protocol::nats::{
    Account, EnsureAccount, RefreshAccount, SignHostUser, SignUser, SignedUser, UserPurpose,
};
use wamn_kubernetes::client::{KubernetesApi, ServiceAccountIdentity};
use wamn_registry::{EnvName, OrgId, name};

use crate::{IdentityServiceError, Inner, response, unavailable};

const PLATFORM_NAMESPACE: &str = "platform";
const OPERATOR_SECRET: &str = "wamn-nats-operator";
const RUNTIME_OPERATOR_SECRET: &str = "wamn-nats-runtime-operator";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const REFRESH_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Settings {
    url: String,
    tls_ca: PathBuf,
    file_store_bytes: i64,
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

    async fn request(
        &self,
        subject: String,
        body: &'static [u8],
    ) -> Result<Bytes, IdentityServiceError> {
        tokio::time::timeout(REQUEST_TIMEOUT, async {
            let client = self
                .client()
                .await
                .map_err(|_| IdentityServiceError::new("connect to NATS resolver failed"))?;
            client
                .request(subject, Bytes::from_static(body))
                .await
                .map(|message| message.payload)
                .map_err(|_| IdentityServiceError::new("request NATS account information failed"))
        })
        .await
        .map_err(|_| IdentityServiceError::new("NATS account information request timed out"))?
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
        file_store_bytes,
    }) = settings
    else {
        return Ok(None);
    };
    if file_store_bytes < 1 {
        return Err(IdentityServiceError::new(
            "NATS file store capacity must be positive",
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
    let bootstrap = key_store::bootstrap(db, issuer, envelope, file_store_bytes)
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

pub(super) async fn ensure_account(
    inner: &Inner,
    mut client: PgConnection,
    mapping: &workload_identity::Env,
    account: &str,
    body: &[u8],
    event: &mut Event,
) -> Response<Full<Bytes>> {
    let Some(state) = &inner.nats else {
        return unavailable();
    };
    let Ok(request) = serde_json::from_slice::<EnsureAccount>(body) else {
        event.outcome = Some(Outcome::RequestInvalid);
        return invalid();
    };
    if !workload_identity::account_allowed(mapping, account, &request.org, &request.env) {
        return scope_refused(event);
    }
    let Ok(count) = i32::try_from(request.project_count) else {
        event.outcome = Some(Outcome::RequestInvalid);
        return invalid();
    };
    // An inactive project can still have a retained deployment and streams.
    let deployed: Result<i64, _> = sqlx::query_scalar(
        "SELECT count(*) FROM registry.project_env WHERE org = $1 AND env = $2 AND active",
    )
    .bind(mapping.org.as_str())
    .bind(mapping.env.as_str())
    .fetch_one(&mut client)
    .await;
    match deployed {
        Ok(deployed) if deployed == i64::from(count) => (),
        Ok(_) => return scope_refused(event),
        Err(error) => {
            tracing::warn!(%error, "read NATS project allocation failed");
            return unavailable();
        }
    }
    match publish_account(
        inner,
        state,
        client,
        &mapping.org,
        &mapping.env,
        Some(count),
    )
    .await
    {
        Ok(Some(public_key)) => json(&Account { public_key }),
        Ok(None) if count == 0 => json(&Account {
            public_key: String::new(),
        }),
        Ok(None) => unavailable(),
        Err(error) if error.is_scope_refused() => scope_refused(event),
        Err(error) if error.is_limit_refused() => {
            tracing::warn!(error = ?error, "NATS environment limits refused");
            response(
                StatusCode::CONFLICT,
                "application/json",
                br#"{"error":"NATS environment limits conflict with usage or store capacity"}"#
                    .to_vec(),
            )
        }
        Err(error) => signing_error(&error),
    }
}

pub(super) async fn refresh_account(
    inner: &Inner,
    client: PgConnection,
    org: &OrgId,
    body: &[u8],
) -> Response<Full<Bytes>> {
    let Some(state) = &inner.nats else {
        return unavailable();
    };
    let Ok(request) = serde_json::from_slice::<RefreshAccount>(body) else {
        return invalid();
    };
    match publish_account(inner, state, client, org, &request.env, None).await {
        Ok(_) => response(StatusCode::NO_CONTENT, "application/json", Vec::new()),
        Err(error) => signing_error(&error),
    }
}

pub(super) fn scope_refused(event: &mut Event) -> Response<Full<Bytes>> {
    event.outcome = Some(Outcome::EnvMismatch);
    super::users::forbidden()
}

async fn publish_account(
    inner: &Inner,
    state: &State,
    client: PgConnection,
    org: &OrgId,
    env: &EnvName,
    project_count: Option<i32>,
) -> Result<Option<String>, nats::Error> {
    key_store::ensure_env(
        &state.authority,
        client,
        &inner.envelope,
        (&inner.issuer, org, env),
        project_count,
        |subject, body| state.request(subject, body),
        async |account, jwt| {
            state
                .resolver_request(nats::UPDATE_SUBJECT, Some(account), jwt)
                .await
        },
    )
    .await
}

/// Apply operator SQL edits and retry pending account publications.
pub(super) async fn refresh_accounts(inner: Weak<Inner>) {
    loop {
        tokio::time::sleep(REFRESH_INTERVAL).await;
        let Some(inner) = inner.upgrade() else { return };
        let Some(state) = inner.nats.as_ref() else {
            return;
        };
        if let Err(error) = refresh_pending(&inner, state).await {
            tracing::warn!(error = ?error, "scan NATS account changes failed");
        }
    }
}

async fn refresh_pending(inner: &Inner, state: &State) -> Result<(), nats::Error> {
    let _slot = inner
        .request_slots
        .acquire()
        .await
        .map_err(|source| nats::Error::new("acquire NATS refresh slot", source))?;
    let connect = async || {
        crate::connect_database(&inner.connection)
            .await
            .map_err(|source| nats::Error::new("connect for NATS limit refresh", source))
    };
    let pending = {
        let mut db = inner.pool.acquire().await.map_err(|source| {
            nats::Error::new("acquire connection for NATS refresh scan", source)
        })?;
        key_store::accounts_to_refresh(&mut db, &inner.issuer).await?
    };
    for (org, env) in pending {
        let org = org
            .parse()
            .map_err(|source| nats::Error::new("read NATS account org", source))?;
        let env = env
            .parse()
            .map_err(|source| nats::Error::new("read NATS account env", source))?;
        if let Err(error) =
            publish_account(inner, state, connect().await?.client, &org, &env, None).await
        {
            tracing::warn!(%org, %env, error = ?error, "refresh NATS environment limits failed");
        }
    }
    Ok(())
}

pub(super) async fn sign_user(
    inner: &Inner,
    mut client: PgConnection,
    api: &KubernetesApi,
    mapping: &workload_identity::Env,
    account: &ServiceAccountIdentity,
    body: &[u8],
    event: &mut Event,
) -> Response<Full<Bytes>> {
    if inner.nats.is_none() {
        return unavailable();
    }
    let Ok(request) = serde_json::from_slice::<SignUser>(body) else {
        event.outcome = Some(Outcome::RequestInvalid);
        return invalid();
    };
    let project_active = if let Some(project) = workload_identity::project(&request.purpose) {
        match workload_identity::project_active(&mut client, mapping, project).await {
            Ok(active) => active,
            Err(error) => {
                tracing::warn!(%error, "read NATS workload project failed");
                return unavailable();
            }
        }
    } else {
        false
    };
    if !workload_identity::user_allowed(mapping, &account.name, &request, project_active) {
        return scope_refused(event);
    }
    if !nats::is_user_public_key(&request.public_key) {
        event.outcome = Some(Outcome::PublicKeyInvalid);
        return invalid();
    }
    let expires_at = match job_expiry(api, account, &request).await {
        Ok(Some(expiry)) => expiry,
        Ok(None) => return scope_refused(event),
        Err(error) => {
            tracing::warn!(%error, "read NATS workload Job failed");
            return unavailable();
        }
    };
    let inbox_prefix = name::nats_user_inbox(&request.public_key);
    let (name, permissions) = workload_identity::env_permissions(&request, &inbox_prefix);
    let user = nats::User {
        public_key: &request.public_key,
        name: &name,
        permissions: &permissions,
        expires_at,
    };
    let signed = key_store::env_user(
        client,
        &inner.envelope,
        &inner.issuer,
        (&account.namespace, &account.name),
        &request,
        &user,
    )
    .await;
    match signed {
        Ok(Some(jwt)) => signed_user(jwt, inbox_prefix, event),
        Ok(None) => {
            event.outcome = Some(Outcome::NatsAccountMissing);
            response(
                StatusCode::NOT_FOUND,
                "application/json",
                br#"{"error":"NATS environment account is absent"}"#.to_vec(),
            )
        }
        Err(error) if error.is_scope_refused() => scope_refused(event),
        Err(error) => credential_signing_error(&error, event),
    }
}

async fn job_expiry(
    api: &KubernetesApi,
    account: &ServiceAccountIdentity,
    request: &SignUser,
) -> Result<Option<Option<i64>>, wamn_kubernetes::Error> {
    let expiry = match &request.purpose {
        UserPurpose::Recovery { project } => {
            let coordinate = name::project_env_resource_name(&request.org, project, &request.env);
            wamn_kubernetes::job::credential_expiry(api, account, "recover", Some(&coordinate))
                .await?
        }
        UserPurpose::StreamAdmin => {
            wamn_kubernetes::job::credential_expiry(api, account, "env", None).await?
        }
        _ => return Ok(Some(None)),
    };
    Ok(expiry.map(Some))
}

pub(super) fn sign_host_user(
    inner: &Inner,
    pod: &str,
    body: &[u8],
    event: &mut Event,
) -> Response<Full<Bytes>> {
    let Some(state) = &inner.nats else {
        return unavailable();
    };
    let Ok(request) = serde_json::from_slice::<SignHostUser>(body) else {
        event.outcome = Some(Outcome::RequestInvalid);
        return invalid();
    };
    if !nats::is_user_public_key(&request.public_key) {
        event.outcome = Some(Outcome::PublicKeyInvalid);
        return invalid();
    }
    let inbox_prefix = name::nats_user_inbox(&request.public_key);
    let permissions = workload_identity::host_permissions(pod, &inbox_prefix);
    let user = nats::User {
        public_key: &request.public_key,
        name: pod,
        permissions: &permissions,
        expires_at: None,
    };
    match state.authority.platform_user(&user) {
        Ok(jwt) => signed_user(jwt, inbox_prefix, event),
        Err(error) => credential_signing_error(&error, event),
    }
}

fn signed_user(jwt: String, inbox_prefix: String, event: &mut Event) -> Response<Full<Bytes>> {
    let reply = json(&SignedUser { jwt, inbox_prefix });
    if reply.status().is_success() {
        event.outcome = Some(Outcome::Succeeded);
    }
    reply
}

fn credential_signing_error(error: &nats::Error, event: &mut Event) -> Response<Full<Bytes>> {
    event.outcome = Some(if error.is_invalid_user_key() {
        Outcome::PublicKeyInvalid
    } else if std::error::Error::source(error)
        .is_some_and(<dyn std::error::Error>::is::<sqlx::Error>)
    {
        Outcome::Unavailable
    } else {
        Outcome::SigningFailed
    });
    signing_error(error)
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
