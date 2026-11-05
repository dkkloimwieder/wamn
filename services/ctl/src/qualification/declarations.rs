//! Application-owned cases, read beside each package's tests.

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use serde::Deserialize;
use wamn_schema_generator::{build::package_output, build_record::source_root};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Declaration {
    cases: Vec<Case>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub(super) struct Case {
    pub executable: String,
    pub name: String,
    pub evidence_env: String,
}

#[derive(Debug)]
enum DeclarationErrorKind {
    Read,
    Decode,
    Field,
}

#[derive(Debug)]
struct DeclarationError {
    kind: DeclarationErrorKind,
    file: PathBuf,
    source: anyhow::Error,
}

impl fmt::Display for DeclarationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} ({:?}): {}",
            self.file.display(),
            self.kind,
            self.source
        )
    }
}

impl std::error::Error for DeclarationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

fn read(file: &Path) -> Result<Vec<Case>, DeclarationError> {
    let error = |kind, source| DeclarationError {
        kind,
        file: file.to_owned(),
        source,
    };
    let bytes =
        fs::read(file).map_err(|source| error(DeclarationErrorKind::Read, source.into()))?;
    let declaration: Declaration = serde_json::from_slice(&bytes)
        .map_err(|source| error(DeclarationErrorKind::Decode, source.into()))?;
    let validate = || -> anyhow::Result<()> {
        for (index, case) in declaration.cases.iter().enumerate() {
            for (field, value) in [
                ("executable", &case.executable),
                ("name", &case.name),
                ("evidence_env", &case.evidence_env),
            ] {
                ensure!(
                    !value.trim().is_empty(),
                    "cases[{index}].{field}: expected a nonempty string"
                );
            }
            ensure!(
                !case.executable.contains(['/', '\\'])
                    && !matches!(case.executable.as_str(), "." | ".."),
                "cases[{index}].executable: expected an executable file name"
            );
            ensure!(
                case.evidence_env.bytes().enumerate().all(|(index, byte)| {
                    byte.is_ascii_alphabetic()
                        || byte == b'_'
                        || (index > 0 && byte.is_ascii_digit())
                }),
                "cases[{index}].evidence_env: expected an environment variable name"
            );
        }
        Ok(())
    };
    validate().map_err(|source| error(DeclarationErrorKind::Field, source))?;
    Ok(declaration.cases)
}

