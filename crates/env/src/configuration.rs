//! Declared package configuration inputs.

/// One sequence default of a package
/// (docs/architecture/data-access.md#counters): its application name from the
/// environment map, its sequence name and its default pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SequenceDefault {
    pub app: wamn_registry::AppName,
    pub name: String,
    pub pattern: String,
}
