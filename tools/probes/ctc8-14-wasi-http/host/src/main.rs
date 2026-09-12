//! Named disposable probe of the pinned native hooks, not a production adapter.

mod hook;
mod server;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, ensure};
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use serde_json::{Value, json};
use tracing::Instrument as _;
use tracing_subscriber::layer::SubscriberExt as _;
use wash_runtime::engine::Engine;
use wash_runtime::engine::ctx::{Ctx, SharedCtx};
use wash_runtime::host::allowed_hosts::AllowedHost;
use wash_runtime::host::http::{DefaultOutgoingHandler, DevRouter, HostHandler, Ingress};
use wasmtime::{
    Store,
    component::{Component, Linker},
};
use wasmtime_wasi::{WasiCtxBuilder, p2::pipe::MemoryOutputPipe};

const WORKLOAD: &str = "ctc8-14-shared-workload";
const GUEST_TRACE: &str = "00-11111111111111111111111111111111-2222222222222222-01";
const RUNTIME_REV: &str = "68ebece9c537f8bb4b5c9999f274ec68d60f35a9";

#[derive(Clone)]
struct RequestCase {
    name: &'static str,
    scheme: &'static str,
    authority: String,
    path: &'static str,
    content_type: &'static str,
    authorization: &'static str,
    traceparent: &'static str,
    component: &'static str,
}