/// Union in lexical order of (executable, exact case name, evidence variable).
/// The build owner resolves package ids in both application source locations.
pub(super) fn declared_cases(repository: &Path, packages: &[String]) -> anyhow::Result<Vec<Case>> {
    let output_root = repository.join("apps/target/wamn");
    let mut cases = BTreeSet::new();
    for package in packages.iter().collect::<BTreeSet<_>>() {
        let source = source_root(&package_output(&output_root, package))?;
        let file = source.join("tests/qualification.json");
        cases
            .extend(read(&file).with_context(|| {
                format!("read qualification declaration for package {package}")
            })?);
    }
    ensure!(
        !cases.is_empty(),
        "the release declares no qualification cases"
    );
    Ok(cases.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qualification::TemporaryResults;
    use serde_json::{Value, json};

    fn cases(root: &Path, packages: &[&str]) -> anyhow::Result<Vec<Case>> {
        declared_cases(
            root,
            &packages
                .iter()
                .map(|package| (*package).to_owned())
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn application_declarations_preserve_the_three_release_sets() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        // The expected set of each release is the union, in lexical order,
        // of the declarations its packages carry.
        let declared = |packages: &[&str]| -> Vec<Case> {
            packages
                .iter()
                .flat_map(|package| {
                    read(&repository.join(format!("apps/{package}/tests/qualification.json")))
                        .unwrap()
                })
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect()
        };
        for packages in [
            &["wamn_receiving"][..],
            &["wamn_receiving", "client_acme_receiving"],
            &["client_acme_receiving", "wamn_receiving", "wamn_receiving"],
            &["wamn_wms"],
        ] {
            let expected = declared(packages);
            assert!(!expected.is_empty(), "{packages:?} declares cases");
            assert_eq!(
                cases(&repository, packages).unwrap(),
                expected,
                "{packages:?}"
            );
        }
    }

    fn write(root: &Path, package: &str, value: &Value) -> PathBuf {
        let file = root
            .join("apps")
            .join(package)
            .join("tests/qualification.json");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, serde_json::to_vec(value).unwrap()).unwrap();
        file
    }

    #[test]
    fn unfamiliar_packages_and_empty_contributions_need_no_platform_policy() {
        let root = TemporaryResults::create().unwrap();
        let entry = json!({"executable":"new-tests", "name":"new::case", "evidence_env":"NEW_EVIDENCE_DIR"});
        write(
            &root.path,
            "unfamiliar",
            &json!({"cases":[entry.clone(), entry]}),
        );
        write(&root.path, "empty", &json!({"cases":[]}));
        // The same build resolver also locates nested platform packages.
        write(&root.path, "platform/nested", &json!({"cases":[entry]}));
        let expected = vec![Case {
            executable: "new-tests".into(),
            name: "new::case".into(),
            evidence_env: "NEW_EVIDENCE_DIR".into(),
        }];
        assert_eq!(
            cases(&root.path, &["empty", "unfamiliar", "nested"]).unwrap(),
            expected
        );
        assert_eq!(cases(&root.path, &["unfamiliar"]).unwrap(), expected);
        assert!(
            cases(&root.path, &["empty"])
                .unwrap_err()
                .to_string()
                .contains("no qualification cases")
        );
        assert!(cases(&root.path, &[]).is_err());
    }

    #[test]
    fn missing_declarations_name_the_package_and_file() {
        let root = TemporaryResults::create().unwrap();
        let error = cases(&root.path, &["missing"]).unwrap_err();
        let error = format!("{error:#}");
        assert!(
            error.contains("package missing") && error.contains("missing/tests/qualification.json"),
            "{error}"
        );
    }

    #[test]
    fn malformed_declarations_name_the_file_and_field() {
        let root = TemporaryResults::create().unwrap();
        for (value, field) in [
            (json!({"cases":[], "extra":true}), "unknown field `extra`"),
            (
                json!({"cases":[{"executable":"tests", "evidence_env":"EVIDENCE"}]}),
                "missing field `name`",
            ),
            (
                json!({"cases":[{"executable":"tests", "name":"case", "evidence_env":"EVIDENCE", "extra":true}]}),
                "unknown field `extra`",
            ),
            (
                json!({"cases":[{"executable":" ", "name":"case", "evidence_env":"EVIDENCE"}]}),
                "cases[0].executable",
            ),
            (
                json!({"cases":[{"executable":"tests", "name":" ", "evidence_env":"EVIDENCE"}]}),
                "cases[0].name",
            ),
            (
                json!({"cases":[{"executable":"tests", "name":"case", "evidence_env":" "}]}),
                "cases[0].evidence_env",
            ),
            (
                json!({"cases":[{"executable":"../tests", "name":"case", "evidence_env":"EVIDENCE"}]}),
                "cases[0].executable",
            ),
            (
                json!({"cases":[{"executable":"tests", "name":"case", "evidence_env":"3BAD"}]}),
                "cases[0].evidence_env",
            ),
        ] {
            let file = write(&root.path, "broken", &value);
            let error = format!("{:#}", cases(&root.path, &["broken"]).unwrap_err());
            assert!(
                error.contains(&file.display().to_string()) && error.contains(field),
                "{error}"
            );
        }
        let file = write(&root.path, "broken", &json!({}));
        fs::write(&file, "{oops").unwrap();
        let error = read(&file).unwrap_err().to_string();
        assert!(
            error.contains(&file.display().to_string()) && error.contains("key must be a string"),
            "{error}"
        );
    }
}
