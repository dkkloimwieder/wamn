//! Envelope encryption for OAuth tokens and pending PKCE verifiers.

use std::{
    collections::VecDeque, fmt, os::unix::fs::PermissionsExt, path::Path, sync::Arc, time::Duration,
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use object_store::{CredentialProvider, gcp::GcpCredential};
use ring::{
    aead,
    rand::{SecureRandom as _, SystemRandom},
};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use tokio::sync::Mutex;
use zeroize::Zeroizing;

const CACHE_ENTRIES: usize = 1024;
const NONCE_BYTES: usize = 12;

/// Deployment configuration shared by application hosts and the control host.
#[derive(Clone, Debug, Deserialize)]
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
            "OAuth encryption {} failed: {}",
            self.operation, self.source
        )
    }
}
impl std::error::Error for Error {}
fn failure(operation: &'static str, source: &'static str) -> Error {
    Error { operation, source }
}

/// The encrypted columns of either OAuth row. Debug omits the stored bytes.
#[derive(Clone)]
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

struct FileKey {
    key: Zeroizing<[u8; 32]>,
    id: String,
    #[cfg(test)]
    unwraps: std::sync::atomic::AtomicUsize,
}
struct Kms {
    client: reqwest::Client,
    tokens: Arc<dyn CredentialProvider<Credential = GcpCredential>>,
    endpoint: String,
    name: String,
}
enum Backend {
    File(FileKey),
    Kms(Kms),
}
struct CachedKey {
    id: String,
    wrapped: Vec<u8>,
    key: Zeroizing<[u8; 32]>,
}

/// One host's envelope backend and least recently used data keys.
pub struct Envelope {
    backend: Backend,
    cache: Mutex<VecDeque<CachedKey>>,
}
impl fmt::Debug for Envelope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Envelope(..)")
    }
}

impl Envelope {
    /// Open the configured backend without reading or writing any token rows.
    pub fn open(configuration: &Configuration) -> Result<Self, Error> {
        let backend = match configuration {
            Configuration::Local => {
                let path = std::env::var_os("WAMN_OAUTH_KEY_FILE")
                    .ok_or_else(|| failure("open", "WAMN_OAUTH_KEY_FILE is absent"))?;
                return Self::from_file(Path::new(&path));
            }
            Configuration::Gcp { project, region } => {
                if ![project, region].iter().all(|value| {
                    !value.is_empty()
                        && value
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                }) {
                    return Err(failure("open", "invalid KMS project or region"));
                }
                let (tokens, _) = crate::pod_identity::providers()
                    .map_err(|_| failure("open", "pod identity client unavailable"))?;
                Backend::Kms(Kms {
                    client: reqwest::Client::builder()
                        .redirect(reqwest::redirect::Policy::none())
                        .timeout(Duration::from_secs(10))
                        .build()
                        .map_err(|_| failure("open", "KMS client unavailable"))?,
                    tokens,
                    endpoint: "https://cloudkms.googleapis.com/v1".into(),
                    name: format!(
                        "projects/{project}/locations/{region}/keyRings/wamn/cryptoKeys/oauth-token"
                    ),
                })
            }
        };
        Ok(Self {
            backend,
            cache: Mutex::new(VecDeque::new()),
        })
    }

    /// Open a local key shared by all hosts of one target.
    pub fn from_file(path: &Path) -> Result<Self, Error> {
        Ok(Self {
            backend: Backend::File(file_key(path)?),
            cache: Mutex::new(VecDeque::new()),
        })
    }

