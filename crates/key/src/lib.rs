//! Local file and Cloud KMS keys for envelope encryption.

use std::{os::unix::fs::PermissionsExt, path::Path, sync::Arc, time::Duration};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use sha2::{Digest as _, Sha256};
use wamn_envelope::encryption::{Configuration, Envelope, Error, Key, NONCE_BYTES, seal, unseal};
use wamn_gcp_metadata::Metadata;
use wamn_http_client::HttpClient;
use zeroize::Zeroizing;

struct FileKey {
    key: Zeroizing<[u8; 32]>,
    id: String,
}
struct Kms {
    client: HttpClient,
    metadata: Arc<Metadata>,
    endpoint: String,
    name: String,
}
enum Backend {
    File(FileKey),
    Kms(Kms),
}

/// Open the configured key for envelope encryption.
pub fn open(configuration: &Configuration) -> Result<Envelope, Error> {
    match configuration {
        Configuration::Local => {
            let path = std::env::var_os("WAMN_OAUTH_KEY_FILE")
                .ok_or_else(|| Error::new("open", "WAMN_OAUTH_KEY_FILE is absent"))?;
            from_file(Path::new(&path))
        }
        Configuration::Gcp { project, region } => {
            Ok(Envelope::new(Box::new(Backend::Kms(kms(project, region)?))))
        }
    }
}

/// Open a local key shared by all hosts of one target.
pub fn from_file(path: &Path) -> Result<Envelope, Error> {
    Ok(Envelope::new(Box::new(Backend::File(file_key(path)?))))
}

/// Open the local envelope key read from the environment's Kubernetes Secret.
pub fn from_secret(key: &[u8]) -> Result<Envelope, Error> {
    let key = data_key(key).map_err(|_| Error::new("open", "local key must contain 32 bytes"))?;
    Ok(Envelope::new(Box::new(Backend::File(local_key(key)))))
}

fn kms(project: &str, region: &str) -> Result<Kms, Error> {
    if ![project, region].iter().all(|value| {
        !value.is_empty()
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    }) {
        return Err(Error::new("open", "invalid KMS project or region"));
    }
    let metadata =
        Metadata::shared().map_err(|_| Error::new("open", "pod identity client unavailable"))?;
    Ok(Kms {
        client: HttpClient::shared().map_err(|_| Error::new("open", "KMS client unavailable"))?,
        metadata,
        endpoint: "https://cloudkms.googleapis.com/v1".into(),
        name: format!("projects/{project}/locations/{region}/keyRings/wamn/cryptoKeys/oauth-token"),
    })
}

#[async_trait::async_trait]
impl Key for Backend {
    async fn wrap(&self, key: &[u8; 32]) -> Result<(String, Vec<u8>), Error> {
        match self {
            Self::File(file) => {
                let (nonce, ciphertext) = seal(&file.key, key)?;
                Ok((file.id.clone(), [nonce, ciphertext].concat()))
            }
            Self::Kms(kms) => {
                let token = kms.token("encrypt").await?;
                kms.wrap(key, &token).await
            }
        }
    }

    async fn unwrap(&self, id: &str, wrapped: &[u8]) -> Result<Zeroizing<[u8; 32]>, Error> {
        let value = match self {
            Self::File(file) => {
                if id != file.id || wrapped.len() < NONCE_BYTES {
                    return Err(Error::new("unwrap", "envelope key does not match"));
                }
                let (nonce, ciphertext) = wrapped.split_at(NONCE_BYTES);
                unseal(&file.key, nonce, ciphertext)?
            }
            Self::Kms(kms) => {
                if !valid_version(&kms.name, id) {
                    return Err(Error::new("unwrap", "envelope key does not match"));
                }
                let token = kms.token("decrypt").await?;
                kms.unwrap(wrapped, &token).await?
            }
        };
        data_key(&value)
    }
}

fn file_key(path: &Path) -> Result<FileKey, Error> {
    use std::io::Read as _;
    let mut file =
        std::fs::File::open(path).map_err(|_| Error::new("open", "key file unavailable"))?;
    let metadata = file
        .metadata()
        .map_err(|_| Error::new("open", "key file metadata unavailable"))?;
    // The kubelet adds group read for the pod fsGroup, so group read is
    // allowed. Special bits, owner execute, group write or execute and any
    // other access refuse, and the owner must read.
    let mode = metadata.permissions().mode();
    if !metadata.is_file() || mode & 0o7137 != 0 || mode & 0o400 == 0 || metadata.len() != 32 {
        return Err(Error::new(
            "open",
            "key file must contain 32 bytes and allow only owner read and write and group read",
        ));
    }
    let mut key = Zeroizing::new([0; 32]);
    file.read_exact(key.as_mut())
        .map_err(|_| Error::new("open", "key file unavailable"))?;
    Ok(local_key(key))
}

fn local_key(key: Zeroizing<[u8; 32]>) -> FileKey {
    let id = format!("file:{}", &hex::encode(Sha256::digest(key.as_ref()))[..16]);
    FileKey { key, id }
}
fn data_key(bytes: &[u8]) -> Result<Zeroizing<[u8; 32]>, Error> {
    bytes
        .try_into()
        .map(Zeroizing::new)
        .map_err(|_| Error::new("unwrap", "invalid data key length"))
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
                .ok_or_else(|| Error::new("KMS", "missing response field"))?,
        )
        .map_err(|_| Error::new("KMS", "invalid response encoding"))
}
impl Kms {
    async fn token(&self, operation: &'static str) -> Result<Arc<str>, Error> {
        self.metadata
            .token()
            .await
            .map_err(|_| Error::new(operation, "pod identity unavailable"))
    }

    async fn wrap(&self, key: &[u8; 32], token: &str) -> Result<(String, Vec<u8>), Error> {
        let reply = self
            .request(
                "encrypt",
                serde_json::json!({"plaintext": STANDARD.encode(key)}),
                token,
            )
            .await?;
        let id = reply["name"]
            .as_str()
            .filter(|id| valid_version(&self.name, id))
            .ok_or_else(|| Error::new("wrap", "KMS returned no matching key version"))?;
        Ok((id.to_owned(), decode(&reply, "ciphertext")?))
    }

    async fn unwrap(&self, wrapped: &[u8], token: &str) -> Result<Zeroizing<Vec<u8>>, Error> {
        let reply = self
            .request(
                "decrypt",
                serde_json::json!({"ciphertext": STANDARD.encode(wrapped)}),
                token,
            )
            .await?;
        Ok(Zeroizing::new(decode(&reply, "plaintext")?))
    }

    async fn request(
        &self,
        operation: &'static str,
        body: serde_json::Value,
        token: &str,
    ) -> Result<serde_json::Value, Error> {
        let request = hyper::Request::post(format!("{}/{}:{operation}", self.endpoint, self.name))
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(
                serde_json::to_vec(&body)
                    .map_err(|_| Error::new(operation, "invalid KMS request"))?,
            ))
            .map_err(|_| Error::new(operation, "invalid KMS request"))?;
        let reply = self
            .client
            .send(request, Duration::from_secs(10), 65_536)
            .await
            .map_err(|_| Error::new(operation, "KMS request failed"))?;
        if !reply.status().is_success() {
            return Err(Error::new(operation, "KMS refused request"));
        }
        serde_json::from_slice(reply.body())
            .map_err(|_| Error::new(operation, "invalid KMS response"))
    }
}

#[cfg(test)]
mod tests;
