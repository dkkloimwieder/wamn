//! Serve the bound HTTP import using the host's HTTP implementation.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

use anyhow::Context as _;
use tracing::Instrument as _;
use wamn_application::InvocationLifetime;
use wamn_authority::EffectOutcome;
use wamn_authority::connection::ConnectionError;
use wamn_authority::connection_authority::HostPolicy;
use wamn_authority::effect_span::{
    EFFECT_OPERATION, EffectIdentity, EffectOutcomeGuard, HTTP_EFFECT_DURATION_MS, effect_span,
    record_effect_ms, record_graph,
};
use wamn_connection::bound_http::{Attempt, BoundHttp, Call, Target};
use wamn_connection::connection_credentials::Secret;
use wamn_connection::connection_facts::{ConnectionAnswer, ConnectionFacts};
use wamn_connection::connection_memory::ConnectionStatus;
use wamn_engine::invocation_scope::InvocationScopes;
use wash_runtime::engine::ctx::{ActiveCtx, SharedCtx, extract_active_ctx};
use wash_runtime::engine::workload::{ResolvedWorkload, WorkloadItem};
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wasmtime::component::Accessor;
use wash_runtime::wit::{WitInterface, WitWorld};

mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        path: [
            "../../wit/wamn-connection",
            "../../wit/wamn-ingress",
            "../../wit/wamn-postgres-0.4",
            "../../wit/wamn-node",
            "../../wit/wamn-operation-0.1",
            "../../wit/wasmcloud-blobstore",
            "../authority/wit",
        ],
        world: "wamn:host/connection-http-plugin@0.1.0",
        imports: { default: async | trappable | tracing },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

use bindings::wamn::connection::http::{self, Header, Request, Response};

pub const CONNECTION_HTTP_ID: &str = "wamn-connection-http";

#[derive(Debug)]
struct ApplicationBinding {
    workload: String,
    application: Arc<wamn_application::application_config::ApplicationConfig>,
}

