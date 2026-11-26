//! Migration observations consumed by deploy and status.

/// A migration version and its SQLx checksum.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MigrationFile {
    pub version: i64,
    pub checksum: String,
}

/// SQLx history compared with the selected immutable migration files.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SchemaReport {
    pub installed: Vec<MigrationFile>,
    pub pending: Vec<MigrationFile>,
    pub missing: Vec<MigrationFile>,
    pub warnings: Vec<String>,
}

impl std::fmt::Display for SchemaReport {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (label, files) in [
            ("installed", &self.installed),
            ("pending", &self.pending),
            ("missing", &self.missing),
        ] {
            writeln!(output, "{label} migrations:")?;
            for file in files {
                writeln!(output, "  {} {}", file.version, file.checksum)?;
            }
        }
        for warning in &self.warnings {
            writeln!(output, "warning: {warning}")?;
        }
        Ok(())
    }
}
