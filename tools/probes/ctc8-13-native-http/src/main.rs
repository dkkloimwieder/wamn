//! Disposable transport observations, not a WAMN transport adoption.

mod server;
mod transport;

use std::collections::BTreeSet;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{Value, json};
use transport::{Abi, call, host as make_host, observation, send};
use wash_runtime::host::http::HostHandler as _;

#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn record(case: &str, verdict: &str, evidence: Value) {
    println!(
        "{}",
        json!({"case":case,"verdict":verdict,"evidence":evidence})
    );
}

#[tokio::main]
async fn main() -> Result<()> {
    wash_runtime::init_crypto();
    record(
        "scope",
        "declared",
        json!({
            "production_base":"dfa1c3187fe8cd671688a442b23106046e502cb6",
            "runtime_pin":"68ebece9c537f8bb4b5c9999f274ec68d60f35a9",
            "entry":"public native HostHandler, no guest WIT execution",
            "credentials":"synthetic probe markers only", "production_adoption":false
        }),
    );
    // The outer ceiling protects the fixture, not the production transport.
    let result = tokio::time::timeout(Duration::from_secs(90), experiments())
        .await
        .context("the whole probe exceeded its fixture deadline")?;
    if let Err(error) = result {
        record("completion", "error", json!({"error":format!("{error:#}")}));
        return Err(error);
    }
    record(
        "completion",
        "observed",
        json!({"protocol_rows":8,"native_adoption_tested":false}),
    );
    Ok(())
}

async fn experiments() -> Result<()> {
    for abi in [Abi::P2, Abi::P3] {
        for tls in [false, true] {
            for grpc in [false, true] {
                protocol(abi, tls, grpc).await?;
            }
        }
        draining(abi).await?;
        quota_keys(abi).await?;
        request_limit(abi).await?;
        bounds_and_failures(abi).await?;
    }
    peer_pinning().await?;
    record(
        "private_wamn_integration",
        "not-tested",
        json!({
            "subjects":["real ConnectionHttp send substitution","candidate bindings including blobstore","original caller and executing nested identity"],
            "reason":"ConnectionHttp::send/execute and closure checks are private; this probe neither copies nor patches them",
            "regressions":"owned by root, separate from these native results"
        }),
    );
    record(
        "native_configuration",
        "source-gap",
        json!({
            "subjects":["retained client count cap","pre-dispatch pinned connector","request/response byte limits","independent pool and quota keys","retry_canceled_requests public setting"],
            "source":"runtime@735b579 host/http_client.rs:600,635,855,901,967",
            "retry_source":"hyper-util-0.1.20 client/legacy/client.rs:1034,1541",
            "not_claimed":"no duplicate effects under every possible reset, no native total-duration limit"
        }),
    );
    Ok(())
}

async fn protocol(abi: Abi, tls: bool, grpc: bool) -> Result<()> {
    let server = server::start(tls, grpc).await?;
    let (host, _) = make_host(Arc::clone(&server.client_tls), 8, 16).await?;
    let mut replies = Vec::new();
    let mut first = BTreeSet::new();
    let mut second = BTreeSet::new();
    for (scope, connections) in [("scope-a", &mut first), ("scope-b", &mut second)] {
        for index in 0..8 {
            let mut request = call(abi, server.address, tls, grpc, scope, "/ok");
            request.marker = format!("synthetic-g{}", index % 2 + 1);
            let reply = send(&host, &request).await?;
            anyhow::ensure!(
                reply.peer == Some(server.address),
                "connected peer metadata differs from the recording server"
            );
            anyhow::ensure!(
                reply.version == if grpc { "HTTP/2.0" } else { "HTTP/1.1" },
                "unexpected wire protocol"
            );
            connections.insert(reply.connection);
            replies.push(observation(&reply));
        }
    }
    anyhow::ensure!(
        first.len() < 8 && second.len() < 8,
        "sequential requests did not reuse transport"
    );
    anyhow::ensure!(
        first.is_disjoint(&second),
        "different native identities shared a connection"
    );
    let requests = server::requests(&server);
    anyhow::ensure!(
        requests.len() == 16,
        "native request count differs from issued requests"
    );
    let marker_crossing = first.iter().any(|id| {
        let markers: BTreeSet<_> = requests
            .iter()
            .filter(|row| row["connection"].as_u64() == Some(*id as u64))
            .filter_map(|row| row["credential_marker"].as_str())
            .collect();
        markers.len() == 2
    });
    anyhow::ensure!(
        marker_crossing,
        "fixture did not observe both credential markers on a reused connection"
    );
    let mut denied = call(abi, server.address, tls, grpc, "scope-a", "/ok");
    denied.allow = false;
    let denial = send(&host, &denied)
        .await
        .expect_err("allowed-host denial must precede a pool hit");
    anyhow::ensure!(
        server::requests(&server).len() == 16,
        "denied request reached the recording server"
    );
    record(
        &format!("protocol-{abi:?}-tls-{tls}-grpc-{grpc}"),
        "observed",
        json!({
            "reuse":true,"native_scope_isolation":true,"request_headers_remain_per_request":true,
            "same_key_cross_generation_connection_reuse":marker_crossing,
            "generation_isolation":"requires a WAMN key/invalidation policy, not supplied by changing headers",
            "allowed_hosts_denial":format!("{denial:#}"),"responses":replies,"requests":requests
        }),
    );
    if tls {
        let (untrusted, _) = make_host(
            wash_runtime::host::http_client::default_client_tls_config(),
            8,
            16,
        )
        .await?;
        let error = send(
            &untrusted,
            &call(abi, server.address, true, grpc, "untrusted", "/ok"),
        )
        .await
        .expect_err("the private fixture certificate must not inherit unrelated trust");
        anyhow::ensure!(
            server::requests(&server).len() == 16,
            "untrusted TLS dispatched HTTP"
        );
        record(
            &format!("tls-refusal-{abi:?}-grpc-{grpc}"),
            "pass",
            json!({"error":format!("{error:#}"),"http_requests":16}),
        );
    }
    Ok(())
}

