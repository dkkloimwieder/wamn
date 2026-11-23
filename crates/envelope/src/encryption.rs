//! Envelope encryption for signing keys, OAuth tokens, and pending PKCE verifiers.

use std::{collections::VecDeque, fmt};

use ring::{
    aead,
    rand::{SecureRandom as _, SystemRandom},
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use zeroize::Zeroizing;

const CACHE_ENTRIES: usize = 1024;
/// Byte length of each AES-GCM nonce.
pub const NONCE_BYTES: usize = 12;

/// Deployment configuration shared by signing and OAuth consumers.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "provider", rename_all = "snake_case", deny_unknown_fields)]
pub enum Configuration {
    /// Read the envelope key from WAMN_OAUTH_KEY_FILE.
    Local,
    /// Use the regional wamn/oauth-token Cloud KMS key and pod identity.
    Gcp { project: String, region: String },
}

/// An encryption failure with fixed operation context and a safe source.
#[derive(Debug)]
pub struct Error {
    pub operation: &'static str,
    source: &'static str,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "envelope encryption {} failed: {}",
            self.operation, self.source
        )
    }
}
impl std::error::Error for Error {}
impl Error {
    /// Create a failure with fixed context and no secret data.
    pub fn new(operation: &'static str, source: &'static str) -> Self {
        Self { operation, source }
    }
}

/// Encrypted material and its wrapped data key. Debug omits the stored bytes.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Encrypted {
    pub ciphertext: Vec<u8>,
    pub nonce: Vec<u8>,
    pub wrapped_key: Vec<u8>,
    pub key_id: String,
}
impl fmt::Debug for Encrypted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Encrypted(..)")
    }
}

/// Plaintext available only through an explicit byte access.
pub struct Plaintext(Zeroizing<Vec<u8>>);
impl Plaintext {
    pub fn expose(&self) -> &[u8] {
        &self.0
    }
}
impl fmt::Debug for Plaintext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Plaintext(..)")
    }
}

/// Wrap and unwrap the random data key that protects one envelope.
#[async_trait::async_trait]
pub trait Key: Send + Sync {
    /// Wrap a data key and return the wrapping key identifier and wrapped bytes.
    async fn wrap(&self, key: &[u8; 32]) -> Result<(String, Vec<u8>), Error>;

    /// Unwrap a data key using the matching wrapping key.
    async fn unwrap(&self, id: &str, wrapped: &[u8]) -> Result<Zeroizing<[u8; 32]>, Error>;
}

struct CachedKey {
    id: String,
    wrapped: Vec<u8>,
    key: Zeroizing<[u8; 32]>,
}

/// One host's envelope backend and least recently used data keys.
pub struct Envelope {
    key: Box<dyn Key>,
    cache: Mutex<VecDeque<CachedKey>>,
}
impl fmt::Debug for Envelope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Envelope(..)")
    }
}

impl Envelope {
    /// Use the supplied wrapping key for this envelope and its data key cache.
    pub fn new(key: Box<dyn Key>) -> Self {
        Self {
            key,
            cache: Mutex::new(VecDeque::new()),
        }
    }

    /// Encrypt one row under a fresh random AES-256 data key.
    pub async fn encrypt(&self, plaintext: &[u8]) -> Result<Encrypted, Error> {
        let mut key = Zeroizing::new([0; 32]);
        random(key.as_mut())?;
        let (nonce, ciphertext) = seal(&key, plaintext)?;
        let (key_id, wrapped_key) = self.key.wrap(&key).await?;
        Ok(Encrypted {
            ciphertext,
            nonce,
            wrapped_key,
            key_id,
        })
    }

    /// Decrypt a row, with at most 1024 unwrapped data keys in this host.
    pub async fn decrypt(&self, row: &Encrypted) -> Result<Plaintext, Error> {
        let cached = {
            let mut cache = self.cache.lock().await;
            cache
                .iter()
                .position(|entry| entry.id == row.key_id && entry.wrapped == row.wrapped_key)
                .and_then(|index| cache.remove(index))
        };
        let key = if let Some(key) = cached {
            key
        } else {
            let key = self.key.unwrap(&row.key_id, &row.wrapped_key).await?;
            CachedKey {
                id: row.key_id.clone(),
                wrapped: row.wrapped_key.clone(),
                key,
            }
        };
        let plaintext = unseal(&key.key, &row.nonce, &row.ciphertext);
        let mut cache = self.cache.lock().await;
        cache.retain(|entry| entry.id != key.id || entry.wrapped != key.wrapped);
        if cache.len() == CACHE_ENTRIES {
            cache.pop_front();
        }
        cache.push_back(key);
        plaintext.map(Plaintext)
    }
}

fn random(bytes: &mut [u8]) -> Result<(), Error> {
    SystemRandom::new()
        .fill(bytes)
        .map_err(|_| Error::new("random", "random source unavailable"))
}
fn cipher(key: &[u8; 32]) -> Result<aead::LessSafeKey, Error> {
    aead::UnboundKey::new(&aead::AES_256_GCM, key)
        .map(aead::LessSafeKey::new)
        .map_err(|_| Error::new("cipher", "invalid AES key"))
}
/// Encrypt bytes with an AES-256 data key and a fresh nonce.
pub fn seal(key: &[u8; 32], plaintext: &[u8]) -> Result<(Vec<u8>, Vec<u8>), Error> {
    let mut nonce = [0; NONCE_BYTES];
    random(&mut nonce)?;
    let mut bytes = Zeroizing::new(plaintext.to_vec());
    cipher(key)?
        .seal_in_place_append_tag(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::empty(),
            &mut *bytes,
        )
        .map_err(|_| Error::new("encrypt", "AES encryption refused"))?;
    Ok((nonce.to_vec(), std::mem::take(&mut *bytes)))
}
/// Authenticate and decrypt bytes with an AES-256 data key.
pub fn unseal(
    key: &[u8; 32],
    nonce: &[u8],
    ciphertext: &[u8],
) -> Result<Zeroizing<Vec<u8>>, Error> {
    let nonce = aead::Nonce::try_assume_unique_for_key(nonce)
        .map_err(|_| Error::new("decrypt", "invalid nonce"))?;
    let mut bytes = Zeroizing::new(ciphertext.to_vec());
    let length = cipher(key)?
        .open_in_place(nonce, aead::Aad::empty(), &mut bytes)
        .map_err(|_| Error::new("decrypt", "AES authentication refused"))?
        .len();
    bytes.truncate(length);
    Ok(bytes)
}
#[cfg(test)]
mod tests;
