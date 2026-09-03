//! Turning an authorized connection snapshot into a confined binding.
//!
//! This is the decision half of binding resolution, kept apart from the
//! database call so it can be proven against fabricated snapshots the way
//! `connection_http`'s authorization tests are.
//!
//! Two properties matter more than the parsing:
//!
//! **The credential is never here.** The snapshot carries a
//! `credential_handle`, not a secret. This module resolves coordinates —
//! endpoint, container, prefix — and hands the handle onward; no
//! credential-shaped value passes through it, which is what makes the
//! descriptor's `HostSignedRequest` injection true rather than aspirational.
//!
//! **A missing wall is a refusal, never a default.** An absent prefix does not
//! mean "the whole container". Defaulting an unspecified confinement to the
//! widest possible scope is how a configuration gap becomes an authority grant,
//! so every coordinate must be present and non-empty or the binding refuses.

use wamn_catalog::ConnectionTypeDescriptor;

use crate::plugins::wamn_postgres::ConnectionEffectSnapshot;

/// Why a snapshot did not yield a usable binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindingError {
    /// The authority chain did not authorize this component for this
    /// capability — unbound, inactive, stale generation, or the wrong
    /// connection type.
    Unauthorized,
    /// The generation carried no definition object.
    NoDefinition,
    /// A required coordinate was absent or empty. Named, because "the binding
    /// is malformed" is not actionable by whoever configured it.
    MissingCoordinate {
        /// The coordinate that was absent or empty.
        field: &'static str,
    },
    /// The generation named no host-held credential.
    NoCredential,
}

impl BindingError {
    /// Stable wire code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Unauthorized => "binding_unauthorized",
            Self::NoDefinition => "binding_no_definition",
            Self::MissingCoordinate { .. } => "binding_missing_coordinate",
            Self::NoCredential => "binding_no_credential",
        }
    }
}

impl core::fmt::Display for BindingError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unauthorized => {
                formatter.write_str("connection is not authorized for this component")
            }
            Self::NoDefinition => {
                formatter.write_str("connection generation carries no definition")
            }
            Self::MissingCoordinate { field } => write!(
                formatter,
                "connection generation is missing the required {field}; an absent confinement \
                 coordinate is refused rather than widened to a default"
            ),
            Self::NoCredential => {
                formatter.write_str("connection generation names no host-held credential")
            }
        }
    }
}

impl std::error::Error for BindingError {}

/// The coordinates of one bound object-store connection.
///
/// Carries a credential HANDLE, never a credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobstoreBinding {
    /// Object-store endpoint.
    pub endpoint: String,
    /// The one container this component may reach.
    pub container: String,
    /// The key prefix confining it within that container.
    pub prefix: String,
    /// Host-held credential reference. Resolved by the vault, not here.
    pub credential_handle: String,
}

/// Resolve an authorized snapshot into binding coordinates.
///
/// Authorization goes through the same `authorize_snapshot` the HTTP
/// connection uses — one copy, one reader — parameterized with the blobstore
/// descriptor.
///
/// # Errors
///
/// [`BindingError`] naming which layer refused.
pub fn resolve(snapshot: &ConnectionEffectSnapshot) -> Result<BlobstoreBinding, BindingError> {
    crate::plugins::connection_http::authorize_snapshot(
        snapshot,
        &ConnectionTypeDescriptor::blobstore_v1(),
    )
    .map_err(|_| BindingError::Unauthorized)?;

    let definition = snapshot
        .definition
        .as_ref()
        .ok_or(BindingError::NoDefinition)?;
    Ok(BlobstoreBinding {
        endpoint: coordinate(definition, "endpoint")?,
        container: coordinate(definition, "container")?,
        prefix: coordinate(definition, "prefix")?,
        credential_handle: snapshot
            .credential_handle
            .clone()
            .filter(|handle| !handle.is_empty())
            .ok_or(BindingError::NoCredential)?,
    })
}

