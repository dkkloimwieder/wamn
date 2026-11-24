//! Exact authorities accepted by the wasmCloud operator.

use wamn_connection::administration::ConnectionError;
use wamn_connection::administration::refuse;

/// Read the fixed socket port of an exact host that the operator can admit.
pub fn wasm_authority_port(value: &str) -> Result<u16, ConnectionError> {
    use wash_runtime::host::allowed_hosts::AllowedHost;

    if value.contains('@') || value.trim() != value {
        return Err(refuse("invalid_input", "authority"));
    }
    let authority = value
        .parse::<AllowedHost>()
        .map_err(|_| refuse("invalid_input", "authority"))?;
    let host = match &authority {
        AllowedHost::Authority(host) => host.host(),
        AllowedHost::Url(url) => url
            .host_str()
            .ok_or_else(|| refuse("invalid_input", "authority"))?,
        _ => return Err(refuse("invalid_input", "authority")),
    };
    if host.contains(':') {
        return Err(refuse("ipv6_authority_not_supported", "authority"));
    }
    authority
        .socket_port()
        .filter(|&port| port != 0)
        .ok_or_else(|| refuse("invalid_input", "authority"))
}
