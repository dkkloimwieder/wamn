use super::*;
use std::sync::atomic::Ordering;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

fn local(bytes: &[u8], mode: u32) -> Result<Envelope, Error> {
    use std::io::Write as _;
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(bytes).unwrap();
    file.as_file()
        .set_permissions(std::fs::Permissions::from_mode(mode))
        .unwrap();
    Ok(Envelope {
        backend: Backend::File(file_key(file.path())?),
        cache: Mutex::new(VecDeque::new()),
    })
}
fn unwraps(envelope: &Envelope) -> usize {
    let Backend::File(file) = &envelope.backend else {
        panic!("file backend required")
    };
    file.unwraps.load(Ordering::Relaxed)
}

#[tokio::test]
async fn file_round_trip_uses_fresh_data_keys_and_redacts_plaintext() {
    let envelope = local(&[7; 32], 0o600).unwrap();
    let row = envelope
        .encrypt(b"private-token-and-verifier")
        .await
        .unwrap();
    let second = envelope
        .encrypt(b"private-token-and-verifier")
        .await
        .unwrap();
    assert_ne!(row.wrapped_key, second.wrapped_key);
    assert_ne!(row.ciphertext, second.ciphertext);
    assert_eq!(
        row.key_id,
        format!("file:{}", &hex::encode(Sha256::digest([7; 32]))[..16])
    );
    let plaintext = envelope.decrypt(&row).await.unwrap();
    assert_eq!(plaintext.expose(), b"private-token-and-verifier");
    for debug in [
        format!("{envelope:?}"),
        format!("{row:?}"),
        format!("{plaintext:?}"),
    ] {
        assert!(!debug.contains("private-token-and-verifier"));
    }
}

#[test]
fn key_file_requires_exact_length_and_private_permissions() {
    for (bytes, mode) in [
        (vec![7; 31], 0o600),
        (vec![7; 33], 0o600),
        (vec![7; 32], 0o644),
        (vec![7; 32], 0o660),
        (vec![7; 32], 0o700),
        (vec![7; 32], 0o604),
        (vec![7; 32], 0o040),
    ] {
        assert!(local(&bytes, mode).is_err());
    }
    // The kubelet adds group read for the pod fsGroup.
    for mode in [0o600, 0o640, 0o400, 0o440] {
        assert!(local(&[7; 32], mode).is_ok());
    }
}

#[tokio::test]
async fn wrong_envelope_key_and_tampered_columns_refuse() {
    let envelope = local(&[7; 32], 0o600).unwrap();
    let other = local(&[8; 32], 0o600).unwrap();
    let row = envelope.encrypt(b"private-token").await.unwrap();
    assert!(other.decrypt(&row).await.is_err());
    let mut relabeled = row.clone();
    let Backend::File(other_key) = &other.backend else {
        unreachable!()
    };
    relabeled.key_id = other_key.id.clone();
    assert!(other.decrypt(&relabeled).await.is_err());
    for column in ["ciphertext", "nonce", "wrapped_key", "key_id"] {
        let mut tampered = row.clone();
        match column {
            "ciphertext" => tampered.ciphertext[0] ^= 1,
            "nonce" => tampered.nonce[0] ^= 1,
            "wrapped_key" => tampered.wrapped_key[20] ^= 1,
            _ => tampered.key_id.push('0'),
        }
        let error = envelope.decrypt(&tampered).await.unwrap_err();
        assert!(!format!("{error:?} {error}").contains("private-token"));
    }
    let mut short_nonce = row;
    short_nonce.nonce.pop();
    assert!(envelope.decrypt(&short_nonce).await.is_err());
}

