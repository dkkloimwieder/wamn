//! sockprobe — E13/E15 runtime raw-socket fixture (wamn-o3u6).
//!
//! MVP outcome: proof floor.
//!
//! A `wasi:cli` command that ATTEMPTS each raw outbound TCP + UDP egress arm via
//! `wasi:sockets` (reached through `std::net` on wasm32-wasip2 — the default
//! command world imports the whole `wasi:sockets` package) and reports the
//! POLICY verdict of each attempt, so the egressbench runtime phase can assert
//! the fork's `linked_call` raw-socket policy (docs/archive/platform/wash-runtime-fork.md, pins
//! 8b76869 E13 / eef76cd E15/E16) WITHOUT matching on error text:
//!
//!   - deny-by-default: with no `wamn.allow-raw-sockets`, `socket_addr_check`
//!     fails the connect and the guest sees `access-denied` (`std`
//!     `ErrorKind::PermissionDenied`) → verdict `denied`.
//!   - opted-in: with `wamn.allow-raw-sockets=true`, the check permits the op
//!     and the connect proceeds — then fails for an unrelated reason against a
//!     dead local port → verdict `connected` / `allowed-failed` (NOT `denied`).
//!
//! HOST-LOOPBACK ARMS (wamn-0h0g.15.52). `host.wasmcloud.internal` — the fork's
//! sentinel address — names the *machine's* own loopback, a separate door from
//! raw egress with its own two-part grant: the workload's
//! `allowedHostLoopbackPorts` AND the host's own `--allow-host-loopback`. Two
//! arms dial that sentinel: `host-loopback-listed` at the port the gate's grant
//! list names when it supplies one, and `host-loopback-unlisted` at a port no
//! run ever lists. The gate names both addresses in the environment, built from
//! the linked fork's own sentinel constant, so this fixture never spells the
//! address and the two cannot drift.
//!
//! The verdict for each arm is written to the file named by
//! `SOCKPROBE_REPORT_PATH` (a mounted host-path volume — the proof report-file
//! pattern), and echoed to stderr. `denied` is the ONLY token the negative
//! asserts on; the positive accepts stable permitted tokens — so neither
//! assertion depends on the exact non-deny error.

use std::io::Write as _;
use std::net::{SocketAddr, TcpStream, UdpSocket};

const TARGET_ENV: &str = "SOCKPROBE_NON_LOOPBACK_TARGET";
const HOST_LOOPBACK_LISTED_ENV: &str = "SOCKPROBE_HOST_LOOPBACK_LISTED";
const HOST_LOOPBACK_UNLISTED_ENV: &str = "SOCKPROBE_HOST_LOOPBACK_UNLISTED";

/// The raw-egress denial the policy raises: `access-denied` on the WIT side maps
/// to `PermissionDenied` in `std` (fork `network.rs` `error_code_from_io`).
fn is_denied(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::PermissionDenied
}

/// Attempt a raw outbound TCP connect; classify the policy verdict.
fn tcp_connect_verdict(target: SocketAddr) -> &'static str {
    match TcpStream::connect(target) {
        Ok(_) => "connected",
        Err(e) if is_denied(&e) => "denied",
        Err(_) => "allowed-failed",
    }
}

/// Attempt `UdpConnect` against a non-loopback address. A service's loopback
/// bind is permitted regardless of the raw-egress opt-in.
fn udp_connect_verdict(target: SocketAddr) -> &'static str {
    let sock = match UdpSocket::bind("127.0.0.1:0") {
        Ok(s) => s,
        Err(_) => return "bind-failed",
    };
    match sock.connect(target) {
        Err(e) if is_denied(&e) => "denied",
        Err(_) => "allowed-failed",
        Ok(()) => "connected",
    }
}

/// Attempt `UdpOutgoingDatagram` on an unconnected socket against a
/// non-loopback address.
fn udp_outgoing_datagram_verdict(target: SocketAddr) -> &'static str {
    let sock = match UdpSocket::bind("127.0.0.1:0") {
        Ok(s) => s,
        Err(_) => return "bind-failed",
    };
    match sock.send_to(b"x", target) {
        Ok(_) => "sent",
        Err(e) if is_denied(&e) => "denied",
        Err(_) => "allowed-failed",
    }
}

fn udp_bind_verdict(address: SocketAddr) -> &'static str {
    match UdpSocket::bind(address) {
        Ok(_) => "bound",
        Err(e) if is_denied(&e) => "denied",
        Err(_) => "bind-failed",
    }
}

/// Attempt a raw outbound TCP connect to the host sentinel address the gate
/// names in `var` — the machine's own loopback, reached through the fork's
/// `host.wasmcloud.internal` zone rather than through `127.0.0.1` (which means
/// the guest's own virtual network).
///
/// `no-target` when the gate named no address: a token no assertion accepts, so
/// an unset variable can never read as a policy verdict.
fn host_loopback_verdict(var: &str) -> &'static str {
    match std::env::var(var)
        .ok()
        .and_then(|value| value.parse::<SocketAddr>().ok())
    {
        Some(target) => tcp_connect_verdict(target),
        None => "no-target",
    }
}

fn main() {
    let Ok(target) = std::env::var(TARGET_ENV)
        .ok()
        .and_then(|value| value.parse::<SocketAddr>().ok())
        .ok_or(())
    else {
        let _ = std::io::stderr().write_all(b"invalid SOCKPROBE_NON_LOOPBACK_TARGET\n");
        return;
    };
    if target.ip().is_loopback() || target.ip().is_unspecified() {
        let _ = std::io::stderr().write_all(b"SOCKPROBE_NON_LOOPBACK_TARGET is not non-loopback\n");
        return;
    }

    let loopback_bind = SocketAddr::from(([127, 0, 0, 1], 0));
    let non_loopback_bind = SocketAddr::new(target.ip(), 0);
    let out = format!(
        "tcp-connect={}\nudp-connect={}\nudp-outgoing-datagram={}\n\
         udp-bind-loopback={}\nudp-bind-non-loopback={}\n\
         host-loopback-listed={}\nhost-loopback-unlisted={}\n",
        tcp_connect_verdict(target),
        udp_connect_verdict(target),
        udp_outgoing_datagram_verdict(target),
        udp_bind_verdict(loopback_bind),
        udp_bind_verdict(non_loopback_bind),
        host_loopback_verdict(HOST_LOOPBACK_LISTED_ENV),
        host_loopback_verdict(HOST_LOOPBACK_UNLISTED_ENV),
    );
    if let Ok(path) = std::env::var("SOCKPROBE_REPORT_PATH") {
        let _ = std::fs::write(&path, &out);
    }
    // Host WasiCtx inherits stderr; the report file is the assertable channel.
    let _ = std::io::stderr().write_all(out.as_bytes());
}
