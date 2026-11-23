use super::*;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

fn local(bytes: &[u8], mode: u32) -> Result<Envelope, Error> {
    use std::io::Write as _;
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(bytes).unwrap();
    file.as_file()
        .set_permissions(std::fs::Permissions::from_mode(mode))
        .unwrap();
    from_file(file.path())
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
    relabeled.key_id = other.encrypt(b"private-token").await.unwrap().key_id;
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
async fn kms_rest_wraps_and_unwraps_with_bearer_and_preserves_the_version() {
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
                assert_eq!(request["ciphertext"], serde_json::json!(STANDARD.encode(b"kms-wrapped-key")));
                serde_json::json!({"plaintext": data_key})
            }.to_string();
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{reply}", reply.len()).as_bytes()).await.unwrap();
        }
    });
    let mut kms = kms("test", "us-central1").unwrap();
    kms.endpoint = format!("http://{address}/v1");
    let token = "fixture-pod-token";
    let data_key = [7; 32];
    let (version, wrapped) = kms.wrap(&data_key, token).await.unwrap();
    assert_eq!(version, format!("{name}/cryptoKeyVersions/7"));
    assert_eq!(
        kms.unwrap(&wrapped, token).await.unwrap().as_slice(),
        data_key
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
    let Configuration::Gcp { project, region } = configuration else {
        panic!("KMS configuration required")
    };
    let kms = kms(&project, &region).unwrap();
    assert_eq!(
        kms.name,
        "projects/test/locations/us-central1/keyRings/wamn/cryptoKeys/oauth-token"
    );
}
