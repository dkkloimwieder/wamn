//! NATS resolver publication and the permission templates owned by identity.

use std::{path::PathBuf, time::Duration};

use async_nats::{Client, ConnectOptions, ServerAddr};
use http_body_util::Full;
use hyper::{Response, StatusCode, body::Bytes};
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use tokio::sync::OnceCell;
use wamn_identity::nats::{self, Authority, Permissions, SubjectPermission};
use wamn_identity_client::nats::{
    Account, ENSURE_ACCOUNT_PATH, EnsureAccount, SIGN_USER_PATH, SignUser, SignedUser, UserKind,
};
use wamn_kubernetes::kubernetes_api::KubernetesApi;
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

    async fn publish_account(
        &self,
        account: &str,
        jwt: String,
    ) -> Result<(), IdentityServiceError> {
        let message = tokio::time::timeout(REQUEST_TIMEOUT, async {
            let client = self
                .client()
                .await
                .map_err(|_| IdentityServiceError::new("connect to NATS resolver failed"))?;
            client
                .request(nats::UPDATE_SUBJECT, jwt.into())
                .await
                .map_err(|_| IdentityServiceError::new("request NATS account update failed"))
        })
        .await
        .map_err(|_| IdentityServiceError::new("NATS account update timed out"))??;
        let reply: UpdateReply = serde_json::from_slice(&message.payload)
            .map_err(|_| IdentityServiceError::new("decode NATS account update reply failed"))?;
        if reply
            .data
            .is_some_and(|data| data.code == 200 && data.account == account)
            && reply.error.is_none()
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
    let Some(Settings { url, tls_ca }) = settings else {
        return Ok(None);
    };
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
    let bootstrap = nats::bootstrap(db, issuer, envelope)
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
        SIGN_USER_PATH => sign_user(inner, state, client, org, body).await,
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
    let (public_key, jwt) = match state
        .authority
        .ensure_environment(
            client,
            &inner.envelope,
            &inner.issuer,
            org,
            &request.env,
            count,
        )
        .await
    {
        Ok(value) => value,
        Err(error) => return signing_error(&error),
    };
    if let Err(error) = state.publish_account(&public_key, jwt).await {
        tracing::warn!(%error, "publish NATS environment account failed");
        return unavailable();
    }
    json(&Account { public_key })
}

async fn sign_user(
    inner: &Inner,
    state: &State,
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
    let inbox_prefix = format!("_INBOX_wamn.{}", request.public_key);
    let (name, permissions) = template(&request, &inbox_prefix);
    let user = nats::User {
        public_key: &request.public_key,
        name: &name,
        permissions: &permissions,
    };
    let signed = match request.kind {
        UserKind::HostScheduler | UserKind::HostData => {
            state.authority.platform_user(&user).map(Some)
        }
        _ => {
            state
                .authority
                .environment_user(
                    client,
                    &inner.envelope,
                    &inner.issuer,
                    org,
                    &request.env,
                    &user,
                )
                .await
        }
    };
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

fn template(request: &SignUser, inbox: &str) -> (String, Permissions) {
    let mut subscribe = vec![format!("{inbox}.>")];
    let workload = || {
        [
            "$JS.API.STREAM.INFO.*",
            "$JS.API.CONSUMER.INFO.>",
            "$JS.API.CONSUMER.CREATE.>",
            "$JS.API.CONSUMER.DELETE.>",
            "$JS.API.CONSUMER.MSG.NEXT.>",
            "$JS.ACK.>",
        ]
        .map(str::to_owned)
        .into()
    };
    let (kind, publish) = match &request.kind {
        UserKind::Workflow => ("workflow".into(), workload()),
        UserKind::Ingress => {
            subscribe.push("_nats_push.>".into());
            ("ingress".into(), workload())
        }
        UserKind::WorkflowNative => ("workflow-native".into(), workload()),
        UserKind::Cdc { project } => (
            format!("cdc-{project}"),
            vec![name::stream_subjects(&request.org, project, &request.env)],
        ),
        UserKind::Recovery { project } => {
            let stream = name::stream_name(&request.org, project, &request.env);
            (
                format!("recovery-{project}"),
                vec![
                    format!("$JS.API.STREAM.INFO.{stream}"),
                    format!("$JS.API.STREAM.MSG.GET.{stream}"),
                ],
            )
        }
        UserKind::StreamAdmin => (
            "stream-admin".into(),
            [
                "$JS.API.STREAM.INFO.*",
                "$JS.API.STREAM.CREATE.*",
                "$JS.API.STREAM.DELETE.*",
                "$JS.API.STREAM.PURGE.*",
                "$JS.API.CONSUMER.INFO.>",
                "$JS.API.CONSUMER.CREATE.>",
            ]
            .map(str::to_owned)
            .into(),
        ),
        UserKind::HostScheduler => {
            // The shared scheduler protocol uses random host IDs. The
            // installation's scheduler subjects apply to every host group.
            const SCHEDULER: [&str; 2] = ["runtime.host.>", "runtime.operator.>"];
            let publish = [
                SCHEDULER[0].to_owned(),
                SCHEDULER[1].to_owned(),
                format!("{}.>", nats::RUNTIME_OPERATOR_INBOX),
                format!("{inbox}.>"),
            ]
            .into();
            subscribe.extend(SCHEDULER.map(str::to_owned));
            ("wasmcloud-runtime".into(), publish)
        }
        UserKind::HostData => ("wasmcloud-data".into(), Vec::new()),
    };
    let name = format!(
        "{kind}-{}",
        name::environment_key(&request.org, &request.env)
    );
    let permissions = Permissions {
        pub_: SubjectPermission {
            deny: if publish.is_empty() {
                vec![">".into()]
            } else {
                Vec::new()
            },
            allow: publish,
        },
        sub: SubjectPermission {
            allow: subscribe,
            deny: Vec::new(),
        },
        resp: None,
    };
    (name, permissions)
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