fn request(name: &'static str, scheme: &'static str, authority: impl Into<String>) -> RequestCase {
    RequestCase {
        name,
        scheme,
        authority: authority.into(),
        path: "/allowed/probe",
        content_type: "application/json",
        authorization: "",
        traceparent: "",
        component: "parent",
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    ensure!(
        std::env::var_os("WAMN_CTC8_14_SECRET").is_some(),
        "set the synthetic WAMN_CTC8_14_SECRET host sentinel"
    );
    let guest_path = std::env::args()
        .nth(1)
        .context("pass the built P2 guest path")?;
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    tracing::subscriber::set_global_default(
        tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("ctc8-14"))),
    )?;
    opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());

    let engine = Engine::builder().with_pooling_allocator(false).build()?;
    let component = Component::from_file(engine.inner(), &guest_path)?;
    let imports: Vec<_> = component
        .component_type()
        .imports(engine.inner())
        .map(|(name, _)| name.to_owned())
        .collect();
    ensure!(
        imports
            .iter()
            .any(|name| name.starts_with("wasi:http/outgoing-handler@")),
        "guest must actually import standard WASI HTTP"
    );
    ensure!(
        !imports.iter().any(|name| name.starts_with("wamn:")),
        "the standard guest must not hide a WAMN transport import"
    );
    let http_imports = wamn_component_policy::ComponentImports::new(
        imports
            .iter()
            .filter(|name| name.starts_with("wasi:http/"))
            .cloned(),
    );
    let denied = wamn_component_policy::analyze_tenant(&http_imports, &BTreeSet::new(), "ctc8-14")
        .expect_err("unchanged production tenant admission must refuse wasi:http");
    ensure!(
        denied.kind() == wamn_component_policy::TenantImportErrorKind::UnadmittedImport,
        "production policy denied for a different reason"
    );
    println!(
        "{}",
        json!({"case":"production_admission", "result":"denied", "imports":imports,
        "runtime_rev":RUNTIME_REV, "wamn_rev":"dfa1c3187fe8cd671688a442b23106046e502cb6"})
    );

    let plain = server::start(None).await?;
    let secure = server::start(Some("127.0.0.1")).await?;
    let wrong_name = server::start(Some("wrong.invalid")).await?;
    let mut roots = rustls::RootCertStore::empty();
    roots.add(secure.certificate.clone().expect("TLS peer certificate"))?;
    roots.add(
        wrong_name
            .certificate
            .clone()
            .expect("wrong-name peer certificate"),
    )?;
    let tls = Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    );
    let calls = Arc::new(Mutex::new(Vec::new()));
    let grpc_selections = Arc::new(AtomicUsize::new(0));
    let aliases = BTreeMap::from([
        (
            "erp.invalid".to_owned(),
            format!("http://{}", plain.address).parse()?,
        ),
        (
            "secure.invalid".to_owned(),
            format!("https://{}", secure.address).parse()?,
        ),
        (
            "wrong-name.invalid".to_owned(),
            format!("https://{}", wrong_name.address).parse()?,
        ),
    ]);
    let handler: Arc<dyn HostHandler> = Arc::new(
        Ingress::builder(DevRouter::default(), "127.0.0.1:0".parse()?)
            .outgoing_handler(hook::ProbeHook {
                native: DefaultOutgoingHandler::with_tls_config(tls),
                aliases,
                calls: Arc::clone(&calls),
                grpc_selections: Arc::clone(&grpc_selections),
            })
            .build()
            .await?,
    );
    let allowed: Arc<[AllowedHost]> = Arc::from([
        "erp.invalid".parse()?,
        "secure.invalid".parse()?,
        "unknown.invalid".parse()?,
        "wrong-name.invalid".parse()?,
        plain.address.to_string().parse()?,
        secure.address.to_string().parse()?,
    ]);
    let exercise = async {
        let normal = request("probe_alias_http", "http", "erp.invalid");
        let observed = invoke(
            &engine,
            &component,
            Some(Arc::clone(&handler)),
            Arc::clone(&allowed),
            &normal,
        )
        .await?;
        require_success(&observed)?;
        let upstream = server::last(&plain)?;
        ensure!(
            upstream["connected_local"] == plain.address.to_string(),
            "wrong connected fixture peer"
        );
        ensure!(
            upstream["host"] == plain.address.to_string(),
            "alias leaked into the upstream Host"
        );
        ensure!(
            upstream["protocol"] == "HTTP/1.1",
            "ordinary native P2 path protocol changed"
        );
        ensure!(
            upstream["host_credential"] == true && upstream["guest_credential"] == false,
            "host credential injection did not reach the real peer"
        );
        let injected_trace = upstream["traceparent"]
            .as_str()
            .context("hook trace absent at real peer")?
            .to_owned();
        print_observation(&normal, &observed, Some(upstream));

        // The guest asks for http, but the probe's binding selects verified TLS.
        let encrypted = request("probe_alias_forces_tls", "http", "secure.invalid");
        let observed = invoke(
            &engine,
            &component,
            Some(Arc::clone(&handler)),
            Arc::clone(&allowed),
            &encrypted,
        )
        .await?;
        require_success(&observed)?;
        let upstream = server::last(&secure)?;
        ensure!(
            upstream["connected_local"] == secure.address.to_string()
                && upstream["host_credential"] == true,
            "TLS alias did not reach its credentialed fixture peer"
        );
        print_observation(&encrypted, &observed, Some(upstream));

        let mut forwarded = normal.clone();
        forwarded.name = "guest_trace_preserved";
        forwarded.traceparent = GUEST_TRACE;
        let observed = invoke(
            &engine,
            &component,
            Some(Arc::clone(&handler)),
            Arc::clone(&allowed),
            &forwarded,
        )
        .await?;
        require_success(&observed)?;
        let upstream = server::last(&plain)?;
        ensure!(
            upstream["traceparent"] == GUEST_TRACE,
            "guest trace precedence changed"
        );
        print_observation(&forwarded, &observed, Some(upstream));

        let parent_call = calls
            .lock()
            .expect("hook observations")
            .last()
            .cloned()
            .expect("parent hook call");
        let mut child = normal.clone();
        child.name = "second_component_same_workload";
        child.component = "child";
        let observed = invoke(
            &engine,
            &component,
            Some(Arc::clone(&handler)),
            Arc::clone(&allowed),
            &child,
        )
        .await?;
        require_success(&observed)?;
        let child_call = calls
            .lock()
            .expect("hook observations")
            .last()
            .cloned()
            .expect("child hook call");
        ensure!(
            parent_call["workload_id"] == child_call["workload_id"],
            "native workload hook changed identity semantics"
        );
        print_observation(&child, &observed, Some(server::last(&plain)?));
        println!(
            "{}",
            json!({"case":"invocation_context_gap", "result":"gap",
            "parent_hook":parent_call, "child_hook":child_call,
            "scope":"two separate component contexts; no production nested-call proof"})
        );

        let mut denied_cases = vec![
            request("probe_unknown_alias", "http", "unknown.invalid"),
            request("non_http_scheme_refused", "ftp", "erp.invalid"),
            request(
                "native_destination_denied_before_hook",
                "http",
                "127.0.0.1:1",
            ),
            request("probe_tls_name_mismatch", "https", "wrong-name.invalid"),
        ];
        let mut path_escape = normal.clone();
        path_escape.name = "probe_path_escape";
        path_escape.path = "/outside";
        denied_cases.push(path_escape);
        let mut forgery = normal.clone();
        forgery.name = "probe_guest_authorization_denied";
        forgery.authorization = server::GUEST_CREDENTIAL;
        denied_cases.push(forgery);
        for case in denied_cases {
            let before = plain.requests.lock().expect("peer observations").len()
                + secure.requests.lock().expect("peer observations").len()
                + wrong_name.requests.lock().expect("peer observations").len();
            let hook_before = calls.lock().expect("hook observations").len();
            let observed = invoke(
                &engine,
                &component,
                Some(Arc::clone(&handler)),
                Arc::clone(&allowed),
                &case,
            )
            .await?;
            ensure!(
                observed["status"].as_i64().is_some_and(|status| status < 0),
                "negative case reached success"
            );
            let after = plain.requests.lock().expect("peer observations").len()
                + secure.requests.lock().expect("peer observations").len()
                + wrong_name.requests.lock().expect("peer observations").len();
            ensure!(
                before == after,
                "a refused call reached an HTTP application"
            );
            if matches!(
                case.name,
                "probe_unknown_alias" | "probe_path_escape" | "probe_guest_authorization_denied"
            ) {
                ensure!(
                    observed["request_denied"] == true,
                    "fixture denial became an unrelated transport failure"
                );
            }
            if case.name == "native_destination_denied_before_hook" {
                ensure!(
                    hook_before == calls.lock().expect("hook observations").len(),
                    "allowlist denial ran the hook"
                );
            }
            print_observation(&case, &observed, None);
        }

        for (name, scheme, peer, content_type) in [
            (
                "native_grpc_h2c_bypasses_hook",
                "http",
                &plain,
                "application/grpc",
            ),
            (
                "native_grpc_tls_bypasses_hook",
                "https",
                &secure,
                "application/grpc",
            ),
            (
                "native_grpc_proto_bypasses_hook",
                "http",
                &plain,
                "application/grpc+proto",
            ),
        ] {
            let mut case = request(name, scheme, peer.address.to_string());
            case.content_type = content_type;
            case.authorization = server::GUEST_CREDENTIAL;
            let hook_before = calls.lock().expect("hook observations").len();
            let grpc_before = grpc_selections.load(Ordering::SeqCst);
            let observed = invoke(
                &engine,
                &component,
                Some(Arc::clone(&handler)),
                Arc::clone(&allowed),
                &case,
            )
            .await?;
            ensure!(
                observed["status"] == 200,
                "native gRPC control did not execute"
            );
            ensure!(
                hook_before == calls.lock().expect("hook observations").len(),
                "gRPC unexpectedly entered send_request"
            );
            ensure!(
                grpc_before + 1 == grpc_selections.load(Ordering::SeqCst),
                "gRPC transport selection not observed"
            );
            let upstream = server::last(peer)?;
            ensure!(
                upstream["protocol"] == "HTTP/2.0",
                "gRPC control did not use HTTP/2"
            );
            ensure!(
                upstream["guest_credential"] == true && upstream["host_credential"] == false,
                "gRPC did not demonstrate the bypass of the credential hook"
            );
            ensure!(
                upstream["traceparent"].is_null(),
                "native gRPC now injects trace headers; update the finding"
            );
            print_observation(&case, &observed, Some(upstream));
        }

        let unbound = request("native_unbound_context", "http", plain.address.to_string());
        let before = plain.requests.lock().expect("peer observations").len();
        let observed = invoke(&engine, &component, None, Arc::clone(&allowed), &unbound).await?;
        ensure!(
            observed["status"].as_i64().is_some_and(|status| status < 0),
            "unbound context acquired HTTP authority"
        );
        ensure!(
            before == plain.requests.lock().expect("peer observations").len(),
            "unbound context reached the recording peer"
        );
        print_observation(&unbound, &observed, None);
        Ok::<_, anyhow::Error>(injected_trace)
    };
    let injected_trace = tokio::time::timeout(Duration::from_secs(120), exercise)
        .await
        .context("probe deadline")??;
    provider.force_flush()?;
    let spans = exporter.get_finished_spans()?;
    let parts: Vec<_> = injected_trace.split('-').collect();
    ensure!(parts.len() == 4, "bad propagated traceparent");
    let hook_span = spans
        .iter()
        .find(|span| {
            span.name == "probe_only_hook"
                && span.span_context.trace_id().to_string() == parts[1]
                && span.span_context.span_id().to_string() == parts[2]
        })
        .context("wire trace has no exported hook span")?;
    ensure!(
        spans.iter().any(|span| span.name == "outbound_http_request"
            && span.parent_span_id == hook_span.span_context.span_id()),
        "native span is not a child of the injection span"
    );
    for span in &spans {
        let attributes: BTreeMap<_, _> = span
            .attributes
            .iter()
            .map(|attribute| (attribute.key.as_str(), attribute.value.to_string()))
            .collect();
        ensure!(
            !format!("{attributes:?}").contains("host-only-sentinel")
                && !format!("{attributes:?}").contains("guest-forgery"),
            "credential sentinel leaked into trace attributes"
        );
        println!(
            "{}",
            json!({"record":"span", "name":span.name,
            "trace_id":span.span_context.trace_id().to_string(), "span_id":span.span_context.span_id().to_string(),
            "parent_span_id":span.parent_span_id.to_string(), "attributes":attributes})
        );
    }
    println!(
        "{}",
        json!({"case":"report", "result":"probe-complete", "production_adoption":false,
        "gaps":["tenant-wasi-http-unadmitted", "native-grpc-bypasses-request-hook",
            "native-grpc-no-wire-trace-injection", "workload-id-is-not-invocation-authority",
            "production-nesting-and-candidate-binding-unproven", "native-connector-peer-pinning-not-exposed",
            "p3-guest-not-probed", "wamn-outcome-vocabulary-not-preserved-by-standard-errors"],
        "limits":"no pooling, rotation or aggregate-quota claim; owned by ctc8.13"})
    );
    Ok(())
}

