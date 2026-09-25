//! The edge key source: a key set read once from a local file.
//!
//! The file is a [`SessionJwks`] document, the same shape the issuer serves.
//! The configuration names the issuer. A new key takes effect after a restart,
//! so the evidence of a loaded key is always fresh.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

use crate::keys::{PublicSessionKey, SessionJwks, decode_public_key};
use crate::token::refused;
use crate::verifier::{KeyEvidence, KeySource};
use crate::{SessionError, SessionErrorKind};

/// An issuer's public keys, read from a file at start.
#[derive(Clone, Debug)]
pub struct FileKeys {
    issuer: Arc<str>,
    keys: Arc<BTreeMap<String, FileKey>>,
}

impl FileKeys {
    /// Read the key set at `path` for `issuer`.
    ///
    /// Refuses an empty issuer, an unreadable or unknown document, a key off
    /// the Ed25519 profile, and a repeated key ID.
    pub fn load(path: impl AsRef<Path>, issuer: &str) -> Result<Self, SessionError> {
        if issuer.trim().is_empty() {
            return Err(key_file("session issuer must be nonempty"));
        }
        let path = path.as_ref();
        let bytes = std::fs::read(path)
            .map_err(|error| key_file(format!("read {}: {error}", path.display())))?;
        let set: SessionJwks = serde_json::from_slice(&bytes)
            .map_err(|error| key_file(format!("parse {}: {error}", path.display())))?;
        let mut keys = BTreeMap::new();
        for key in set.keys {
            decode_public_key(&key)?;
            let kid = key.kid.clone();
            if keys.insert(kid.clone(), FileKey(key)).is_some() {
                return Err(SessionError::new(
                    SessionErrorKind::InvalidKey,
                    format!("key ID {kid} repeats in {}", path.display()),
                ));
            }
        }
        Ok(Self {
            issuer: issuer.into(),
            keys: Arc::new(keys),
        })
    }
}

#[async_trait]
impl KeySource for FileKeys {
    type Evidence = FileKey;
    type Error = SessionError;

    fn issuer(&self) -> &str {
        &self.issuer
    }

    async fn key(&self, kid: &str) -> Result<FileKey, SessionError> {
        self.keys.get(kid).cloned().ok_or_else(refused)
    }
}

/// A key from the file. It stays fresh for the life of the process.
#[derive(Clone, Debug)]
pub struct FileKey(PublicSessionKey);

impl KeyEvidence for FileKey {
    fn public_key(&self) -> &PublicSessionKey {
        &self.0
    }

    fn is_fresh(&self) -> bool {
        true
    }
}

fn key_file(message: impl Into<Box<str>>) -> SessionError {
    SessionError::new(SessionErrorKind::KeyFile, message)
}
