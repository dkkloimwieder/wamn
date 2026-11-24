//! Stock wasmCloud host matching for connection HTTP calls.

use http::Uri;
use wamn_authority::connection_authority::{CanonicalAuthority, HostPolicy};
use wash_runtime::host::allowed_hosts::AllowedHost;

/// Applies wasmCloud host matching to a connection's exact authority.
pub static CONNECTION_HOST_POLICY: WasmHostPolicy<'static> = WasmHostPolicy(None);

/// Applies the stock host rules to an HTTP target.
#[derive(Debug)]
pub struct WasmHostPolicy<'a>(Option<&'a [AllowedHost]>);

impl HostPolicy for WasmHostPolicy<'_> {
    fn allows(&self, authority: &CanonicalAuthority, target: &Uri) -> bool {
        if let Some(entries) = self.0 {
            return entries.iter().any(|entry| entry.matches(target));
        }
        let host = authority.host();
        let port = authority.port();
        let explicit = if host.contains(':') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        };
        AllowedHost::Authority(explicit.parse().expect("canonical connection authority"))
            .matches(target)
    }
}

#[cfg(test)]
mod tests;
