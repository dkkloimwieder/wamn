//! sockprobe — E13/E15 runtime raw-socket fixture (wamn-o3u6).
//!
//! A `wasi:cli` command that ATTEMPTS raw outbound TCP + UDP egress via
//! `wasi:sockets` (reached through `std::net` on wasm32-wasip2 — the default
//! command world imports the whole `wasi:sockets` package) and reports the
//! POLICY verdict of each attempt, so the egressbench runtime phase can assert
//! the fork's `linked_call` raw-socket policy (docs/wash-runtime-fork.md, pins
//! 8b76869 E13 / eef76cd E15/E16) WITHOUT matching on error text:
//!
//!   - deny-by-default: with no `wamn.allow-raw-sockets`, `socket_addr_check`
//!     fails the connect and the guest sees `access-denied` (`std`
//!     `ErrorKind::PermissionDenied`) → verdict `denied`.
//!   - opted-in: with `wamn.allow-raw-sockets=true`, the check permits the op
//!     and the connect proceeds — then fails for an unrelated reason against a
//!     dead local port → verdict `connected` / `allowed-failed` (NOT `denied`).
//!
//! The verdict for each protocol is written as `tcp=<v>` / `udp=<v>` to the file
//! named by `SOCKPROBE_REPORT_PATH` (a mounted host-path volume — the memhog
//! report-file pattern), and echoed to stderr. `denied` is the ONLY token the
//! negative asserts on; the positive asserts NOT-`denied` — so neither
//! assertion depends on the exact non-deny error.

use std::io::Write as _;
use std::net::{TcpStream, UdpSocket};

/// A local port with (almost) nothing listening: a *permitted* connect fails
/// fast with connection-refused rather than hanging, so the opted-in positive
/// needs no timeout. `socket_addr_check` fires BEFORE the connect either way
/// (fork `host_tcp.rs`/`host_udp.rs`), so the deny-by-default negative is
/// immediate regardless of the target.
const TARGET: &str = "127.0.0.1:9";

/// The raw-egress denial the policy raises: `access-denied` on the WIT side maps
/// to `PermissionDenied` in `std` (fork `network.rs` `error_code_from_io`).
fn is_denied(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::PermissionDenied
}

/// Attempt a raw outbound TCP connect; classify the policy verdict.
fn tcp_verdict() -> &'static str {
    match TcpStream::connect(TARGET) {
        Ok(_) => "connected",
        Err(e) if is_denied(&e) => "denied",
        Err(_) => "allowed-failed",
    }
}

/// Attempt raw outbound UDP egress; classify the policy verdict. The socket must
/// bind first (a service's loopback bind is permitted regardless of the
/// raw-egress opt-in); the gated op is the connect (`UdpConnect`) / send
/// (`UdpOutgoingDatagram`) — either surfacing `access-denied` means `denied`.
fn udp_verdict() -> &'static str {
    let sock = match UdpSocket::bind("127.0.0.1:0") {
        Ok(s) => s,
        Err(_) => return "bind-failed",
    };
    match sock.connect(TARGET) {
        Err(e) if is_denied(&e) => "denied",
        Err(_) => "allowed-failed",
        Ok(()) => match sock.send(b"x") {
            Err(e) if is_denied(&e) => "denied",
            _ => "connected",
        },
    }
}

fn main() {
    let out = format!("tcp={}\nudp={}\n", tcp_verdict(), udp_verdict());
    if let Ok(path) = std::env::var("SOCKPROBE_REPORT_PATH") {
        let _ = std::fs::write(&path, &out);
    }
    // Host WasiCtx inherits stderr; the report file is the assertable channel.
    let _ = std::io::stderr().write_all(out.as_bytes());
}