#[tokio::test]
async fn data_key_cache_reuses_unwraps_and_evicts_the_least_recently_used() {
    let envelope = local(&[7; 32], 0o600).unwrap();
    let mut rows = Vec::new();
    for _ in 0..CACHE_ENTRIES {
        let row = envelope.encrypt(b"token").await.unwrap();
        envelope.decrypt(&row).await.unwrap();
        rows.push(row);
    }
    assert_eq!(unwraps(&envelope), CACHE_ENTRIES);
    envelope.decrypt(&rows[0]).await.unwrap();
    assert_eq!(unwraps(&envelope), CACHE_ENTRIES);
    let extra = envelope.encrypt(b"token").await.unwrap();
    envelope.decrypt(&extra).await.unwrap();
    assert_eq!(envelope.cache.lock().await.len(), CACHE_ENTRIES);
    envelope.decrypt(&rows[0]).await.unwrap();
    assert_eq!(unwraps(&envelope), CACHE_ENTRIES + 1);
    envelope.decrypt(&rows[1]).await.unwrap();
    assert_eq!(unwraps(&envelope), CACHE_ENTRIES + 2);
}

#[derive(Debug)]
struct FixtureToken;
#[async_trait::async_trait]
impl CredentialProvider for FixtureToken {
    type Credential = GcpCredential;
    async fn get_credential(&self) -> object_store::Result<Arc<GcpCredential>> {
        Ok(Arc::new(GcpCredential {
            bearer: "fixture-pod-token".into(),
        }))
    }
}

#[tokio::test]
async fn kms_rest_wraps_and_unwraps_with_pod_identity_and_preserves_the_version() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let name = "projects/test/locations/us-central1/keyRings/wamn/cryptoKeys/oauth-token";
    let server = tokio::spawn(async move {
        let mut data_key = String::new();
        for operation in ["encrypt", "decrypt"] {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (headers, request) = read_request(&mut stream).await;
            assert!(
                headers
                    .to_ascii_lowercase()
                    .contains("authorization: bearer fixture-pod-token")
            );
            assert!(headers.starts_with(&format!("POST /v1/{name}:{operation} HTTP/1.1")));
            let reply = if operation == "encrypt" {
                data_key = request["plaintext"].as_str().unwrap().into();
                assert_eq!(request.as_object().unwrap().len(), 1);
                assert_eq!(STANDARD.decode(&data_key).unwrap().len(), 32);
                serde_json::json!({"name": format!("{name}/cryptoKeyVersions/7"), "ciphertext": STANDARD.encode(b"kms-wrapped-key")})
            } else {
                assert_eq!(request, serde_json::json!({"ciphertext": STANDARD.encode(b"kms-wrapped-key")}));
                serde_json::json!({"plaintext": data_key})
            }.to_string();
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{reply}", reply.len()).as_bytes()).await.unwrap();
        }
    });
    let envelope = Envelope {
        backend: Backend::Kms(Kms {
            client: HttpClient::shared().unwrap(),
            tokens: Arc::new(FixtureToken),
            endpoint: format!("http://{address}/v1"),
            name: name.into(),
        }),
        cache: Mutex::new(VecDeque::new()),
    };
    let row = envelope.encrypt(b"private-token").await.unwrap();
    assert_eq!(row.key_id, format!("{name}/cryptoKeyVersions/7"));
    assert_eq!(
        envelope.decrypt(&row).await.unwrap().expose(),
        b"private-token"
    );
    assert_eq!(
        envelope.decrypt(&row).await.unwrap().expose(),
        b"private-token"
    );
    server.await.unwrap();
}

async fn read_request(stream: &mut tokio::net::TcpStream) -> (String, serde_json::Value) {
    let mut bytes = Vec::new();
    let (header_end, length) = loop {
        let mut buffer = [0; 4096];
        let count = stream.read(&mut buffer).await.unwrap();
        assert_ne!(count, 0);
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
            let length: usize = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length: "))
                .unwrap()
                .parse()
                .unwrap();
            break (end + 4, length);
        }
    };
    while bytes.len() < header_end + length {
        let mut buffer = [0; 4096];
        let count = stream.read(&mut buffer).await.unwrap();
        assert_ne!(count, 0);
        bytes.extend_from_slice(&buffer[..count]);
    }
    let request = serde_json::from_slice(&bytes[header_end..]).unwrap();
    (
        String::from_utf8(bytes[..header_end].to_vec()).unwrap(),
        request,
    )
}

