//! Canonical metadata-catalog types (3.1).
//!
//! The catalog is **data, not DDL**: a versioned set of entities, each with
//! typed fields plus indexes and constraints, wired by relations. It is the
//! model the DDL compiler (3.2) turns into migrations, the generated API (4.1)
//! exposes as CRUD, the designer UI (3.3) edits, and the RLS builder (3.5)
//! attaches policies to.
//!
//! **Neutral primitives only** (D14): the core catalog knows entities, fields,
//! types, relations, and constraints — not receipts, lots, or holds. Opinionated
//! domain models (a unified lot/serial treatment, an asset/historian model) are
//! optional modules layered on top; a client whose ontology disagrees swaps the
//! module, not the platform.
//!
//! This crate models catalog *structure* and validates its well-formedness. It
//! does not emit DDL (3.2), generate an API (4.1), render an ERD (3.3), or
//! compile RLS (3.5) — those consume this model.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The catalog-model **format** version this crate implements. Distinct from a
/// catalog's own [`Catalog::version`]. Compatibility rule (mirrors the WIT and
/// flow-schema freezes): `0.1.x` is additive/clarifying only; a breaking change
/// waits for `0.2`.
pub const SCHEMA_VERSION: &str = "0.1";

// Generates a schema-transparent string newtype for a catalog id. Each is a
// near-drop-in for `String` — `Deref<Target = str>`, `as_str`, `Display`,
// `From<&str>`/`From<String>`, and `PartialEq` against `str`/`&str`/`String` —
// but a *distinct* type, so mixing an entity id with a field id is a compile
// error (the confusion `wamn-api`'s router used to risk between
// `Relation::from`/`to` and `Relation::from_field`). Two invariants:
//
//   - `Debug` delegates to the inner `String`, so a validation message that
//     prints an id with `{:?}` reads `"foo"`, not `EntityId("foo")`.
//   - schema-transparent: `#[serde(transparent)]` also drives schemars' own
//     transparent derive (non-referenceable, inlined as a plain string), so the
//     published `docs/catalog-model.schema.json` contract is byte-unchanged.
macro_rules! id_newtype {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(
            Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
        )]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// The id as a string slice.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Debug for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                std::fmt::Debug::fmt(&self.0, f)
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                std::fmt::Display::fmt(&self.0, f)
            }
        }

        impl std::ops::Deref for $name {
            type Target = str;
            fn deref(&self) -> &str {
                &self.0
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self(s.to_owned())
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self(s)
            }
        }

        impl PartialEq<str> for $name {
            fn eq(&self, other: &str) -> bool {
                self.as_str() == other
            }
        }

        impl PartialEq<&str> for $name {
            fn eq(&self, other: &&str) -> bool {
                self.as_str() == *other
            }
        }

        impl PartialEq<String> for $name {
            fn eq(&self, other: &String) -> bool {
                self.as_str() == other.as_str()
            }
        }
    };
}

id_newtype! {
    /// A stable entity identifier, unique within a catalog. A logical slug; the DDL
    /// compiler (3.2) maps it to a physical table identifier.
    EntityId
}

id_newtype! {
    /// A stable field identifier, unique within its entity. Logical; 3.2 maps it to
    /// a physical column identifier. Stable across renames, so a field rename is a
    /// *change* in the [`crate::diff`], not a remove + add.
    FieldId
}

/// One version of a catalog — the unit stored, versioned, and promoted between
/// environments (3.4).
///
/// Every entity is assumed to carry a platform-managed surrogate primary key
/// (an `id`, injected by the DDL compiler 3.2); references therefore target an
/// *entity*, not a named column, and natural keys are expressed as [`Constraint::Unique`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Catalog {
    /// The catalog-model format version (e.g. `"0.1"`). See [`SCHEMA_VERSION`].
    pub schema_version: String,
    /// Stable identifier shared across every version of this catalog (typically
    /// per project).
    pub catalog_id: String,
    /// Monotonic version of this catalog (>= 1).
    pub version: u32,
    /// Human-readable label (editor).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The entities (tables) of the model.
    pub entities: Vec<Entity>,
    /// Relations between entities (navigational / API-expansion metadata over
    /// the physical foreign keys, plus many-to-many and hierarchical trees).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relations: Vec<Relation>,
}

