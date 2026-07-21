//! 5.5d — the `keygen` verb: generate an ed25519 signing keypair for the
//! builder. The hex-PKCS#8 PRIVATE key is banked in a K8s Secret (the main loop
//! creates the real one; `deploy/platform/builder-signing-key.yaml` documents the
//! shape); the hex PUBLIC key is the `buildproof` verification fingerprint.

use std::path::PathBuf;

use anyhow::Context as _;
use clap::Args;

use crate::sign::SigningKey;

#[derive(Args, Debug)]
pub struct KeygenArgs {
    /// Path to write the hex-PKCS#8 ed25519 PRIVATE key (bank in the Secret).
    #[arg(long)]
    pub private_key: PathBuf,

    /// Path to write the hex ed25519 PUBLIC key (the buildproof verification key).
    #[arg(long)]
    pub public_key: PathBuf,
}

pub async fn run(args: KeygenArgs) -> anyhow::Result<()> {
    let (key, pkcs8) = SigningKey::generate()?;
    let public_hex = key.public_key_hex();

    tokio::fs::write(&args.private_key, hex::encode(&pkcs8))
        .await
        .with_context(|| format!("write private key {}", args.private_key.display()))?;
    tokio::fs::write(&args.public_key, &public_hex)
        .await
        .with_context(|| format!("write public key {}", args.public_key.display()))?;

    println!(
        "generated ed25519 keypair: private {} / public {} (fingerprint {public_hex})",
        args.private_key.display(),
        args.public_key.display()
    );
    Ok(())
}