async fn draining(abi: Abi) -> Result<()> {
    let server = server::start(false, false).await?;
    let (host, quotas) = make_host(Arc::clone(&server.client_tls), 1, 1).await?;
    let active = Arc::clone(&host);
    let old = call(abi, server.address, false, false, "stable-quota", "/hold");
    let old = tokio::spawn(async move { send(&active, &old).await });
    server::wait_requests(&server, 1).await?;
    host.on_workload_unbind("stable-quota").await?;
    let mut new = call(abi, server.address, false, false, "stable-quota", "/ok");
    new.timeout = Duration::from_millis(80);
    new.marker = "synthetic-g2".to_owned();
    let refused = send(&host, &new)
        .await
        .expect_err("draining generation must retain its quota slot");
    anyhow::ensure!(
        quotas.for_guest("stable-quota").outbound_http_available() == 0,
        "old generation released quota while still in flight"
    );
    anyhow::ensure!(
        server::requests(&server).len() == 1,
        "new generation exceeded draining quota"
    );
    server.release.add_permits(1);
    let old_reply = old.await??;
    server::wait_closed(&server).await?;
    new.timeout = Duration::from_secs(2);
    let new_reply = send(&host, &new).await?;
    anyhow::ensure!(
        old_reply.connection != new_reply.connection,
        "invalidation reused the old generation connection"
    );
    record(
        &format!("draining-{abi:?}"),
        "pass",
        json!({"during_drain":format!("{refused:#}"),
        "old":observation(&old_reply),"new":observation(&new_reply),"requests":server::requests(&server)}),
    );
    Ok(())
}

async fn quota_keys(abi: Abi) -> Result<()> {
    let server = server::start(false, false).await?;
    let (host, quotas) = make_host(Arc::clone(&server.client_tls), 1, 8).await?;
    let mut pending = Vec::new();
    for key in ["logical-scope:g1", "logical-scope:g2"] {
        let host = Arc::clone(&host);
        let request = call(abi, server.address, false, false, key, "/hold");
        pending.push(tokio::spawn(async move { send(&host, &request).await }));
    }
    server::wait_requests(&server, 2).await?;
    let live = server.observed.active.load(Ordering::SeqCst);
    anyhow::ensure!(
        live == 2,
        "quota multiplication witness requires two simultaneous connections"
    );
    anyhow::ensure!(
        quotas
            .for_guest("logical-scope:g1")
            .outbound_http_available()
            == 0
            && quotas
                .for_guest("logical-scope:g2")
                .outbound_http_available()
                == 0,
        "both generation keys must own distinct charged quotas"
    );
    server.release.add_permits(2);
    for task in pending {
        task.await??;
    }
    record(
        &format!("generation-quota-keys-{abi:?}"),
        "gap",
        json!({
            "per_native_key_limit":1,"logical_scope_live_connections":live,
            "reason":"different generation pool keys mint different per-key quotas",
            "requests":server::requests(&server)
        }),
    );
    Ok(())
}

async fn request_limit(abi: Abi) -> Result<()> {
    let server = server::start(false, true).await?;
    let (host, _) = make_host(Arc::clone(&server.client_tls), 1, 1).await?;
    let mut pending = Vec::new();
    for _ in 0..4 {
        let host = Arc::clone(&host);
        let request = call(abi, server.address, false, true, "one-h2-pool", "/hold");
        pending.push(tokio::spawn(async move { send(&host, &request).await }));
    }
    server::wait_requests(&server, 4).await?;
    let connections = server.observed.active.load(Ordering::SeqCst);
    anyhow::ensure!(
        connections == 1,
        "H2 request-concurrency witness requires one connection"
    );
    server.release.add_permits(4);
    for task in pending {
        task.await??;
    }
    record(
        &format!("request-concurrency-{abi:?}"),
        "gap",
        json!({
            "connection_limit":1,"simultaneous_requests":4,"connections":connections,
            "reason":"a connection quota is not a request quota", "requests":server::requests(&server)
        }),
    );
    Ok(())
}

