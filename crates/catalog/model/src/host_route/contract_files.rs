//! The resolved contract files of a host route set.
//!
//! A set's directory holds `contracts/<model>/<action>.<part>.json`, one file
//! per operation part that no other operation shares. A part that several
//! operations share is authored once as `shapes/<shape>.json`, and
//! `shapes.json` assigns each shape to its operation files by name. Equal
//! parts are never merged by inference: a part is shared only when
//! `shapes.json` says so.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::HostRouteSet;

/// Refusal to resolve the contract files of a host route set.
#[derive(Debug)]
pub struct HostRouteContractError {
    path: PathBuf,
    detail: Box<str>,
    source: Option<io::Error>,
}

impl HostRouteContractError {
    fn new(path: &Path, detail: impl Into<Box<str>>) -> Self {
        Self {
            path: path.to_owned(),
            detail: detail.into(),
            source: None,
        }
    }

    fn io(path: &Path, detail: &str, source: io::Error) -> Self {
        Self {
            path: path.to_owned(),
            detail: detail.into(),
            source: Some(source),
        }
    }
}

impl fmt::Display for HostRouteContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.path.display(), self.detail)
    }
}

impl std::error::Error for HostRouteContractError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|source| source as &(dyn std::error::Error + 'static))
    }
}

impl HostRouteSet {
    /// Every contract file of this set by its path below `contracts/`, such
    /// as `connection/get.result.json`, with each shared shape copied to the
    /// operation files that `shapes.json` assigns it to. The generator reads
    /// this map; the per-operation files exist only as its build output.
    pub fn contract_files(self) -> Result<BTreeMap<String, Vec<u8>>, HostRouteContractError> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/host_route");
        match self {
            Self::Application => resolve(&root),
            Self::Control => resolve(&root.join("control")),
        }
    }
}

/// Resolve the contract files of the set authored at `root`.
fn resolve(root: &Path) -> Result<BTreeMap<String, Vec<u8>>, HostRouteContractError> {
    let mut files = BTreeMap::new();
    read_tree(&root.join("contracts"), "", &mut files)?;

    let assignment_path = root.join("shapes.json");
    let assignment = fs::read(&assignment_path).map_err(|source| {
        HostRouteContractError::io(&assignment_path, "read the shape assignment", source)
    })?;
    let assignment: BTreeMap<String, Vec<String>> =
        serde_json::from_slice(&assignment).map_err(|error| {
            HostRouteContractError::new(
                &assignment_path,
                format!("the shape assignment maps each shape to its files: {error}"),
            )
        })?;

    let shape_root = root.join("shapes");
    let mut shapes = BTreeMap::new();
    read_tree(&shape_root, "", &mut shapes)?;
    for shape in shapes.keys() {
        let name = shape.strip_suffix(".json").unwrap_or(shape);
        if !assignment.contains_key(name) {
            return Err(HostRouteContractError::new(
                &shape_root.join(shape),
                "the shape is assigned to no operation",
            ));
        }
    }
    for (name, targets) in assignment {
        let Some(bytes) = shapes.get(&format!("{name}.json")) else {
            return Err(HostRouteContractError::new(
                &assignment_path,
                format!("the shape {name} has no file shapes/{name}.json"),
            ));
        };
        if targets.is_empty() {
            return Err(HostRouteContractError::new(
                &assignment_path,
                format!("the shape {name} is assigned to no operation"),
            ));
        }
        for target in targets {
            if files.insert(target.clone(), bytes.clone()).is_some() {
                return Err(HostRouteContractError::new(
                    &assignment_path,
                    format!("{target} has two sources: the shape {name} and another"),
                ));
            }
        }
    }
    Ok(files)
}

/// Read every file below `directory` into `files`, keyed by `/`-separated
/// paths relative to the first call's directory.
fn read_tree(
    directory: &Path,
    prefix: &str,
    files: &mut BTreeMap<String, Vec<u8>>,
) -> Result<(), HostRouteContractError> {
    let entries = fs::read_dir(directory)
        .map_err(|source| HostRouteContractError::io(directory, "read the directory", source))?;
    for entry in entries {
        let entry = entry.map_err(|source| {
            HostRouteContractError::io(directory, "read the directory", source)
        })?;
        let path = entry.path();
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            return Err(HostRouteContractError::new(
                &path,
                "the file name is not UTF-8",
            ));
        };
        let key = format!("{prefix}{name}");
        if path.is_dir() {
            read_tree(&path, &format!("{key}/"), files)?;
        } else {
            let bytes = fs::read(&path)
                .map_err(|source| HostRouteContractError::io(&path, "read the file", source))?;
            files.insert(key, bytes);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Directory(PathBuf);

    impl Directory {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "wamn-host-route-shapes-{name}-{}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(path.join("contracts/item")).unwrap();
            fs::create_dir_all(path.join("shapes")).unwrap();
            Self(path)
        }

        fn write(&self, relative: &str, contents: &str) {
            fs::write(self.0.join(relative), contents).unwrap();
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_shape_is_copied_to_each_assigned_operation_file() {
        let directory = Directory::new("copied");
        directory.write("contracts/item/get.result.json", "own\n");
        directory.write("shapes/item.errors.json", "shared\n");
        directory.write(
            "shapes.json",
            r#"{"item.errors": ["item/get.errors.json", "item/list.errors.json"]}"#,
        );
        let files = resolve(&directory.0).unwrap();
        assert_eq!(
            files,
            BTreeMap::from([
                ("item/get.errors.json".to_owned(), b"shared\n".to_vec()),
                ("item/get.result.json".to_owned(), b"own\n".to_vec()),
                ("item/list.errors.json".to_owned(), b"shared\n".to_vec()),
            ])
        );
    }

    #[test]
    fn an_operation_file_has_one_source() {
        let directory = Directory::new("twice");
        directory.write("contracts/item/get.errors.json", "own\n");
        directory.write("shapes/item.errors.json", "shared\n");
        directory.write(
            "shapes.json",
            r#"{"item.errors": ["item/get.errors.json"]}"#,
        );
        let error = resolve(&directory.0).unwrap_err().to_string();
        assert!(
            error.contains("item/get.errors.json has two sources"),
            "{error}"
        );
    }

    #[test]
    fn every_shape_is_assigned_and_every_assignment_has_a_shape() {
        let directory = Directory::new("unassigned");
        directory.write("shapes/item.errors.json", "shared\n");
        directory.write("shapes.json", "{}");
        let error = resolve(&directory.0).unwrap_err().to_string();
        assert!(error.contains("assigned to no operation"), "{error}");

        let directory = Directory::new("missing");
        directory.write(
            "shapes.json",
            r#"{"item.errors": ["item/get.errors.json"]}"#,
        );
        let error = resolve(&directory.0).unwrap_err().to_string();
        assert!(
            error.contains("has no file shapes/item.errors.json"),
            "{error}"
        );
    }

    /// Both sets resolve.
    #[test]
    fn the_host_route_sets_resolve() {
        for set in [HostRouteSet::Application, HostRouteSet::Control] {
            let files = set.contract_files().unwrap();
            assert!(files.keys().any(|key| key.ends_with(".operation.json")));
        }
    }
}
