//! First-party principal records and input validation.

use std::fmt;

use wamn_identity_protocol::PrincipalId;

use crate::identity_error::{IdentityError, IdentityErrorType, IdentityRefusal};

/// Maximum accepted principal-subject length.
pub const MAX_SUBJECT_LEN: usize = 254;

/// Maximum accepted display-name length.
pub const MAX_DISPLAY_NAME_LEN: usize = 200;

/// The type of first-party platform principal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrincipalType {
    /// A user of the first-party identity service.
    User,
    /// A platform component that writes in the system database. It cannot
    /// authenticate.
    Platform,
}

impl PrincipalType {
    /// Return the stable database literal.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Platform => "platform",
        }
    }

    fn parse(value: &str) -> Result<Self, IdentityError> {
        match value {
            "user" => Ok(Self::User),
            "platform" => Ok(Self::Platform),
            other => Err(IdentityError::new(
                IdentityErrorType::CorruptData,
                format!("unknown stored principal type {other:?}"),
            )),
        }
    }
}

impl fmt::Display for PrincipalType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Whether a stored principal may authenticate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrincipalStatus {
    /// The principal may authenticate through an admitted presenter.
    Active,
    /// The principal is retained but cannot authenticate.
    Disabled,
}

impl PrincipalStatus {
    /// Return the stable database literal.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Disabled => "disabled",
        }
    }

    fn parse(value: &str) -> Result<Self, IdentityError> {
        match value {
            "active" => Ok(Self::Active),
            "disabled" => Ok(Self::Disabled),
            other => Err(IdentityError::new(
                IdentityErrorType::CorruptData,
                format!("unknown stored principal status {other:?}"),
            )),
        }
    }
}

impl fmt::Display for PrincipalStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A stored first-party principal. This identity record does not authenticate
/// the current caller as that principal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    id: PrincipalId,
    type_: PrincipalType,
    subject: Box<str>,
    display_name: Box<str>,
    status: PrincipalStatus,
}

impl Principal {
    /// Decode the principal's stored type and status with its record fields.
    pub fn from_storage(
        id: PrincipalId,
        type_: &str,
        subject: String,
        display_name: String,
        status: &str,
    ) -> Result<Self, IdentityError> {
        Ok(Self {
            id,
            type_: PrincipalType::parse(type_)?,
            subject: subject.into(),
            display_name: display_name.into(),
            status: PrincipalStatus::parse(status)?,
        })
    }

    /// Return the opaque principal ID.
    pub fn id(&self) -> &PrincipalId {
        &self.id
    }

    /// Return whether this is a user or platform principal.
    pub const fn principal_type(&self) -> PrincipalType {
        self.type_
    }

    /// Return the normalized first-party subject.
    pub fn subject(&self) -> &str {
        &self.subject
    }

    /// Return whether this principal is active or disabled.
    pub const fn status(&self) -> PrincipalStatus {
        self.status
    }
}

/// An authenticated principal produced only by an admitted presenter.
///
/// It has no public constructor and deliberately implements no deserialization
/// trait. Transport adapters therefore cannot turn a request field directly
/// into trusted invocation context.
///
/// ```compile_fail
/// use wamn_authentication::principal::AuthenticatedPrincipal;
/// let forged = AuthenticatedPrincipal { principal: todo!() };
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedPrincipal {
    principal: Principal,
}

impl AuthenticatedPrincipal {
    pub(super) fn admitted(principal: Principal) -> Self {
        Self { principal }
    }

    /// Return the verified principal record.
    pub fn principal(&self) -> &Principal {
        &self.principal
    }
}

pub fn canonical_subject(value: &str) -> Result<String, IdentityError> {
    let value = value.trim().to_ascii_lowercase();
    if value.is_empty()
        || value.len() > MAX_SUBJECT_LEN
        || !value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || (index > 0 && matches!(byte, b'.' | b'_' | b'@' | b'+' | b'-'))
        })
    {
        return Err(IdentityError::new(
            IdentityErrorType::InvalidInput,
            "principal subject must be a lowercase identity token of at most 254 bytes",
        ));
    }
    Ok(value)
}

