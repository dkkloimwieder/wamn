//! Resolve the KCL input in the same order as the package compiler.

use std::path::{Path, PathBuf};

pub fn binary(root: &Path, value: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let executable = |path: &Path| {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::metadata(path)
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    };
    let path = value("PATH");
    let resolve = |name: &Path| {
        if name.components().count() > 1 {
            let path = root.join(name);
            return executable(&path).then_some(path);
        }
        std::env::split_paths(path.as_ref()?)
            .map(|directory| root.join(directory).join(name))
            .find(|path| executable(path))
    };
    if let Some(path) = value("WAMN_KCL") {
        return resolve(Path::new(&path));
    }
    let pins: toml::Value = toml::from_str(include_str!("../../mise.toml")).ok()?;
    let version = pins["tools"]["http:kcl"]["version"].as_str()?;
    let local = root.join(".tools/kcl").join(version).join("kcl");
    if executable(&local) {
        return Some(local);
    }
    resolve(Path::new("kcl"))
}
