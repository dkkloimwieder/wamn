use std::fmt;

use wamn_schema_introspection::migration_policy::DefinitionKind;

/// Stable apply-package refusal prefix.
pub const APPLY_PACKAGE_REFUSAL: &str = "apply-package-refused";
/// Server refusal translated when release membership seals a package version.
pub const PACKAGE_VERSION_SEALED_REFUSAL: &str = "package-version-sealed";
/// A new coordinate must extend the one installed leaf for its package family.
pub const PREDECESSOR_NOT_CURRENT_REFUSAL: &str = "predecessor-not-current";
/// An overlay attempted to mutate a definition owned by another package.
pub const BASE_DEFINITION_MUTATION_REFUSAL: &str = "base-definition-mutation-refused";
/// A shared relation did not publish additive client-field authority.
pub const RELATION_NOT_CLIENT_EXTENSIBLE_REFUSAL: &str = "relation-not-client-extensible";
/// A migration addition lacks its exact manifest ownership declaration.
pub const DEFINITION_OWNER_DECLARATION_MISSING_REFUSAL: &str =
    "definition-owner-declaration-missing";
/// A live definition lacks or disagrees with its durable owner fact.
pub const DEFINITION_OWNER_CONFLICT_REFUSAL: &str = "definition-owner-conflict";
/// PostgreSQL did not expose the definition a migration reported creating.
pub const DEFINITION_NOT_FOUND_REFUSAL: &str = "definition-not-found";

/// Remedy-distinct apply-package refusal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyPackageErrorKind {
    PackageVersionSealed,
    PredecessorNotCurrent,
    PredecessorPrefixMismatch,
    BaseDefinitionMutation,
    RelationNotClientExtensible,
    DefinitionOwnerDeclarationMissing,
    DefinitionOwnerConflict,
    DefinitionNotFound,
}

impl ApplyPackageErrorKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PackageVersionSealed => PACKAGE_VERSION_SEALED_REFUSAL,
            Self::PredecessorNotCurrent => PREDECESSOR_NOT_CURRENT_REFUSAL,
            Self::PredecessorPrefixMismatch => {
                wamn_schema_control::PackageMigrationErrorKind::PredecessorPrefixMismatch.as_str()
            }
            Self::BaseDefinitionMutation => BASE_DEFINITION_MUTATION_REFUSAL,
            Self::RelationNotClientExtensible => RELATION_NOT_CLIENT_EXTENSIBLE_REFUSAL,
            Self::DefinitionOwnerDeclarationMissing => DEFINITION_OWNER_DECLARATION_MISSING_REFUSAL,
            Self::DefinitionOwnerConflict => DEFINITION_OWNER_CONFLICT_REFUSAL,
            Self::DefinitionNotFound => DEFINITION_NOT_FOUND_REFUSAL,
        }
    }
}

/// Contextual failure at the package application boundary.
#[derive(Debug)]
pub struct ApplyPackageError {
    pub(super) kind: ApplyPackageErrorKind,
    pub(super) coordinate: String,
    pub(super) predecessor_version: Option<String>,
    pub(super) current_version: Option<String>,
    pub(super) path: Option<String>,
    pub(super) schema: Option<String>,
    pub(super) relation: Option<String>,
    pub(super) definition_kind: Option<DefinitionKind>,
    pub(super) definition: Option<String>,
    pub(super) owner_package: Option<String>,
    pub(super) detail: String,
    pub(super) source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl ApplyPackageError {
    pub const fn kind(&self) -> ApplyPackageErrorKind {
        self.kind
    }

    pub fn coordinate(&self) -> &str {
        &self.coordinate
    }

    pub fn predecessor_version(&self) -> Option<&str> {
        self.predecessor_version.as_deref()
    }

    pub fn current_version(&self) -> Option<&str> {
        self.current_version.as_deref()
    }

    pub fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }

    pub fn schema(&self) -> Option<&str> {
        self.schema.as_deref()
    }

    pub fn relation(&self) -> Option<&str> {
        self.relation.as_deref()
    }

    pub fn definition(&self) -> Option<&str> {
        self.definition.as_deref()
    }

    pub fn owner_package(&self) -> Option<&str> {
        self.owner_package.as_deref()
    }
}

impl fmt::Display for ApplyPackageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{APPLY_PACKAGE_REFUSAL} ({}): coordinate={}",
            self.kind.as_str(),
            self.coordinate
        )?;
        if let Some(predecessor) = &self.predecessor_version {
            write!(formatter, "; predecessor-version={predecessor}")?;
        } else if self.kind == ApplyPackageErrorKind::PredecessorNotCurrent {
            formatter.write_str("; predecessor-version=<none>")?;
        }
        if let Some(current) = &self.current_version {
            write!(formatter, "; current-version={current}")?;
        }
        if let Some(path) = &self.path {
            write!(formatter, "; file={path}")?;
        }
        if let Some(schema) = &self.schema {
            write!(formatter, "; schema={schema}")?;
        }
        if let Some(relation) = &self.relation {
            write!(formatter, "; relation={relation}")?;
        }
        if let Some(kind) = self.definition_kind {
            write!(formatter, "; definition-kind={}", kind.as_str())?;
        }
        if let Some(definition) = &self.definition {
            write!(formatter, "; definition={definition}")?;
        }
        if let Some(owner) = &self.owner_package {
            write!(formatter, "; owner-package={owner}")?;
        }
        write!(formatter, "; {}", self.detail)
    }
}

impl std::error::Error for ApplyPackageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn std::error::Error + 'static))
    }
}
