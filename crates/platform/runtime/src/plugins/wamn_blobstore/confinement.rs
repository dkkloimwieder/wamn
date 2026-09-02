//! Bucket and prefix walls for the blobstore capability.
//!
//! The connection descriptor (`ConnectionTypeDescriptor::blobstore_v1`) makes
//! the endpoint, bucket and prefix environment-owned and leaves the author
//! only an object key relative to the prefix. This module is where that
//! ownership split stops being a declaration and becomes a refusal: an author
//! can name an OBJECT, and cannot name a CONTAINER.
//!
//! Every key a guest supplies passes through [`resolve_key`]. Nothing else
//! constructs a store path.

/// The S3 object-key ceiling, in bytes. Keys are UTF-8 and capped at 1024
/// bytes by the service; a longer key is refused here rather than by a remote
/// error we would have to interpret.
pub const MAX_KEY_BYTES: usize = 1024;

/// Why a guest-supplied object key was refused.
///
/// Every variant is a containment breach or a malformed key, never a transport
/// condition — this type is decided before any request exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyRefusal {
    /// The key was empty.
    Empty,
    /// The key was absolute, which would address the bucket root rather than a
    /// location inside the prefix.
    Absolute,
    /// The key contained a `..` segment, the ordinary way out of a prefix.
    ParentTraversal,
    /// The key contained a NUL or other control character.
    ControlCharacter,
    /// The key exceeded [`MAX_KEY_BYTES`].
    TooLong,
    /// The resolved key did not fall under the bound prefix. This is the
    /// belt-and-braces arm: it should be unreachable given the others, and it
    /// fires rather than trusting that reasoning.
    EscapedPrefix,
}

impl KeyRefusal {
    /// Stable wire code for the refusal.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Empty => "key_empty",
            Self::Absolute => "key_absolute",
            Self::ParentTraversal => "key_parent_traversal",
            Self::ControlCharacter => "key_control_character",
            Self::TooLong => "key_too_long",
            Self::EscapedPrefix => "key_escaped_prefix",
        }
    }
}

/// Resolve one guest-supplied key against the bound prefix.
///
/// The returned string is the ONLY value that may be handed to the object
/// store. `prefix` is environment-owned and trusted; `author_key` is guest
/// input and is not.
///
/// # Errors
///
/// Returns a [`KeyRefusal`] naming the containment rule the key broke.
pub fn resolve_key(prefix: &str, author_key: &str) -> Result<String, KeyRefusal> {
    if author_key.is_empty() {
        return Err(KeyRefusal::Empty);
    }
    if author_key.len() > MAX_KEY_BYTES {
        return Err(KeyRefusal::TooLong);
    }
    if author_key.starts_with('/') {
        return Err(KeyRefusal::Absolute);
    }
    if author_key.chars().any(char::is_control) {
        return Err(KeyRefusal::ControlCharacter);
    }
    // Reject `..` as a whole SEGMENT. A substring check would also refuse the
    // legitimate key `report..2026.zpl`, and a containment rule that refuses
    // valid names teaches authors to work around it.
    if author_key.split('/').any(|segment| segment == "..") {
        return Err(KeyRefusal::ParentTraversal);
    }

    let trimmed_prefix = prefix.trim_end_matches('/');
    let resolved = if trimmed_prefix.is_empty() {
        author_key.to_owned()
    } else {
        format!("{trimmed_prefix}/{author_key}")
    };
    if resolved.len() > MAX_KEY_BYTES {
        return Err(KeyRefusal::TooLong);
    }
    if !trimmed_prefix.is_empty() && !resolved.starts_with(&format!("{trimmed_prefix}/")) {
        return Err(KeyRefusal::EscapedPrefix);
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ordinary_key_lands_under_the_prefix() {
        assert_eq!(
            resolve_key("acme/labels", "pallet/PAL-000042.zpl").expect("ordinary key resolves"),
            "acme/labels/pallet/PAL-000042.zpl"
        );
    }

    /// A trailing slash on the environment-owned prefix must not produce a
    /// doubled separator, which S3 would treat as a DIFFERENT key — the same
    /// logical object written twice under two names breaks the deterministic
    /// -key overwrite rule 2c depends on.
    #[test]
    fn a_trailing_prefix_slash_does_not_double_the_separator() {
        assert_eq!(
            resolve_key("acme/labels/", "a.zpl").expect("resolves"),
            "acme/labels/a.zpl"
        );
        assert_eq!(
            resolve_key("acme/labels", "a.zpl").expect("resolves"),
            resolve_key("acme/labels/", "a.zpl").expect("resolves"),
            "the two prefix spellings must resolve identically"
        );
    }

    #[test]
    fn an_empty_prefix_is_allowed_and_keeps_the_key_verbatim() {
        assert_eq!(resolve_key("", "a.zpl").expect("resolves"), "a.zpl");
    }

    /// The containment breaches, each by its own route out of the prefix.
    #[test]
    fn every_escape_route_is_refused() {
        for (key, expected) in [
            ("", KeyRefusal::Empty),
            ("/etc/passwd", KeyRefusal::Absolute),
            ("../../other-tenant/secret", KeyRefusal::ParentTraversal),
            ("a/../../b", KeyRefusal::ParentTraversal),
            ("..", KeyRefusal::ParentTraversal),
            ("a\0b", KeyRefusal::ControlCharacter),
            ("a\nb", KeyRefusal::ControlCharacter),
        ] {
            assert_eq!(
                resolve_key("acme/labels", key),
                Err(expected),
                "key {key:?} must be refused as {expected:?}"
            );
        }
    }

    /// `..` is refused as a segment, not as a substring: a key that merely
    /// CONTAINS two dots is legitimate and must survive.
    #[test]
    fn a_double_dot_inside_a_name_is_not_a_traversal() {
        assert_eq!(
            resolve_key("p", "report..2026.zpl").expect("dots inside a name are fine"),
            "p/report..2026.zpl"
        );
        assert_eq!(
            resolve_key("p", "..hidden").expect("a leading double dot in a name is fine"),
            "p/..hidden"
        );
    }

    #[test]
    fn an_over_long_key_is_refused_before_and_after_prefixing() {
        let long = "a".repeat(MAX_KEY_BYTES + 1);
        assert_eq!(resolve_key("p", &long), Err(KeyRefusal::TooLong));

        // Just short enough alone, too long once the prefix is applied.
        let borderline = "a".repeat(MAX_KEY_BYTES - 1);
        assert_eq!(
            resolve_key("prefix", &borderline),
            Err(KeyRefusal::TooLong),
            "the ceiling applies to the RESOLVED key, not the author's half"
        );
    }

    /// Whatever the input, a resolved key always sits under the bound prefix.
    /// This is the property the walls exist for, asserted directly.
    #[test]
    fn every_resolved_key_sits_under_the_prefix() {
        for key in [
            "a.zpl",
            "deep/nested/path/o.bin",
            "..hidden",
            "with space.txt",
            "unicode-\u{e9}\u{fc}.txt",
        ] {
            let resolved = resolve_key("acme/labels", key).expect("valid key");
            assert!(
                resolved.starts_with("acme/labels/"),
                "{key:?} resolved to {resolved:?}, outside the prefix"
            );
        }
    }
}
