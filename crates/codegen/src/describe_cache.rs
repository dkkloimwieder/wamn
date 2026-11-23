//! Committed evidence from the package verification database.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context as _, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use wamn_digest::{Digest, bytes_sha256, canonical_json_bytes};
use wamn_schema::ir::CatalogIr;

use crate::describe::DescribedStatement;
use crate::history::RelationColumn;
use wamn_package::manifest::source_name::SourceName;

const DIRECTORY: &str = ".wamn/describe";
const FILE: &str = "v1.json";
const POSTGRES_MAJOR: u32 = 18;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Evidence {
    pub inputs: Digest,
    pub postgres: u32,
    pub catalog: CatalogIr,
    pub statements: BTreeMap<String, DescribedStatement>,
    pub sql: BTreeMap<String, Digest>,
    pub columns: BTreeMap<SourceName, Vec<RelationColumn>>,
}

pub(crate) fn inputs(
    root: &Path,
    authored: &Value,
    bases: &[wamn_component_policy::PackageManifest],
    output_root: &Path,
) -> Result<Digest> {
    let mut migrations = BTreeMap::new();
    migration_inputs(root, "", &mut migrations)?;
    for base in bases {
        migration_inputs(
            &output_root.join(base.package.id.as_str()),
            base.package.id.as_str(),
            &mut migrations,
        )?;
    }
    let base_schemas = bases
        .iter()
        .flat_map(|base| base.models.values().map(|model| model.schema.as_str()))
        .collect::<std::collections::BTreeSet<_>>();
    let lock: toml::Value = toml::from_str(include_str!("../../../Cargo.lock"))?;
    // SQLx describes the statements, so its name, version and source decide
    // the evidence. Its dependency list changes with unrelated lock edits.
    let sqlx = lock["package"]
        .as_array()
        .context("read SQLx identity")?
        .iter()
        .filter(|package| {
            package["name"]
                .as_str()
                .is_some_and(|name| name.starts_with("sqlx"))
        })
        .map(|package| {
            json!({
                "name": package.get("name"),
                "version": package.get("version"),
                "source": package.get("source"),
            })
        })
        .collect::<Vec<_>>();
    // Catalog selection and verification scaffolding come from KCL. The
    // resulting SQL corpus supplies the keys for every statement description.
    let models = authored["models"]
        .as_object()
        .context("models declaration")?
        .iter()
        .map(|(name, model)| {
            (
                name,
                [
                    &model["schema"],
                    &model["table"],
                    &model["owner"],
                    &model["audit_log"],
                ],
            )
        })
        .collect::<BTreeMap<_, _>>();
    let internal = authored
        .get("internal_relations")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|relations| relations.values().map(|relation| &relation["table"]))
        .collect::<Vec<_>>();
    let value = json!([
        crate::build::GENERATOR_ID,
        POSTGRES_MAJOR,
        sqlx,
        migrations,
        base_schemas,
        &authored["package"]["id"],
        models,
        internal,
    ]);
    Ok(bytes_sha256(&canonical_json_bytes(&value)))
}

fn migration_inputs(root: &Path, owner: &str, files: &mut BTreeMap<String, Digest>) -> Result<()> {
    for directory in ["migrations", "history"] {
        let entries = match fs::read_dir(root.join(directory)) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|extension| extension == "sql") {
                let name = entry.file_name();
                let name = name.to_str().context("migration filename is not UTF-8")?;
                files.insert(
                    format!("{owner}/{directory}/{name}"),
                    bytes_sha256(&fs::read(path)?),
                );
            }
        }
    }
    Ok(())
}

pub(crate) fn postgres_identity(version: u32) -> Result<()> {
    ensure!(
        version / 10_000 == POSTGRES_MAJOR,
        "describe requires PostgreSQL {POSTGRES_MAJOR}, found {version}"
    );
    Ok(())
}

/// Refuse missing or extra entries in the committed evidence directory.
pub(crate) fn read(root: &Path) -> Result<Vec<u8>> {
    let directory = root.join(DIRECTORY);
    let path = directory.join(FILE);
    for entry in fs::read_dir(&directory).with_context(|| {
        format!(
            "read {}; run wamn build --refresh-describe",
            directory.display()
        )
    })? {
        let entry = entry?;
        ensure!(
            entry.file_name() == FILE && entry.file_type()?.is_file(),
            "unexpected describe cache entry {}",
            entry.path().display()
        );
    }
    fs::read(&path)
        .with_context(|| format!("read {}; run wamn build --refresh-describe", path.display()))
}

pub(crate) fn decode(bytes: &[u8], input: Digest) -> Result<Evidence> {
    let evidence: Evidence = serde_json::from_slice(bytes).context("decode describe evidence")?;
    ensure!(
        evidence.inputs == input,
        "stale describe cache; run wamn build --refresh-describe"
    );
    postgres_identity(evidence.postgres)?;
    ensure!(
        encode(&evidence)? == bytes,
        "describe cache is not canonical; run wamn build --refresh-describe"
    );
    Ok(evidence)
}

pub(crate) fn encode(evidence: &Evidence) -> Result<Vec<u8>> {
    Ok(canonical_json_bytes(&serde_json::to_value(evidence)?))
}

pub(crate) fn write(root: &Path, bytes: &[u8]) -> Result<()> {
    let directory = root.join(DIRECTORY);
    fs::create_dir_all(&directory)?;
    // The cache has one file, so refresh can replace it atomically.
    let path = directory.join(FILE);
    if !fs::read(&path).is_ok_and(|existing| existing == bytes) {
        crate::fingerprint::write_atomic(&path, bytes)?;
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_name() != FILE {
            if entry.file_type()?.is_dir() {
                fs::remove_dir_all(entry.path())?;
            } else {
                fs::remove_file(entry.path())?;
            }
        }
    }
    Ok(())
}
