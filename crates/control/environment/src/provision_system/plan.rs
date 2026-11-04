use super::{NonZeroU32, STANDARD, digest, hmac, pbkdf2};
use base64::Engine as _;

/// The SCRAM-SHA-256 verifier that PostgreSQL stores for `password`
/// (RFC 7677, PostgreSQL's default 4096 iterations). The password is ASCII
/// hex, so SASLprep leaves it unchanged.
pub(super) fn scram_sha256_verifier(password: &str, salt: &[u8]) -> String {
    const ITERATIONS: u32 = 4096;
    let mut salted = [0_u8; 32];
    pbkdf2::derive(
        pbkdf2::PBKDF2_HMAC_SHA256,
        NonZeroU32::new(ITERATIONS).expect("4096 is not zero"),
        salt,
        password.as_bytes(),
        &mut salted,
    );
    let key = hmac::Key::new(hmac::HMAC_SHA256, &salted);
    let client_key = hmac::sign(&key, b"Client Key");
    let stored_key = digest::digest(&digest::SHA256, client_key.as_ref());
    let server_key = hmac::sign(&key, b"Server Key");
    format!(
        "SCRAM-SHA-256${ITERATIONS}:{}${}:{}",
        STANDARD.encode(salt),
        STANDARD.encode(stored_key.as_ref()),
        STANDARD.encode(server_key.as_ref())
    )
}
