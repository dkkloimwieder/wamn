//! `docker-credential-wamn`: a Docker credential helper that answers with a
//! token from the GKE metadata server.
//!
//! wash-runtime pulls a workload's components through `docker_credential`,
//! which runs `docker-credential-<name> get` for a registry named under
//! `credHelpers` in `$DOCKER_CONFIG/config.json`. This helper answers `get`
//! with the one token function of the host's own readers,
//! [`read_metadata_registry_credentials`], so both pull paths use one token
//! source. It never answers the username `<token>`, which wash-runtime refuses
//! as an identity token. It supports no other command.

use std::io::{Read as _, Write as _};
use std::process::ExitCode;

use wamn_runtime::registry_credentials::read_metadata_registry_credentials;

fn main() -> ExitCode {
    if std::env::args().nth(1).as_deref() != Some("get") {
        eprintln!("docker-credential-wamn supports only get");
        return ExitCode::FAILURE;
    }
    let mut server = String::new();
    if let Err(error) = std::io::stdin().read_to_string(&mut server) {
        eprintln!("docker-credential-wamn: read the registry from standard input: {error}");
        return ExitCode::FAILURE;
    }
    let server = server.trim();
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("docker-credential-wamn: start the runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    let credentials = match runtime.block_on(read_metadata_registry_credentials(server)) {
        Ok(credentials) => credentials,
        Err(error) => {
            eprintln!("docker-credential-wamn: {error}");
            return ExitCode::FAILURE;
        }
    };
    let answer = serde_json::json!({
        "ServerURL": server,
        "Username": credentials.username(),
        "Secret": credentials.password(),
    });
    let mut stdout = std::io::stdout();
    if serde_json::to_writer(&mut stdout, &answer).is_err() || stdout.flush().is_err() {
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