/// A table in the model.
///
/// **System entities** (`is_system`) are provided by the platform (e.g.
/// `users`): their *system* fields are structure-locked — the designer may not
/// drop or retype them — but the entity stays **extensible**, so a project may
/// add its own custom (non-system) fields (the POC's `users.cert_level`). A
/// system field on a non-system entity is contradictory and rejected.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Entity {
    /// Unique within the catalog.
    pub id: EntityId,
    /// Logical name (the DDL compiler maps it to a physical table name).
    pub name: String,
    /// `true` for a platform-provided entity whose system fields are
    /// structure-locked but which remains extensible with custom fields.
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_system: bool,
    /// Human-readable label (editor).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Human-readable description (editor).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The fields (columns) of the entity.
    pub fields: Vec<Field>,
    /// Secondary indexes on the entity's fields.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub indexes: Vec<Index>,
    /// Table-level constraints (composite uniqueness, checks).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<Constraint>,
}

/// A column in an entity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Field {
    /// Unique within the entity.
    pub id: FieldId,
    /// Logical name (the DDL compiler maps it to a physical column name).
    pub name: String,
    /// The field's type. See [`FieldType`].
    #[serde(rename = "type")]
    pub field_type: FieldType,
    /// `true` if the column is nullable. Defaults to `false` (NOT NULL) — a
    /// schema designer states nullability explicitly.
    #[serde(default, skip_serializing_if = "is_false")]
    pub nullable: bool,
    /// Optional default — an opaque JSON literal or SQL expression string,
    /// interpreted by the DDL compiler (3.2), not by this crate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    /// `true` if the field holds sensitive data (e.g. supplier pricing). A
    /// neutral flag the field-level mask (4.3) keys on; this crate does not
    /// enforce masking.
    #[serde(default, skip_serializing_if = "is_false")]
    pub sensitive: bool,
    /// `true` for a structure-locked field of a system entity (cannot be dropped
    /// or retyped). Requires the owning entity to be `is_system`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_system: bool,
    /// Human-readable label (editor).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Human-readable description (editor).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// The field type system (3.1 owns this; 3.3 is the palette UI over it).
///
/// Industrial-friendly by construction: timestamps carry a time zone, quantities
/// are **exact decimals** with an optional unit — there is deliberately **no
/// float type**, because floats are disallowed for material quantities and
/// formulations (a hard POC requirement). References are foreign keys to another
/// entity's managed primary key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum FieldType {
    /// Variable-length text, optionally length-capped.
    Text {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_len: Option<u32>,
    },
    /// 32-bit signed integer.
    Int,
    /// 64-bit signed integer.
    BigInt,
    /// Boolean.
    Bool,
    /// UUID.
    Uuid,
    /// Arbitrary JSON document (`jsonb`).
    Json,
    /// Calendar date (no time).
    Date,
    /// Instant with time zone (`timestamptz`) — the only timestamp type.
    Timestamptz,
    /// Enumeration over a fixed set of string variants.
    Enum { variants: Vec<String> },
    /// **Exact-decimal** numeric with fixed `precision`/`scale` and an optional
    /// unit (e.g. `kg`, `pct`). Floats are intentionally not representable.
    Numeric {
        precision: u32,
        scale: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unit: Option<String>,
    },
    /// Foreign key to another entity's managed primary key.
    Reference { entity: EntityId },
}

/// A secondary index on one or more of an entity's fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Index {
    /// Unique within the entity.
    pub name: String,
    /// The fields covered, in order (by [`Field::id`]).
    pub fields: Vec<FieldId>,
    /// `true` for a unique index.
    #[serde(default, skip_serializing_if = "is_false")]
    pub unique: bool,
}