/// Accept the same user email `principals_email_check` accepts: one `@`, no
/// space on either side, a dot in the domain, and at most 254 bytes. The rule
/// is deliberately loose, because the address is delivered to, not parsed.
///
/// The whole address folds to lower case, exactly as [`canonical_subject`]
/// folds a subject. By the mail standard the local part is case-sensitive, so
/// this deployment folds more than the standard allows. That is a decision and
/// not an oversight. One rule beside an identical rule beats two rules that
/// split the field, and `UNIQUE (email)` then catches `A@x.test` and
/// `a@x.test` as the one address a reader already reads them as. The fold is
/// Unicode and not ASCII, because `principals_email_check` admits a non-ASCII
/// address and a narrower fold would split that address in two.
pub fn require_email(value: &str) -> Result<String, IdentityError> {
    let value = value.trim().to_lowercase();
    let Some((local, domain)) = value.split_once('@') else {
        return Err(invalid_email());
    };
    if value.len() > MAX_SUBJECT_LEN
        || local.is_empty()
        || domain.is_empty()
        || value.contains(char::is_whitespace)
        || domain.contains('@')
        || !domain.contains('.')
        || domain.starts_with('.')
        || domain.ends_with('.')
    {
        return Err(invalid_email());
    }
    Ok(value)
}

fn invalid_email() -> IdentityError {
    IdentityError::refused(
        IdentityErrorType::InvalidInput,
        IdentityRefusal::Invalid("email"),
        "email must be a local part, an @, and a dotted domain, in at most 254 bytes",
    )
}

pub fn require_display_name(value: &str) -> Result<String, IdentityError> {
    let value = value.trim();
    if value.is_empty() || value.len() > MAX_DISPLAY_NAME_LEN {
        return Err(IdentityError::refused(
            IdentityErrorType::InvalidInput,
            IdentityRefusal::Invalid("display_name"),
            "display name must contain 1 to 200 bytes",
        ));
    }
    Ok(value.to_owned())
}

/// Refuse the email and display name of a user that the identity rules
/// refuse, before a caller asks identity to create the user.
pub fn require_user_contact(email: &str, display_name: &str) -> Result<(), IdentityError> {
    require_email(email)?;
    require_display_name(display_name)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wamn_role::RoleName;

    #[test]
    fn principal_type_literals_round_trip() {
        for type_ in [PrincipalType::User, PrincipalType::Platform] {
            assert_eq!(PrincipalType::parse(type_.as_str()).unwrap(), type_);
        }
        assert_eq!(PrincipalType::Platform.as_str(), "platform");
        assert_eq!(
            PrincipalType::parse("robot").unwrap_err().error_type(),
            IdentityErrorType::CorruptData
        );
    }

    #[test]
    fn subjects_and_roles_are_canonical_fail_closed_tokens() {
        assert_eq!(
            canonical_subject("  USER@example.com ").unwrap(),
            "user@example.com"
        );
        assert_eq!(
            " Project-Author ".parse::<RoleName>().unwrap().to_string(),
            "project-author"
        );
        for invalid in ["", "two words", "/root", "-leading"] {
            assert!(
                canonical_subject(invalid).is_err(),
                "accepted subject {invalid:?}"
            );
        }
        for invalid in ["", "two words", "_admin", "-admin"] {
            assert!(
                invalid.parse::<RoleName>().is_err(),
                "accepted role {invalid:?}"
            );
        }
    }

    /// A user email folds to lower case in full, like a subject, so two
    /// spellings of one address become one value (`wamn-0h0g.9.18`). The
    /// `UNIQUE (email)` in `deploy/sql/system-schema.sql` then refuses
    /// the second user, and `identity_live` pins that refusal.
    #[test]
    fn a_user_email_folds_to_one_lowercase_address() {
        assert_eq!(
            require_email("  Case.User@Example.Test ").unwrap(),
            "case.user@example.test"
        );
        assert_eq!(
            require_email("CASE.USER@EXAMPLE.TEST").unwrap(),
            require_email("case.user@example.test").unwrap()
        );
        for invalid in [
            "",
            "no-at-sign",
            "no@domaindot",
            "two words@example.test",
            "@example.test",
            "user@",
            "one@two@example.test",
        ] {
            assert!(
                require_email(invalid).is_err(),
                "accepted email {invalid:?}"
            );
        }
    }
}