/// HTTP host plugin. Only importing components receive its linker entries.
#[derive(Debug)]
pub struct ConnectionHttp {
    scopes: Arc<InvocationScopes>,
    transport: Arc<dyn BoundHttp>,
    host_policy: &'static dyn HostPolicy,
    applications: Arc<RwLock<HashMap<String, ApplicationBinding>>>,
    component: Option<String>,
}
impl ConnectionHttp {
    pub fn new(
        scopes: Arc<InvocationScopes>,
        transport: Arc<dyn BoundHttp>,
        host_policy: &'static dyn HostPolicy,
    ) -> Self {
        Self {
            scopes,
            transport,
            host_policy,
            applications: Arc::default(),
            component: None,
        }
    }
    async fn send(&self, scope: &str, request: Request) -> Result<Response, ConnectionError> {
        let execution = self
            .scopes
            .execution(scope)
            .ok_or(ConnectionError::AttestationInvalid)?;
        let context = crate::invocation::context(&self.scopes, scope)
            .map_err(|_| ConnectionError::AttestationInvalid)?;
        let invocation = crate::invocation::invocation(&self.scopes, scope)
            .ok_or(ConnectionError::AttestationInvalid)?;
        let alias = request
            .requirement
            .parse()
            .map_err(|_| ConnectionError::Incompatible)?;
        let binding = {
            let applications = self
                .applications
                .read()
                .expect("HTTP application bindings poisoned");
            let application = self
                .component
                .as_ref()
                .and_then(|id| applications.get(id))
                .ok_or(ConnectionError::Unbound)?;
            if application.application != context.release {
                return Err(ConnectionError::AttestationInvalid);
            }
            invocation
                .connection(&context.release, &alias)
                .ok_or(ConnectionError::Unbound)?
        };
        if binding.runtime_type != wamn_connection_type::RuntimeType::Host
            || binding.integration_type.as_str() != "http"
        {
            return Err(ConnectionError::Incompatible);
        }
        let addresses =
            Target::from_target_type(&binding.target_type).ok_or(ConnectionError::Incompatible)?;
        let read_only = binding
            .mode
            .read_only(&invocation.package_id, &binding.name)
            .ok_or(ConnectionError::Incompatible)?;
        if read_only
            && !matches!(
                request.method.to_ascii_uppercase().as_str(),
                "GET" | "HEAD" | "OPTIONS"
            )
        {
            return Err(ConnectionError::AuthorityDenied);
        }
        let id = binding
            .connection_id
            .parse()
            .map_err(|_| ConnectionError::Unbound)?;
        let facts = &context.connections;
        let answer = facts
            .answer(&id)
            .await
            .map_err(|_| ConnectionError::CredentialUnavailable)?
            .filter(|answer| answer.fact.status == ConnectionStatus::Active)
            .ok_or(ConnectionError::Unbound)?;
        let execution: Arc<dyn InvocationLifetime> = Arc::new(execution);
        let request = wamn_connection::bound_http::Request {
            method: request.method,
            path_and_query: request.path_and_query,
            headers: request
                .headers
                .into_iter()
                .map(|header| wamn_connection::bound_http::Header {
                    name: header.name,
                    value: header.value,
                })
                .collect(),
            body: request.body,
            idempotency_key: request.idempotency_key,
        };
        let principal = || {
            self.scopes
                .read::<wamn_connection::invocation::Scope, _>(scope, |record| record.principal)
                .flatten()
        };
        let call = Call {
            facts,
            connection: id,
            target: addresses,
            execution: &execution,
            host_policy: self.host_policy,
            request: &request,
            principal: &principal,
        };
        let response = self.send_active(&call, &answer).await?;
        Ok(Response {
            external_system: response.external_system,
            status: response.status,
            headers: response
                .headers
                .into_iter()
                .map(|header| Header {
                    name: header.name,
                    value: header.value,
                })
                .collect(),
            body: response.body,
        })
    }
    async fn send_active(
        &self,
        call: &Call<'_>,
        answer: &ConnectionAnswer,
    ) -> Result<wamn_connection::bound_http::Response, ConnectionError> {
        let facts = call.facts;
        let id = call.connection;
        let secret = credential(facts, answer, None).await?;
        let (first, token) = self
            .transport
            .attempt(
                call,
                Attempt {
                    answer,
                    material: secret.as_deref().map(Secret::expose),
                    rejected: None,
                },
            )
            .await?;
        if !matches!(first.status, 401 | 403) {
            return Ok(first);
        }
        facts
            .oauth_tokens()
            .remove(&(id, answer.fact.active_generation));
        let fresh = facts
            .read_again(&id)
            .await
            .map_err(|_| ConnectionError::CredentialUnavailable)?
            .filter(|answer| answer.fact.status == ConnectionStatus::Active)
            .ok_or(ConnectionError::Unbound)?;
        let same_credential = fresh.fact.credential_id == answer.fact.credential_id;
        let refreshed = credential(
            facts,
            &fresh,
            if same_credential {
                secret.as_ref()
            } else {
                None
            },
        )
        .await?;
        drop(secret);
        let same_generation =
            same_credential && fresh.fact.active_generation == answer.fact.active_generation;
        let rejected_token = token.as_deref().filter(|_| same_generation);
        let result = self
            .transport
            .attempt(
                call,
                Attempt {
                    answer: &fresh,
                    material: refreshed.as_deref().map(Secret::expose),
                    rejected: rejected_token,
                },
            )
            .await;
        match result {
            Ok((response, _)) => {
                if matches!(response.status, 401 | 403) {
                    facts
                        .oauth_tokens()
                        .remove(&(id, fresh.fact.active_generation));
                }
                Ok(response)
            }
            Err(_) if rejected_token.is_some() => Ok(first),
            Err(error) => Err(error),
        }
    }
}
async fn credential(
    facts: &ConnectionFacts,
    answer: &ConnectionAnswer,
    rejected: Option<&Arc<Secret>>,
) -> Result<Option<Arc<Secret>>, ConnectionError> {
    match answer.fact.credential_id {
        Some(id) => facts
            .secret(id, rejected)
            .await
            .map(Some)
            .map_err(|_| ConnectionError::CredentialUnavailable),
        None => Ok(None),
    }
}

