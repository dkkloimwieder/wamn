//! Canonical HTTP operation fingerprints for `stable-key-dedup-v1`.
//!
//! The codec is deliberately portable and environment-independent. Its only
//! inputs are the canonical method, connection-relative target, semantic
//! headers selected by the HTTP contract, and a SHA-256 body digest. Endpoint,
//! binding, credential, generation, evidence, and resolved network address are
//! absent from the input type.
//!
//! Normalization is deterministic: method and header names use uppercase and
//! lowercase ASCII respectively; header order is lexical; surrounding header
//! OWS is removed; and percent encodings use uppercase hex while unreserved
//! bytes are decoded. Portable targets use a single leading `/`; the sole
//! normalizer strips it before the connection-relative target enters this
//! codec and the authority resolver. Ambiguous dot segments, authorities, or
//! encoded path separators fail closed. Duplicate semantic header names fail
//! rather than relying on protocol-specific joining. Transport tracing metadata
//! (`traceparent`) is not an operation-semantic header.

use sha2::{Digest as _, Sha256};

/// The domain and format version included in every canonical preimage.
pub const HTTP_OPERATION_FINGERPRINT_VERSION: &str = "stable-key-dedup-v1";

const DOMAIN: &[u8] = b"wamn:connection/http-operation-fingerprint\0";
const FORBIDDEN_SEMANTIC_HEADERS: &[&str] = &[
    "authorization",
    "connection",
    "content-length",
    "host",
    "idempotency-key",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// A portable HTTP target normalized exactly once for canonical consumers.
///
/// The private field prevents the resolver and fingerprint codec from
/// accepting a target that bypassed [`normalize_portable_http_target`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalHttpTarget(Box<str>);

impl CanonicalHttpTarget {
    /// Return the connection-relative spelling consumed by canonical stacks.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Normalize the sole portable spelling by stripping exactly one leading `/`.
pub fn normalize_portable_http_target(
    portable: &str,
) -> Result<CanonicalHttpTarget, PortableHttpTargetError> {
    if portable.starts_with("//") {
        return Err(PortableHttpTargetError::new(
            "portable HTTP path-and-query cannot start with //",
        ));
    }
    let Some(canonical) = portable.strip_prefix('/') else {
        return Err(PortableHttpTargetError::new(
            "portable HTTP path-and-query must start with /",
        ));
    };
    if canonical.is_empty() {
        return Err(PortableHttpTargetError::new(
            "portable HTTP path-and-query must contain a path",
        ));
    }
    let first_segment = canonical.split(['/', '?', '#']).next().unwrap_or_default();
    if canonical.contains(['#', '\\']) || first_segment.contains(':') {
        return Err(PortableHttpTargetError::new(
            "portable HTTP path-and-query is ambiguous",
        ));
    }
    Ok(CanonicalHttpTarget(canonical.into()))
}

/// A portable HTTP target that cannot enter canonical resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortableHttpTargetError {
    detail: Box<str>,
}

impl PortableHttpTargetError {
    fn new(detail: impl Into<Box<str>>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for PortableHttpTargetError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for PortableHttpTargetError {}

/// A SHA-256 digest of the exact HTTP request body bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HttpBodyDigest([u8; 32]);

impl HttpBodyDigest {
    /// Digest request body bytes for use in an operation fingerprint.
    pub fn sha256(body: &[u8]) -> Self {
        Self(Sha256::digest(body).into())
    }

    /// Use an already-computed SHA-256 request body digest.
    pub fn from_sha256(digest: [u8; 32]) -> Self {
        Self(digest)
    }

    /// Return the fixed-width SHA-256 bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// One contract-selected semantic request header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HttpSemanticHeader<'a> {
    pub name: &'a str,
    pub value: &'a str,
}

/// Whether an authored header participates in the portable operation identity.
pub fn is_http_operation_semantic_header(name: &str) -> bool {
    !name.eq_ignore_ascii_case("traceparent")
}

/// The complete portable input to the HTTP operation fingerprint codec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpOperation<'a> {
    pub method: &'a str,
    pub target: CanonicalHttpTarget,
    pub semantic_headers: &'a [HttpSemanticHeader<'a>],
    pub body_digest: HttpBodyDigest,
}

/// A validated canonical preimage and its SHA-256 fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpOperationFingerprint {
    canonical_bytes: Box<[u8]>,
    digest: [u8; 32],
}