async fn bounds_and_failures(abi: Abi) -> Result<()> {
    let server = server::start(false, false).await?;
    let (host, _) = make_host(Arc::clone(&server.client_tls), 8, 16).await?;
    let mut sizes = Vec::new();
    for path in ["/large-body", "/large-header"] {
        let mut request = call(abi, server.address, false, false, "sizes", path);
        request.body_bytes = 128 * 1024;
        request.padding = 4096;
        let reply = send(&host, &request).await?;
        anyhow::ensure!(
            if path == "/large-body" {
                reply.body_bytes == 128 * 1024
            } else {
                reply.header_bytes > 4096
            },
            "size fixture did not exceed its declared diagnostic threshold"
        );
        sizes.push(observation(&reply));
    }
    record(
        &format!("byte-limits-{abi:?}"),
        "gap",
        json!({
            "probe_threshold_bytes_not_product_policy":1024,"responses":sizes,
            "requests":server::requests(&server),"reason":"native configuration provides no requested aggregate byte budget"
        }),
    );
    let mut slow = call(abi, server.address, false, false, "deadlines", "/slow-head");
    slow.timeout = Duration::from_millis(80);
    let timeout = send(&host, &slow)
        .await
        .expect_err("slow headers must exceed the native first-byte deadline");
    let mut streaming = call(abi, server.address, false, false, "streaming", "/slow-body");
    streaming.timeout = Duration::from_millis(100);
    let streamed = send(&host, &streaming).await?;
    anyhow::ensure!(
        streamed.elapsed_ms >= 180,
        "stream fixture did not exceed the phase timeout in total"
    );
    let reset = send(
        &host,
        &call(abi, server.address, false, false, "reset", "/truncate"),
    )
    .await
    .expect_err("the recorded mutation must lose its response");
    let observed = server::requests(&server);
    anyhow::ensure!(
        observed
            .iter()
            .filter(|row| row["path"] == "/truncate")
            .count()
            == 1,
        "native transport repeated the mutation after response loss"
    );
    anyhow::ensure!(
        observed
            .iter()
            .filter(|row| row["path"] == "/slow-head")
            .count()
            == 1,
        "timed-out mutation was not recorded exactly once"
    );
    record(
        &format!("deadlines-and-response-loss-{abi:?}"),
        "observed",
        json!({
            "native_head_timeout":format!("{timeout:#}"),"stream":observation(&streamed),
            "native_total_deadline":"not supplied by the phase deadlines",
            "response_loss":format!("{reset:#}"),"mutation_dispatches":1,"timeout_did_not_undo_upstream":true,
            "retry_claim":"only this response-loss case, not a proof over all races",
            "p2_body_note":"the direct native body omits the guest HostIncomingBody timeout wrapper",
            "requests":observed
        }),
    );
    Ok(())
}

struct SelectedPeer(SocketAddr);

impl wamn_runtime::connection_authority::DnsResolver for SelectedPeer {
    fn resolve(
        &self,
        _: &str,
        _: u16,
    ) -> impl Future<
        Output = Result<Vec<SocketAddr>, wamn_runtime::connection_authority::AuthorityError>,
    > + Send {
        std::future::ready(Ok(vec![self.0]))
    }
}

impl wamn_runtime::connection_authority::NetworkPolicy for SelectedPeer {
    fn allows(&self, address: SocketAddr) -> bool {
        address == self.0
    }
}

async fn peer_pinning() -> Result<()> {
    use wamn_runtime::connection_authority::{
        TlsPolicy, TransportDecision, parse_http_connection_authority, resolve_http_request,
    };
    let server = server::start(false, false).await?;
    let (host, _) = make_host(Arc::clone(&server.client_tls), 4, 8).await?;
    let approved = SocketAddr::from((Ipv4Addr::new(127, 0, 0, 2), server.address.port()));
    let selected = SelectedPeer(approved);
    let authority = parse_http_connection_authority(
        &format!("http://localhost:{}", server.address.port()),
        TlsPolicy::Disabled,
        None,
    )?;
    let target = wamn_execution_contract::node_contract::normalize_portable_http_target("/ok")?;
    let decision =
        resolve_http_request(&authority, &target, &["*".parse()?], &selected, &selected).await?;
    anyhow::ensure!(
        matches!(decision.transport, TransportDecision::Direct { ref origin } if origin.address == approved),
        "WAMN resolver did not select the restricted peer"
    );
    let mut request = call(Abi::P2, server.address, false, false, "peer-witness", "/ok");
    request.url = decision.logical_url.into();
    let reply = send(&host, &request).await?;
    anyhow::ensure!(
        reply.peer == Some(server.address) && reply.peer != Some(approved),
        "fixture did not expose independent native resolution"
    );
    record(
        "connected-peer-versus-pinning",
        "gap",
        json!({
            "wamn_authorized_peer":approved.to_string(),"native_response":observation(&reply),
            "requests":server::requests(&server),"reason":"public native transport cannot consume WAMN's pinned endpoint",
            "warning":"observing the mismatch after dispatch does not enforce destination authorization"
        }),
    );
    Ok(())
}