#[async_trait::async_trait]
impl HostPlugin for ConnectionHttp {
    fn id(&self) -> &'static str {
        CONNECTION_HTTP_ID
    }
    fn world(&self) -> WitWorld {
        WitWorld {
            imports: HashSet::from([WitInterface::from("wamn:connection/http@0.1.0")]),
            exports: HashSet::new(),
        }
    }
    async fn on_workload_item_bind<'a>(
        &self,
        item: &mut WorkloadItem<'a>,
        _: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        let WorkloadItem::Component(component) = &*item else {
            anyhow::bail!("HTTP connection requires an application component");
        };
        let bound = Arc::new(Self {
            scopes: Arc::clone(&self.scopes),
            transport: Arc::clone(&self.transport),
            host_policy: self.host_policy,
            applications: Arc::clone(&self.applications),
            component: Some(component.id().to_owned()),
        });
        item.add_plugin(CONNECTION_HTTP_ID, bound);
        http::add_to_linker::<_, SharedCtx>(item.linker()?, extract_active_ctx)?;
        Ok(())
    }
    async fn on_workload_resolved(
        &self,
        workload: &ResolvedWorkload,
        component_id: &str,
    ) -> anyhow::Result<()> {
        let components = workload.components();
        let components = components.read().await;
        let route = components
            .values()
            .find(|component| component.name() == "ingress")
            .context("ingress missing")?;
        let application =
            wamn_engine::application::bound_application_config(&self.scopes, route.id())
                .context("wamn.application missing")?;
        if application
            .components
            .iter()
            .any(|component| !component.connections.is_empty())
        {
            self.applications
                .write()
                .expect("HTTP application bindings poisoned")
                .insert(
                    component_id.to_owned(),
                    ApplicationBinding {
                        workload: workload.id().to_owned(),
                        application,
                    },
                );
        }
        Ok(())
    }
    async fn on_workload_unbind(&self, workload: &str, _: WitInterfaces<'_>) -> anyhow::Result<()> {
        self.applications
            .write()
            .expect("HTTP application bindings poisoned")
            .retain(|_, binding| binding.workload != workload);
        Ok(())
    }
}
impl http::Host for ActiveCtx<'_> {}
impl<T: Send + 'static> http::HostWithStore<T> for SharedCtx {
    async fn send(
        accessor: &Accessor<T, Self>,
        request: Request,
    ) -> wash_runtime::wasmtime::Result<Result<Response, http::ConnectionError>> {
        let (plugin, scope) = accessor.with(|mut access| {
            let ctx = access.get();
            Ok::<_, wash_runtime::wasmtime::Error>((
                ctx.try_get_plugin::<ConnectionHttp>(CONNECTION_HTTP_ID)?,
                Arc::clone(&ctx.component_id),
            ))
        })?;
        let trace = accessor
            .with(|mut access| wamn_engine::invocation_trace::invocation_trace(&mut access.get()));
        trace
            .run(async move {
                let context = crate::invocation::context(&plugin.scopes, &scope).ok();
                let project_key = context
                    .as_ref()
                    .map(|context| context.connections.project_env_key());
                let project = context
                    .as_ref()
                    .map_or("", |context| context.connections.project().as_str());
                let span = effect_span!(
                    "wamn.connection_http",
                    EffectIdentity {
                        project_env_key: project_key,
                        project,
                        component: &scope
                    },
                    None,
                    effect.operation = "send",
                );
                let invocation = crate::invocation::invocation(&plugin.scopes, &scope);
                record_graph(
                    &span,
                    invocation
                        .as_ref()
                        .map(wamn_connection::connection::ConnectionInvocation::effect_graph),
                );
                let mut observed = EffectOutcomeGuard::new(
                    &span,
                    invocation.and_then(|invocation| invocation.effects),
                );
                let started = std::time::Instant::now();
                let result = plugin.send(&scope, request).instrument(span).await;
                record_effect_ms(
                    &HTTP_EFFECT_DURATION_MS,
                    EFFECT_OPERATION,
                    "send",
                    project,
                    started.elapsed(),
                );
                let outcome = match &result {
                    Ok(_) => EffectOutcome::Responded,
                    Err(ConnectionError::Timeout) => EffectOutcome::Timeout,
                    Err(ConnectionError::Transport(detail))
                        if detail == "connection-client-unavailable" =>
                    {
                        EffectOutcome::RefusedBeforeDispatch
                    }
                    Err(ConnectionError::Transport(detail))
                        if detail == "connection-response-lost" =>
                    {
                        EffectOutcome::ResponseLost
                    }
                    Err(ConnectionError::Transport(_)) => EffectOutcome::EffectUncertain,
                    Err(_) => EffectOutcome::RefusedBeforeDispatch,
                };
                observed.settle(outcome, result.is_err());
                Ok(result.map_err(wit_error))
            })
            .await
    }
}
fn wit_error(error: ConnectionError) -> http::ConnectionError {
    match error {
        ConnectionError::Unbound => http::ConnectionError::Unbound,
        ConnectionError::Incompatible => http::ConnectionError::Incompatible,
        ConnectionError::AuthorityDenied => http::ConnectionError::AuthorityDenied,
        ConnectionError::AttestationInvalid => http::ConnectionError::AttestationInvalid,
        ConnectionError::CredentialUnavailable => http::ConnectionError::CredentialUnavailable,
        ConnectionError::Timeout => http::ConnectionError::Timeout,
        ConnectionError::Transport(detail) => http::ConnectionError::Transport(detail),
        ConnectionError::Unauthorized => http::ConnectionError::Unauthorized,
    }
}
