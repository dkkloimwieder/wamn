//! Exact catalog projection of one package's generated event registrations.

use std::collections::BTreeMap;
use std::fmt;

use crate::EventRegistration;

/// Insert or update one package-owned catalog registration only when facts differ.
pub const UPSERT_CATALOG_REGISTRATION_SQL: &str = "INSERT INTO catalog.event_registrations AS current \
       (tenant_id, package_id, registration_id, entity_id, registration) \
     VALUES ($1, $2, $3, $4, $5::jsonb) \
     ON CONFLICT (tenant_id, package_id, registration_id) DO UPDATE \
       SET entity_id = EXCLUDED.entity_id, \
           registration = EXCLUDED.registration \
     WHERE (current.entity_id, current.registration) \
           IS DISTINCT FROM \
           (EXCLUDED.entity_id, EXCLUDED.registration)";

/// Remove residue outside one package's generated registration artifact.
pub const DELETE_STALE_CATALOG_REGISTRATIONS_SQL: &str = "DELETE FROM catalog.event_registrations \
      WHERE tenant_id = $1 AND package_id = $2 \
        AND registration_id <> ALL($3::text[])";

/// One row projected from a generated registration declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogRegistrationRow {
    pub registration_id: String,
    pub entity_id: String,
    pub registration_json: String,
}

/// Exact package-coordinate reconciliation input for the catalog projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogRegistrationProjection {
    pub package_id: String,
    pub rows: Vec<CatalogRegistrationRow>,
    pub retained_registration_ids: Vec<String>,
}

/// Stable classification for an invalid generated registration projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistrationProjectionErrorKind {
    EmptyPackageId,
    RegistrationKeyMismatch,
    RegistrationOwnerMismatch,
}

/// Contextual failure to project a generated registration artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistrationProjectionError {
    kind: RegistrationProjectionErrorKind,
    registration_id: Option<String>,
    expected: String,
    actual: String,
}

impl RegistrationProjectionError {
    pub fn kind(&self) -> RegistrationProjectionErrorKind {
        self.kind
    }
}

impl fmt::Display for RegistrationProjectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            RegistrationProjectionErrorKind::EmptyPackageId => {
                formatter.write_str("registration projection package id is empty")
            }
            RegistrationProjectionErrorKind::RegistrationKeyMismatch => write!(
                formatter,
                "registration artifact key {:?} disagrees with document id {:?}",
                self.expected, self.actual
            ),
            RegistrationProjectionErrorKind::RegistrationOwnerMismatch => write!(
                formatter,
                "registration {:?} owner {:?} disagrees with projected package {:?}",
                self.registration_id.as_deref().unwrap_or_default(),
                self.actual,
                self.expected
            ),
        }
    }
}

impl std::error::Error for RegistrationProjectionError {}

/// Project one package's generated declarations into exact catalog rows.
pub fn project_catalog_registrations(
    package_id: &str,
    declarations: &BTreeMap<String, EventRegistration>,
) -> Result<CatalogRegistrationProjection, RegistrationProjectionError> {
    if package_id.is_empty() {
        return Err(RegistrationProjectionError {
            kind: RegistrationProjectionErrorKind::EmptyPackageId,
            registration_id: None,
            expected: String::new(),
            actual: String::new(),
        });
    }
    let mut rows = Vec::with_capacity(declarations.len());
    let mut retained_registration_ids = Vec::with_capacity(declarations.len());
    for (artifact_key, registration) in declarations {
        if artifact_key != &registration.registration_id {
            return Err(RegistrationProjectionError {
                kind: RegistrationProjectionErrorKind::RegistrationKeyMismatch,
                registration_id: Some(registration.registration_id.clone()),
                expected: artifact_key.clone(),
                actual: registration.registration_id.clone(),
            });
        }
        if registration.package_id != package_id {
            return Err(RegistrationProjectionError {
                kind: RegistrationProjectionErrorKind::RegistrationOwnerMismatch,
                registration_id: Some(registration.registration_id.clone()),
                expected: package_id.to_owned(),
                actual: registration.package_id.clone(),
            });
        }
        retained_registration_ids.push(registration.registration_id.clone());
        rows.push(CatalogRegistrationRow {
            registration_id: registration.registration_id.clone(),
            entity_id: registration.entity.clone(),
            registration_json: serde_json::to_string(registration)
                .expect("EventRegistration serializes"),
        });
    }
    Ok(CatalogRegistrationProjection {
        package_id: package_id.to_owned(),
        rows,
        retained_registration_ids,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Op, RegistrationInput, SCHEMA_VERSION};

    fn registration(owner: &str, source: &str) -> EventRegistration {
        EventRegistration {
            schema_version: SCHEMA_VERSION.into(),
            registration_id: "quality.create_inspection".into(),
            package_id: owner.into(),
            source_package_id: source.into(),
            entity: "receipt".into(),
            ops: vec![Op::Insert],
            input: RegistrationInput::Event,
            condition: None,
        }
    }

    #[test]
    fn one_generated_declaration_projects_owner_and_source_without_trigger_lookup() {
        let declaration = registration("client_acme_receiving", "wamn_receiving");
        let declarations =
            BTreeMap::from([(declaration.registration_id.clone(), declaration.clone())]);
        let projection =
            project_catalog_registrations("client_acme_receiving", &declarations).unwrap();
        assert_eq!(projection.package_id, "client_acme_receiving");
        assert_eq!(
            projection.retained_registration_ids,
            ["quality.create_inspection"]
        );
        assert_eq!(projection.rows.len(), 1);
        let document: EventRegistration =
            serde_json::from_str(&projection.rows[0].registration_json).unwrap();
        assert_eq!(document, declaration);
        assert_eq!(document.source_package_id, "wamn_receiving");
    }

    #[test]
    fn artifact_key_and_owner_must_match_the_projected_coordinate() {
        let declaration = registration("client_acme_receiving", "wamn_receiving");
        let wrong_key = BTreeMap::from([("other".into(), declaration.clone())]);
        assert_eq!(
            project_catalog_registrations("client_acme_receiving", &wrong_key)
                .unwrap_err()
                .kind(),
            RegistrationProjectionErrorKind::RegistrationKeyMismatch
        );
        let declarations = BTreeMap::from([(declaration.registration_id.clone(), declaration)]);
        assert_eq!(
            project_catalog_registrations("other", &declarations)
                .unwrap_err()
                .kind(),
            RegistrationProjectionErrorKind::RegistrationOwnerMismatch
        );
    }
}