impl HttpOperationFingerprint {
    /// Return the versioned portable preimage.
    pub fn canonical_bytes(&self) -> &[u8] {
        &self.canonical_bytes
    }

    /// Return the SHA-256 digest of the canonical preimage.
    pub fn digest(&self) -> &[u8; 32] {
        &self.digest
    }
}

/// Stable classification for a refused operation fingerprint input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpOperationFingerprintErrorKind {
    InvalidMethod,
    InvalidRelativeTarget,
    InvalidSemanticHeader,
    EnvironmentField,
    InputTooLarge,
}

/// A fail-closed HTTP operation fingerprint error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpOperationFingerprintError {
    kind: HttpOperationFingerprintErrorKind,
    detail: Box<str>,
}

impl HttpOperationFingerprintError {
    fn new(kind: HttpOperationFingerprintErrorKind, detail: impl Into<Box<str>>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }

    /// Return the stable error classification.
    pub fn kind(&self) -> HttpOperationFingerprintErrorKind {
        self.kind
    }
}

impl std::fmt::Display for HttpOperationFingerprintError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for HttpOperationFingerprintError {}

/// Canonicalize and fingerprint one portable HTTP operation.
pub fn fingerprint_http_operation(
    operation: &HttpOperation<'_>,
) -> Result<HttpOperationFingerprint, HttpOperationFingerprintError> {
    let method = canonical_method(operation.method)?;
    let target = canonical_relative_target(operation.target.as_str())?;
    let mut headers = canonical_semantic_headers(operation.semantic_headers)?;
    headers.sort_unstable();

    let mut canonical = Vec::with_capacity(
        DOMAIN.len()
            + HTTP_OPERATION_FINGERPRINT_VERSION.len()
            + method.len()
            + target.len()
            + headers
                .iter()
                .map(|(name, value)| name.len() + value.len())
                .sum::<usize>()
            + 128,
    );
    canonical.extend_from_slice(DOMAIN);
    push_frame(
        &mut canonical,
        b"codec",
        HTTP_OPERATION_FINGERPRINT_VERSION.as_bytes(),
    )?;
    push_frame(&mut canonical, b"method", method.as_bytes())?;
    push_frame(&mut canonical, b"relative-target", target.as_bytes())?;
    push_count(&mut canonical, b"semantic-header-count", headers.len())?;
    for (name, value) in headers {
        push_frame(&mut canonical, b"semantic-header-name", name.as_bytes())?;
        push_frame(&mut canonical, b"semantic-header-value", value.as_bytes())?;
    }
    push_frame(
        &mut canonical,
        b"body-sha256",
        operation.body_digest.as_bytes(),
    )?;

    let digest = Sha256::digest(&canonical).into();
    Ok(HttpOperationFingerprint {
        canonical_bytes: canonical.into_boxed_slice(),
        digest,
    })
}

fn canonical_method(method: &str) -> Result<String, HttpOperationFingerprintError> {
    if method.is_empty() || !method.bytes().all(is_http_token_byte) {
        return Err(HttpOperationFingerprintError::new(
            HttpOperationFingerprintErrorKind::InvalidMethod,
            "HTTP method must be a non-empty ASCII token",
        ));
    }
    Ok(method.to_ascii_uppercase())
}

fn canonical_relative_target(target: &str) -> Result<String, HttpOperationFingerprintError> {
    if target.is_empty()
        || target.starts_with('/')
        || target.contains(['#', '\\'])
        || !target.is_ascii()
        || target
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b' ')
    {
        return Err(invalid_target());
    }

    let (path, query) = target
        .split_once('?')
        .map_or((target, None), |(path, query)| (path, Some(query)));
    if path.is_empty()
        || path
            .split('/')
            .next()
            .is_some_and(|segment| segment.contains(':'))
        || path.split('/').any(|segment| matches!(segment, "." | ".."))
    {
        return Err(invalid_target());
    }
    let path = canonical_percent_encoding(path, true)?;
    if path.split('/').any(|segment| matches!(segment, "." | "..")) {
        return Err(invalid_target());
    }

    let mut canonical = path;
    if let Some(query) = query {
        canonical.push('?');
        canonical.push_str(&canonical_percent_encoding(query, false)?);
    }
    Ok(canonical)
}