    /// Encrypt one row under a fresh random AES-256 data key.
    pub async fn encrypt(&self, plaintext: &[u8]) -> Result<Encrypted, Error> {
        let mut key = Zeroizing::new([0; 32]);
        random(key.as_mut())?;
        let (nonce, ciphertext) = seal(&key, plaintext)?;
        let (key_id, wrapped_key) = match &self.backend {
            Backend::File(file) => {
                let (nonce, ciphertext) = seal(&file.key, key.as_ref())?;
                (file.id.clone(), [nonce, ciphertext].concat())
            }
            Backend::Kms(kms) => {
                let reply = kms
                    .request(
                        "encrypt",
                        serde_json::json!({"plaintext": STANDARD.encode(key.as_ref())}),
                    )
                    .await?;
                let id = reply["name"]
                    .as_str()
                    .filter(|id| valid_version(&kms.name, id))
                    .ok_or_else(|| failure("wrap", "KMS returned no matching key version"))?;
                (id.to_owned(), decode(&reply, "ciphertext")?)
            }
        };
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
            let key = match &self.backend {
                Backend::File(file) => {
                    if row.key_id != file.id || row.wrapped_key.len() < NONCE_BYTES {
                        return Err(failure("unwrap", "envelope key does not match"));
                    }
                    #[cfg(test)]
                    file.unwraps
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let value = unseal(
                        &file.key,
                        &row.wrapped_key[..NONCE_BYTES],
                        &row.wrapped_key[NONCE_BYTES..],
                    )?;
                    data_key(&value)?
                }
                Backend::Kms(kms) => {
                    if !valid_version(&kms.name, &row.key_id) {
                        return Err(failure("unwrap", "envelope key does not match"));
                    }
                    let reply = kms
                        .request(
                            "decrypt",
                            serde_json::json!({"ciphertext": STANDARD.encode(&row.wrapped_key)}),
                        )
                        .await?;
                    let value = Zeroizing::new(decode(&reply, "plaintext")?);
                    data_key(&value)?
                }
            };
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

fn file_key(path: &Path) -> Result<FileKey, Error> {
    use std::io::Read as _;
    let mut file =
        std::fs::File::open(path).map_err(|_| failure("open", "key file unavailable"))?;
    let metadata = file
        .metadata()
        .map_err(|_| failure("open", "key file metadata unavailable"))?;
    if !metadata.is_file()
        || metadata.permissions().mode() & 0o7777 != 0o600
        || metadata.len() != 32
    {
        return Err(failure(
            "open",
            "key file must contain 32 bytes and use mode 0600",
        ));
    }
    let mut key = Zeroizing::new([0; 32]);
    file.read_exact(key.as_mut())
        .map_err(|_| failure("open", "key file unavailable"))?;
    let id = format!("file:{}", &hex::encode(Sha256::digest(key.as_ref()))[..16]);
    Ok(FileKey {
        key,
        id,
        #[cfg(test)]
        unwraps: std::sync::atomic::AtomicUsize::new(0),
    })
}
fn data_key(bytes: &[u8]) -> Result<Zeroizing<[u8; 32]>, Error> {
    bytes
        .try_into()
        .map(Zeroizing::new)
        .map_err(|_| failure("unwrap", "invalid data key length"))
}
fn random(bytes: &mut [u8]) -> Result<(), Error> {
    SystemRandom::new()
        .fill(bytes)
        .map_err(|_| failure("random", "random source unavailable"))
}
fn cipher(key: &[u8; 32]) -> Result<aead::LessSafeKey, Error> {
    aead::UnboundKey::new(&aead::AES_256_GCM, key)
        .map(aead::LessSafeKey::new)
        .map_err(|_| failure("cipher", "invalid AES key"))
}
fn seal(key: &[u8; 32], plaintext: &[u8]) -> Result<(Vec<u8>, Vec<u8>), Error> {
    let mut nonce = [0; NONCE_BYTES];
    random(&mut nonce)?;
    let mut bytes = Zeroizing::new(plaintext.to_vec());
    cipher(key)?
        .seal_in_place_append_tag(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::empty(),
            &mut *bytes,
        )
        .map_err(|_| failure("encrypt", "AES encryption refused"))?;
    Ok((nonce.to_vec(), std::mem::take(&mut *bytes)))
}
fn unseal(key: &[u8; 32], nonce: &[u8], ciphertext: &[u8]) -> Result<Zeroizing<Vec<u8>>, Error> {
    let nonce = aead::Nonce::try_assume_unique_for_key(nonce)
        .map_err(|_| failure("decrypt", "invalid nonce"))?;
    let mut bytes = Zeroizing::new(ciphertext.to_vec());
    let length = cipher(key)?
        .open_in_place(nonce, aead::Aad::empty(), &mut bytes)
        .map_err(|_| failure("decrypt", "AES authentication refused"))?
        .len();
    bytes.truncate(length);
    Ok(bytes)
}
fn valid_version(name: &str, version: &str) -> bool {
    version
        .strip_prefix(name)
        .and_then(|tail| tail.strip_prefix("/cryptoKeyVersions/"))
        .is_some_and(|version| {
            !version.is_empty() && version.bytes().all(|byte| byte.is_ascii_digit())
        })
}
fn decode(reply: &serde_json::Value, field: &str) -> Result<Vec<u8>, Error> {
    STANDARD
        .decode(
            reply[field]
                .as_str()
                .ok_or_else(|| failure("KMS", "missing response field"))?,
        )
        .map_err(|_| failure("KMS", "invalid response encoding"))
}
impl Kms {
    async fn request(
        &self,
        operation: &'static str,
        body: serde_json::Value,
    ) -> Result<serde_json::Value, Error> {
        let token = self
            .tokens
            .get_credential()
            .await
            .map_err(|_| failure(operation, "pod identity unavailable"))?;
        let reply = self
            .client
            .post(format!("{}/{}:{operation}", self.endpoint, self.name))
            .bearer_auth(&token.bearer)
            .json(&body)
            .send()
            .await
            .map_err(|_| failure(operation, "KMS request failed"))?;
        if !reply.status().is_success() {
            return Err(failure(operation, "KMS refused request"));
        }
        reply
            .json()
            .await
            .map_err(|_| failure(operation, "invalid KMS response"))
    }
}

#[cfg(test)]
mod tests;
