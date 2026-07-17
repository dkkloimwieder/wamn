//! The direct-import credential-vault THREAT fixture (cjv.3).
//!
//! It imports `wamn:node/credentials` DIRECTLY (not through the SDK `CapsCtx`
//! facade the trusted flow-runner uses) and calls `get(name)` on whatever name
//! the gate asks for — mirroring exactly what an untrusted custom node
//! (wamn-bd5) can do at the real WIT boundary. It has NO way to grant itself
//! (it does not import the trusted `wamn:runner/credentials` channel), so the
//! host's per-execution grant is the only thing standing between it and every
//! secret in its project. The `credprobe` gate registers a narrow grant
//! host-side and asserts an ungranted / unregistered-project `get` is refused.

wit_bindgen::generate!({
    world: "cred-probe",
    path: "wit",
    generate_all,
});

use wamn::node::credentials::{self, CredentialError};

struct Component;
export!(Component);

impl Guest for Component {
    fn probe(name: String) -> String {
        match credentials::get(&name) {
            Ok(secret) => format!("ok:{secret}"),
            Err(CredentialError::NotGranted) => "err:not-granted".to_string(),
            Err(CredentialError::NotFound) => "err:not-found".to_string(),
            Err(CredentialError::Unavailable) => "err:unavailable".to_string(),
        }
    }
}
