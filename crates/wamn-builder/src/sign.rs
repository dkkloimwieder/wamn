//! 5.5d — artifact provenance: an ed25519 detached signature over
//! `sha256(wasm)`, recorded as OCI annotations at push.
//!
//! GREENFIELD: the existing HMAC in `wamn-node-invoke` is runner→node MESSAGE
//! auth, not artifact provenance; there is no cosign/sigstore in the tree. We use
//! `ring`'s `Ed25519KeyPair` (already resolved in the workspace lock via the TLS
//! stack — no new heavy dep, not a re-implementation of the primitive). The
//! signed message is the raw `sha256(wasm)` digest; verification recomputes it,
//! so a signature binds the exact artifact bytes.
//!
//! Keys are hex-encoded text (Secret/ConfigMap-friendly): the private key is the
//! PKCS#8 document, the public key the 32-byte raw ed25519 key.

use anyhow::{Context as _, anyhow};
use ring::rand::SystemRandom;
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};
use sha2::{Digest, Sha256};

/// The OCI annotation carrying the hex ed25519 signature over `sha256(wasm)`.
pub const SIGNATURE_ANNOTATION: &str = "wamn.node.signature";
/// The OCI annotation carrying the signed digest (`sha256:<hex>`).
pub const SIGNED_DIGEST_ANNOTATION: &str = "wamn.node.signed-digest";
/// The OCI annotation carrying the signer's hex ed25519 public key (the
/// verification fingerprint).
pub const PUBLIC_KEY_ANNOTATION: &str = "wamn.node.public-key";

/// The `sha256(wasm)` hex digest (no `sha256:` prefix) — the signed message.
pub fn artifact_digest_hex(wasm: &[u8]) -> String {
    hex::encode(Sha256::digest(wasm))
}

/// A loaded ed25519 signing keypair (from a PKCS#8 document).
pub struct SigningKey {
    keypair: Ed25519KeyPair,
}

impl SigningKey {
    /// Generate a fresh keypair; returns it plus the PKCS#8 document bytes (the
    /// private material to bank in the Secret).
    pub fn generate() -> anyhow::Result<(Self, Vec<u8>)> {
        let rng = SystemRandom::new();
        let pkcs8 =
            Ed25519KeyPair::generate_pkcs8(&rng).map_err(|e| anyhow!("ed25519 keygen: {e}"))?;
        let keypair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref())
            .map_err(|e| anyhow!("load generated keypair: {e}"))?;
        Ok((Self { keypair }, pkcs8.as_ref().to_vec()))
    }

    /// Load a keypair from a hex-encoded PKCS#8 document.
    pub fn from_pkcs8_hex(pkcs8_hex: &str) -> anyhow::Result<Self> {
        let pkcs8 = hex::decode(pkcs8_hex.trim()).context("decode hex PKCS#8 private key")?;
        let keypair = Ed25519KeyPair::from_pkcs8(&pkcs8)
            .map_err(|e| anyhow!("load ed25519 keypair from PKCS#8: {e}"))?;
        Ok(Self { keypair })
    }

    /// Read a hex-PKCS#8 private key file and load the keypair.
    pub async fn from_file(path: &std::path::Path) -> anyhow::Result<Self> {
        let hexed = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("read signing key {}", path.display()))?;
        Self::from_pkcs8_hex(&hexed)
    }

    /// The signer's public key, hex-encoded (the verification fingerprint).
    pub fn public_key_hex(&self) -> String {
        hex::encode(self.keypair.public_key().as_ref())
    }

    /// Sign `sha256(wasm)`; returns the hex signature.
    pub fn sign_artifact(&self, wasm: &[u8]) -> String {
        let digest = Sha256::digest(wasm);
        hex::encode(self.keypair.sign(&digest).as_ref())
    }
}

/// Verify a hex ed25519 `signature` over `sha256(wasm)` against a hex
/// `public_key`. `Err` on a decode failure OR a signature mismatch — the
/// mutation-(c) target: neutering this admits an unsigned / tampered artifact.
pub fn verify_artifact(
    public_key_hex: &str,
    wasm: &[u8],
    signature_hex: &str,
) -> anyhow::Result<()> {
    let public_key = hex::decode(public_key_hex.trim()).context("decode hex public key")?;
    let signature = hex::decode(signature_hex.trim()).context("decode hex signature")?;
    let digest = Sha256::digest(wasm);
    UnparsedPublicKey::new(&ED25519, public_key)
        .verify(&digest, &signature)
        .map_err(|_| {
            anyhow!("ed25519 signature verification FAILED (wrong key or tampered artifact)")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_then_verify_round_trips() {
        let (key, _pkcs8) = SigningKey::generate().unwrap();
        let wasm = b"\x00asm\x0d\x00\x01\x00the-node";
        let sig = key.sign_artifact(wasm);
        let pk = key.public_key_hex();
        assert!(verify_artifact(&pk, wasm, &sig).is_ok());
    }

    #[test]
    fn verify_rejects_a_tampered_artifact() {
        let (key, _) = SigningKey::generate().unwrap();
        let sig = key.sign_artifact(b"original-bytes");
        let pk = key.public_key_hex();
        // Same signature, DIFFERENT bytes -> the recomputed digest differs.
        assert!(verify_artifact(&pk, b"tampered-bytes", &sig).is_err());
    }

    #[test]
    fn verify_rejects_a_wrong_key() {
        let (key_a, _) = SigningKey::generate().unwrap();
        let (key_b, _) = SigningKey::generate().unwrap();
        let wasm = b"the-node";
        let sig = key_a.sign_artifact(wasm);
        // key_b did not sign it.
        assert!(verify_artifact(&key_b.public_key_hex(), wasm, &sig).is_err());
    }

    #[test]
    fn pkcs8_hex_round_trips_the_key() {
        let (key, pkcs8) = SigningKey::generate().unwrap();
        let reloaded = SigningKey::from_pkcs8_hex(&hex::encode(&pkcs8)).unwrap();
        // Same key material -> same public key + verifiable signatures.
        assert_eq!(key.public_key_hex(), reloaded.public_key_hex());
        let wasm = b"node-bytes";
        assert!(
            verify_artifact(&reloaded.public_key_hex(), wasm, &key.sign_artifact(wasm)).is_ok()
        );
    }
}