#[test]
fn gcp_configuration_selects_the_regional_kms_key() {
    let configuration: Configuration =
        serde_json::from_str(r#"{"provider":"gcp","project":"test","region":"us-central1"}"#)
            .unwrap();
    let envelope = Envelope::open(&configuration).unwrap();
    let Backend::Kms(kms) = envelope.backend else {
        panic!("KMS backend required")
    };
    assert_eq!(
        kms.name,
        "projects/test/locations/us-central1/keyRings/wamn/cryptoKeys/oauth-token"
    );
}

#[tokio::test]
async fn slow_kms_unwrap_allows_cached_decrypt_and_keeps_cache_bounded() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let name = "projects/test/locations/us-central1/keyRings/wamn/cryptoKeys/oauth-token";
    let key_id = format!("{name}/cryptoKeyVersions/7");
    let data_key = [9; 32];
    let (nonce, ciphertext) = seal(&data_key, b"private-token").unwrap();
    let cached_row = Encrypted {
        nonce: nonce.clone(),
        ciphertext: ciphertext.clone(),
        key_id: key_id.clone(),
        wrapped_key: 0_usize.to_be_bytes().to_vec(),
    };
    let slow_row = Encrypted {
        nonce,
        ciphertext,
        key_id: key_id.clone(),
        wrapped_key: b"slow".to_vec(),
    };
    let cache = (0..CACHE_ENTRIES)
        .map(|index| CachedKey {
            id: key_id.clone(),
            wrapped: index.to_be_bytes().to_vec(),
            key: Zeroizing::new(data_key),
        })
        .collect();
    let envelope = Arc::new(Envelope {
        backend: Backend::Kms(Kms {
            client: HttpClient::shared().unwrap(),
            tokens: Arc::new(FixtureToken),
            endpoint: format!("http://{address}/v1"),
            name: name.into(),
        }),
        cache: Mutex::new(cache),
    });
    let (first_received, first_ready) = tokio::sync::oneshot::channel();
    let (second_received, second_ready) = tokio::sync::oneshot::channel();
    let (release, resume) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let mut streams = Vec::new();
        for received in [first_received, second_received] {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (headers, request) = read_request(&mut stream).await;
            assert!(headers.starts_with(&format!("POST /v1/{name}:decrypt HTTP/1.1")));
            assert_eq!(
                request,
                serde_json::json!({"ciphertext": STANDARD.encode(b"slow")})
            );
            streams.push(stream);
            received.send(()).unwrap();
        }
        resume.await.unwrap();
        let reply = serde_json::json!({"plaintext": STANDARD.encode(data_key)}).to_string();
        for mut stream in streams {
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{reply}", reply.len()).as_bytes()).await.unwrap();
        }
    });
    let first = {
        let envelope = Arc::clone(&envelope);
        let row = slow_row.clone();
        tokio::spawn(async move { envelope.decrypt(&row).await })
    };
    tokio::time::timeout(Duration::from_secs(5), first_ready)
        .await
        .unwrap()
        .unwrap();
    let plaintext = tokio::time::timeout(Duration::from_secs(5), envelope.decrypt(&cached_row))
        .await
        .expect("a stalled KMS request must not block a cached decrypt")
        .unwrap();
    assert_eq!(plaintext.expose(), b"private-token");
    let second = {
        let envelope = Arc::clone(&envelope);
        let row = slow_row.clone();
        tokio::spawn(async move { envelope.decrypt(&row).await })
    };
    tokio::time::timeout(Duration::from_secs(5), second_ready)
        .await
        .unwrap()
        .unwrap();
    release.send(()).unwrap();
    for pending in [first, second] {
        assert_eq!(pending.await.unwrap().unwrap().expose(), b"private-token");
    }
    server.await.unwrap();
    let cache = envelope.cache.lock().await;
    assert_eq!(cache.len(), CACHE_ENTRIES);
    assert_eq!(
        cache
            .iter()
            .filter(|entry| entry.id == slow_row.key_id && entry.wrapped == slow_row.wrapped_key)
            .count(),
        1
    );
    assert!(
        cache
            .iter()
            .any(|entry| entry.wrapped == cached_row.wrapped_key)
    );
}