async fn invoke(
    engine: &Engine,
    component: &Component,
    handler: Option<Arc<dyn HostHandler>>,
    allowed: Arc<[AllowedHost]>,
    case: &RequestCase,
) -> anyhow::Result<Value> {
    let stdout = MemoryOutputPipe::new(16 * 1024);
    let stderr = MemoryOutputPipe::new(16 * 1024);
    let mut wasi = WasiCtxBuilder::new();
    wasi.args(&[
        "ctc8-14-guest",
        case.scheme,
        &case.authority,
        case.path,
        case.content_type,
        case.authorization,
        case.traceparent,
    ])
    .stdout(stdout.clone())
    .stderr(stderr);
    let mut ctx = Ctx::builder(WORKLOAD, case.component)
        .with_wasi_ctx(wasi.build())
        .with_allowed_hosts(allowed);
    if let Some(handler) = handler {
        ctx = ctx.with_http_handler(handler);
    }
    let mut store = Store::new(
        engine.inner(),
        SharedCtx::new(ctx.build()).with_guest_memory(engine.guest_memory()),
    );
    wash_runtime::engine::guest_memory::install_memory_limiter(&mut store);
    store.set_epoch_deadline(u64::MAX / 2);
    let mut linker = Linker::new(engine.inner());
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    wasmtime_wasi_http::p2::add_only_http_to_linker_async(&mut linker)?;
    let command =
        wasmtime_wasi::p2::bindings::Command::instantiate_async(&mut store, component, &linker)
            .await?;
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        command
            .wasi_cli_run()
            .call_run(&mut store)
            .instrument(tracing::info_span!(
                "probe_invocation",
                component = case.component,
                case = case.name
            )),
    )
    .await
    .context("guest attempt deadline")?;
    if result.is_err() {
        return Ok(json!({"status":-2, "guest_result":"trap"}));
    }
    ensure!(result?.is_ok(), "guest exited with failure");
    serde_json::from_slice(&stdout.contents()).context("decode the real guest's observation")
}

fn require_success(observed: &Value) -> anyhow::Result<()> {
    ensure!(
        observed["status"] == 200,
        "real guest request did not reach HTTP 200"
    );
    ensure!(
        observed["headers_unchanged"] == true && observed["authorization_visible"] == false,
        "host credential appeared in guest-owned fields"
    );
    ensure!(
        observed["host_environment_visible"] == false,
        "host environment leaked to guest"
    );
    Ok(())
}

fn print_observation(case: &RequestCase, guest: &Value, upstream: Option<Value>) {
    println!(
        "{}",
        json!({"case":case.name, "result":"observed", "guest":guest, "upstream":upstream})
    );
}