/// One required, non-empty coordinate from the generation definition.
fn coordinate(definition: &serde_json::Value, field: &'static str) -> Result<String, BindingError> {
    definition
        .get(field)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or(BindingError::MissingCoordinate { field })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn authorized_snapshot() -> ConnectionEffectSnapshot {
        let descriptor = ConnectionTypeDescriptor::blobstore_v1();
        ConnectionEffectSnapshot {
            wiring_hash: "hash".to_owned(),
            component: Some("label-writer".to_owned()),
            interface_version: Some("0.1.0".to_owned()),
            operation: Some("wamn:node/handler@0.1.0".to_owned()),
            registered_operation: None,
            requirement_json: Some(serde_json::json!({
                "requirement": {
                    "requirement-type": descriptor.requirement_type,
                    "contract": descriptor.contract,
                }
            })),
            requirement_hash: Some("rh".to_owned()),
            node_permitted: true,
            binding_active: true,
            binding_valid: true,
            instance_id: Some("instance".to_owned()),
            validation_hash: None,
            requirement_type: Some(descriptor.requirement_type.clone()),
            contract: Some(descriptor.contract.clone()),
            instance_enabled: true,
            active_generation: Some(7),
            instance_revision: Some(1),
            generation: Some(7),
            definition: Some(serde_json::json!({
                "endpoint": "http://minio.wamn-system.svc:9000",
                "container": "wamn-labels",
                "prefix": "acme/labels",
            })),
            definition_hash: Some("dh".to_owned()),
            credential_handle: Some("vault://wamn-object-store".to_owned()),
        }
    }

    #[test]
    fn an_authorized_snapshot_yields_its_coordinates() {
        let binding = resolve(&authorized_snapshot()).expect("authorized snapshot binds");
        assert_eq!(binding.container, "wamn-labels");
        assert_eq!(binding.prefix, "acme/labels");
        assert_eq!(binding.endpoint, "http://minio.wamn-system.svc:9000");
        assert_eq!(binding.credential_handle, "vault://wamn-object-store");
    }

    /// The whole point of the walls: an absent prefix must NOT mean the whole
    /// container. A configuration gap widening into an authority grant is the
    /// failure this refuses.
    #[test]
    fn an_absent_coordinate_refuses_rather_than_widening() {
        for field in ["endpoint", "container", "prefix"] {
            let mut snapshot = authorized_snapshot();
            let definition = snapshot.definition.as_mut().expect("definition");
            definition.as_object_mut().expect("object").remove(field);
            assert_eq!(
                resolve(&snapshot),
                Err(BindingError::MissingCoordinate { field }),
                "{field} must refuse when absent"
            );
        }
    }

    /// An empty string is an absent coordinate wearing a value. An empty
    /// prefix would grant the entire container.
    #[test]
    fn an_empty_coordinate_is_treated_as_absent() {
        let mut snapshot = authorized_snapshot();
        snapshot.definition.as_mut().expect("definition")["prefix"] =
            serde_json::Value::String(String::new());
        assert_eq!(
            resolve(&snapshot),
            Err(BindingError::MissingCoordinate { field: "prefix" })
        );
    }

    /// Every authority layer refuses on its own, through the shared reader.
    #[test]
    fn each_missing_authority_layer_refuses() {
        for break_it in [
            (|s: &mut ConnectionEffectSnapshot| s.node_permitted = false)
                as fn(&mut ConnectionEffectSnapshot),
            |s| s.binding_active = false,
            |s| s.binding_valid = false,
            |s| s.instance_enabled = false,
            |s| s.instance_id = None,
            |s| s.generation = Some(1),
        ] {
            let mut snapshot = authorized_snapshot();
            break_it(&mut snapshot);
            assert_eq!(resolve(&snapshot), Err(BindingError::Unauthorized));
        }
    }

    /// The snapshot's type and contract must be blobstore's. An HTTP binding
    /// must not resolve as a blobstore one — this is the same discrimination
    /// the parameterized authorizer proves, exercised from the other side.
    #[test]
    fn an_http_binding_does_not_resolve_as_blobstore() {
        let http = ConnectionTypeDescriptor::http_v1();
        let mut snapshot = authorized_snapshot();
        snapshot.requirement_type = Some(http.requirement_type.clone());
        snapshot.contract = Some(http.contract.clone());
        snapshot.requirement_json = Some(serde_json::json!({
            "requirement": {
                "requirement-type": http.requirement_type,
                "contract": http.contract,
            }
        }));
        assert_eq!(resolve(&snapshot), Err(BindingError::Unauthorized));
    }

    /// No credential-shaped value passes through this module — only a handle.
    #[test]
    fn a_binding_carries_a_handle_and_never_a_secret() {
        let binding = resolve(&authorized_snapshot()).expect("binds");
        let rendered = format!("{binding:?}");
        assert!(
            rendered.contains("vault://"),
            "the handle is carried: {rendered}"
        );
        assert!(
            !rendered.contains("ACCESS_KEY") && !rendered.contains("secret-value"),
            "no credential material may appear: {rendered}"
        );
    }

    #[test]
    fn a_snapshot_without_a_credential_handle_refuses() {
        let mut snapshot = authorized_snapshot();
        snapshot.credential_handle = None;
        assert_eq!(resolve(&snapshot), Err(BindingError::NoCredential));
    }
}