/// A table-level constraint. Kept to neutral primitives — composite uniqueness
/// (the POC's `(receipt-no, supplier-id)`) and a boolean check. Opinionated
/// domain constraints belong in modules (D14).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Constraint {
    /// Composite (or single-column) uniqueness over the named fields.
    Unique { name: String, fields: Vec<FieldId> },
    /// A boolean check expression, interpreted by the DDL compiler (3.2).
    Check { name: String, expression: String },
}

impl Constraint {
    /// The constraint's name (unique within its entity).
    pub fn name(&self) -> &str {
        match self {
            Constraint::Unique { name, .. } | Constraint::Check { name, .. } => name,
        }
    }
}

/// A relation between entities — navigational metadata over the physical foreign
/// keys (a `Reference` [`Field`] is the FK column itself), used by the API
/// generator (4.1) for nested expansion and by the ERD (3.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Relation {
    /// Unique within the catalog.
    pub id: String,
    /// Logical name (editor / API accessor name).
    pub name: String,
    /// The relation's cardinality.
    pub cardinality: Cardinality,
    /// The owning / child side. For `one-to-many` this is the entity holding the
    /// foreign key; for `hierarchical` it is the self-referential entity (so
    /// `from == to`).
    pub from: EntityId,
    /// The referenced / parent side.
    pub to: EntityId,
    /// The foreign-key field on `from` backing the relation (a `Reference`
    /// field). Optional — the DDL compiler may manage it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_field: Option<FieldId>,
    /// The join entity for a `many-to-many` relation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub through: Option<EntityId>,
    /// Human-readable description (editor).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A relation's cardinality. `hierarchical` is a self-referential tree (the
/// closure/genealogy / asset-tree shape D14 requires industrial modules to be
/// able to express).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Cardinality {
    OneToMany,
    ManyToMany,
    Hierarchical,
}

impl Catalog {
    /// Parse a catalog from canonical JSON (import — the 3.4 promotion format).
    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }

    /// Serialize a catalog to canonical pretty JSON (export). Default-valued
    /// fields are omitted, so exported catalogs are minimal and re-import to an
    /// identical value (round-trip).
    pub fn to_json(&self) -> String {
        // Infallible for this type; a plain data struct never fails to encode.
        serde_json::to_string_pretty(self).expect("Catalog serializes")
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_debug_delegates_to_the_inner_string() {
        // Load-bearing invariant: validation messages print ids with `{:?}` (see
        // `validate.rs`), so an id must Debug as `"foo"`, NOT `EntityId("foo")` —
        // a derived Debug would silently rewrite every such message.
        assert_eq!(format!("{:?}", EntityId::from("foo")), "\"foo\"");
        assert_eq!(format!("{:?}", FieldId::from("bar")), "\"bar\"");
    }

    #[test]
    fn id_reads_as_the_plain_string() {
        let e = EntityId::from("orders");
        assert_eq!(e.to_string(), "orders"); // Display
        assert_eq!(e.as_str(), "orders"); // inherent
        assert_eq!(&*e, "orders"); // Deref<Target = str>
        assert_eq!(e.len(), 6); // via Deref to str
        assert!(e == "orders"); // PartialEq<&str>
        assert!(e == *"orders"); // PartialEq<str>
        let owned = String::from("orders");
        assert!(e == owned); // PartialEq<String>
        // From<String> and From<&str> agree.
        assert_eq!(EntityId::from(String::from("x")), EntityId::from("x"));
    }

    #[test]
    fn id_serializes_transparently_as_a_bare_string() {
        // `#[serde(transparent)]` round-trips as a plain JSON string; it also
        // drives schemars' transparent (inline, non-referenceable) derive that
        // keeps docs/catalog-model.schema.json byte-unchanged (see the
        // `committed_schema_matches_types` integration test).
        let e = EntityId::from("sites");
        assert_eq!(serde_json::to_string(&e).unwrap(), "\"sites\"");
        assert_eq!(
            serde_json::from_str::<EntityId>("\"sites\"").unwrap(),
            EntityId::from("sites")
        );
    }
}
