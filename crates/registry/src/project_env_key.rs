//! Canonical keys of project deployments.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

use crate::{EnvName, Triple};

/// A project deployment key spelled `<org>--<env>--<project>`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ProjectEnvKey(String);

/// A deployment key refused by the registry coordinate rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseProjectEnvKeyError;

impl fmt::Display for ParseProjectEnvKeyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("expected org--env--project with valid slugs sharing at most 41 bytes")
    }
}
impl std::error::Error for ParseProjectEnvKeyError {}

impl TryFrom<String> for ProjectEnvKey {
    type Error = ParseProjectEnvKeyError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let mut parts = value.split("--");
        let (Some(org), Some(env), Some(project)) = (parts.next(), parts.next(), parts.next())
        else {
            return Err(ParseProjectEnvKeyError);
        };
        if parts.next().is_some()
            || org.parse::<crate::OrgId>().is_err()
            || env.parse::<EnvName>().is_err()
            || project.parse::<crate::ProjectId>().is_err()
            || org.len() + env.len() + project.len() > 41
        {
            return Err(ParseProjectEnvKeyError);
        }
        Ok(Self(value))
    }
}
impl FromStr for ProjectEnvKey {
    type Err = ParseProjectEnvKeyError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_from(value.to_owned())
    }
}
impl From<ProjectEnvKey> for String {
    fn from(value: ProjectEnvKey) -> Self {
        value.0
    }
}
impl ProjectEnvKey {
    /// Build the platform palette key directly because parsing rejects the reserved `wamn` org.
    #[must_use]
    pub fn platform_palette() -> Self {
        Self("wamn--palette--platform".to_owned())
    }

    /// Build the package admission key directly because parsing rejects the reserved `wamn` org.
    #[must_use]
    pub fn package_admission() -> Self {
        Self("wamn--package--artifact".to_owned())
    }

    /// Borrow the persisted and wire spelling.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Return the coordinates carried by this validated key.
    pub fn triple(&self) -> Triple {
        let mut parts = self.0.split("--");
        let org = parts.next().expect("a key contains an organization");
        let env = parts.next().expect("a key contains an environment");
        let project = parts.next().expect("a key contains a project");
        Triple::new(
            org.parse().expect("a key contains a valid organization"),
            EnvName::from_key_component(env),
            project.parse().expect("a key contains a valid project"),
        )
    }
}
impl fmt::Display for ProjectEnvKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}
impl sqlx::Type<sqlx::Postgres> for ProjectEnvKey {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <String as sqlx::Type<sqlx::Postgres>>::type_info()
    }
    fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
        <String as sqlx::Type<sqlx::Postgres>>::compatible(ty)
    }
}
impl sqlx::postgres::PgHasArrayType for ProjectEnvKey {
    fn array_type_info() -> sqlx::postgres::PgTypeInfo {
        <String as sqlx::postgres::PgHasArrayType>::array_type_info()
    }
}
impl<'q> sqlx::Encode<'q, sqlx::Postgres> for ProjectEnvKey {
    fn encode_by_ref(
        &self,
        buffer: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        <&str as sqlx::Encode<'q, sqlx::Postgres>>::encode(self.as_str(), buffer)
    }
}
impl<'r> sqlx::Decode<'r, sqlx::Postgres> for ProjectEnvKey {
    fn decode(value: sqlx::postgres::PgValueRef<'r>) -> Result<Self, sqlx::error::BoxDynError> {
        Ok(<String as sqlx::Decode<'r, sqlx::Postgres>>::decode(value)?.try_into()?)
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn parse_rule() {
        for (value, valid) in [
            ("acme--dev--billing".to_owned(), true),
            ("a--wamn--9".to_owned(), true),
            (format!("a--dev--{}", "p".repeat(37)), true),
            (format!("a--dev--{}", "p".repeat(38)), false),
            (String::new(), false),
            ("a--b".to_owned(), false),
            ("a--b--c--d".to_owned(), false),
            ("a----b".to_owned(), false),
            ("a--Dev--b".to_owned(), false),
            ("a--a_b--b".to_owned(), false),
            ("wamn--dev--b".to_owned(), false),
            ("a--dev--wamn-db".to_owned(), false),
            ("project_env:a:b:c".to_owned(), false),
            ("a--dev--b_a".to_owned(), false),
        ] {
            let parsed = value.parse::<super::ProjectEnvKey>();
            assert_eq!(parsed.is_ok(), valid, "{value:?}");
            if let Ok(key) = parsed {
                assert_eq!(String::from(key.clone()), value);
                assert_eq!(
                    serde_json::from_value::<super::ProjectEnvKey>(
                        serde_json::to_value(&key).unwrap()
                    )
                    .unwrap(),
                    key
                );
            }
        }
    }
}
