use super::DIAGNOSTIC_BYTES;

pub(super) fn diagnostic_tail(bytes: &[u8]) -> std::borrow::Cow<'_, str> {
    String::from_utf8_lossy(&bytes[bytes.len().saturating_sub(DIAGNOSTIC_BYTES)..])
}
