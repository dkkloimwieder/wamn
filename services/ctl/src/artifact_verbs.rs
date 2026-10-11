//! `wamn inspect` and `wamn diff`: read layer 0 of pushed package artifacts
//! (docs/plan/platform-deploy.md §7.2).
//!
//! Layer 0 of a package artifact is the canonical `wamn.json` of the package.
//! Both verbs read the verified artifact that `catalog.package_artifacts`
//! records, through `wamn-package-artifact`, so each layer is checked against
//! its digest.

use std::fmt::Write as _;

use anyhow::{Context as _, ensure};
use clap::Args;
use serde_json::Value;
use wamn_control::package_artifact::PACKAGE_MANIFEST_MEDIA_TYPE;
use wamn_package_artifact::{PackageRegistry, Repository, verified_digest};

use crate::package_verbs::RegistryArgs;

/// Arguments of `wamn inspect`.
#[derive(Debug, Args)]
pub struct InspectArgs {
    /// Package artifact `<package_id>-<version>` that `push-package` pushed.
    pub package: String,
    /// The registry flags of `push-package`.
    #[command(flatten)]
    pub registry: RegistryArgs,
}

/// Arguments of `wamn diff`.
#[derive(Debug, Args)]
pub struct DiffArgs {
    /// The first package artifact `<package_id>-<version>`.
    pub from: String,
    /// The second package artifact `<package_id>-<version>`.
    pub to: String,
    /// The registry flags of `push-package`.
    #[command(flatten)]
    pub registry: RegistryArgs,
}

/// Print layer 0 of the package artifact, its exact bytes.
///
/// # Errors
///
/// When the artifact is not verified or cannot be pulled.
pub async fn inspect(args: InspectArgs) -> anyhow::Result<()> {
    let manifest = layer_zero(&args.registry.registry(), &args.package).await?;
    println!(
        "{}",
        String::from_utf8(manifest).context("layer 0 is not UTF-8")?
    );
    Ok(())
}

/// Print every JSON pointer where layer 0 of the two artifacts differs.
///
/// # Errors
///
/// When an artifact is not verified, cannot be pulled, or its layer 0 is not
/// JSON.
pub async fn diff(args: DiffArgs) -> anyhow::Result<()> {
    let registry = args.registry.registry();
    let mut documents = Vec::new();
    for package in [&args.from, &args.to] {
        let bytes = layer_zero(&registry, package).await?;
        documents.push(
            serde_json::from_slice::<Value>(&bytes)
                .with_context(|| format!("parse layer 0 of {package}"))?,
        );
    }
    print!("{}", diff_documents(&documents[0], &documents[1]));
    Ok(())
}

/// Layer 0 of the verified artifact `package` (`<package_id>-<version>`).
async fn layer_zero(registry: &PackageRegistry, package: &str) -> anyhow::Result<Vec<u8>> {
    let (package_id, version) = package
        .split_once('-')
        .with_context(|| format!("{package} is not <package_id>-<version>"))?;
    let digest = verified_digest(registry, package_id, version).await?;
    let layers = Repository::open(registry, package)?.pull(&digest).await?;
    let first = layers
        .into_iter()
        .next()
        .with_context(|| format!("package artifact {package} has no layers"))?;
    ensure!(
        first.media_type == PACKAGE_MANIFEST_MEDIA_TYPE,
        "layer 0 of package artifact {package} is {}, not {PACKAGE_MANIFEST_MEDIA_TYPE}",
        first.media_type
    );
    Ok(first.bytes)
}

/// One line per JSON pointer where `from` and `to` differ: `- <pointer> <old>`
/// for a removed value, `+ <pointer> <new>` for an added one, and both for a
/// changed one. Objects and arrays are compared member by member.
fn diff_documents(from: &Value, to: &Value) -> String {
    let mut lines = String::new();
    diff_values("", Some(from), Some(to), &mut lines);
    lines
}

fn diff_values(pointer: &str, from: Option<&Value>, to: Option<&Value>, lines: &mut String) {
    match (from, to) {
        (Some(Value::Object(left)), Some(Value::Object(right))) => {
            let mut keys = left.keys().chain(right.keys()).collect::<Vec<_>>();
            keys.sort();
            keys.dedup();
            for key in keys {
                let escaped = key.replace('~', "~0").replace('/', "~1");
                diff_values(
                    &format!("{pointer}/{escaped}"),
                    left.get(key),
                    right.get(key),
                    lines,
                );
            }
        }
        (Some(Value::Array(left)), Some(Value::Array(right))) => {
            for index in 0..left.len().max(right.len()) {
                diff_values(
                    &format!("{pointer}/{index}"),
                    left.get(index),
                    right.get(index),
                    lines,
                );
            }
        }
        (left, right) if left == right => {}
        (left, right) => {
            if let Some(value) = left {
                let _ = writeln!(lines, "- {pointer} {value}");
            }
            if let Some(value) = right {
                let _ = writeln!(lines, "+ {pointer} {value}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::diff_documents;

    #[test]
    fn the_diff_names_each_changed_pointer() {
        let from = json!({"package": {"id": "a", "version": "1.0.0"}, "tables": ["x", "y"], "a/b": 1});
        let to = json!({"package": {"id": "a", "version": "1.1.0"}, "tables": ["x"], "c": true, "a/b": 1});
        assert_eq!(
            diff_documents(&from, &to),
            "+ /c true\n\
             - /package/version \"1.0.0\"\n\
             + /package/version \"1.1.0\"\n\
             - /tables/1 \"y\"\n"
        );
        assert_eq!(diff_documents(&from, &from), "");
    }
}