fn canonical_percent_encoding(
    value: &str,
    path: bool,
) -> Result<String, HttpOperationFingerprintError> {
    let bytes = value.as_bytes();
    let mut canonical = String::with_capacity(value.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            canonical.push(char::from(bytes[index]));
            index += 1;
            continue;
        }
        if index + 2 >= bytes.len()
            || !bytes[index + 1].is_ascii_hexdigit()
            || !bytes[index + 2].is_ascii_hexdigit()
        {
            return Err(invalid_target());
        }
        let decoded = (hex_value(bytes[index + 1]) << 4) | hex_value(bytes[index + 2]);
        if decoded == 0 || (path && matches!(decoded, b'/' | b'\\')) {
            return Err(invalid_target());
        }
        if is_unreserved(decoded) {
            canonical.push(char::from(decoded));
        } else {
            canonical.push('%');
            canonical.push(char::from(upper_hex(decoded >> 4)));
            canonical.push(char::from(upper_hex(decoded & 0x0f)));
        }
        index += 3;
    }
    Ok(canonical)
}

fn canonical_semantic_headers(
    headers: &[HttpSemanticHeader<'_>],
) -> Result<Vec<(String, String)>, HttpOperationFingerprintError> {
    let mut canonical = Vec::with_capacity(headers.len());
    for header in headers {
        if header.name.is_empty() || !header.name.bytes().all(is_http_token_byte) {
            return Err(HttpOperationFingerprintError::new(
                HttpOperationFingerprintErrorKind::InvalidSemanticHeader,
                "semantic header name must be a non-empty ASCII token",
            ));
        }
        let name = header.name.to_ascii_lowercase();
        if FORBIDDEN_SEMANTIC_HEADERS
            .binary_search(&name.as_str())
            .is_ok()
        {
            return Err(HttpOperationFingerprintError::new(
                HttpOperationFingerprintErrorKind::EnvironmentField,
                format!("environment or system-owned header {name:?} cannot be semantic"),
            ));
        }
        if header.value.bytes().any(|byte| {
            byte == b'\r' || byte == b'\n' || (byte.is_ascii_control() && byte != b'\t')
        }) {
            return Err(HttpOperationFingerprintError::new(
                HttpOperationFingerprintErrorKind::InvalidSemanticHeader,
                format!("semantic header {name:?} contains a control byte"),
            ));
        }
        let value = header.value.trim_matches([' ', '\t']).to_string();
        if canonical
            .iter()
            .any(|(existing, _): &(String, String)| existing == &name)
        {
            return Err(HttpOperationFingerprintError::new(
                HttpOperationFingerprintErrorKind::InvalidSemanticHeader,
                format!("duplicate semantic header {name:?}"),
            ));
        }
        canonical.push((name, value));
    }
    Ok(canonical)
}

fn push_count(
    output: &mut Vec<u8>,
    tag: &[u8],
    count: usize,
) -> Result<(), HttpOperationFingerprintError> {
    let count = u32::try_from(count).map_err(|_| input_too_large())?;
    push_frame(output, tag, &count.to_be_bytes())
}

fn push_frame(
    output: &mut Vec<u8>,
    tag: &[u8],
    value: &[u8],
) -> Result<(), HttpOperationFingerprintError> {
    let tag_len = u8::try_from(tag.len()).map_err(|_| input_too_large())?;
    let value_len = u32::try_from(value.len()).map_err(|_| input_too_large())?;
    output.push(tag_len);
    output.extend_from_slice(tag);
    output.extend_from_slice(&value_len.to_be_bytes());
    output.extend_from_slice(value);
    Ok(())
}

fn is_http_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

fn is_unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
}

fn hex_value(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        b'A'..=b'F' => byte - b'A' + 10,
        _ => unreachable!("hex_value is called only after is_ascii_hexdigit"),
    }
}

fn upper_hex(nibble: u8) -> u8 {
    match nibble {
        0..=9 => b'0' + nibble,
        10..=15 => b'A' + nibble - 10,
        _ => unreachable!("upper_hex accepts one nibble"),
    }
}

fn invalid_target() -> HttpOperationFingerprintError {
    HttpOperationFingerprintError::new(
        HttpOperationFingerprintErrorKind::InvalidRelativeTarget,
        "HTTP target must be an unambiguous connection-relative path and optional query",
    )
}

fn input_too_large() -> HttpOperationFingerprintError {
    HttpOperationFingerprintError::new(
        HttpOperationFingerprintErrorKind::InputTooLarge,
        "HTTP operation fingerprint field exceeds the portable codec limit",
    )
}
