//! Secret credential formats. Diagnostics never include secret material.

use ring::rand::{SecureRandom as _, SystemRandom};
use std::{fmt, str::FromStr};
use subtle::ConstantTimeEq as _;
use zeroize::Zeroizing;

use crate::PAT_TOKEN_PREFIX;

/// The purpose that fixes a credential's prefix.
#[derive(Debug, Clone, Copy)]
pub enum SecretType {
    Invitation,
    Reset,
    Renewal,
}
impl SecretType {
    fn prefix(self) -> &'static str {
        match self {
            Self::Invitation => "wamn_inv_",
            Self::Reset => "wamn_reset_",
            Self::Renewal => "wamn_renew_",
        }
    }
}

/// A purpose-bound random credential.
pub struct SecretToken(Zeroizing<String>);
impl SecretToken {
    pub fn mint(type_: SecretType) -> Result<Self, TokenError> {
        Ok(Self(Zeroizing::new(format!(
            "{}{}",
            type_.prefix(),
            random_hex::<32>()?
        ))))
    }
    pub fn parse(type_: SecretType, value: &str) -> Result<Self, TokenError> {
        let suffix = value.strip_prefix(type_.prefix()).ok_or(TokenError)?;
        if suffix.len() != 64 || !is_lower_hex(suffix) {
            return Err(TokenError);
        }
        Ok(Self(Zeroizing::new(value.into())))
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
    pub fn into_secret(self) -> Zeroizing<String> {
        self.0
    }
    pub fn digest(&self) -> wamn_digest::Digest {
        wamn_digest::bytes_sha256(self.expose().as_bytes())
    }
    pub fn matches(&self, stored: &wamn_digest::Digest) -> bool {
        self.digest().as_bytes().ct_eq(stored.as_bytes()).into()
    }
}
impl fmt::Debug for SecretToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretToken(<redacted>)")
    }
}

/// A personal access token with a public lookup prefix and a random secret.
pub struct PatToken {
    lookup: crate::pat_prefix::PatPrefix,
    value: Zeroizing<String>,
}
impl PatToken {
    pub fn mint() -> Result<Self, TokenError> {
        let lookup = random_hex::<8>()?.parse().map_err(|_| TokenError)?;
        let value = Zeroizing::new(format!(
            "{PAT_TOKEN_PREFIX}{lookup}_{}",
            random_hex::<32>()?
        ));
        Ok(Self { lookup, value })
    }
    /// The parsed public lookup identity, without the token secret.
    pub fn lookup(&self) -> &crate::pat_prefix::PatPrefix {
        &self.lookup
    }
    pub fn expose(&self) -> &str {
        &self.value
    }
    pub fn into_secret(self) -> Zeroizing<String> {
        self.value
    }
    pub fn digest(&self) -> wamn_digest::Digest {
        wamn_digest::bytes_sha256(self.expose().as_bytes())
    }
    pub fn matches(&self, stored: &wamn_digest::Digest) -> bool {
        self.digest().as_bytes().ct_eq(stored.as_bytes()).into()
    }
}
impl FromStr for PatToken {
    type Err = TokenError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (lookup, secret) = value
            .strip_prefix(PAT_TOKEN_PREFIX)
            .and_then(|v| v.split_once('_'))
            .ok_or(TokenError)?;
        let lookup = lookup.parse().map_err(|_| TokenError)?;
        if secret.len() != 64 || !is_lower_hex(secret) {
            return Err(TokenError);
        }
        Ok(Self {
            lookup,
            value: Zeroizing::new(value.into()),
        })
    }
}
impl fmt::Debug for PatToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PatToken(<redacted>)")
    }
}

impl fmt::Display for PatToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.expose())
    }
}

fn random_hex<const N: usize>() -> Result<String, TokenError> {
    let mut bytes = Zeroizing::new([0; N]);
    SystemRandom::new()
        .fill(bytes.as_mut())
        .map_err(|_| TokenError)?;
    Ok(hex::encode(*bytes))
}
fn is_lower_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| matches!(byte,b'0'..=b'9'|b'a'..=b'f'))
}

/// A refused credential format or unavailable entropy source.
#[derive(Debug)]
pub struct TokenError;
impl fmt::Display for TokenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("credential unavailable")
    }
}
impl std::error::Error for TokenError {}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credentials_round_trip_and_refuse_malformed_or_wrong_secrets() {
        let token = PatToken::mint().unwrap();
        let parsed: PatToken = token.expose().parse().unwrap();
        assert!(parsed.matches(&token.digest()));
        assert!(!PatToken::mint().unwrap().matches(&token.digest()));
        for malformed in [
            "",
            "wamn_pat_",
            "wamn_pat_0123456789abcdef_bad",
            "wamn_pat_0123456789ABCDEf_0000000000000000000000000000000000000000000000000000000000000000",
        ] {
            assert!(malformed.parse::<PatToken>().is_err());
        }
        for type_ in [
            SecretType::Invitation,
            SecretType::Reset,
            SecretType::Renewal,
        ] {
            let token = SecretToken::mint(type_).unwrap();
            assert!(
                SecretToken::parse(type_, token.expose())
                    .unwrap()
                    .matches(&token.digest())
            );
            assert!(!SecretToken::mint(type_).unwrap().matches(&token.digest()));
            assert!(SecretToken::parse(type_, "bad").is_err());
        }
    }
}
