//! Write the identity journey's chart inputs through the production builder.

use std::path::Path;

use anyhow::{Context as _, ensure};
use wamn_control::platform::{identity, journey};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    ensure!(
        args.len() >= 9,
        "expected chart output release namespace image issuer database-secret tls-secret foundation|session [target ...]"
    );
    let mode = match args[8].as_str() {
        "foundation" => {
            ensure!(args.len() == 9, "foundation mode has no session targets");
            identity::Mode::Foundation
        }
        "session" => {
            ensure!(args.len() > 9, "session mode needs target Secrets");
            identity::Mode::Session {
                targets: args[9..].to_vec(),
                nats: None,
            }
        }
        mode => anyhow::bail!("unknown identity mode {mode}"),
    };
    let entry = journey::identity(&identity::ChartInput {
        chart: Path::new(&args[0]),
        name: &args[2],
        namespace: &args[3],
        image: &args[4],
        pull_policy: "Never",
        issuer: &args[5],
        database_secret: &args[6],
        tls_secret: &args[7],
        operator_ca_secret: "",
        resend_secret: "",
        resend_from: "",
        invite_url: "",
        mode,
    })?;
    std::fs::write(&args[1], serde_yaml::to_string(&entry["values"])?)
        .context("write identity chart values")?;
    Ok(())
}
